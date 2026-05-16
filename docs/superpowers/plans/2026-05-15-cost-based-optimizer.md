# Cost-Based Optimizer Extensions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `pattern_cost`, `clause_cost`, and `branch_cost` to `optimizer.rs`, then use them to sort `not`/`not-join` clauses and `or`/`or-join` branches by cost ascending so cheap filters run first.

**Architecture:** Three new cost functions land in `optimizer.rs` (unconditional — available on all targets). The sorting call-sites in `executor.rs` and `evaluator.rs` are gated behind `#[cfg(not(feature = "wasm"))]`, matching the existing pattern-ordering gate. No change to `plan()` signature or return type.

**Tech Stack:** Rust, Cargo, `cargo test`, `cargo clippy`

---

## File Structure

| File | Change |
|------|--------|
| `src/query/datalog/optimizer.rs` | Add private `pattern_cost()`, pub `clause_cost()`, pub `branch_cost()` before the `#[cfg(test)]` block at line 199 |
| `src/query/datalog/executor.rs` | Sort `not_clauses`/`not_join_clauses` after line 592 (execute_query) and after line 1060 (execute_query_with_rules); sort branches in `apply_or_clauses()` before the `for branch in branches` loops |
| `src/query/datalog/evaluator.rs` | Sort `not_clauses`/`not_join_clauses` after line 787 (StratifiedEvaluator mixed-rules loop) |

---

### Task 1: Git worktree

**Files:**
- No file changes — worktree setup only

- [ ] **Step 1: Invoke the using-git-worktrees skill**

```
Use superpowers:using-git-worktrees to create a worktree for branch feature/issue-205-cost-based-optimizer
```

Expected: worktree created at `.worktrees/issue-205-cost-based-optimizer`, checked out to new branch `feature/issue-205-cost-based-optimizer`.

---

### Task 2: Failing unit tests for cost functions

**Files:**
- Modify: `src/query/datalog/optimizer.rs:199` (append to existing `#[cfg(test)]` module)

The existing test module starts at line 199. Add new tests at the end of it (before the closing `}`).

- [ ] **Step 1: Read the end of the existing test module**

```bash
tail -n 30 src/query/datalog/optimizer.rs
```

Note the exact closing `}` line number so you can insert before it.

- [ ] **Step 2: Add failing tests for the three cost functions**

Append the following test functions inside the existing `#[cfg(test)] mod tests { ... }` block, before its closing `}`:

```rust
    // ── cost model tests ──────────────────────────────────────────────────

    fn sym(s: &str) -> EdnValue {
        EdnValue::Symbol(s.to_string())
    }

    fn lit_str(s: &str) -> EdnValue {
        EdnValue::Str(s.to_string())
    }

    fn attr(s: &str) -> AttributeSpec {
        use crate::query::datalog::types::AttributeSpec;
        AttributeSpec::Real(EdnValue::Keyword(s.to_string()))
    }

    fn attr_var() -> AttributeSpec {
        use crate::query::datalog::types::AttributeSpec;
        AttributeSpec::Real(EdnValue::Symbol("?a".to_string()))
    }

    fn make_cost_pattern(
        entity: EdnValue,
        attribute: AttributeSpec,
        value: EdnValue,
    ) -> Pattern {
        use crate::query::datalog::types::AttributeSpec;
        Pattern::new(entity, attribute.into_edn(), value)
    }

    #[test]
    fn test_pattern_cost_fully_bound() {
        // entity bound (UUID), attribute real keyword, value bound literal — 3 bound → cost 1
        let p = Pattern::new(
            EdnValue::Uuid(Uuid::new_v4()),
            EdnValue::Keyword(":person/name".to_string()),
            lit_str("Alice"),
        );
        assert_eq!(pattern_cost(&p), 1);
    }

    #[test]
    fn test_pattern_cost_two_bound() {
        // attribute + value bound, entity variable — 2 bound → cost 10
        let p = Pattern::new(
            sym("?e"),
            EdnValue::Keyword(":person/name".to_string()),
            lit_str("Alice"),
        );
        assert_eq!(pattern_cost(&p), 10);
    }

    #[test]
    fn test_pattern_cost_one_bound() {
        // only attribute bound — 1 bound → cost 100
        let p = Pattern::new(
            sym("?e"),
            EdnValue::Keyword(":person/name".to_string()),
            sym("?v"),
        );
        assert_eq!(pattern_cost(&p), 100);
    }

    #[test]
    fn test_pattern_cost_unbound() {
        // all variables — 0 bound → cost 10_000
        let p = Pattern::new(
            sym("?e"),
            EdnValue::Symbol("?a".to_string()),
            sym("?v"),
        );
        assert_eq!(pattern_cost(&p), 10_000);
    }

    #[test]
    fn test_clause_cost_pattern() {
        // clause_cost delegates to pattern_cost for Pattern variant
        let p = Pattern::new(
            sym("?e"),
            EdnValue::Keyword(":person/name".to_string()),
            lit_str("Alice"),
        );
        let clause = WhereClause::Pattern(p);
        assert_eq!(clause_cost(&clause), 10); // attr + value bound = 2 → 10
    }

    #[test]
    fn test_clause_cost_expr() {
        // Expr is pure computation — cost 0
        let clause = WhereClause::Expr {
            var: "?x".to_string(),
            expr: Expr::Lit(crate::graph::types::Value::Integer(42)),
        };
        assert_eq!(clause_cost(&clause), 0);
    }

    #[test]
    fn test_clause_cost_not_body_uses_min() {
        // Not body: one selective pattern (cost 10) + one full-scan pattern (cost 10_000)
        // clause_cost → min = 10
        let selective = Pattern::new(
            sym("?e"),
            EdnValue::Keyword(":person/name".to_string()),
            lit_str("Alice"),
        );
        let full_scan = Pattern::new(sym("?x"), EdnValue::Symbol("?a".to_string()), sym("?v"));
        let clause = WhereClause::Not(vec![
            WhereClause::Pattern(selective),
            WhereClause::Pattern(full_scan),
        ]);
        assert_eq!(clause_cost(&clause), 10);
    }

    #[test]
    fn test_clause_cost_not_body_expr_only() {
        // Not body with no patterns (expr only) → cost 0
        let clause = WhereClause::Not(vec![WhereClause::Expr {
            var: "?x".to_string(),
            expr: Expr::Lit(crate::graph::types::Value::Integer(1)),
        }]);
        assert_eq!(clause_cost(&clause), 0);
    }

    #[test]
    fn test_branch_cost_empty_branch() {
        // Empty branch → 0
        assert_eq!(branch_cost(&[]), 0);
    }

    #[test]
    fn test_branch_cost_no_patterns() {
        // Branch with only Expr clauses → 0
        let branch = vec![WhereClause::Expr {
            var: "?x".to_string(),
            expr: Expr::Lit(crate::graph::types::Value::Integer(99)),
        }];
        assert_eq!(branch_cost(&branch), 0);
    }

    #[test]
    fn test_clause_cost_or_sums_branch_costs() {
        // Or with two branches:
        // branch 1: one pattern with cost 10
        // branch 2: one pattern with cost 100
        // clause_cost(Or) = sum = 110
        let b1 = vec![WhereClause::Pattern(Pattern::new(
            sym("?e"),
            EdnValue::Keyword(":person/name".to_string()),
            lit_str("Alice"),
        ))];
        let b2 = vec![WhereClause::Pattern(Pattern::new(
            sym("?e"),
            EdnValue::Keyword(":person/age".to_string()),
            sym("?v"),
        ))];
        let clause = WhereClause::Or(vec![b1, b2]);
        assert_eq!(clause_cost(&clause), 110); // 10 + 100
    }
```

- [ ] **Step 3: Run the tests and verify they fail with "not found" errors**

```bash
cargo test -p minigraf pattern_cost branch_cost clause_cost -- --nocapture 2>&1 | head -40
```

Expected: compile errors — `pattern_cost`, `clause_cost`, `branch_cost` not found. This is the TDD red phase.

---

### Task 3: Implement cost functions in optimizer.rs

**Files:**
- Modify: `src/query/datalog/optimizer.rs` (insert before line 199)

- [ ] **Step 1: Insert the three cost functions before the `#[cfg(test)]` block**

Open `src/query/datalog/optimizer.rs`. Find line 199 (`#[cfg(test)]`). Insert the following block immediately before it (after line 197, which is the closing `}` of `plan()`):

