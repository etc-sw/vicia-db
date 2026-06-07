# Minigraf Refactoring and Algorithm Improvement Plan

Historical branch: `vetch/minigraf-refactor-plan`

Historical worktree: `.worktrees/vetch-minigraf-refactor-plan`

Status: planning artifact only. This is not the active implementation branch.
Use `docs/VETCH_DELTA_STORAGE_ROADMAP.md` for the current Vicia/Vetch
delta-storage line.

## Philosophy Fit

This plan fits Minigraf's "SQLite for bi-temporal graph databases" philosophy when it stays inside these constraints:

- Keep the database embedded, zero-configuration, and dependency-light.
- Prefer reliability and clarity over clever indexing changes.
- Preserve the single-file storage model unless evidence proves a file-format change is necessary and worth a migration plan.
- Treat bi-temporal ledger identity as first-class, including Vetch's need to reason about ref-like graph edges and exact write history.
- Benchmark before replacing simple rebuild logic with more complex incremental algorithms.

The plan intentionally avoids new dependencies, client/server architecture, distributed storage, alternate query languages, or broad API churn.

## Goals

- Reduce misleading or dead optimization surfaces in the Datalog execution path.
- Identify performance work that is supported by benchmarks rather than intuition.
- Make Vetch-relevant ledger/history behavior harder to regress.
- Improve maintainability in the largest query/storage files without changing public behavior.
- Separate low-risk cleanup from higher-risk algorithm changes.

## Non-goals

- No public API redesign.
- No new query language.
- No storage format change in the first implementation pass.
- No new dependencies unless a later benchmark-backed proposal is explicitly approved.
- No broad module split just because large files exist.
- No change that weakens full-history fact identity or export semantics.

## Current Hotspots

| Area | Evidence | Risk |
| --- | --- | --- |
| Checkpoint/index rebuild cost | `src/storage/persistent_facts.rs:820-840`, `875-909`, `917-920` stream committed entries, merge pending facts, rebuild all four B+trees, then CRC data pages | Small write checkpoints can cost O(total indexed facts/pages) |
| Matcher index hints | `src/query/datalog/executor.rs:512-516`, `545-555`; `src/query/datalog/matcher.rs:44-59`, `312-329` | Hints look useful but often fall back to scanning all facts |
| Selective fetch fallback | `src/query/datalog/executor.rs:37-58`, `412-428` | Nested unbound `not`/`not-join`/`or` patterns can force full scans |
| Exclusion key allocation | `src/query/datalog/executor.rs:651-689`, `763-778`, `827-850` | Repeated `String`/`Vec` allocation in negative-clause evaluation |
| Expression evaluation duplication | `src/query/datalog/evaluator.rs:532-536`; `src/query/datalog/executor.rs:2163`, `2298`, `2340` | Semantics can drift between executor and recursive evaluator |
| All-target lint debt | `cargo clippy --lib -- -D warnings` passes; all-target clippy fails mostly in tests/examples/benches plus a few lib-test lints | Cleanup noise can obscure algorithm regressions |
| Large files | `executor.rs` ~5792 lines, `parser.rs` ~3484, `persistent_facts.rs` ~2189, `graph/storage.rs` ~2053 | Refactor temptation is high; behavior locks must come first |

## Recommended Order

1. R0: Baseline evidence and regression guardrails.
2. R1: Make matcher hint semantics honest.
3. R2: Benchmark checkpoint/index rebuild cost.
4. R5: Share expression evaluation.
5. R4: Refactor `not`/`not-join` exclusion keys.
6. R3: Split positive candidate fetch from nested negative/or clauses.
7. R6: Clean all-target clippy debt in a separate lane.

R1 and R5 are the safest maintainability wins. R2 should run before any storage algorithm change. R3 is potentially valuable but should wait until R1/R4 reduce query-path noise.

## R0: Baseline Evidence and Guardrails

### Problem

Several improvement candidates are plausible, but Minigraf should not trade simple, reliable behavior for unmeasured complexity.

### Proposed Work

- Add focused tests that preserve Vetch-relevant fact identity:
  - same entity/attribute with different values in the same transaction
  - same entity/attribute/value with assert and retract in one write transaction
  - `Value::Ref` values in identity-sensitive index/export paths
  - `asserted` and `tx_id` included where "full fact identity" is required
