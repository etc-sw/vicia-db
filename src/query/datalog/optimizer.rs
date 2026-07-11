//! Query optimizer: index selection and join ordering for Datalog patterns.
//!
//! `plan()` is the single entry point. It assigns an `IndexHint` to each
//! pattern and (outside the `wasm` feature) sorts patterns by selectivity.

use crate::query::datalog::types::{AttributeSpec, EdnValue, Expr, Pattern, WhereClause};

/// A real fact pattern and its following per-fact pseudo-attribute patterns
/// form one join-ordering unit. The matcher keeps the selected fact's temporal
/// metadata in hidden bindings; moving another real pattern between the fact
/// and those pseudo patterns would overwrite that metadata. Expressions may
/// still be pushed between entries because they do not replace hidden metadata.
struct FactPatternBundle {
    entries: Vec<(WhereClause, IndexHint)>,
    /// The exact entity component of the real pattern that owns this bundle.
    /// `None` denotes a standalone pseudo pattern, which has no fact to attach to.
    fact_entity: Option<EdnValue>,
}

impl FactPatternBundle {
    fn new(clause: WhereClause, hint: IndexHint) -> Self {
        let fact_entity = match &clause {
            WhereClause::Pattern(Pattern {
                entity,
                attribute: AttributeSpec::Real(_),
                ..
            }) => Some(entity.clone()),
            _ => None,
        };
        Self {
            entries: vec![(clause, hint)],
            fact_entity,
        }
    }

    fn accepts_per_fact_pseudo(&self, pattern: &Pattern) -> bool {
        matches!(
            &pattern.attribute,
            AttributeSpec::Pseudo(pseudo) if pseudo.is_per_fact()
        ) && self.fact_entity.as_ref() == Some(&pattern.entity)
    }

    #[cfg(not(feature = "wasm"))]
    fn selectivity(&self) -> u8 {
        self.entries
            .first()
            .and_then(|(clause, _)| match clause {
                WhereClause::Pattern(pattern) => Some(selectivity_score(pattern)),
                _ => None,
            })
            .unwrap_or(0)
    }
}

/// Which covering index to use for a given pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexHint {
    /// EAVT: entity-first scan. Also used when nothing is bound (full scan).
    Eavt,
    /// AEVT: attribute-first scan.
    Aevt,
    /// AVET: attribute + value equality / range lookup.
    Avet,
    /// VAET: reverse reference lookup (Ref value only, no attribute).
    Vaet,
}

/// Return true if the component is a logic variable (unbound).
fn is_variable(v: &EdnValue) -> bool {
    v.is_variable()
}

/// Return true if the value component is a bound entity literal (UUID/Ref).
fn is_entity_literal(v: &EdnValue) -> bool {
    matches!(v, EdnValue::Uuid(_))
}

/// Return true if the attribute is a bound (non-variable) real attribute.
/// Pseudo-attributes are never index-bound (they are not stored attributes).
fn attr_is_index_bound(a: &AttributeSpec) -> bool {
    match a {
        AttributeSpec::Real(edn) => !is_variable(edn),
        AttributeSpec::Pseudo(_) => false,
    }
}

/// Count the number of non-variable components in a pattern.
/// Higher score = more selective.
///
/// Gated on non-wasm: WASM/browser targets typically work with small datasets where
/// the overhead of computing scores and sorting can equal or exceed the benefit.
/// Source order is preserved on WASM for deterministic, debuggable query behaviour.
#[cfg(not(feature = "wasm"))]
fn selectivity_score(p: &Pattern) -> u8 {
    let e = !is_variable(&p.entity);
    let a = attr_is_index_bound(&p.attribute);
    let v = !is_variable(&p.value);
    (e as u8).saturating_add(a as u8).saturating_add(v as u8)
}