```rust
/// Static 4-tier cardinality estimate for a single pattern.
///
/// Derived from selectivity_score but returns u64 cost (lower = cheaper) rather
/// than a selectivity score. Unconditional: available on all targets including WASM.
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
/// Unconditional: available on all targets including WASM.
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
/// Unconditional: available on all targets including WASM. The *sorting* call-sites
/// that consume this function are gated behind `#[cfg(not(feature = "wasm"))]`.
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

```

- [ ] **Step 2: Run cost-function unit tests to verify they pass**

```bash
cargo test -p minigraf pattern_cost branch_cost clause_cost -- --nocapture 2>&1 | tail -20
```

Expected: all new tests PASS. No compile errors.

- [ ] **Step 3: Run full test suite to check for regressions**

```bash
cargo test 2>&1 | tail -5
```

Expected: all existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add src/query/datalog/optimizer.rs
git commit -m "feat(optimizer): add pattern_cost, branch_cost, clause_cost (#205)

Static 4-tier cardinality cost model. Unconditional (all targets).
Sorting call-sites gated on cfg(not(feature = \"wasm\")) added in next commit."
```

---

### Task 4: Sort not-clauses in executor.rs (both query functions)

**Files:**
- Modify: `src/query/datalog/executor.rs:592` and `src/query/datalog/executor.rs:1060`

- [ ] **Step 1: Write integration tests for not-clause ordering**

Add the following test at the end of `executor.rs`'s `#[cfg(test)] mod tests { ... }` block:

```rust
    #[test]
    fn test_not_clause_ordering_correctness() {
        // Two `not` clauses in expensive-first source order; results must be
        // identical to cheap-first order — the sorting is semantics-preserving.
        use crate::graph::types::Value;
        use crate::storage::backend::memory::MemoryBackend;
        use crate::db::Minigraf;

        let db = Minigraf::open_with_backend(MemoryBackend::new()).unwrap();
        db.execute(
            r#"(transact [
                [:db/add "e1" :item/name "widget" :item/tag "cheap"]
                [:db/add "e2" :item/name "gadget" :item/tag "cheap"]
                [:db/add "e3" :item/name "doohickey"]
              ])"#,
        )
        .unwrap();

        // Query: items that are NOT named "widget" AND NOT named "gadget"
        // Only "doohickey" survives both filters.
        let result = db
            .execute(
                r#"(query {:find [?name]
                           :where [[?e :item/name ?name]
                                   (not [?e :item/name "widget"])
                                   (not [?e :item/name "gadget"])]})"#,
            )
            .unwrap();
        assert_eq!(result.len(), 1, "expected exactly one result");
        assert_eq!(
            result[0].get("name").cloned().or_else(|| result[0].get("?name").cloned()),
            Some(Value::String("doohickey".to_string()))
        );
    }
```

- [ ] **Step 2: Run the test to verify it passes (behaviour baseline)**

```bash
cargo test -p minigraf test_not_clause_ordering_correctness -- --nocapture
```

Expected: PASS — this is a correctness regression guard, not a TDD new-feature test.

- [ ] **Step 3: Add not-clause sorting after line 592 in execute_query()**

In `src/query/datalog/executor.rs`, find the block ending at line 592 (`.collect();` closing `not_join_clauses`). Insert the following two lines immediately after it:

```rust
        // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
        #[cfg(not(feature = "wasm"))]
        not_clauses.sort_by_key(|body| optimizer::branch_cost(body));
        // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
        #[cfg(not(feature = "wasm"))]
        not_join_clauses.sort_by_key(|(_, clauses)| optimizer::branch_cost(clauses));
```

- [ ] **Step 4: Add not-clause sorting after line 1060 in execute_query_with_rules()**

Find the second `.collect();` closing `not_join_clauses` (around line 1060). Insert the same block:

```rust
        // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
        #[cfg(not(feature = "wasm"))]
        not_clauses.sort_by_key(|body| optimizer::branch_cost(body));
        // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
        #[cfg(not(feature = "wasm"))]
        not_join_clauses.sort_by_key(|(_, clauses)| optimizer::branch_cost(clauses));
```

- [ ] **Step 5: Run full test suite**