- Make the identity contract explicit before changing indexes or exports:

| Surface | Required identity fields | Notes |
| --- | --- | --- |
| Index keys used for point/range lookup | `entity`, `attribute`, encoded `value`; include `tx_id` and `asserted` only where the index represents ledger history rather than current facts | Current-view indexes may collapse by E/A/V intentionally; history indexes must not |
| Query selectors over current facts | `entity`, `attribute`, encoded `value`, temporal visibility | Must keep Datalog semantics stable; should not expose retracted rows as current results |
| Full-history export / fact log | `entity`, `attribute`, encoded `value`, `tx_id`, `tx_count`, `asserted`, `valid_from`, `valid_to` | This is the Vetch audit surface and must preserve assert-plus-retract rows |
| Receipt or row identity for Vetch ledger use | `entity`, `attribute`, encoded `value`, `tx_id`, `asserted`; include `tx_count` when ordering must survive equal millisecond timestamps | Prevents different-value collapse and exact same E/A/V assert/retract collapse |
| Value encoding tests | all `Value` variants, especially `Value::Ref` and `Value::Keyword` | Ref-like graph edges are not optional for Vetch |

- Add or update benchmark cases before algorithmic edits:
  - checkpoint after small pending writes on a large committed graph
  - positive-only selective fetch
  - query with nested `not`, `not-join`, `or`, and unbound subpatterns
  - negative-clause exclusion over many bindings

### Tests and Benchmarks

- `cargo test --test multivalue_index_test`
- `cargo test --test retract_valid_time_test`
- `cargo test --test fact_log_export_test`
- New benchmark or test fixture for checkpoint append cost.
- New benchmark or test fixture for negative-clause exclusion cost.

### Done Criteria

- Vetch ledger identity regressions fail tests before implementation changes land.
- Each modified index/export/receipt surface names whether it is current-view identity or full-history identity.
- Benchmark baselines are recorded in the relevant docs or benchmark output notes.
- No algorithmic rewrite starts without a failing test, a benchmark delta target, or a clear simplification target.

## R1: Make Matcher Hint Semantics Honest

### Problem

`PatternMatcher` can be built from a filtered fact slice with empty `Indexes`, but execution still passes planner hints into `match_with_hint_seeded`. In `matcher.rs`, `match_pattern_with_hint` can discover `FactRef`s but then ignore them and scan all facts anyway. That makes the hint layer easy to overestimate during future performance work.

### Proposed Work

Choose one of these paths after a small test/benchmark spike:

- Preferred first pass: de-emphasize or remove the no-op hint path only for slice-backed matchers with empty `Indexes`, and make `selective_fact_fetch` the explicit index optimization boundary for that path.
- Alternative: implement real slice-local lookup or `FactRef` resolution for hint-backed matching.

The first pass should favor deletion and clarity. It must not weaken any real storage-backed or index-backed lookup path. Real slice-local indexing is only justified if a benchmark shows it improves common workloads without complicating correctness.

### Tests and Benchmarks

- Unit test that confirms hinted and non-hinted matching return identical bindings.
- Regression test for seeded matching with entity, attribute, value, and ref values.
- Benchmark query patterns with and without bound entity/attribute/value.

### Risks

- Removing the hint path may look like a performance regression even if it was not effective.
- Implementing real hint resolution may duplicate storage index responsibilities.

### Done Criteria

- Matcher code no longer presents a misleading optimization surface.
- Empty-index slice-backed matcher behavior is clarified without removing useful index-backed behavior.
- Query behavior is unchanged.
- Benchmark evidence documents whether the change is performance-neutral or beneficial.

## Gate 1 Closeout: Identity and Selector Invariants

Status: closed after R0/R1 implementation in commit `7dd3df5`.

Gate 1 confirmed one important implementation fact: `PatternMatcher` is not an
index identity surface. It receives planner hints, but it matches against an
already selected fact snapshot. Storage-level narrowing belongs before matching,
currently at the executor's `selective_fact_fetch` boundary. Until a real
`FactRef`-to-snapshot resolver exists, matcher hints are advisory and must
preserve scan semantics.

