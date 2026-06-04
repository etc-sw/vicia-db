# Per-Query Complexity Limits — Design Spec

**Issue**: #288  
**Date**: 2026-06-04  
**Status**: Approved

---

## Summary

Add `:max-derived-facts N` and `:max-results N` as optional per-query keys in the Datalog query syntax, allowing callers to override the database-level `OpenOptions` limits for a single query without rebuilding the `Minigraf` instance.

Also raise `DEFAULT_MAX_DERIVED_FACTS` from `100_000` to `1_000_000` to cover typical embedded workloads (N² over ~1,000 nodes).

---

## Motivation

`OpenOptions` already exposes `max_derived_facts` and `max_results` as database-level limits (added April 2026). However, different queries in the same database session can have very different resource profiles:

- A transitive-closure query over a 1,000-commit git history needs ~1M derived facts
- A simple attribute lookup needs the default 1M cap
- A safety-critical query might want a tighter cap than the database default

Rebuilding a `Minigraf` instance to change limits between queries is impractical. Per-query syntax solves this cleanly.

---

## Syntax

Two new optional keys in the query vector, parsed identically to `:as-of` and `:valid-at`:

```datalog
(query [:find ?ancestor
        :where (ancestor ?ancestor "abc123")
        :max-derived-facts 5000000
        :max-results 10000])
```

Both keys are optional and order-independent. Either or both may be omitted; omitted keys fall back to the `OpenOptions` database-level limit.

---

## Design

### 1. `types.rs` — `DatalogQuery`

Add two optional fields:

```rust
pub struct DatalogQuery {
    pub find: Vec<FindSpec>,
    pub where_clauses: Vec<WhereClause>,
    pub as_of: Option<AsOf>,
    pub valid_at: Option<ValidAt>,
    pub with_vars: Vec<String>,
    pub max_derived_facts: Option<usize>,   // new: per-query override
    pub max_results: Option<usize>,         // new: per-query override
}
```

- `None` = use the executor's configured limit (from `OpenOptions`)
- `Some(n)` = use `n` for this query only
- `0` is rejected at parse time

All constructors (`DatalogQuery::new`, `DatalogQuery::from_patterns`) initialize both fields to `None`.

### 2. `parser.rs` — Query parser

Recognize `:max-derived-facts` and `:max-results` in the query vector:

- Argument: positive integer literal (≥ 1)
- `0` → parse error: `":max-derived-facts must be ≥ 1"`
- Duplicate key → parse error: `"duplicate :max-derived-facts"`
- Non-integer argument → parse error

Parse position: same pass as `:as-of` / `:valid-at` / `:with` — keyword consumed, integer argument consumed.

### 3. `executor.rs` — `execute_query_with_rules`

Compute effective limits before dispatching to `StratifiedEvaluator`:

```rust
let effective_max_derived = query.max_derived_facts
    .unwrap_or(self.max_derived_facts);
let effective_max_results = query.max_results
    .unwrap_or(self.max_results);
```

Pass `effective_max_derived` and `effective_max_results` to `StratifiedEvaluator::new` (and transitively to `RecursiveEvaluator::new`) for that invocation.

Also apply `effective_max_results` to the result-set truncation in the post-processing step.

The executor's `self.max_derived_facts` / `self.max_results` are never mutated — no shared state change, no `set_limits()` call.

### 4. `evaluator.rs` — Raise the default

```rust
// Before:
pub const DEFAULT_MAX_DERIVED_FACTS: usize = 100_000;

// After:
pub const DEFAULT_MAX_DERIVED_FACTS: usize = 1_000_000;
```

Update the doc comment on `OpenOptions::max_derived_facts` to reflect the new default.

---

## Affected files

| File | Change |
|---|---|
| `src/query/datalog/types.rs` | Add `max_derived_facts` / `max_results` to `DatalogQuery`; update constructors |
| `src/query/datalog/parser.rs` | Parse `:max-derived-facts` and `:max-results` keywords |
| `src/query/datalog/executor.rs` | Compute effective limits and pass to evaluator |
| `src/query/datalog/evaluator.rs` | Raise `DEFAULT_MAX_DERIVED_FACTS` to `1_000_000` |
| `src/db.rs` | Update `OpenOptions` doc comment for new default |

---

## Not in scope

- Session-level `(set-limit ...)` persistent across commands (explicitly excluded)
- Magic sets / demand-driven evaluation (tracked separately in #289)
- New CLI/REPL commands for limits (limits are set inline in the query)

---

## Testing

- Unit tests in `parser.rs`: valid keys, `0` rejected, duplicate rejected, non-integer rejected
- Unit tests in `executor.rs`: per-query override takes effect; does not bleed into next query; `None` falls back to executor default
- Integration test: recursive rule that would exceed database default succeeds with `:max-derived-facts` override
- Existing limit tests remain green (default raised, not removed)