```bash
cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/query/datalog/executor.rs
git commit -m "feat(executor): sort not/not-join clauses by cost ascending (#205)

Cheap filters run first; bindings rejected early never reach expensive filters.
Gated on cfg(not(feature = \"wasm\")) — small datasets + determinism on WASM."
```

---

### Task 5: Sort or/or-join branches in apply_or_clauses()

**Files:**
- Modify: `src/query/datalog/executor.rs` — `apply_or_clauses()` function

`apply_or_clauses()` starts at line 1821. There are two `for branch in branches` loops:
- **Slow path** (seeded, has Not): loop at line 1846
- **Fast path** (no Not, hash-join): loop at line 1880

Both operate on the same `branches` binding. We introduce a sorted copy before both paths.

- [ ] **Step 1: Find the `any_branch_has_not` binding inside the `WhereClause::Or` arm**

Read `src/query/datalog/executor.rs` lines 1832–1845. The structure is:

```rust
WhereClause::Or(branches) => {
    let any_branch_has_not = branches.iter().any(|b| { ... });

    if any_branch_has_not {
        // slow path: for branch in branches { ... }
        ...
    }
    // fast path: for branch in branches { ... }
```

- [ ] **Step 2: Add branch sorting before the `any_branch_has_not` check**

Insert immediately after the opening `WhereClause::Or(branches) => {` line:

```rust
                // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
                // Note: all branches are still evaluated (no short-circuit); this ordering
                // is infrastructure for issue #250. See that issue for short-circuit optimization.
                #[cfg(not(feature = "wasm"))]
                let branches: Vec<_> = {
                    let mut b: Vec<_> = branches.iter().collect();
                    b.sort_by_key(|br| optimizer::branch_cost(br));
                    b.into_iter().map(|br| br.as_slice()).collect()
                };
                #[cfg(not(feature = "wasm"))]
                let branches: &[_] = &branches;
                #[cfg(feature = "wasm")]
                let branches: Vec<_> = branches.iter().map(|b| b.as_slice()).collect();
                #[cfg(feature = "wasm")]
                let branches: &[_] = &branches;
```

Wait — this approach is overly complex because `branches` is already a `&Vec<Vec<WhereClause>>` from the match arm. A simpler approach: introduce a locally-sorted `Vec` that shadows `branches` as a `Vec<&Vec<WhereClause>>`, and replace the two loop references.

**Revised Step 2**: Insert immediately after `WhereClause::Or(branches) => {`:

```rust
                // Sort branches by cost ascending so cheaper branches evaluate first.
                // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
                // Note: all branches still evaluated (no short-circuit); ordering is
                // infrastructure for issue #250.
                let sorted_branches: Vec<&Vec<WhereClause>>;
                #[cfg(not(feature = "wasm"))]
                {
                    let mut b: Vec<&Vec<WhereClause>> = branches.iter().collect();
                    b.sort_by_key(|br| optimizer::branch_cost(br));
                    sorted_branches = b;
                }
                #[cfg(feature = "wasm")]
                {
                    sorted_branches = branches.iter().collect();
                }
```

Then replace `for branch in branches {` in the **slow path** (line ~1846) with `for branch in &sorted_branches {`.

And replace `for branch in branches {` in the **fast path** (line ~1880) with `for branch in &sorted_branches {`.

In each loop, `branch` was a `&Vec<WhereClause>`. After the replacement it is a `&&Vec<WhereClause>`, so add a `*` dereference or use `branch` directly — Rust auto-derefs in most call positions so it should compile unchanged, but verify.

- [ ] **Step 3: Do the same for the `WhereClause::OrJoin` arm**

Find the `WhereClause::OrJoin { join_vars, branches }` arm (around line 1974). The loop is `for branch in branches` (around line 1995). Apply the same sorted_branches pattern.

Insert immediately after `WhereClause::OrJoin { join_vars, branches } => {`:

```rust
                let sorted_oj_branches: Vec<&Vec<WhereClause>>;
                #[cfg(not(feature = "wasm"))]
                {
                    let mut b: Vec<&Vec<WhereClause>> = branches.iter().collect();
                    b.sort_by_key(|br| optimizer::branch_cost(br));
                    sorted_oj_branches = b;
                }
                #[cfg(feature = "wasm")]
                {
                    sorted_oj_branches = branches.iter().collect();
                }
```