Identity invariants for the next slices:

| Surface | Invariant |
| --- | --- |
| Current-view query selectors | Current queries operate on a net projection: tx-time/as-of filter, then `net_asserted_facts`, then valid-time filter, then matcher scan. They should not expose retracted rows as current results. |
| Selective fetch deduplication | Candidate fetch must deduplicate by full fact identity: `entity`, `attribute`, encoded `value`, `valid_from`, `valid_to`, `tx_count`, `tx_id`, `asserted`. This keeps candidate sets from collapsing ledger rows before net/current projection. |
| Full-history export / fact log | Exported records must preserve `entity`, `attribute`, `value`, `tx_id`, `tx_count`, valid-time scope, and `asserted`. It is an audit surface, not a current-view selector. |
| History index keys | EAVT/AEVT/AVET/VAET history keys include `tx_id` and `asserted`; VAET must preserve `Value::Ref` edges. Same Ref E/A/V rows with different `tx_id` must not collapse. |
| Same write transaction edge case | A write transaction stamps all pending facts with one final `tx_id` and one final `tx_count` at commit. Therefore exact same E/A/V retract-plus-assert rows in one write transaction require `asserted` in the identity key. |
| Vetch receipt/audit row identity | Minimum row identity is E/A/V plus `tx_id` plus `asserted`; include `tx_count` when ordering must survive equal millisecond timestamps or when replay order must be explicit. |

Gate 1 follow-up rules:

- R2 checkpoint benchmarks may proceed using these identity rules as fixed constraints.
- Storage optimizations must not move a full-history surface onto current-view identity.
- Query optimizations must prove their candidate set is a superset before applying net/current projection.
- Any future reintroduction of matcher-level index lookup must include a real resolver from `FactRef` to the current matcher snapshot and tests covering `Value::Ref`, `tx_id`, and `asserted`.

## R2: Benchmark Checkpoint and Index Rebuild Cost

### Problem

Checkpoint/save currently rebuilds all four on-disk B+tree indexes from committed plus pending facts. This is simple and reliable, but it may be too expensive for Vetch workloads that append small corrections to a large ledger.

### Proposed Work

- Add a checkpoint-cost benchmark that varies:
  - committed fact count: start with 10k, 100k, and 1M facts if local runtime is acceptable
  - pending fact count: start with 1, 10, 100, and 1k facts
  - value shape, including `Value::Ref`
  - retraction/assertion mix
- Measure:
  - checkpoint wall time
  - index page count
  - fact page count
  - WAL replay/checkpoint behavior if relevant
- Keep the first implementation pass benchmark-only.

Initial decision thresholds:

- If checkpoint time scales mostly with pending facts for Vetch-sized graphs, keep the current simple rebuild path.
- If checkpoint time scales linearly with total committed facts and a 1-to-10 fact append on a 100k+ fact graph is materially slower than Vetch's expected save cadence, evaluate batching guidance before storage algorithm changes.
- If batching cannot meet the target cadence, write a separate storage design note before implementing delta pages or incremental B+tree mutation.
- Treat any file-format change as a separate phase with migration and crash-recovery proof.

If benchmarks show unacceptable cost, evaluate these options in order:

1. Batched checkpoint policy or caller guidance.
2. Append-friendly index delta pages with compaction.
3. Incremental B+tree update path.

### Rejected for First Pass

- Immediate incremental B+tree mutation: higher correctness and file-format risk without current measurements.
- Extra sidecar index files: conflicts with the single-file design direction unless there is overwhelming evidence.
- New database dependency: violates self-contained and embedded-first goals.

### Tests and Benchmarks

- Existing storage migration and persistence tests.
- New benchmark or ignored test for large committed graph plus small pending append.
- Crash recovery test should remain green if checkpoint logic later changes.

### Done Criteria

- We can state the cost curve for current checkpoint behavior.
- The benchmark report names the graph size and checkpoint cadence that triggered, or failed to trigger, more complex storage work.
- Any later storage algorithm proposal has numeric acceptance criteria.
- No file format change is proposed without migration, crash-safety, and rollback analysis.

### R2 Closeout

Status: measured in `docs/BENCHMARKS.md` using `tests/checkpoint_rebuild_benchmark.rs`.

