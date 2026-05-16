# Wave 3 — Reliability & Testing Design

**Date**: 2026-05-16
**Issues**: #209, #210, #212, #213, #214, #215, #216, #217, #219, #220, #221
**Phase**: Wave 3 of the open-issue sequence

---

## Overview

Wave 3 is a pure reliability and testing wave — no new user-facing features. It adds crash-recovery tests, fuzz targets, fault injection, migration fixtures, concurrency stress tests, property-based correctness tests, CI coverage gates, a long-haul smoke suite, and a compatibility test corpus.

---

## Key Design Decisions

| Concern | Decision |
|---|---|
| Fuzzing infrastructure | cargo-fuzz (libFuzzer), separate `fuzz/` crate, nightly only |
| Fault injection | `FaultInjectingBackend` wrapper, `#[cfg(test)]` only |
| Reference evaluator tests | proptest + naive reference evaluator |
| Coverage gates | cargo-llvm-cov + GitHub Actions |

---

## Execution Structure

Wave 3 lands in 6 PRs:

```
PR 1 (foundation)  →  FaultInjectingBackend + fuzz/ crate + proptest dev-dep
                         ↓ (unblocks all below — work in parallel worktrees)
PR 2  WAL cluster        PR 3  Storage/migration     PR 4  Query correctness
#209 crash-recovery      #215 migration fixtures      #221 proptest ref evaluator
#214 fault-injection     #216 index corruption        #213 Datalog parser fuzz
                         #217 concurrency stress       #212 coverage gates CI
                         ↓
PR 5  Long-haul (#220 real-world smoke)
                         ↓
PR 6  Compat gate (#219 XTDB/Datomic corpus)
```

PR 1 contains no test content — infrastructure only. PRs 2–4 are fully independent after PR 1 merges and can be worked in parallel worktrees. PR 5 waits on PRs 2–4. PR 6 is the explicit gate.

---

## Section 1: FaultInjectingBackend

**File**: `src/storage/backend/fault_inject.rs` (entirely `#[cfg(test)]`)

```rust
pub struct FaultInjectingBackend<B: StorageBackend> {
    inner: B,
    config: Arc<Mutex<FaultConfig>>,
}

struct FaultConfig {
    fail_write_after: Option<u64>,
    fail_sync_after: Option<u64>,
    fail_delete_after: Option<u64>,
    write_count: u64,
    sync_count: u64,
    delete_count: u64,
}
```

- Wraps any `StorageBackend` — typically `MemoryBackend` for crash-recovery or a temp-file `FileBackend` for durability tests
- "Fail on the Nth call" covers both pre-write and post-write crash scenarios
- Returns real `io::Error` — production code sees a genuine error
- `Arc<Mutex<FaultConfig>>` allows mid-scenario reconfiguration
- Zero production binary impact

