# Design: Cost-Based Optimizer Extensions (Wave 2 PR 2)

**Issue**: #205 (cost-based optimizer extensions for new clause types)  
**Date**: 2026-05-15  
**Branch**: single worktree + PR

---

## Summary

Extend `optimizer.rs` with a static cardinality cost model and use it to order `not`/`not-join` clauses and `or`/`or-join` branches by estimated evaluation cost. Cheap filters run first; expensive ones only process the bindings that survived earlier filters.

Aggregation post-processing cost estimation is **out of scope** — aggregation always runs last and cannot be reordered.

Follow-up issue for Or short-circuit optimization (which builds on this cost model): #250.

---

## Current Behaviour

- `not`/`not-join` clauses are applied in source order — an expensive not-clause that scans many facts runs before a cheap one, wasting work on bindings that would have been rejected cheaply.
- `or`/`or-join` branches are evaluated in source order — no cost-based selection.
- The optimizer has no concept of cost for clause types other than `Pattern` and `Expr`.

---

## Design

### 1. Cost model — `optimizer.rs`

#### `pattern_cost(p: &Pattern) -> u64` (private)

Static 4-tier model derived from the existing `selectivity_score()`:

| Bound EAV components | Cost |
|---|---|
| 3 (all bound) | 1 |
| 2 | 10 |
| 1 | 100 |
| 0 (nothing bound) | 10,000 |

#### `pub fn clause_cost(clause: &WhereClause) -> u64`

| Clause type | Cost formula |
|---|---|
| `Pattern(p)` | `pattern_cost(p)` |
| `Expr { .. }` | 0 (pure computation, no fact scan) |
| `Not(body)` | `min(pattern_cost(p) for p in body patterns)`, or 0 if body has no patterns |
| `NotJoin { clauses, .. }` | same as `Not` using `clauses` |
| `Or(branches)` | `sum(branch_cost(b) for b in branches)` |
| `OrJoin { branches, .. }` | same as `Or` |
| other | `u64::MAX` (never passed; defensive) |

#### `pub fn branch_cost(branch: &[WhereClause]) -> u64`

`min(pattern_cost(p) for p in branch patterns)`, or 0 if branch has no patterns.

**Rationale for `min` in not/or bodies**: In a multi-pattern join, the most selective pattern (lowest cardinality) dominates — the join can never produce more rows than the smallest input. Using `min` avoids overestimating the cost of a body that contains at least one highly selective pattern.

**WASM behaviour**: `clause_cost()` and `branch_cost()` are unconditional (available on all targets). The *sorting* call sites are gated behind `#[cfg(not(feature = "wasm"))]`, consistent with the existing pattern ordering gate.

---

### 2. Executor changes — `executor.rs`

#### `not`/`not-join` ordering

In both `execute_query()` and `execute_query_with_rules()`, after extracting `not_clauses` and `not_join_clauses` and before the filter loop, sort each by `clause_cost()` ascending:

```rust
// WASM omission: small datasets + determinism — see optimizer::selectivity_score().
#[cfg(not(feature = "wasm"))]
not_clauses.sort_by_key(|body| optimizer::clause_cost(&WhereClause::Not(body.clone())));

// WASM omission: small datasets + determinism — see optimizer::selectivity_score().
#[cfg(not(feature = "wasm"))]
not_join_clauses.sort_by_key(|(vars, body)| {
    optimizer::clause_cost(&WhereClause::NotJoin {
        join_vars: vars.clone(),
        clauses: body.clone(),
    })
});
```

**Effect**: Cheap not-filters run first. Bindings rejected by a cheap filter never reach more expensive filters. No change to semantics.

#### `or`/`or-join` branch ordering

In `apply_or_clauses()`, before evaluating branches in both the fast path (no Not in branches) and the slow path (seeded), sort branches by `branch_cost()` ascending:

```rust
// WASM omission: small datasets + determinism — see optimizer::selectivity_score().
// Note: all branches are still evaluated (no short-circuit); this ordering is
// infrastructure for #250. See that issue for the short-circuit optimization.
#[cfg(not(feature = "wasm"))]
let sorted_branches: Vec<_> = {
    let mut b = branches.to_vec();
    b.sort_by_key(|br| optimizer::branch_cost(br));
    b
};
// iterate sorted_branches instead of branches
```

---

### 3. Evaluator changes — `evaluator.rs`

The `StratifiedEvaluator` mixed-rules loop applies `not`/`not-join` as post-filters in the `'binding:` loop. After extracting `not_clauses` and `not_join_clauses` (which already happens before the `'binding:` loop), sort both collections by `clause_cost()` ascending:

```rust
// WASM omission: small datasets + determinism — see optimizer::selectivity_score().
#[cfg(not(feature = "wasm"))]
not_clauses.sort_by_key(|body| optimizer::clause_cost(&WhereClause::Not(body.clone())));

// WASM omission: small datasets + determinism — see optimizer::selectivity_score().
#[cfg(not(feature = "wasm"))]
not_join_clauses.sort_by_key(|(vars, clauses)| {
    optimizer::clause_cost(&WhereClause::NotJoin {
        join_vars: vars.clone(),
        clauses: clauses.clone(),
    })
});
```

---

### 4. Testing

**`optimizer.rs` unit tests** — `clause_cost()` and `branch_cost()`:
- Fully-bound pattern → cost 1
- 2-bound pattern → cost 10
- 1-bound pattern → cost 100
- Unbound pattern → cost 10,000
- `Not` body: one selective + one full-scan pattern → cost = 1 (min)
- `Not` body with no patterns (expr-only) → cost 0
- `Or` with two branches → cost = sum of branch min-costs
- `branch_cost` of empty branch → 0

**`executor.rs` integration tests** — not ordering:
- Query with two `not` clauses in expensive-first source order → results identical to cheap-first order (correctness regression guard)
- Query with two `not` clauses where cheap one eliminates most bindings → results correct

**Validation**: The existing `bench_negation` group in `benches/minigraf_bench.rs` provides the baseline for measuring `not`-ordering improvement. No new benchmark group is added in this PR.

---

## Invariants Preserved

- **Correctness**: Sorting only changes evaluation order, never the set of bindings that satisfy a clause. The `not` filter is semantically commutative when applied to independent not-bodies over the same outer binding set.
- **WASM portability**: Cost functions unconditional; sorting gated behind `#[cfg(not(feature = "wasm"))]`.
- **Backwards compatibility**: No change to `plan()` signature or return type. `clause_cost()` and `branch_cost()` are new public additions.

---

## Files Changed

| File | Change |
|------|--------|
| `src/query/datalog/optimizer.rs` | New `pattern_cost()`, `clause_cost()`, `branch_cost()` |
| `src/query/datalog/executor.rs` | Cost-sorted `not_clauses`, `not_join_clauses` in both query functions; cost-sorted branches in `apply_or_clauses()` |
| `src/query/datalog/evaluator.rs` | Cost-sorted `not_clauses`, `not_join_clauses` in mixed-rules loop |