The benchmark fixture covered committed fact counts of 10K, 100K, and 1M with pending fact counts of 1, 10, 100, and 1K. Pending writes included `Value::Ref` assertions and legacy retractions. The result is not pending-proportional: a one-fact pending checkpoint measured 44.907 ms at 10K committed facts, 405.497 ms at 100K, and 4,829.691 ms at 1M. Gate 2 should treat current checkpoint cost as strongly tied to total committed graph size and decide separately whether batching guidance is sufficient or a storage design note is needed.

### Gate 2 Closeout: Storage Direction

Decision: adopt batching guidance immediately, and require a separate delta/index storage design note before any storage algorithm or file-format change. Current full index rebuild remains acceptable for small graphs, infrequent checkpoints, and reliability-first callers. It is not acceptable as the only long-term answer if Vetch needs frequent checkpoints on 100K+ fact ledgers.

The preferred design candidate, if Vetch proves that batching cannot meet checkpoint cadence, is append-friendly index delta pages with explicit compaction. This preserves the single-file direction better than sidecar indexes and is easier to reason about than immediate incremental B+tree mutation. A design note must cover:

- base-plus-delta lookup and merge semantics for EAVT, AEVT, AVET, and VAET
- full-history identity preservation, including `Value::Ref`, `tx_id`, `tx_count`, `asserted`, and valid-time fields
- current-view selector behavior after merging base and delta facts
- compaction trigger, crash recovery, checksum/header update order, and full-rebuild fallback
- file-format migration and rollback story if new page metadata is required
- numeric acceptance criteria against the R2 benchmark fixture

Rejected for Gate 2 implementation: immediate incremental B+tree mutation. It may be a later optimization, but it touches page splits, partial writes, checksums, and four-index consistency before Vetch has proven that append-friendly delta pages are insufficient.

### Gate 2 Update: Vetch 1M Baseline and Borrowed Ideas

Vetch now treats 1M+ facts as a baseline workload because source import, projection versioning, local activity logs, and region/case-memory recalculation can reach that scale before multimodal/object graph expansion. That changes Gate 2 from "maybe design delta/index if batching is insufficient" to "write the delta/index design note as the next storage artifact." This worktree still must not implement storage algorithm or file-format changes without that design note.

Current roadmap scope for this worktree:

- Define the delta index target: small append checkpoint/flush cost should be tied to pending/delta size, not committed graph size.
- Treat checkpointing as work that Vetch can push outside the interactive agent rhythm.
- Translate GrafeoDB's writable layered compact store idea into Minigraf terms: committed base B+trees plus append-friendly delta index segments plus explicit `recompact()`/full-rebuild fallback.
- Consider Bloom filters and zone maps only as segment-level skip metadata for facts/index entries, not as a general query-engine rewrite.
- Consider streaming execution for checkpoint, export, rebuild, and compaction paths where buffering all committed entries is the current cost source.
- Borrow design patterns from existing Rust storage engines without adopting them wholesale:
  - Fjall/LSM: sorted delta segments, journal durability, and compaction policy.
  - sled: page/update deltas with threshold-based squashing.
  - redb: crash-safe root/commit-slot discipline and checksum framing.
  - Sanakirja/Persy: versioned roots, copy-on-write page discipline, and single-file transaction constraints.
- Preserve full-history identity across base and delta: `entity`, `attribute`, encoded `value`, `valid_from`, `valid_to`, `tx_count`, `tx_id`, and `asserted`.

Reference survey for the delta/index design note: `docs/DELTA_INDEX_REFERENCE_SURVEY.md`.

First implementation slice design and test spec: `docs/DELTA_INDEX_DESIGN.md`.

Future roadmap parking lot outside this worktree's implementation scope:

- Vector-first storage in Minigraf core, including `Value::Vector`, HNSW, SIMD distance functions, and vector quantization.
- BM25, hybrid text/vector search, RRF, and graph+vector query planning as Minigraf core features. These belong first in Vetch projection/search layers backed by receipts.
- Heavy multimodal payload storage, embedding bulk storage, OCR/transcript/chunk payloads, and object-detection artifacts. Minigraf should store authority graph pointers, hashes, metadata, and relationships; Vetch should own rebuildable object/search/vector stores.
- Broad execution-engine rewrites such as push-based vectorized execution, morsel-driven parallelism, Block-STM, full columnar storage, DPccp join optimization, adaptive query execution, and transparent spilling.
- Adopting an external Rust database backend as a dependency. Revisit only if a later benchmark-backed migration proposal proves that preserving Minigraf's single-file, bi-temporal, full-history identity semantics is cheaper than implementing a narrow delta-index layer.