**Usage**:
- (#209, #214) WAL tests: `FaultInjectingBackend<MemoryBackend>` for crash-recovery, `FaultInjectingBackend<FileBackend>` for fsync failure

---

## Section 2: fuzz/ Crate

**Location**: `fuzz/` at repo root (standard cargo-fuzz layout)

```
fuzz/
  Cargo.toml
  fuzz_targets/
    wal_entry.rs       # WAL header + entry decoding (#210)
    file_header.rs     # .graph file header + checksum validation (#210)
    fact_page.rs       # packed fact page decoding (#210)
    btree_page.rs      # B+tree page/node decoding (#210)
    datalog_parser.rs  # Datalog query parser — string input (#213)
    datalog_eval.rs    # expression clauses, predicates, bind slots (#213)
  corpus/
    wal_entry/         # seed: truncated headers, bad checksums, oversized lengths
    file_header/
    fact_page/
    btree_page/
    datalog_parser/    # seed: past parser/security edge cases
    datalog_eval/
```

Each target pattern:
```rust
fuzz_target!(|data: &[u8]| {
    // corrupt input must return Err or safe empty result, never panic
    let _ = decode_wal_entry(data);
});
```

**CI**: Fuzz targets do NOT run on every PR. A separate `fuzz.yml` workflow runs nightly with `cargo fuzz run <target> -- -max_total_time=60` (60s per target). Requires nightly toolchain.

---

## Section 3: proptest Reference Evaluator (#221)

**File**: `tests/property_test.rs`

**Part 1 — Reference evaluator** (~200 lines, intentionally naive):
```rust
// Naive triple-loop join — no optimization, no stratification, no caching.
// Intentionally independent of production executor/evaluator code.
fn ref_eval(facts: &[Fact], query: &DatalogQuery) -> Vec<Binding> { ... }
```

Handles only query shapes used in generated tests: basic joins, `not`, temporal filters, simple recursion. Correct by inspection.

**Part 2 — proptest harness**:
```rust
proptest! {
    #[test]
    fn query_results_match_reference(
        graph in arb_small_graph(),   // 2–10 entities, 5–30 facts
        query in arb_query(&graph),   // valid query over the graph's schema
    ) {
        let minigraf_result = run_minigraf(&graph, &query);
        let ref_result = ref_eval(&graph, &query);
        assert_eq!(normalize(minigraf_result), normalize(ref_result));
    }
}
```

**Generators cover**:
- Negation combined with temporal filters
- Disjunction combined with aggregation
- Recursive rules combined with negation (stratification-valid only)
- Impossible paths (expect empty results)
- Equivalent query rewrites (must produce identical result sets)

`normalize()` sorts result rows before comparison to eliminate ordering noise. `proptest` is a `[dev-dependencies]` only entry — no production binary impact.

---

## Section 4: CI Coverage Gates (#212)

**Tooling**: `cargo-llvm-cov` via `taiki-e/install-action`

**Addition to `ci.yml`**:
```yaml
- uses: taiki-e/install-action@cargo-llvm-cov
- run: cargo llvm-cov --lcov --output-path lcov.info
- uses: codecov/codecov-action@v4
  with: { files: lcov.info }
```

**New `coverage-gates.yml`** — fails PR if critical modules drop below threshold:

| Module | Initial threshold |
|---|---|
| `src/wal.rs` | 80% line coverage |
| `src/db.rs` | 75% |
| `src/storage/persistent_facts.rs` | 75% |
| `src/storage/btree_v6.rs` | 70% |
| `src/query/datalog/executor.rs` | 80% |
| `src/query/datalog/evaluator.rs` | 80% |
| `src/query/datalog/stratification.rs` | 80% |

Thresholds are set to what modules achieve after Wave 3 tests land — ratcheted upward in future waves. Values live in `coverage-thresholds.toml` (checked in, bumped via PR).

---

## Section 5: Cluster PR Details

### PR 2 — WAL cluster (#209, #214)

Adds to `tests/wal_test.rs`:

**#209 — crash-recovery matrix (7 cases)**:
- Valid WAL entry followed by truncated length/header bytes
- Valid WAL entry followed by truncated payload bytes
- Valid WAL entry followed by bad checksum
- Committed transaction + simulated crash before checkpoint
- Committed rollback + simulated crash
- Multiple committed transactions + corrupt final entry
- Corrupt tail never applied; committed entries before it still replay

**#214 — fault-injection (5 cases)** using `FaultInjectingBackend`:
- WAL append fails before fact applied to shared storage
- WAL sync fails after bytes written
- Checkpoint succeeds but WAL deletion fails
- Checkpoint/main-file sync failure returns error (not silent)
- No lock leak or stuck write state after any error path

### PR 3 — Storage/migration (#215, #216, #217)

**#215** — `tests/migration_matrix_test.rs`:
- One fixture per legacy format (v1–v6) in `tests/fixtures/legacy/`
- Each migrates and asserts: header version, query-visible facts, boundary-sized facts survive, WAL replay around migration is idempotent
- Corrupt legacy fixtures fail loudly with actionable error messages

**#216** — `tests/index_corruption_test.rs`:
- Corrupted index checksum with intact fact pages
- Corrupted B+tree leaf page bytes
- Corrupted B+tree internal page bytes
- Root page pointer mismatch / impossible page ID
- Query results after rebuild match pre-corruption expected facts

**#217** — extends `tests/concurrency_test.rs`:
- Many readers while writer commits
- Failed write path followed by successful write on same thread
- Rollback after partial transaction work
- Repeated open/write/checkpoint/query loops across threads
- Lock starvation / leaked write state after errors
- Deterministic CI bounds + larger `#[ignore]` variant for nightly

### PR 4 — Query correctness (#221, #213, #212)

- `tests/property_test.rs` — proptest reference evaluator (see Section 3)
- `fuzz/fuzz_targets/datalog_parser.rs` + `datalog_eval.rs` with seed corpus (see Section 2)
- `.github/workflows/coverage-gates.yml` + `coverage-thresholds.toml` (see Section 4)

### PR 5 — Long-haul (#220)

**File**: `tests/smoke_test.rs`

- `#[ignore]` by default; run via `cargo test -- --include-ignored smoke`
- Imports 500 entities / 5000 facts with mixed attributes and references
- Runs representative Datalog, temporal, recursive, aggregate, and prepared queries
- Reopens and checkpoints repeatedly (10 cycles)
- Asserts correctness invariants after each cycle
- Standard CI skips the suite (due to `#[ignore]`); a separate `smoke.yml` nightly workflow runs it on a schedule

### PR 6 — Compat gate (#219)

**Files**: `tests/xtdb_compat_test.rs` + `tests/datomic_compat_test.rs`

- XTDB is Apache 2.0 — verbatim porting of test semantics/data is permitted
- Datomic test material is more restricted — semantic ports (rewritten from scratch, same intent)
- Each file documents: source corpus, porting approach (verbatim vs. semantic), and explicitly lists skipped cases where behavior is intentionally out of scope or incompatible with Minigraf's philosophy
- Divergences documented inline with rationale
- Run under `cargo test` or a clearly named `#[ignore]` target if expensive

---

## Acceptance Criteria Summary

| PR | Gate |
|---|---|
| PR 1 | `FaultInjectingBackend` compiles; `cargo +nightly fuzz build` succeeds; `proptest` resolves |
| PR 2 | All 12 WAL test cases pass; no lock leaks |
| PR 3 | All migration, corruption, and concurrency tests pass |
| PR 4 | proptest runs without failures; fuzz targets build; coverage gates enforce thresholds |
| PR 5 | Smoke suite passes under `--include-ignored`; invariants hold after all 10 cycles |
| PR 6 | Both compat suites pass; skipped cases documented; license review complete |
