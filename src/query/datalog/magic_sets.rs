use crate::graph::types::{EntityId, Value};
use crate::query::datalog::rules::RuleRegistry;
use crate::query::datalog::types::{DatalogQuery, EdnValue, Rule, WhereClause};
use std::collections::{HashMap, HashSet};

/// Classify each arg in rule invocations as bound ('b') or free ('f').
/// Single left-to-right pass; all variables in Pattern entity/value positions are
/// considered grounded after the pattern (Datalog: pattern binds all its variables).
pub(crate) fn compute_query_adornments(
    where_clauses: &[WhereClause],
) -> HashMap<String, Vec<char>> {
    let mut grounded: HashSet<String> = HashSet::new();
    let mut adornments: HashMap<String, Vec<char>> = HashMap::new();

    for clause in where_clauses {
        match clause {
            WhereClause::Pattern(p) => {
                if let Some(var) = p.entity.as_variable() {
                    grounded.insert(var.to_string());
                }
                if let Some(var) = p.value.as_variable() {
                    grounded.insert(var.to_string());
                }
            }
            WhereClause::RuleInvocation { predicate, args } => {
                let adornment: Vec<char> = args
                    .iter()
                    .map(|arg| {
                        if let Some(var) = arg.as_variable() {
                            if grounded.contains(var) { 'b' } else { 'f' }
                        } else {
                            'b' // literal
                        }
                    })
                    .collect();
                adornments.entry(predicate.clone()).or_insert(adornment);
            }
            _ => {}
        }
    }

    adornments
}

/// Returns true if at least one position in the adornment is bound.
#[allow(dead_code)]
pub(crate) fn has_bound_arg(adornment: &[char]) -> bool {
    adornment.contains(&'b')
}

/// Convert adornment to string: ['b','f'] → "bf".
#[allow(dead_code)]
pub(crate) fn adornment_string(adornment: &[char]) -> String {
    adornment.iter().collect()
}

/// Magic predicate name: "__magic_ancestor_bf".
#[allow(dead_code)]
pub(crate) fn magic_pred_name(pred: &str, adornment: &[char]) -> String {
    format!("__magic_{}_{}", pred, adornment_string(adornment))
}

#[allow(dead_code)]
pub(crate) fn rewrite(
    _query: &DatalogQuery,
    _registry: &RuleRegistry,
) -> Option<(RuleRegistry, Vec<(EntityId, String, Value)>)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::datalog::types::{FindSpec, Pattern, Rule, WhereClause};

    #[test]
    fn test_rewrite_empty_query_returns_none() {
        let query = DatalogQuery::new(
            vec![FindSpec::Variable("?x".to_string())],
            vec![],
        );
        let registry = RuleRegistry::new();
        assert!(rewrite(&query, &registry).is_none());
    }

    fn pat(entity: &str, attr: &str, value: &str) -> WhereClause {
        WhereClause::Pattern(Pattern::new(
            if entity.starts_with('?') {
                EdnValue::Symbol(entity.to_string())
            } else {
                EdnValue::Keyword(entity.to_string())
            },
            EdnValue::Keyword(attr.to_string()),
            if value.starts_with('?') {
                EdnValue::Symbol(value.to_string())
            } else {
                EdnValue::String(value.to_string())
            },
        ))
    }

    fn rule_inv(pred: &str, args: &[&str]) -> WhereClause {
        WhereClause::RuleInvocation {
            predicate: pred.to_string(),
            args: args
                .iter()
                .map(|a| {
                    if a.starts_with('?') {
                        EdnValue::Symbol(a.to_string())
                    } else {
                        EdnValue::String(a.to_string())
                    }
                })
                .collect(),
        }
    }

    #[allow(dead_code)]
    fn make_rule(pred: &str, head_args: &[&str], body: Vec<WhereClause>) -> Rule {
        Rule {
            head: std::iter::once(EdnValue::Symbol(pred.to_string()))
                .chain(head_args.iter().map(|a| EdnValue::Symbol(a.to_string())))
                .collect(),
            body,
        }
    }

    #[test]
    fn test_literal_arg_is_bound() {
        let clauses = vec![rule_inv("ancestor", &["abc123", "?y"])];
        let adornments = compute_query_adornments(&clauses);
        assert_eq!(adornments.get("ancestor"), Some(&vec!['b', 'f']));
    }

    #[test]
    fn test_free_var_is_free() {
        let clauses = vec![rule_inv("ancestor", &["?x", "?y"])];
        let adornments = compute_query_adornments(&clauses);
        assert_eq!(adornments.get("ancestor"), Some(&vec!['f', 'f']));
    }

    #[test]
    fn test_var_grounded_by_preceding_pattern() {
        // [?x :name "Alice"] (ancestor ?x ?y) → ?x grounded → bf
        let clauses = vec![pat("?x", ":name", "Alice"), rule_inv("ancestor", &["?x", "?y"])];
        let adornments = compute_query_adornments(&clauses);
        assert_eq!(adornments.get("ancestor"), Some(&vec!['b', 'f']));
    }

    #[test]
    fn test_all_free_has_no_bound() {
        let clauses = vec![rule_inv("ancestor", &["?x", "?y"])];
        let adornments = compute_query_adornments(&clauses);
        let ad = adornments.get("ancestor").unwrap();
        assert!(!has_bound_arg(ad));
    }
}