## R3: Split Positive Candidate Fetch from Nested Clauses

### Problem

`collect_all_patterns` includes nested patterns from `not`, `not-join`, `or`, and `or-join`. If any collected pattern lacks a bound entity or attribute, `selective_fact_fetch` returns `None`, causing a full scan. A query with a selective positive clause can therefore lose its index advantage because of an unbound nested clause.

### Proposed Work

- Split candidate-source planning into two concepts:
  - base positive patterns that can seed the initial fact candidate set
  - nested clauses that are evaluated after candidate bindings are established
- Candidate fetch must be a superset of possible results, never a premature filter:
  - use a selective anchor only when it is present in every branch that can produce a result
  - for `or`/`or-join`, compute branch-local anchors or fall back to full scan when no common safe anchor exists
  - for `not`/`not-join`, never use a negative-only pattern to seed positive candidates
  - if binding visibility is ambiguous, prefer the current full-scan fallback
- Keep negative/or semantics unchanged.
- Add tests where:
  - a selective positive clause exists
  - a nested negative or or-clause is unbound
  - results match current behavior while candidate fetch stays selective
  - an `or` branch lacks the selective anchor and therefore must not be pruned away

### Tests and Benchmarks

- Query executor tests for `not`, `not-join`, `or`, and `or-join`.
- New regression test proving nested unbound clauses do not force a full candidate scan when positive base clauses are selective.
- Benchmark before and after on a graph with many irrelevant facts.

### Risks

- Clause ordering and binding visibility can be subtle.
- Incorrectly treating nested patterns as base filters could change query semantics.

### Done Criteria

- Positive selective fetch is not disabled by unrelated nested clauses.
- Candidate fetch is proven to be a superset of final results for `not`, `not-join`, `or`, and `or-join` cases.
- Branches without a common safe anchor fall back to the current conservative scan behavior.
- All negative/or correctness tests remain green.
- Benchmark shows reduced scanned facts or lower wall time on the target case.

## R4: Refactor `not` and `not-join` Exclusion Keys

### Problem

Negative-clause evaluation currently builds exclusion keys as `HashSet<Vec<(String, Value)>>` and reconstructs probe keys per binding. This is clear but allocation-heavy.

### Proposed Work

- Introduce an internal key specification, for example:

```rust
struct ExclusionKeySpec {
    key_vars: Vec<String>,
    has_expr: bool,
}
```

- Normalize exclusion keys as ordered value tuples keyed by precomputed variable positions.
- Avoid repeated construction of `(String, Value)` pairs for each binding where possible.
- Keep expression-containing negative clauses on the conservative path until their semantics are fully covered.

### Tests and Benchmarks

- Existing `not` and `not-join` tests.
- Tests for multi-variable keys, missing variables, and expression clauses.
- Benchmark with many bindings and repeated negative probes.

### Risks

- Key-shape inference errors can create false exclusions or missed exclusions.
- Expression clauses may not fit a simple tuple-key model.

### Done Criteria

- Query results are unchanged.
- Allocation-heavy key construction is reduced in the hot path.
- Code is easier to reason about than the current sample-row key probing.

## R5: Share Expression Evaluation

### Problem

Expression evaluation logic is duplicated between executor and evaluator paths. The evaluator already notes that its code mirrors executor behavior and should be unified. Duplication increases the chance that recursive/rule evaluation drifts from direct query evaluation.

### Proposed Work

- Move shared expression helpers into a focused internal module, for example `src/query/datalog/expr.rs`.
- Keep public query types unchanged.
- Preserve existing behavior for:
  - truthiness
  - comparisons
  - arithmetic or predicate calls currently supported
  - UDF-related expression behavior
  - window/predicate interactions where applicable
- Make executor and evaluator call the shared helpers.