/// Select the most efficient index for a single pattern.
///
/// Selection table:
///   Entity bound (± anything)         → EAVT
///   Attribute + Value (any non-Var)    → AVET
///   Attribute only                     → AEVT
///   Value is entity literal, no attr   → VAET (reverse traversal)
///   Nothing bound                      → EAVT (full scan)
pub fn select_index(p: &Pattern) -> IndexHint {
    let e_bound = !is_variable(&p.entity);
    let a_bound = attr_is_index_bound(&p.attribute);
    let v_bound = !is_variable(&p.value);

    if e_bound {
        return IndexHint::Eavt;
    }
    if a_bound && v_bound {
        return IndexHint::Avet;
    }
    if a_bound {
        return IndexHint::Aevt;
    }
    if v_bound && is_entity_literal(&p.value) {
        return IndexHint::Vaet;
    }
    // Nothing bound: full scan through EAVT
    IndexHint::Eavt
}

/// Collect all logic-variable names (`?foo`) referenced in an Expr tree.
fn expr_vars(expr: &Expr) -> Vec<String> {
    match expr {
        Expr::Var(s) => vec![s.clone()],
        Expr::Lit(_) | Expr::Slot(_) => vec![],
        Expr::BinOp(_, l, r) => {
            let mut vars = expr_vars(l);
            vars.extend(expr_vars(r));
            vars
        }
        Expr::UnaryOp(_, inner) => expr_vars(inner),
    }
}

/// Collect the logic-variable names bound (output) by a Pattern.
/// Only Symbol values starting with `?` count — literals never bind.
fn pattern_bound_vars(p: &Pattern) -> Vec<String> {
    let mut vars = Vec::new();
    if is_variable(&p.entity)
        && let EdnValue::Symbol(s) = &p.entity
    {
        vars.push(s.clone());
    }
    if let AttributeSpec::Real(attr) = &p.attribute
        && is_variable(attr)
        && let EdnValue::Symbol(s) = attr
    {
        vars.push(s.clone());
    }
    if is_variable(&p.value)
        && let EdnValue::Symbol(s) = &p.value
    {
        vars.push(s.clone());
    }
    vars
}