Replace `for branch in branches {` (around line 1995) with `for branch in &sorted_oj_branches {`.

- [ ] **Step 4: Run full test suite**

```bash
cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 5: Clippy**

```bash
cargo clippy -- -D warnings 2>&1 | grep -v "^$" | head -30
```

Expected: no warnings. If clippy flags unused variable warnings for `sorted_branches` in a cfg branch, use `let _ =` or restructure so both branches use the same name.

- [ ] **Step 6: Commit**

```bash
git add src/query/datalog/executor.rs
git commit -m "feat(executor): sort or/or-join branches by cost ascending (#205)

Infrastructure for issue #250 (or short-circuit). All branches still evaluated.
Gated on cfg(not(feature = \"wasm\"))."
```

---

### Task 6: Sort not-clauses in evaluator.rs (StratifiedEvaluator)

**Files:**
- Modify: `src/query/datalog/evaluator.rs:787`

The `not_join_clauses` collection ends at line 787 (`.collect();`). Insert sorting immediately after.

- [ ] **Step 1: Insert not-clause sorting after line 787**

In `src/query/datalog/evaluator.rs`, after the `.collect();` that closes `not_join_clauses`:

```rust
                // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
                #[cfg(not(feature = "wasm"))]
                not_clauses.sort_by_key(|body| crate::query::datalog::optimizer::branch_cost(body));
                // WASM omission: small datasets + determinism — see optimizer::selectivity_score().
                #[cfg(not(feature = "wasm"))]
                not_join_clauses
                    .sort_by_key(|(_, clauses)| crate::query::datalog::optimizer::branch_cost(clauses));
```

Note: `evaluator.rs` uses the full path `crate::query::datalog::optimizer` rather than a short `optimizer` alias (verify the existing imports in the file before deciding whether to add a `use` line or use the full path).

- [ ] **Step 2: Run full test suite**

```bash
cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 3: Run clippy**

```bash
cargo clippy -- -D warnings 2>&1 | grep -v "^$" | head -30
```

Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add src/query/datalog/evaluator.rs
git commit -m "feat(evaluator): sort not/not-join clauses by cost in StratifiedEvaluator (#205)

Consistent with executor.rs; cheap filters run first in mixed-rules loop.
Gated on cfg(not(feature = \"wasm\"))."
```

---

### Task 7: Full verification and PR

**Files:**
- No new file changes

- [ ] **Step 1: Run the complete test suite**

```bash
cargo test 2>&1 | tail -10
```

Expected: all tests pass (currently ~850+).

- [ ] **Step 2: Run clippy clean**

```bash
cargo clippy -- -D warnings 2>&1 | grep -E "^error" | wc -l
```

Expected: 0.

- [ ] **Step 3: Run cargo fmt check**

```bash
cargo fmt --check
```

Expected: no diff. If there is a diff, run `cargo fmt` and commit the style fix.

- [ ] **Step 4: Verify WASM build compiles**

```bash
cargo build --features wasm --target wasm32-unknown-unknown 2>&1 | tail -5
```

Expected: compiles without errors. (If the `wasm32-unknown-unknown` target is not installed, run `rustup target add wasm32-unknown-unknown` first.)

- [ ] **Step 5: Open the PR**

```bash
gh pr create \
  --title "feat: cost-based not/or ordering (#205)" \
  --body "$(cat <<'EOF'
## Summary
- Adds `pattern_cost`, `branch_cost`, `clause_cost` to `optimizer.rs` (unconditional, all targets)
- Sorts `not`/`not-join` clauses cheapest-first in `execute_query`, `execute_query_with_rules`, and `StratifiedEvaluator`
- Sorts `or`/`or-join` branches cheapest-first in `apply_or_clauses` (infrastructure for #250)
- Sorting gated on `#[cfg(not(feature = "wasm"))]` — small datasets + determinism on WASM

Closes #205

## Test plan
- [ ] All existing tests pass (`cargo test`)
- [ ] New unit tests for all cost tiers in `optimizer.rs`
- [ ] New integration test: not-clause ordering produces correct results
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] WASM build compiles

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 6: Monitor CI until green**

```bash
gh pr checks --watch
```

Expected: all checks green. Fix any failures before proceeding.