### Tests and Benchmarks

- Existing predicate, UDF, recursive rule, and complex query tests.
- Add targeted tests where direct query and recursive/rule evaluation exercise the same expression shape.

### Risks

- Shared helpers can become too broad if they absorb unrelated executor responsibilities.
- Minor behavior differences may surface once the duplicate implementations are unified.

### Done Criteria

- Duplicate expression logic is removed or materially reduced.
- Executor and evaluator semantics are tested through the same expression cases.
- No public API or parser syntax changes.

## R6: All-target Clippy Cleanup Lane

### Problem

Library clippy passes, but all-target clippy reports warnings in tests, examples, benches, and a few lib-test paths. This is mostly cleanup debt, not a core algorithm problem.

### Proposed Work

- Keep this lane separate from algorithm changes.
- Fix lib-test warnings first:
  - unused fault-injection imports in `src/storage/backend/mod.rs`
  - `sort_by_key` suggestion in `src/graph/storage.rs`
  - unnecessary casts in `src/graph/storage.rs`
  - approximate constant in `src/repl.rs`
- Then decide whether test/example/bench unwrap and indexing warnings should be fixed or allowed.
- Do not weaken the existing testing convention around debug-format logging of UUID-containing values.

### Tests

- `cargo clippy --lib -- -D warnings`
- `cargo clippy --all-targets --message-format short -- -D warnings`
- `cargo test`

### Risks

- Mechanical lint churn can obscure meaningful diffs.
- Replacing clear test `unwrap`/indexing with verbose code can reduce readability.

### Done Criteria

- Either all-target clippy passes, or remaining warnings are intentionally documented.
- Algorithmic changes are not mixed into lint cleanup commits.

## Verification Matrix

Use the smallest verification set that proves the current slice, then run the broader checks before merging.

| Slice | Required verification |
| --- | --- |
| R0 | targeted ledger/index/export tests, new benchmark compiles, `cargo clippy --lib -- -D warnings` |
| R1 | matcher/query tests, seeded match regression, `cargo test --test complex_queries_test`, `cargo clippy --lib -- -D warnings` |
| R2 | checkpoint benchmark, persistence tests, crash/WAL recovery tests if code changes |
| R3 | query executor tests for nested negative/or clauses, benchmark scanned-fact reduction |
| R4 | `not`/`not-join` tests, allocation-sensitive benchmark if available |
| R5 | predicate/UDF/recursive query tests, direct-vs-rule expression regression |
| R6 | `cargo clippy --all-targets --message-format short -- -D warnings`, `cargo test` |

Baseline commands:

```bash
cargo fmt --check
cargo test
cargo clippy --lib -- -D warnings
cargo clippy --test multivalue_index_test --test retract_valid_time_test --test fact_log_export_test -- -D warnings
git diff --check
```

## Merge Gates

Before merging any implementation branch based on this plan:

- Behavior-changing code has regression tests.
- Performance-changing code has before/after evidence.
- Vetch ledger identity still includes ref-like values where relevant.
- Public full-history export remains a history surface, not a current-view shortcut.
- Storage changes preserve single-file reliability and migration safety.
- No new dependency is added without explicit approval.
- `cargo fmt --check`, `cargo test`, and `cargo clippy --lib -- -D warnings` pass at minimum.

## Open Questions

- What committed graph size and checkpoint cadence should Vetch treat as realistic for first benchmarks?
- Is full index rebuild acceptable if Vetch batches writes and checkpoints less often?
- Should `PatternMatcher` remain a pure matcher over already-filtered facts, or should it own a real slice-local index?
- Do Vetch receipt/export consumers require exact preservation of same E/A/V assert-plus-retract in the same write transaction, or only protection against different-value collapse?

## Recommended Next Slice

Start with R0 plus R1:

1. Add or tighten ledger identity tests, including `Value::Ref`, `asserted`, and `tx_id`.
2. Add a small matcher-hint regression test that demonstrates hinted and unhinted matching are equivalent.
3. Remove or clarify the misleading hint path only if tests prove it is behavior-neutral.
4. Run `cargo test` and `cargo clippy --lib -- -D warnings`.

This gives Vetch a safer base before tackling checkpoint or selective-fetch algorithms.