/// Plan a list of where clauses: assign index hints to Pattern entries, push Expr
/// entries to the earliest position where all their variables are bound by preceding
/// patterns, and (non-wasm) sort patterns by selectivity.
///
/// Only `WhereClause::Pattern` and `WhereClause::Expr` variants should be passed in.
/// `Not`, `NotJoin`, `Or`, `OrJoin`, and `RuleInvocation` variants are handled by
/// the executor/evaluator and must not appear here.
///
/// Returns an interleaved `Vec<(WhereClause, Option<IndexHint>)>` where Pattern entries
/// carry `Some(hint)` and Expr entries carry `None`.
pub fn plan(
    clauses: Vec<WhereClause>,
    _indexes: &crate::storage::index::Indexes,
) -> Vec<(WhereClause, Option<IndexHint>)> {
    // Separate patterns into semantic fact bundles and collect expressions.
    // A bundle is the smallest unit the selectivity sorter may move.
    let mut bundles: Vec<FactPatternBundle> = Vec::new();
    let mut exprs: Vec<WhereClause> = Vec::new();
    let mut open_fact_bundle: Option<usize> = None;

    for clause in clauses {
        match &clause {
            WhereClause::Pattern(p) => {
                let hint = select_index(p);
                if open_fact_bundle.is_some_and(|bundle_index| {
                    bundles
                        .get(bundle_index)
                        .is_some_and(|bundle| bundle.accepts_per_fact_pseudo(p))
                }) {
                    if let Some(bundle) =
                        open_fact_bundle.and_then(|bundle_index| bundles.get_mut(bundle_index))
                    {
                        bundle.entries.push((clause, hint));
                    }
                } else {
                    bundles.push(FactPatternBundle::new(clause, hint));
                    open_fact_bundle = bundles
                        .last()
                        .and_then(|bundle| bundle.fact_entity.as_ref())
                        .map(|_| bundles.len() - 1);
                }
            }
            WhereClause::Expr { .. } => {
                // Expressions do not overwrite the hidden fact metadata. Keep
                // the bundle open and schedule the expression at its earliest
                // dependency-safe pattern position below.
                exprs.push(clause);
            }
            // Other variants must not be passed to plan(); silently skip.
            _ => open_fact_bundle = None,
        }
    }

    // Stable sort fact bundles by their real pattern's selectivity descending
    // (non-wasm only). Clauses inside a bundle always retain source order.
    // Ties retain bundle source order, ensuring deterministic output.
    // WASM omission: see selectivity_score() — small datasets, determinism.
    #[cfg(not(feature = "wasm"))]
    bundles.sort_by_key(|bundle| std::cmp::Reverse(bundle.selectivity()));

    // Schedule each Expr after the earliest individual pattern where all its
    // inputs are bound. Expressions may sit inside a fact bundle: only another
    // real pattern can overwrite the hidden metadata the bundle protects.
    let mut exprs_after_entry: Vec<Vec<Vec<WhereClause>>> = bundles
        .iter()
        .map(|bundle| (0..bundle.entries.len()).map(|_| Vec::new()).collect())
        .collect();
    let mut trailing_exprs: Vec<WhereClause> = Vec::new();
    for expr_clause in exprs {
        let vars: std::collections::HashSet<String> =
            if let WhereClause::Expr { expr, .. } = &expr_clause {
                expr_vars(expr).into_iter().collect()
            } else {
                Default::default()
            };

        let mut bound: std::collections::HashSet<String> = Default::default();
        // Default: append after the final bundle. This covers no-var expressions
        // and variables never bound by a pattern.
        let mut insertion: Option<(usize, usize)> = None;

        if !vars.is_empty() {
            for (bundle_index, bundle) in bundles.iter().enumerate() {
                for (entry_index, (clause, _)) in bundle.entries.iter().enumerate() {
                    if let WhereClause::Pattern(pattern) = clause {
                        bound.extend(pattern_bound_vars(pattern));
                    }
                    if vars.is_subset(&bound) {
                        insertion = Some((bundle_index, entry_index));
                        break;
                    }
                }
                if insertion.is_some() {
                    break;
                }
            }
        }

        if let Some((bundle_index, entry_index)) = insertion
            && let Some(scheduled) = exprs_after_entry
                .get_mut(bundle_index)
                .and_then(|entries| entries.get_mut(entry_index))
        {
            scheduled.push(expr_clause);
        } else {
            trailing_exprs.push(expr_clause);
        }
    }

    let mut result = Vec::new();
    for (bundle_index, bundle) in bundles.into_iter().enumerate() {
        for (entry_index, (clause, hint)) in bundle.entries.into_iter().enumerate() {
            result.push((clause, Some(hint)));
            if let Some(scheduled) = exprs_after_entry
                .get_mut(bundle_index)
                .and_then(|entries| entries.get_mut(entry_index))
            {
                result.extend(scheduled.drain(..).map(|clause| (clause, None)));
            }
        }
    }
    result.extend(trailing_exprs.into_iter().map(|clause| (clause, None)));

    result
}

/// Static 4-tier cardinality estimate for a single pattern.
///
/// Derived from selectivity_score but returns u64 cost (lower = cheaper) rather
/// than a selectivity score. Available on all targets; on WASM the dead_code lint
/// is suppressed because the sorting call-sites are omitted there.
#[cfg_attr(feature = "wasm", allow(dead_code))]
fn pattern_cost(p: &Pattern) -> u64 {
    let e = !is_variable(&p.entity);
    let a = attr_is_index_bound(&p.attribute);
    let v = !is_variable(&p.value);
    match (e as u8) + (a as u8) + (v as u8) {
        3 => 1,
        2 => 10,
        1 => 100,
        _ => 10_000,
    }
}

/// Estimated cost for a body/branch slice — the minimum `pattern_cost` across all
/// Pattern clauses, or 0 if the body contains no patterns (expr-only bodies are
/// cheap pure computation).
///
/// Rationale for `min`: In a multi-pattern join the most selective pattern dominates —
/// the join cannot produce more rows than the smallest input.
///
/// Available on all targets; on WASM the dead_code lint is suppressed because
/// the sorting call-sites are omitted there.
#[cfg_attr(feature = "wasm", allow(dead_code))]
pub fn branch_cost(branch: &[WhereClause]) -> u64 {
    branch
        .iter()
        .filter_map(|c| {
            if let WhereClause::Pattern(p) = c {
                Some(pattern_cost(p))
            } else {
                None
            }
        })
        .min()
        .unwrap_or(0)
}

/// Estimated evaluation cost for any `WhereClause`.
///
/// | Clause type        | Cost |
/// |--------------------|------|
/// | `Pattern`          | `pattern_cost(p)` |
/// | `Expr`             | 0 (pure computation) |
/// | `Not(body)`        | `branch_cost(body)` |
/// | `NotJoin{clauses}` | `branch_cost(clauses)` |
/// | `Or(branches)`     | sum of `branch_cost` per branch |
/// | `OrJoin{branches}` | sum of `branch_cost` per branch |
/// | other              | `u64::MAX` (defensive; not expected in practice) |
///
/// Available on all targets; on WASM the dead_code lint is suppressed because
/// the sorting call-sites are omitted there.
#[cfg_attr(feature = "wasm", allow(dead_code))]
pub fn clause_cost(clause: &WhereClause) -> u64 {
    match clause {
        WhereClause::Pattern(p) => pattern_cost(p),
        WhereClause::Expr { .. } => 0,
        WhereClause::Not(body) => branch_cost(body),
        WhereClause::NotJoin { clauses, .. } => branch_cost(clauses),
        WhereClause::Or(branches) => branches.iter().map(|b| branch_cost(b)).sum(),
        WhereClause::OrJoin { branches, .. } => branches.iter().map(|b| branch_cost(b)).sum(),
        _ => u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::Value;
    #[cfg(not(feature = "wasm"))]
    use crate::query::datalog::types::PseudoAttr;
    use crate::query::datalog::types::{BinOp, EdnValue, Expr, Pattern, WhereClause};
    use uuid::Uuid;

    fn make_pattern(entity: EdnValue, attribute: EdnValue, value: EdnValue) -> Pattern {
        Pattern::new(entity, attribute, value)
    }

    fn var(s: &str) -> EdnValue {
        EdnValue::Symbol(format!("?{s}"))
    }
    fn kw(s: &str) -> EdnValue {
        EdnValue::Keyword(s.to_string())
    }
    fn str_val(s: &str) -> EdnValue {
        EdnValue::String(s.to_string())
    }
    fn entity_lit() -> EdnValue {
        EdnValue::Uuid(Uuid::new_v4())
    }

    #[test]
    fn test_entity_bound_selects_eavt() {
        let p = make_pattern(entity_lit(), var("a"), var("v"));
        assert_eq!(select_index(&p), IndexHint::Eavt);
    }

    #[test]
    fn test_entity_and_attr_bound_selects_eavt() {
        let p = make_pattern(entity_lit(), kw(":name"), var("v"));
        assert_eq!(select_index(&p), IndexHint::Eavt);
    }

    #[test]
    fn test_attr_and_value_bound_selects_avet() {
        let p = make_pattern(var("e"), kw(":name"), str_val("Alice"));
        assert_eq!(select_index(&p), IndexHint::Avet);
    }

    #[test]
    fn test_attr_and_ref_bound_selects_avet() {
        // A UUID value with a bound attribute → AVET (not VAET, because attr is bound)
        let p = make_pattern(var("e"), kw(":friend"), entity_lit());
        assert_eq!(select_index(&p), IndexHint::Avet);
    }

    #[test]
    fn test_attr_only_selects_aevt() {
        let p = make_pattern(var("e"), kw(":name"), var("v"));
        assert_eq!(select_index(&p), IndexHint::Aevt);
    }

    #[test]
    fn test_ref_only_selects_vaet() {
        // UUID value but no bound attribute → VAET
        let p = make_pattern(var("e"), var("a"), entity_lit());
        assert_eq!(select_index(&p), IndexHint::Vaet);
    }

    #[test]
    fn test_nothing_bound_selects_eavt_full_scan() {
        let p = make_pattern(var("e"), var("a"), var("v"));
        assert_eq!(select_index(&p), IndexHint::Eavt);
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_join_ordering_moves_selective_pattern_first() {
        use crate::storage::index::Indexes;
        let p1 = make_pattern(var("e"), kw(":age"), var("a")); // selectivity 1 (attr only)
        let p2 = make_pattern(entity_lit(), kw(":name"), var("v")); // selectivity 2 (entity + attr)
        let p1_attr = p1.attribute.clone();
        let p2_attr = p2.attribute.clone();
        let planned = plan(
            vec![WhereClause::Pattern(p1), WhereClause::Pattern(p2)],
            &Indexes::new(),
        );
        let first_attr = match &planned[0].0 {
            WhereClause::Pattern(p) => p.attribute.clone(),
            _ => panic!("expected Pattern at index 0"),
        };
        let second_attr = match &planned[1].0 {
            WhereClause::Pattern(p) => p.attribute.clone(),
            _ => panic!("expected Pattern at index 1"),
        };
        assert_ne!(
            first_attr, p1_attr,
            "Lower-selectivity pattern must not be first"
        );
        assert_eq!(
            first_attr, p2_attr,
            "Higher-selectivity pattern must be first"
        );
        assert_eq!(
            second_attr, p1_attr,
            "Lower-selectivity pattern must be second"
        );
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_join_ordering_keeps_per_fact_metadata_with_real_pattern() {
        use crate::storage::index::Indexes;

        let entity = var("e");
        let low_selectivity_real = WhereClause::Pattern(make_pattern(
            entity.clone(),
            kw(":event/first"),
            var("first"),
        ));
        let first_tx = WhereClause::Pattern(Pattern::pseudo(
            entity.clone(),
            PseudoAttr::TxCount,
            var("first_tx"),
        ));
        let high_selectivity_real = WhereClause::Pattern(make_pattern(
            entity.clone(),
            kw(":event/second"),
            str_val("two"),
        ));
        let second_tx = WhereClause::Pattern(Pattern::pseudo(
            entity,
            PseudoAttr::TxCount,
            var("second_tx"),
        ));

        let planned = plan(
            vec![
                low_selectivity_real,
                first_tx,
                high_selectivity_real,
                second_tx,
            ],
            &Indexes::new(),
        );

        let attributes: Vec<AttributeSpec> = planned
            .into_iter()
            .filter_map(|(clause, _)| match clause {
                WhereClause::Pattern(pattern) => Some(pattern.attribute),
                _ => None,
            })
            .collect();
        assert_eq!(
            attributes,
            vec![
                AttributeSpec::Real(kw(":event/second")),
                AttributeSpec::Pseudo(PseudoAttr::TxCount),
                AttributeSpec::Real(kw(":event/first")),
                AttributeSpec::Pseudo(PseudoAttr::TxCount),
            ],
            "selectivity sorting must move complete fact-metadata bundles"
        );
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_expr_pushdown_inside_bundle_preserves_real_pseudo_order() {
        use crate::storage::index::Indexes;

        let entity = var("e");
        let real = WhereClause::Pattern(make_pattern(entity.clone(), kw(":event/id"), var("id")));
        let tx_count = WhereClause::Pattern(Pattern::pseudo(
            entity.clone(),
            PseudoAttr::TxCount,
            var("tx_count"),
        ));
        let tx_id = WhereClause::Pattern(Pattern::pseudo(entity, PseudoAttr::TxId, var("tx_id")));
        let expr = WhereClause::Expr {
            expr: Expr::UnaryOp(
                crate::query::datalog::types::UnaryOp::StringQ,
                Box::new(Expr::Var("?id".to_string())),
            ),
            binding: None,
        };

        let planned = plan(vec![real, tx_count, tx_id, expr], &Indexes::new());
        assert!(matches!(
            planned.as_slice(),
            [
                (WhereClause::Pattern(_), Some(_)),
                (WhereClause::Expr { .. }, None),
                (WhereClause::Pattern(_), Some(_)),
                (WhereClause::Pattern(_), Some(_))
            ]
        ));
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_expr_between_real_and_pseudo_remains_a_bundle_boundary() {
        use crate::storage::index::Indexes;

        let entity = kw(":expr-meta");
        let real = WhereClause::Pattern(make_pattern(entity.clone(), kw(":value/n"), var("n")));
        let expr = WhereClause::Expr {
            expr: Expr::BinOp(
                BinOp::Add,
                Box::new(Expr::Var("?n".to_string())),
                Box::new(Expr::Lit(Value::Integer(1))),
            ),
            binding: Some("?expected".to_string()),
        };
        let tx_count = WhereClause::Pattern(Pattern::pseudo(
            entity,
            PseudoAttr::TxCount,
            var("expected"),
        ));

        let planned = plan(vec![real, expr, tx_count], &Indexes::new());
        assert!(matches!(
            planned.as_slice(),
            [
                (WhereClause::Pattern(_), Some(_)),
                (WhereClause::Expr { .. }, None),
                (WhereClause::Pattern(_), Some(_))
            ]
        ));
    }

    // ── expr_vars() ──────────────────────────────────────────────────────────

    #[test]
    fn test_expr_vars_var() {
        let e = Expr::Var("?age".to_string());
        assert_eq!(expr_vars(&e), vec!["?age".to_string()]);
    }

    #[test]
    fn test_expr_vars_lit_is_empty() {
        let e = Expr::Lit(Value::Integer(42));
        assert!(expr_vars(&e).is_empty());
    }

    #[test]
    fn test_expr_vars_binop() {
        let e = Expr::BinOp(
            BinOp::Gt,
            Box::new(Expr::Var("?age".to_string())),
            Box::new(Expr::Lit(Value::Integer(30))),
        );
        assert_eq!(expr_vars(&e), vec!["?age".to_string()]);
    }

    #[test]
    fn test_expr_vars_nested_binop_collects_all() {
        // (> (+ ?a ?b) ?c)
        let e = Expr::BinOp(
            BinOp::Gt,
            Box::new(Expr::BinOp(
                BinOp::Add,
                Box::new(Expr::Var("?a".to_string())),
                Box::new(Expr::Var("?b".to_string())),
            )),
            Box::new(Expr::Var("?c".to_string())),
        );
        let vars = expr_vars(&e);
        assert!(vars.contains(&"?a".to_string()));
        assert!(vars.contains(&"?b".to_string()));
        assert!(vars.contains(&"?c".to_string()));
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn test_expr_vars_unary_op() {
        use crate::query::datalog::types::UnaryOp;
        let e = Expr::UnaryOp(UnaryOp::IntegerQ, Box::new(Expr::Var("?v".to_string())));
        assert_eq!(expr_vars(&e), vec!["?v".to_string()]);
    }

    // ── plan() — new signature and push-down ─────────────────────────────────

    #[test]
    fn test_plan_pattern_carries_some_hint() {
        #[cfg(not(feature = "wasm"))]
        {
            use crate::storage::index::Indexes;
            let p = WhereClause::Pattern(make_pattern(var("e"), kw(":val"), var("v")));
            let planned = plan(vec![p], &Indexes::new());
            assert!(
                planned[0].1.is_some(),
                "Pattern entry must carry Some(IndexHint)"
            );
        }
    }

    #[test]
    fn test_plan_expr_carries_none_hint() {
        #[cfg(not(feature = "wasm"))]
        {
            use crate::storage::index::Indexes;
            let p = WhereClause::Pattern(make_pattern(var("e"), kw(":val"), var("v")));
            let expr = WhereClause::Expr {
                expr: Expr::Lit(Value::Boolean(true)),
                binding: None,
            };
            let planned = plan(vec![p, expr], &Indexes::new());
            let expr_entry = planned
                .iter()
                .find(|(c, _)| matches!(c, WhereClause::Expr { .. }));
            assert!(expr_entry.is_some());
            assert!(
                expr_entry.unwrap().1.is_none(),
                "Expr entry must carry None hint"
            );
        }
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_expr_pushed_after_binding_pattern() {
        use crate::storage::index::Indexes;
        // Three patterns with equal selectivity (1 attr bound each) — stable sort preserves
        // original order: [p1, p2, p3]. Expr needs ?v, bound by p2 (pos 1).
        // Expected output: [p1, p2, expr, p3].
        let p1 = WhereClause::Pattern(make_pattern(var("e"), kw(":name"), var("n")));
        let p2 = WhereClause::Pattern(make_pattern(var("e"), kw(":val"), var("v")));
        let p3 = WhereClause::Pattern(make_pattern(var("e"), kw(":dept"), var("d")));
        let expr = WhereClause::Expr {
            expr: Expr::BinOp(
                BinOp::Gt,
                Box::new(Expr::Var("?v".to_string())),
                Box::new(Expr::Lit(Value::Integer(30))),
            ),
            binding: None,
        };
        let planned = plan(vec![p1, p2, p3, expr], &Indexes::new());
        assert_eq!(planned.len(), 4);
        // Item at index 2 must be the Expr (pushed after p2 which binds ?v at index 1).
        assert!(
            matches!(planned[2].0, WhereClause::Expr { .. }),
            "Expr must be at index 2"
        );
        // Item at index 3 must be a Pattern (p3).
        assert!(
            matches!(planned[3].0, WhereClause::Pattern(_)),
            "p3 must be at index 3"
        );
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_expr_no_vars_goes_to_end() {
        use crate::storage::index::Indexes;
        let p1 = WhereClause::Pattern(make_pattern(var("e"), kw(":val"), var("v")));
        let expr = WhereClause::Expr {
            expr: Expr::Lit(Value::Boolean(true)),
            binding: None,
        };
        let planned = plan(vec![p1, expr], &Indexes::new());
        assert_eq!(planned.len(), 2);
        assert!(
            matches!(planned[1].0, WhereClause::Expr { .. }),
            "no-var Expr must be last"
        );
    }

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn test_expr_unbound_var_goes_to_end() {
        use crate::storage::index::Indexes;
        // ?x is never bound by any pattern
        let p1 = WhereClause::Pattern(make_pattern(var("e"), kw(":val"), var("v")));
        let expr = WhereClause::Expr {
            expr: Expr::BinOp(
                BinOp::Gt,
                Box::new(Expr::Var("?x".to_string())),
                Box::new(Expr::Lit(Value::Integer(0))),
            ),
            binding: None,
        };
        let planned = plan(vec![p1, expr], &Indexes::new());
        assert_eq!(planned.len(), 2);
        assert!(
            matches!(planned[1].0, WhereClause::Expr { .. }),
            "Expr with unbound var must be last"
        );
    }

    // ── cost model tests ──────────────────────────────────────────────────
    // These tests call pattern_cost / branch_cost / clause_cost which are
    // unconditional (available on all targets).

    #[test]
    fn test_pattern_cost_fully_bound() {
        // entity bound (UUID), attribute real keyword, value bound literal — 3 bound → cost 1
        let p = Pattern::new(
            EdnValue::Uuid(Uuid::new_v4()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        );
        assert_eq!(pattern_cost(&p), 1);
    }

    #[test]
    fn test_pattern_cost_two_bound() {
        // attribute + value bound, entity variable — 2 bound → cost 10
        let p = Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        );
        assert_eq!(pattern_cost(&p), 10);
    }

    #[test]
    fn test_pattern_cost_one_bound() {
        // only attribute bound — 1 bound → cost 100
        let p = Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::Symbol("?v".to_string()),
        );
        assert_eq!(pattern_cost(&p), 100);
    }

    #[test]
    fn test_pattern_cost_unbound() {
        // all variables — 0 bound → cost 10_000
        let p = Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Symbol("?a".to_string()),
            EdnValue::Symbol("?v".to_string()),
        );
        assert_eq!(pattern_cost(&p), 10_000);
    }

    #[test]
    fn test_clause_cost_pattern_two_bound() {
        // clause_cost delegates to pattern_cost for Pattern variant
        // attr + value bound = 2 → cost 10
        let p = Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        );
        assert_eq!(clause_cost(&WhereClause::Pattern(p)), 10);
    }

    #[test]
    fn test_clause_cost_expr_is_zero() {
        // Expr is pure computation — cost 0
        let clause = WhereClause::Expr {
            expr: Expr::Lit(Value::Integer(42)),
            binding: None,
        };
        assert_eq!(clause_cost(&clause), 0);
    }

    #[test]
    fn test_clause_cost_not_body_uses_min() {
        // Not body: one cost-10 pattern + one cost-10_000 pattern → min = 10
        let selective = Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        );
        let full_scan = Pattern::new(
            EdnValue::Symbol("?x".to_string()),
            EdnValue::Symbol("?a".to_string()),
            EdnValue::Symbol("?v".to_string()),
        );
        let clause = WhereClause::Not(vec![
            WhereClause::Pattern(selective),
            WhereClause::Pattern(full_scan),
        ]);
        assert_eq!(clause_cost(&clause), 10);
    }

    #[test]
    fn test_clause_cost_not_body_expr_only_is_zero() {
        // Not body with no patterns (expr only) → cost 0
        let clause = WhereClause::Not(vec![WhereClause::Expr {
            expr: Expr::Lit(Value::Integer(1)),
            binding: None,
        }]);
        assert_eq!(clause_cost(&clause), 0);
    }

    #[test]
    fn test_branch_cost_empty_branch() {
        // Empty branch → 0
        assert_eq!(branch_cost(&[]), 0);
    }

    #[test]
    fn test_branch_cost_expr_only_is_zero() {
        // Branch with only Expr clauses → 0
        let branch = vec![WhereClause::Expr {
            expr: Expr::Lit(Value::Integer(99)),
            binding: None,
        }];
        assert_eq!(branch_cost(&branch), 0);
    }

    #[test]
    fn test_clause_cost_or_sums_branch_costs() {
        // Or with two branches:
        // branch 1: one pattern with cost 10 (attr+value bound)
        // branch 2: one pattern with cost 100 (attr only bound)
        // clause_cost(Or) = sum = 110
        let b1 = vec![WhereClause::Pattern(Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        ))];
        let b2 = vec![WhereClause::Pattern(Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/age".to_string()),
            EdnValue::Symbol("?v".to_string()),
        ))];
        let clause = WhereClause::Or(vec![b1, b2]);
        assert_eq!(clause_cost(&clause), 110); // 10 + 100
    }

    #[test]
    fn test_clause_cost_not_join_uses_branch_cost() {
        // NotJoin with one selective pattern (cost 10) → cost 10
        let p = Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        );
        let clause = WhereClause::NotJoin {
            join_vars: vec!["?e".to_string()],
            clauses: vec![WhereClause::Pattern(p)],
        };
        assert_eq!(clause_cost(&clause), 10);
    }

    #[test]
    fn test_clause_cost_or_join_sums_branch_costs() {
        // OrJoin with two branches: cost 10 + cost 100 = 110
        let b1 = vec![WhereClause::Pattern(Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        ))];
        let b2 = vec![WhereClause::Pattern(Pattern::new(
            EdnValue::Symbol("?e".to_string()),
            EdnValue::Keyword(":person/age".to_string()),
            EdnValue::Symbol("?v".to_string()),
        ))];
        let clause = WhereClause::OrJoin {
            join_vars: vec!["?e".to_string()],
            branches: vec![b1, b2],
        };
        assert_eq!(clause_cost(&clause), 110);
    }

    #[test]
    fn test_clause_cost_not_body_fully_bound_min_is_one() {
        // Not body: one fully-bound pattern (cost 1) + one full-scan (cost 10_000)
        // clause_cost → min = 1
        let fully_bound = Pattern::new(
            EdnValue::Uuid(Uuid::new_v4()),
            EdnValue::Keyword(":person/name".to_string()),
            EdnValue::String("Alice".to_string()),
        );
        let full_scan = Pattern::new(
            EdnValue::Symbol("?x".to_string()),
            EdnValue::Symbol("?a".to_string()),
            EdnValue::Symbol("?v".to_string()),
        );
        let clause = WhereClause::Not(vec![
            WhereClause::Pattern(full_scan),
            WhereClause::Pattern(fully_bound),
        ]);
        assert_eq!(clause_cost(&clause), 1);
    }
}
