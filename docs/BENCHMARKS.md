# Minigraf Benchmarks

## Post-1.0 Updates

v1.1.x shipped several query and storage path changes that affect benchmark numbers:

- **Selective B+tree lookup** (#208, v1.1.0): `filter_facts_for_query` now uses index-backed per-FactRef resolution for patterns with a bound entity or attribute, instead of streaming all packed pages. Point-attribute and join queries at ≥10K now call `resolve()` once per matching fact — trading lower per-query I/O against higher per-fact call overhead at large N.
- **Backend mutex fix** (#279, v1.1.1): `CommittedFactLoaderImpl::resolve` was holding the backend mutex across `PageCache::get_or_load`, serialising concurrent readers even on cache hits. Fixed by deferring mutex acquisition to `read_page` (cache misses only).
- **Per-resolve overhead fix** (#281, v1.1.1): #279 introduced `Arc::clone` per `resolve()` call. With 10k+ FactRefs per query iteration, this caused measurable single-threaded regressions (+22% on `point_attribute/10k`). Fixed by pre-building `MutexStorageBackend` once per `CommittedFactLoaderImpl` instance.

Numbers updated below reflect the Bencher CI baseline (ubuntu-latest runner) where noted. The Query Latency and Time-Travel tables were fully re-measured 2026-07-11 on the A0 environment (see Environment), replacing the last v0.8.0-era 100K/1M rows.

**Live benchmark history**: [https://bencher.dev/perf/minigraf/plots](https://bencher.dev/perf/minigraf/plots)

Benchmark results for Minigraf. Core query benchmarks were updated in v0.13.1 (Phase 7.4 — query path snapshot fix). New benchmark groups for window functions, temporal metadata, UDFs, count-distinct, and regex filter added in v0.17.0 (Phase 7.8). Negation, disjunction, aggregation, and expression benchmarks were first run on v0.13.0 and selectively re-run on v0.13.1. Throughput reporting (facts/sec, aggregate ops/sec), retraction benchmarks, prepared query benchmarks, and checkpoint@1M added in v0.20.1.

## Environment

| Property | Value |
|---|---|
| CPU | Intel Core i7-1065G7 @ 1.30GHz (4 cores / 8 threads) |
| RAM | 16 GB |
| OS | Manjaro Linux 6.12.73-1 |
| Rust | 1.94.0 |
| Profile | `release` (`opt-level = 3`, `lto = "thin"`, `panic = "abort"`) |
| Swap | None |

Sections marked "A0 environment" (Query Latency, Time-Travel, and the A0
caller-shaped suites, all measured 2026-07-11) used a different machine:

| Property | Value (A0 environment) |
|---|---|
| CPU | AMD Ryzen 7 7800X3D (8 cores / 16 threads) |
| RAM | 32 GB |
| OS | Linux 6.6 (WSL2) |
| Rust | 1.96.0-nightly |
| Browser | Chrome for Testing 150 (headless) |

Cross-machine comparisons between the two environments are indicative only;
within-table scale-to-scale shape is the evidence, not absolute deltas
against older i7 rows.

### 1.01M committed attribute scan locality and streaming aggregate

Measured 2026-07-12 on the A0/HAL7800 environment against the same clean
342 MiB v11 graph containing 1.01M `:cmp/value` integer facts. Each row is a
fresh release `minigraf --session` process measured with `/usr/bin/time -v`;
session output is discarded. The before rows are the clean `f7aa6cc` baseline.

| Query | Before wall / max RSS | After wall / max RSS | Change |
|---|---:|---:|---:|
| `(count ?v)` over `:cmp/value` | 5.39 s / 563 MiB | 2.01 s / 379 MiB | -62.7% wall, -32.7% RSS |
| return all 1.01M `?v` values | 5.41 s / 508 MiB | 2.56 s / 506 MiB | -52.7% wall, RSS unchanged |

The time reduction comes from resolving committed AEVT references in physical
`(page_id, slot_index)` order, so all referenced slots are decoded while their
packed page remains in the bounded cache. Datalog does not promise implicit
row ordering, and net-assertion/valid-time selection is order-independent. The
aggregate memory reduction comes from visiting single-pattern matches into an
incremental group sink rather than retaining one binding map per fact. Full
result queries still intentionally materialize their result rows and therefore
retain the prior approximately 506 MiB RSS shape; a future bounded result cursor
is required to change that public result boundary.

### Cross-database scan comparison contract (v2)

The cross-database stress harness does not publish one ambiguous `fullScan`
ranking. It records two independently verified workloads:

| Workload | Comparable boundary | Included engines |
|---|---|---|
| `engineAggregate` | Engine computes count and sum, returns one scalar row | Vicia Datalog, CozoScript, SQLite SQL; redb is N/A |
| `materializedScan` | Adapter owns every value in one `Vec<i64>`, then the shared Rust fold computes count/checksum | Vicia, Cozo, SQLite, redb |

Each workload runs in a fresh child process after the stress database is built.
Its elapsed time excludes open, while its `VmHWM` includes open plus that one
scan only. Build/checkpoint/append memory and the other scan can therefore not
pollute the read-memory comparison. Both workloads must independently equal the
same arithmetic count and checksum. The v2 summarizer also checks the exact
engine-specific execution-boundary label and rejects a redb aggregate result;
redb has no query engine and must not receive a simulated scalar fast path.
Only a full-profile summary whose source checkout was clean is marked
`acceptanceEligible`; the Markdown and JSON summaries both carry the exact
source commit and testbed identity.

The historical 2026-07-12 v1 baseline remains useful for build, append, point
read, reopen, storage, and crash evidence. Its single scan column compared
Vicia/Cozo materialization with SQLite engine aggregation and redb iteration,
and its process-wide RSS included build/checkpoint history; do not use those two
v1 fields for cross-engine ranking.

Benchmarks were run with [Criterion 0.8](https://bheisler.github.io/criterion.rs/book/). Each benchmark group is described below.

### Evidence contract, milestones, and CI coverage

The operational development gate is
[`BENCHMARK_MILESTONES.md`](BENCHMARK_MILESTONES.md). Its machine-readable
authority is `benchmarks/milestones.json`, which owns milestone ids, profiles,
commands, decision questions, and absolute budgets for native, browser, and
Vetch product evidence.

Nightly CI treats `benches/minigraf_bench.rs` as the Criterion group source of
truth. `node scripts/check-benchmark-coverage.mjs` verifies both directions:
every literal Criterion group must match a nightly workflow filter, and every
workflow filter must match at least one group. This prevents a newly added group
from compiling successfully while remaining absent from longitudinal tracking.

All native caller-shaped harnesses retain their existing human-readable output
and can also emit `vicia.benchmark.receipt.v1` JSON. For example:

```bash
VICIA_BENCH_RECEIPT=target/vetch-cadence-smoke.json \
VICIA_BENCH_TESTBED=local-a0 \
cargo bench --bench vetch_cadence_benchmark -- smoke
node scripts/check-benchmark-receipt.mjs target/vetch-cadence-smoke.json
```

The receipt records milestone ownership, source and executable provenance,
testbed/host identity, executable and fixture SHA-256, configuration, sorted raw
latency samples, quantiles through p99, mean, standard deviation, MAD, CV, file
growth, expected/actual correctness, and catalog-derived budget checks. Receipt
validation recomputes the summaries, correctness verdict, budgets, failures,
and acceptance eligibility. Smoke receipts are never
acceptance-eligible; full receipts additionally require a clean source checkout
and sufficient observations for each p95-gated metric. Bencher prediction
thresholds remain longitudinal signals rather than percentage-regression
allowances.

### How to read these numbers

**All times are per-call latency** — the time for a single operation (one insert, one query, one open, etc.), not a total or cumulative time.

**Some benchmarks also report throughput** (elements/second, shown as `K elem/s` or `elem/s`):
- **Batch inserts / retractions**: throughput is facts/second — `Throughput::Elements(100)` over a 100-fact batch, enabling apples-to-apples comparison with single-fact inserts.
- **Concurrent groups**: throughput is aggregate ops/second across *all threads combined* — `Throughput::Elements(n_threads)` per Criterion iteration. This answers "does total system throughput scale with thread count?" independently of per-thread latency.

Criterion measures this by running each operation repeatedly and computing a median:

1. **Warm-up** (3 s): the operation is run and discarded to let CPU caches and OS buffers reach steady state.
2. **Measurement**: Criterion collects N *samples*. For each sample it runs the operation M times (chosen automatically so the sample takes long enough to time accurately), records the total elapsed time, then divides by M to get a single per-call estimate.
3. **Reported time**: the **median** across all N samples. The median is used rather than the mean because it is robust to occasional slow outliers (OS scheduler jitter, CPU frequency scaling, etc.).

Sample counts vary by benchmark speed:
- Fast operations (inserts, ~µs): **100 samples** (default) — thousands of iterations per sample.
- Slow operations (queries at large scale, recursion, concurrent scans): **10 samples** — only a handful of iterations are feasible per sample.

The column headers (e.g. "1K facts", "10K facts") indicate the **size of the database at the time the operation was measured**, not how many operations were performed.

---

## Insert Latency

Measures per-fact insert latency at three dataset sizes (1K / 10K / 100K facts in the database at insert time).

### In-Memory Backend

| Benchmark | 1K facts | 10K facts | 100K facts |
|---|---|---|---|
| `single_fact` (transact one fact at a time) | 2.65 µs | 2.74 µs | 2.69 µs |
| `batch_100` (100 facts per transact call) | 317 µs | 318 µs | 315 µs |
| `explicit_tx` (WriteTransaction, single fact) | 2.69 µs | 2.70 µs | 2.83 µs |

Single-fact insert is constant across dataset sizes — the in-memory pending index is O(1) per insert.

### File-Backed Backend

| Benchmark | 1K facts | 10K facts | 100K facts |
|---|---|---|---|
| `single_fact` | 3.77 µs | 3.55 µs | 3.51 µs |
| `batch_100` | 210 µs | 212 µs | 221 µs |
| `explicit_tx` | 3.60 µs | 3.63 µs | 3.54 µs |

File-backed insert latency is constant — writes go to the WAL sidecar, not the `.graph` file directly, so insert cost is independent of database size.

### Batch Insert Throughput (facts/sec)

`batch_100` with `Throughput::Elements(100)` — reports facts/sec for a 100-fact batch at each DB scale (v0.20.1).

| Backend | 1K | 10K | 100K | 1M |
|---|---|---|---|---|
| In-memory | 139 K/s | 130 K/s | 129 K/s | 128 K/s |
| File-backed (WAL) | 120 K/s | 120 K/s | 123 K/s | 137 K/s |

Throughput is essentially flat across DB sizes for both backends — confirms the O(1)-per-insert property of the WAL path. In-memory is ~10% faster than file-backed; the difference is WAL fsync overhead. At 1M facts, file-backed throughput is slightly higher than at 100K due to batch amortisation over a warmer path (OS page cache pre-warmed from the populate phase).

---

## Retraction Throughput

Measures `(retract [...])` performance — a first-class bi-temporal operation that logically deletes facts by asserting `asserted=false` entries. Uses `batch_100` (100 retractions per call) with `Throughput::Elements(100)` to report facts/sec.

| DB size | Throughput | Latency/batch |
|---|---|---|
| 1K | 148 K/s | 677 µs |
| 10K | 147 K/s | 681 µs |
| 100K | 146 K/s | 686 µs |
| 1M | 143 K/s | 700 µs |

Retraction throughput matches batch insert throughput (~130–148 K facts/sec) and is equally flat across DB sizes. The retraction path writes a `asserted=false` WAL entry per fact — structurally identical to an insert — so parity with insertion cost is expected. The slight decline at 1M reflects a larger in-memory pending index during the measurement window.

---

## Query Latency

Measures single-query latency against in-memory databases pre-loaded with
1K / 10K / 100K / 1M facts. Full table re-measured 2026-07-11 on the A0
environment (see Environment) with `MINIGRAF_BENCH_MODE=full`; criterion
median of 10 samples. This supersedes the earlier mixed-origin table whose
100K/1M cells were v0.8.0-era (pre-selective-lookup: `point_entity` 266 ms /
4.33 s at 100K/1M).

| Benchmark | 1K | 10K | 100K | 1M |
|---|---|---|---|---|
| `point_entity` (query by entity + attribute) | 3.8 µs | 3.8 µs | 3.9 µs | 4.1 µs |
| `point_attribute` (query by attribute only) | 1.5 ms | 14.9 ms | 150 ms | 485 ms |
| `join_3pattern` (3-clause join) | 4.1 ms | 48.8 ms | 259 ms | 920 ms |

`point_entity` is now flat across scales — O(k) where k = matching facts —
because patterns with a bound entity or attribute use selective B+tree index
lookups (EAVT/AEVT/AVET) since v1.1.0 (#208) instead of a full page scan.
`point_attribute` and `join_3pattern` return result sets proportional to N,
so they remain linear: the cost is result materialization, not the lookup.
Starting from Phase 7.4, the non-rules query path no longer rebuilds
in-memory EAVT/AEVT/AVET/VAET indexes on each call — facts are passed as a
pre-filtered `Arc<[Fact]>` slice.

---

## Time-Travel Query Latency

Re-measured 2026-07-11 (A0 environment, same run as Query Latency).
Supersedes the v0.8.0-era table (`as_of_counter` 276 ms / 4.49 s at
100K/1M).

| Benchmark | 1K | 10K | 100K | 1M |
|---|---|---|---|---|
| `as_of_counter` (`:as-of` by tx counter) | 4.2 µs | 4.2 µs | 4.3 µs | 4.5 µs |
| `valid_at` (`:valid-at` timestamp) | 4.5 µs | 4.6 µs | 4.7 µs | 4.8 µs |

Entity-bound time travel is flat across scales: the selective index path
(v1.1.0 #208, Q1-B as-of pushdown) applies to temporal reads as well, and
temporal filtering adds well under a microsecond over the plain point read.

---

## Prepared Query Latency

`PreparedQuery` (parse-once/execute-many via `db.prepare(...)` + `pq.execute(...)`) moves parser overhead out of the hot path. Relevant for AI agents that issue the same query pattern repeatedly with different bind values (v0.20.1).

| Benchmark | 1K | 10K |
|---|---|---|
| `value_lookup` (`[?e :val $val]`, returns 1 result) | 1.52 ms | 17.3 ms |
| `threshold_filter` (`[(< ?v $threshold)]`, returns ~50% of facts) | 5.34 ms | 57.8 ms |

`value_lookup` scans all facts for a matching `:val` attribute (AVET index path); `threshold_filter` additionally evaluates an expression predicate on every binding. Both scale linearly with DB size. The parse step is paid once at `prepare` time and is not reflected in these numbers.

---

## Recursive Rules

| Benchmark | Time |
|---|---|
| `chain/depth_10` (linear chain, 10 hops) | 2.75 ms |
| `chain/depth_100` (linear chain, 100 hops) | 16.27 s |
| `fanout/w10_d3` (fanout width=10, depth=3) | 5.12 s |

Recursive rule evaluation uses semi-naive fixed-point iteration. Deep chains scale super-linearly: each iteration must re-evaluate all intermediate facts. The semi-naive evaluator avoids redundant recomputation, but `chain/depth_100` still requires ~100 iterations of growing intermediate tables.

---

## Database Open / Replay

Measures cold-open latency (loading a committed `.graph` file) and WAL replay latency.

| Benchmark | 1K | 10K | 100K | 1M |
|---|---|---|---|---|
| `checkpointed` (open committed v6 file) | 7.24 ms | 12.20 ms | 118.9 ms | 1.314 s |
| `wal_replay` (replay uncommitted WAL) | 8.30 ms | 13.4 ms | — | — |

**Phase 6.5 improvement:** v6 open no longer loads indexes into RAM. At 1M facts, open time dropped from **3.14 s → 1.31 s** (2.4×). At 100K: **259 ms → 119 ms** (2.2×). The remaining cost is dominated by WAL check plus page-cache warming on the first query.

At small sizes (1K), v6 open is slower than v5 (7.2 ms vs 1.83 ms) — the per-open overhead (header I/O, B+tree root setup, WAL check) is not amortised enough at 1K facts to overcome the benefit of not loading a tiny index.

---

## Checkpoint

Measures time to flush the WAL to committed `.graph` pages (including B+tree rebuild for all four indexes).

| Benchmark | 1K | 10K | 100K |
|---|---|---|---|
| `checkpoint` | 1.25 ms | 11.80 ms | — |

> 100K and 1M variants added in v0.20.1 but not yet run on this machine (each iteration requires a fresh 100K/1M-fact WAL setup — setup cost dominates at `sample_size(10)`). Numbers will be added in the next benchmark pass.

Checkpoint now includes a merge-sort of committed + pending entries and a B+tree rebuild across all four indexes (EAVT, AEVT, AVET, VAET). At 10K facts this is **11.8 ms** — slightly faster than the v5 paged-blob serialisation (16.5 ms), as the B+tree writer makes fewer random-access passes.

### R2: Checkpoint Rebuild After Small Pending Writes

Run: 2026-06-05, `cargo test --release --test checkpoint_rebuild_benchmark -- --ignored --nocapture`.

Fixture: `tests/checkpoint_rebuild_benchmark.rs` builds a checkpointed base file, copies it, adds pending writes through the public API with auto-checkpoint disabled, then measures one explicit `checkpoint()` call. Pending writes include `Value::Ref` assertions and legacy retractions. The fixture is an ignored test, not a Criterion benchmark, so these are single-run measurements meant to answer the R2 scaling question rather than produce CI-grade distributions.

| Committed facts | Pending facts | Pending assertions | Pending retractions | Checkpoint time | Base file bytes | Post-checkpoint file bytes | WAL bytes before checkpoint |
|---|---:|---:|---:|---:|---:|---:|---:|
| 10K | 1 | 1 | 0 | 44.907 ms | 3,973,120 | 3,981,312 | 126 |
| 10K | 10 | 8 | 2 | 47.043 ms | 3,973,120 | 3,985,408 | 786 |
| 10K | 100 | 75 | 25 | 60.152 ms | 3,973,120 | 4,030,464 | 7,382 |
| 10K | 1K | 750 | 250 | 185.111 ms | 3,973,120 | 4,464,640 | 73,384 |
| 100K | 1 | 1 | 0 | 405.497 ms | 40,050,688 | 40,058,880 | 126 |
| 100K | 10 | 8 | 2 | 409.203 ms | 40,050,688 | 40,062,976 | 786 |
| 100K | 100 | 75 | 25 | 568.861 ms | 40,050,688 | 40,103,936 | 7,382 |
| 100K | 1K | 750 | 250 | 749.671 ms | 40,050,688 | 40,538,112 | 73,384 |
| 1M | 1 | 1 | 0 | 4,829.691 ms | 407,191,552 | 407,195,648 | 127 |
| 1M | 10 | 8 | 2 | 5,368.482 ms | 407,191,552 | 407,212,032 | 796 |
| 1M | 100 | 75 | 25 | 4,468.865 ms | 407,191,552 | 407,236,608 | 7,482 |
| 1M | 1K | 750 | 250 | 4,492.069 ms | 407,191,552 | 407,670,784 | 74,384 |

R2 observation: checkpoint cost is strongly tied to total committed graph size. With a one-fact pending append, cost rises from 44.9 ms at 10K committed facts to 405.5 ms at 100K and 4.83 s at 1M. Pending size has a secondary effect, especially at smaller committed sizes, but the measurements do not look pending-proportional. Gate 2 adopted batching guidance as the immediate policy and requires a separate delta/index storage design note before any storage algorithm or file-format change. The first design candidate is append-friendly index delta pages with explicit compaction, not immediate incremental B+tree mutation.

### T6: Delta Checkpoint After Small Pending Writes

Run: 2026-06-05, `cargo test --release --test checkpoint_rebuild_benchmark -- --ignored --nocapture`.

Fixture: same public-API fixture as R2, now updated for the v10 single-segment delta path. It builds a checkpointed base file, copies it, adds pending writes with auto-checkpoint disabled, then measures:

- `delta_flush_ms`: one explicit `checkpoint()` on a clean base with pending facts.
- `reopen_delta_ms`: reopening the file after the selected delta manifest is published.
- `recompact_proxy_ms`: one additional pending fact followed by `checkpoint()` while a delta manifest is visible. This is the current public proxy for recompact because the branch does not expose a public `recompact()` API yet.

Pending writes include `Value::Ref` assertions and legacy retractions. The fixture is still an ignored single-run benchmark, not a Criterion distribution.

| Committed facts | Pending facts | Assertions/retractions | Delta flush | Reopen delta | Recompact proxy | Base pages | Delta pages | Recompact pages | Delta WAL bytes | Recompact WAL bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10K | 1 | 1/0 | 9.614 ms | 2.845 ms | 51.744 ms | 970 | 972 | 972 | 126 | 126 |
| 10K | 10 | 8/2 | 12.252 ms | 3.788 ms | 47.045 ms | 970 | 973 | 973 | 786 | 126 |
| 10K | 100 | 75/25 | 20.328 ms | 3.096 ms | 43.079 ms | 970 | 980 | 984 | 7,382 | 126 |
| 10K | 1K | 750/250 | 105.387 ms | 5.592 ms | 74.707 ms | 970 | 1,057 | 1,089 | 73,384 | 126 |
| 100K | 1 | 1/0 | 56.493 ms | 28.744 ms | 382.917 ms | 9,778 | 9,780 | 9,780 | 126 | 126 |
| 100K | 10 | 8/2 | 163.232 ms | 28.154 ms | 413.220 ms | 9,778 | 9,781 | 9,781 | 786 | 126 |
| 100K | 100 | 75/25 | 136.122 ms | 29.136 ms | 422.527 ms | 9,778 | 9,788 | 9,790 | 7,382 | 126 |
| 100K | 1K | 750/250 | 176.673 ms | 34.197 ms | 453.180 ms | 9,778 | 9,865 | 9,896 | 73,384 | 126 |
| 1M | 1 | 1/0 | 512.109 ms | 307.388 ms | 6,453.747 ms | 99,412 | 99,414 | 99,414 | 127 | 127 |
| 1M | 10 | 8/2 | 507.650 ms | 302.631 ms | 7,072.497 ms | 99,412 | 99,415 | 99,416 | 796 | 127 |
| 1M | 100 | 75/25 | 541.249 ms | 284.025 ms | 7,005.091 ms | 99,412 | 99,422 | 99,425 | 7,482 | 127 |
| 1M | 1K | 750/250 | 634.182 ms | 280.315 ms | 6,713.898 ms | 99,412 | 99,501 | 99,528 | 74,384 | 127 |

T6 observation: the delta path is a large improvement over R2 full rebuild for the critical 1M base plus one pending fact case: 4,829.691 ms -> 512.109 ms. Page growth is also bounded for small deltas, which indicates that the checkpoint is publishing appended delta pages rather than rebuilding all base index pages.

T6 does not fully satisfy the stricter Vetch target yet. Delta flush and delta reopen still scale with committed file size: 10K/1 pending is 9.614 ms, 100K/1 pending is 56.493 ms, and 1M/1 pending is 512.109 ms. The likely cause is visible in the storage path: delta publish and reopen both compute a checksum over all data pages when a selected delta is present. Recompact proxy remains O(total facts), which is acceptable only if scheduled outside the interactive agent rhythm.

Next gate: make delta publish and delta reopen validate only the newly appended delta segment/manifest plus stable base metadata, while preserving full-file checksum validation for full rebuild, repair, and explicit recompact.

### T7A: Delta Checksum Scope

Run: 2026-06-05, `cargo test --release --test checkpoint_rebuild_benchmark -- --ignored --nocapture`.

Fixture update: the copied base file is now `sync_all()`ed before the timed delta checkpoint. This keeps setup I/O out of `delta_flush_ms`; otherwise the checkpoint's `sync()` can flush dirty pages from the benchmark's just-copied 407 MB base file.

Code change: v10 delta manifests now carry base identity (base page count, fact page count, base checkpoint tx_count, base roots, and base checksum). Delta publish keeps the base checksum in page 0 and validates only the new delta segment, manifest payload, and header slot. Reopen validates manifest/base identity and selected delta bytes instead of recomputing a checksum over all data pages. Full rebuild and recompact proxy still compute full-file checksums.

| Committed facts | Pending facts | Assertions/retractions | Delta flush | Reopen delta | Recompact proxy | Base pages | Delta pages | Recompact pages | Delta WAL bytes | Recompact WAL bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10K | 1 | 1/0 | 3.283 ms | 0.081 ms | 40.681 ms | 970 | 972 | 972 | 126 | 126 |
| 10K | 10 | 8/2 | 5.212 ms | 0.136 ms | 42.266 ms | 970 | 973 | 973 | 786 | 126 |
| 10K | 100 | 75/25 | 12.054 ms | 0.296 ms | 52.829 ms | 970 | 980 | 984 | 7,382 | 126 |
| 10K | 1K | 750/250 | 105.020 ms | 2.536 ms | 84.589 ms | 970 | 1,057 | 1,089 | 73,384 | 126 |
| 100K | 1 | 1/0 | 4.162 ms | 0.098 ms | 524.669 ms | 9,778 | 9,780 | 9,780 | 126 | 126 |
| 100K | 10 | 8/2 | 6.999 ms | 0.150 ms | 528.993 ms | 9,778 | 9,781 | 9,781 | 786 | 126 |
| 100K | 100 | 75/25 | 15.525 ms | 0.329 ms | 506.770 ms | 9,778 | 9,788 | 9,790 | 7,382 | 126 |
| 100K | 1K | 750/250 | 116.350 ms | 4.368 ms | 491.319 ms | 9,778 | 9,865 | 9,896 | 73,384 | 126 |
| 1M | 1 | 1/0 | 5.266 ms | 0.114 ms | 6,922.956 ms | 99,412 | 99,414 | 99,414 | 127 | 127 |
| 1M | 10 | 8/2 | 4.637 ms | 0.112 ms | 6,105.198 ms | 99,412 | 99,415 | 99,416 | 796 | 127 |
| 1M | 100 | 75/25 | 15.547 ms | 0.336 ms | 6,677.004 ms | 99,412 | 99,422 | 99,425 | 7,482 | 127 |
| 1M | 1K | 750/250 | 302.879 ms | 4.272 ms | 9,588.647 ms | 99,412 | 99,501 | 99,528 | 74,384 | 127 |

T7A observation: the strict Vetch small-write target is now met for the measured single-segment path. The critical 1M base plus one pending fact case improved from the R2 full-rebuild baseline of 4,829.691 ms and the T6 delta baseline of 512.109 ms to 5.266 ms. Reopen improved from 307.388 ms to 0.114 ms. Small delta publish and reopen are now tied to pending/delta size plus sync overhead, not committed graph size. Recompact proxy remains O(total facts), as intended for work scheduled outside the interactive agent rhythm.

### T7B: Double-Buffered Manifest Publish

Run: 2026-06-05, `cargo test --release --test checkpoint_rebuild_benchmark -- --ignored --nocapture`.

Fixture update: the third timing column is now `second_delta_flush_ms`, not `recompact_proxy_ms`. T7B changes the visible-delta checkpoint policy: a second small write over a selected delta publishes a replacement single-segment delta through the inactive manifest slot instead of forcing an interactive full rebuild. Explicit recompact/full rebuild remains a separate O(total) maintenance path.

Code change: v10 header extension slots are now the real publish boundary. Checkpoint writes the new segment and manifest, syncs them, then publishes the descriptor into the inactive slot. Reopen validates both slots independently and selects the highest generation whose slot descriptor, manifest payload, and referenced segment pages all verify. A corrupt newer slot, manifest payload, or delta segment falls back to the previous valid slot; no valid committed delta slot remains an error.

| Committed facts | Pending facts | Assertions/retractions | Delta flush | Reopen delta | Second delta flush | Base pages | Delta pages | Second delta pages | Delta WAL bytes | Second delta WAL bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10K | 1 | 1/0 | 3.606 ms | 0.081 ms | 3.120 ms | 970 | 972 | 974 | 126 | 126 |
| 10K | 10 | 8/2 | 4.570 ms | 0.139 ms | 4.296 ms | 970 | 973 | 976 | 786 | 126 |
| 10K | 100 | 75/25 | 13.572 ms | 0.304 ms | 15.838 ms | 970 | 980 | 990 | 7,382 | 126 |
| 10K | 1K | 750/250 | 120.629 ms | 3.444 ms | 128.456 ms | 970 | 1,057 | 1,144 | 73,384 | 126 |
| 100K | 1 | 1/0 | 2.843 ms | 0.099 ms | 3.830 ms | 9,778 | 9,780 | 9,782 | 126 | 126 |
| 100K | 10 | 8/2 | 4.572 ms | 0.102 ms | 3.751 ms | 9,778 | 9,781 | 9,784 | 786 | 126 |
| 100K | 100 | 75/25 | 13.704 ms | 0.649 ms | 12.283 ms | 9,778 | 9,788 | 9,798 | 7,382 | 126 |
| 100K | 1K | 750/250 | 102.439 ms | 2.412 ms | 109.317 ms | 9,778 | 9,865 | 9,952 | 73,384 | 126 |
| 1M | 1 | 1/0 | 3.336 ms | 0.088 ms | 2.852 ms | 99,412 | 99,414 | 99,416 | 127 | 127 |
| 1M | 10 | 8/2 | 4.395 ms | 0.148 ms | 3.853 ms | 99,412 | 99,415 | 99,418 | 796 | 127 |
| 1M | 100 | 75/25 | 11.734 ms | 0.325 ms | 13.440 ms | 99,412 | 99,422 | 99,433 | 7,482 | 127 |
| 1M | 1K | 750/250 | 109.515 ms | 2.547 ms | 113.091 ms | 99,412 | 99,501 | 99,590 | 74,384 | 127 |

T7B observation: T7A's 1M+1 small-write gate did not regress. It improved from 5.266 ms / 0.114 ms to 3.336 ms / 0.088 ms in this run. The second write over an already visible delta is also pending-sized at 2.852 ms for 1M+1, because it publishes through the inactive manifest slot instead of rebuilding the base graph.

### T7C: Accumulated Delta Receipt Cadence

Run: 2026-06-05, `cargo bench --bench delta_accumulation_benchmark`.

Fixture: `benches/delta_accumulation_benchmark.rs` builds a checkpointed 1M-fact base, copies it per scenario, disables auto-checkpoint, then appends receipt-like `Value::Ref` facts and explicitly checkpoints after each receipt batch. It measures flush latency for every checkpoint and samples reopen/current/as-of query latency at up to 32 evenly spaced probe points per scenario. The query probes intentionally model Vetch's "write receipt, then read it into the next agent brief" rhythm.

Base file: 407,179,264 bytes / 99,409 pages. `actual_delta_facts` is computed from `export_fact_log()` after each scenario. `corrupt_latest_fallback` corrupts the latest visible delta segment and verifies reopen falls back to the previous valid manifest slot.

| Facts/checkpoint | Checkpoints | Delta facts | Probes | Flush p50 | Flush p95 | Flush max | Reopen p50 | Reopen p95 | Reopen max | Current query p50 | Current query p95 | Current query max | As-of query p50 | As-of query p95 | As-of query max | File growth | Page growth | Actual delta facts | Corrupt fallback |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 10 | 10 | 10 | 3.310 ms | 3.882 ms | 3.882 ms | 0.087 ms | 0.159 ms | 0.159 ms | 0.073 ms | 0.148 ms | 0.148 ms | 2,149.602 ms | 2,349.469 ms | 2,349.469 ms | 86,016 B | 21 | 10 | true |
| 1 | 100 | 100 | 32 | 7.645 ms | 12.737 ms | 13.685 ms | 0.200 ms | 0.348 ms | 0.429 ms | 0.067 ms | 0.111 ms | 0.125 ms | 1,954.557 ms | 2,176.345 ms | 2,257.758 ms | 2,560,000 B | 625 | 100 | true |
| 1 | 1K | 1K | 32 | 53.280 ms | 102.385 ms | 117.191 ms | 1.185 ms | 2.947 ms | 3.475 ms | 0.080 ms | 0.124 ms | 0.129 ms | 1,940.586 ms | 2,313.958 ms | 2,369.837 ms | 193,953,792 B | 47,352 | 1,000 | true |
| 1 | 10K | 10K | 32 | 580.898 ms | 1,051.300 ms | 1,559.688 ms | 16.829 ms | 26.618 ms | 36.794 ms | 0.239 ms | 0.843 ms | 0.881 ms | 2,187.777 ms | 2,724.895 ms | 3,006.894 ms | 18,910,830,592 B | 4,616,902 | 10,000 | true |
| 10 | 100 | 1K | 32 | 52.522 ms | 97.086 ms | 107.716 ms | 1.247 ms | 2.239 ms | 2.543 ms | 0.071 ms | 0.097 ms | 0.120 ms | 1,661.411 ms | 1,754.994 ms | 1,780.161 ms | 19,558,400 B | 4,775 | 1,000 | true |
| 10 | 1K | 10K | 32 | 518.268 ms | 1,010.723 ms | 1,174.454 ms | 13.825 ms | 27.198 ms | 27.213 ms | 0.130 ms | 0.199 ms | 0.199 ms | 1,735.617 ms | 2,418.004 ms | 2,470.194 ms | 1,878,642,688 B | 458,653 | 10,000 | true |
| 100 | 100 | 10K | 32 | 613.283 ms | 1,017.925 ms | 1,231.897 ms | 12.504 ms | 29.157 ms | 31.001 ms | 0.132 ms | 0.221 ms | 0.224 ms | 1,865.914 ms | 2,015.893 ms | 2,439.979 ms | 189,546,496 B | 46,276 | 10,000 | true |

T7C observation: the current single-segment replacement path is not viable as the long-running Vetch receipt cadence. The 1M base plus one fact repeated to 1K accumulated delta facts already misses the proposed single-segment gate: hot flush p95 is 102.385 ms, above the 50 ms target. At 10K accumulated delta facts, flush p95 is roughly one second and file growth reaches 18.9 GB for the one-fact checkpoint cadence. Batching reduces file growth sharply, but not hot flush p95: both 10x1K and 100x100 end near 1 second p95 at 10K accumulated delta facts.

Reopen remains acceptable for the measured matrix (p95 <= 29.157 ms), and immediate current-query latency after writes remains sub-millisecond even at 10K accumulated delta facts. The separate blocker is as-of/replay query latency: every scenario spends about 1.75-2.72 s p95 to read the just-written receipt through the as-of path. That is not a delta publish failure; it is a read/query path problem that matters for Vetch agent briefs.

T7C verdict: proceed to a multi-segment manifest design before treating this as a production Vetch storage rhythm. Keep durable append immediate, allow receipt/slice checkpoint batching, schedule recompact only in idle/background/maintenance windows, and forbid foreground full rebuild for normal Vetch work.

### T8B: Multi-Segment Mini Gate

Run: 2026-06-05, `MINIGRAF_DELTA_ACCUMULATION_MODE=t8b-mini cargo bench --bench delta_accumulation_benchmark`.

Fixture update: the same `benches/delta_accumulation_benchmark.rs` harness now supports a `t8b-mini` mode that runs the two T8B gate scenarios before the full T8C matrix. The CSV output now includes `segment_count`, computed by scanning visible delta segment payload markers before the corrupt-latest fallback check.

Base file: 407,179,264 bytes / 99,409 pages. `actual_delta_facts` is computed from `export_fact_log()` after each scenario. `corrupt_latest_fallback` corrupts the latest visible delta segment and verifies reopen falls back to the previous valid manifest slot.

| Facts/checkpoint | Checkpoints | Delta facts | Probes | Flush p50 | Flush p95 | Flush max | Reopen p50 | Reopen p95 | Reopen max | Current query p50 | Current query p95 | Current query max | As-of query p50 | As-of query p95 | As-of query max | File growth | Page growth | Actual delta facts | Segment count | Corrupt fallback |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 1K | 1K | 32 | 7.692 ms | 11.679 ms | 15.874 ms | 3.245 ms | 6.290 ms | 6.386 ms | 0.059 ms | 0.067 ms | 0.068 ms | 1,409.446 ms | 1,449.337 ms | 1,464.959 ms | 12,234,752 B | 2,987 | 1,000 | 1,000 | true |
| 10 | 100 | 1K | 32 | 5.713 ms | 6.882 ms | 7.233 ms | 1.326 ms | 2.644 ms | 2.707 ms | 0.055 ms | 0.083 ms | 0.092 ms | 1,411.079 ms | 1,454.465 ms | 1,464.000 ms | 1,228,800 B | 300 | 1,000 | 100 | true |

T8B observation: multi-segment append fixes the measured T7C failure for the 1K accumulated-delta gate. The one-fact cadence drops from T7C's 102.385 ms flush p95 to 11.679 ms, and max drops from 117.191 ms to 15.874 ms. Reopen stays far under the 250-500 ms gate, immediate current-query reads remain sub-millisecond, and corrupt-latest fallback still works.

The remaining issue is unchanged from T7C: as-of/replay receipt reads still take about 1.45 s p95 on the 1M base. That is not a T8 storage-publish blocker; it remains a separate Q1 read-path lane for Vetch agent briefs.

T8B verdict: proceed to T8C full accumulation matrix. Do not add a manifest-cost fix or recompact threshold before T8C; keep T9 as the follow-up only if the full matrix shows long-term segment/file growth pressure.

### T8C: Multi-Segment Full Accumulation Matrix

Run: 2026-06-05, `cargo bench --bench delta_accumulation_benchmark`.

Fixture: same as T8B, now running the full accumulated receipt matrix. The benchmark builds a checkpointed 1M-fact base, copies it per scenario, disables auto-checkpoint, appends receipt-like `Value::Ref` facts, explicitly checkpoints after each receipt batch, samples reopen/current/as-of query latency at up to 32 probe points, counts visible delta segments, and verifies corrupt-latest fallback for every scenario.

Base file: 407,179,264 bytes / 99,409 pages.

| Facts/checkpoint | Checkpoints | Delta facts | Probes | Flush p50 | Flush p95 | Flush max | Reopen p50 | Reopen p95 | Reopen max | Current query p50 | Current query p95 | Current query max | As-of query p50 | As-of query p95 | As-of query max | File growth | Page growth | Actual delta facts | Segment count | Corrupt fallback |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 10 | 10 | 10 | 3.636 ms | 5.397 ms | 5.397 ms | 0.086 ms | 0.130 ms | 0.130 ms | 0.055 ms | 0.070 ms | 0.070 ms | 1,404.633 ms | 1,486.731 ms | 1,486.731 ms | 81,920 B | 20 | 10 | 10 | true |
| 1 | 100 | 100 | 32 | 3.216 ms | 3.915 ms | 4.681 ms | 0.348 ms | 0.694 ms | 0.701 ms | 0.055 ms | 0.076 ms | 0.084 ms | 1,416.453 ms | 1,500.385 ms | 1,530.367 ms | 819,200 B | 200 | 100 | 100 | true |
| 1 | 1K | 1K | 32 | 7.615 ms | 12.318 ms | 46.607 ms | 3.312 ms | 6.589 ms | 8.203 ms | 0.058 ms | 0.071 ms | 0.082 ms | 1,448.560 ms | 1,784.936 ms | 1,813.797 ms | 12,234,752 B | 2,987 | 1,000 | 1,000 | true |
| 1 | 10K | 10K | 32 | 52.916 ms | 99.818 ms | 133.904 ms | 32.171 ms | 67.537 ms | 73.892 ms | 0.074 ms | 0.121 ms | 0.123 ms | 1,436.584 ms | 1,758.060 ms | 1,954.915 ms | 662,257,664 B | 161,684 | 10,000 | 10,000 | true |
| 10 | 100 | 1K | 32 | 5.669 ms | 7.139 ms | 8.454 ms | 1.321 ms | 2.759 ms | 2.831 ms | 0.063 ms | 0.102 ms | 0.111 ms | 1,547.368 ms | 1,611.668 ms | 1,733.350 ms | 1,228,800 B | 300 | 1,000 | 100 | true |
| 10 | 1K | 10K | 32 | 20.162 ms | 36.821 ms | 62.489 ms | 13.439 ms | 27.639 ms | 27.731 ms | 0.069 ms | 0.125 ms | 0.140 ms | 1,569.639 ms | 1,666.077 ms | 1,684.640 ms | 16,330,752 B | 3,987 | 10,000 | 1,000 | true |
| 100 | 100 | 10K | 32 | 25.199 ms | 38.347 ms | 39.371 ms | 10.947 ms | 24.001 ms | 24.016 ms | 0.082 ms | 0.129 ms | 0.179 ms | 1,596.430 ms | 1,668.256 ms | 1,713.440 ms | 4,505,600 B | 1,100 | 10,000 | 100 | true |

T8C observation: multi-segment append is a decisive improvement over T7C single-segment replacement, but it is not a complete long-term policy by itself. The 1x10K scenario improves from T7C's 1,051.300 ms flush p95 and 18.9 GB file growth to 99.818 ms p95 and 662.3 MB growth, with corrupt fallback still true. That is much better, but it still shows segment-count/manifest accumulation entering the hot path once the database reaches 10K tiny segments.

The batching rows separate fact count from segment count. Both 10x1K and 100x100 contain 10K delta facts, but their segment counts are 1K and 100, and their flush p95 stays at 36.821 ms and 38.347 ms. Immediate current-query reads remain sub-millisecond across the matrix. Reopen remains well under the 250-500 ms gate even at 10K segments, with p95 67.537 ms. As-of/replay receipt reads remain about 1.5-1.8 s p95 and still belong to the separate Q1 read-path lane.

T8C verdict: keep multi-segment publish as the default delta checkpoint path for now, but add T9 segment/file-growth thresholds before treating this as production-ready for unbounded per-receipt checkpoint cadence. The next storage slice should be T9A threshold and maintenance policy: bound segment count and manifest/file growth through idle/background recompact, while keeping foreground Vetch work on pending-sized checkpoints. Do not add a broad storage engine, sidecar index, or query API change for this result.

### Q1-A: Agent-Brief Read-Path Benchmark Gate

Run: 2026-06-06, `MINIGRAF_AGENT_BRIEF_BENCH_MODE=smoke cargo bench --bench agent_brief_read_path_benchmark`.

Fixture: `benches/agent_brief_read_path_benchmark.rs` builds a checkpointed base, appends receipt-like `Value::Ref` facts through the delta checkpoint path, then measures four Vetch agent-brief read surfaces at probe points:

- current point query: latest receipt by entity and `:bench/ref`
- formatted as-of point query: same entity with `:as-of <tx_count>` and `:valid-at :any-valid-time`
- prepared as-of point query: same shape through `PreparedQuery`
- export/replay proxy: `export_fact_log()` followed by filtering records from the latest tx

Smoke base file: 3,977,216 bytes / 971 pages. The smoke mode uses a 10K base so the harness can be verified quickly.

| Mode | Scenario | Base facts | Facts/checkpoint | Checkpoints | Delta facts | Probes | Current p95 | As-of p95 | Prepared as-of p95 | Export recent filter p95 | File growth |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| smoke | smoke_single_receipt | 10K | 1 | 1 | 1 | 1 | 0.041 ms | 7.940 ms | 7.849 ms | 2.070 ms | 8,192 B |
| smoke | smoke_receipt_stream_10 | 10K | 1 | 10 | 10 | 5 | 0.038 ms | 9.549 ms | 9.336 ms | 2.234 ms | 81,920 B |

Q1-A observation: prepared as-of is not materially faster than formatted as-of in the smoke run, so parser/string formatting overhead is not the main blocker. The current point query remains cheap, while `:as-of` point lookup is already hundreds of times slower on a 10K base. Full export plus recent filtering is also O(total facts), but it is faster than current as-of at 10K because it avoids Datalog matching work after materialization.

Full Q1-A pre-Q1-B run: 2026-06-06, `cargo bench --bench agent_brief_read_path_benchmark`.

| Mode | Scenario | Base facts | Facts/checkpoint | Checkpoints | Delta facts | Probes | Current p95 | As-of p95 | Prepared as-of p95 | Export recent filter p95 | File growth |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| full | single_receipt | 1M | 1 | 1 | 1 | 1 | 0.105 ms | 1,257.698 ms | 1,260.495 ms | 179.836 ms | 8,192 B |
| full | receipt_stream_100 | 1M | 1 | 100 | 100 | 32 | 0.060 ms | 1,499.003 ms | 1,462.962 ms | 192.015 ms | 819,200 B |
| full | batched_receipts_1000 | 1M | 10 | 100 | 1,000 | 32 | 0.070 ms | 1,456.026 ms | 1,623.022 ms | 221.938 ms | 1,228,800 B |

Full Q1-A verdict: proceed with Q1-B as-of selective pushdown first. Prepared execution is not materially faster than formatted as-of on the 1M run, so a prepared helper would not solve the blocker. Export plus recent filtering is still a full-log path and remains around 180-220 ms p95, but it is not the immediate blocker for entity-scoped receipt reads.

### Q1-B: Agent-Brief As-Of Selective Pushdown

Run: 2026-06-06, `cargo bench --bench agent_brief_read_path_benchmark`.

Change: entity/attribute-bound `:as-of` queries now try the existing selective committed-index fetch before transaction-time filtering. Queries that use rules stay on the full fact base. No public API, file-format, checkpoint, or recompact policy changed.

| Mode | Scenario | Base facts | Facts/checkpoint | Checkpoints | Delta facts | Probes | Current p95 | As-of p95 | Prepared as-of p95 | Export recent filter p95 | File growth |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| full | single_receipt | 1M | 1 | 1 | 1 | 1 | 0.046 ms | 0.017 ms | 0.013 ms | 199.950 ms | 8,192 B |
| full | receipt_stream_100 | 1M | 1 | 100 | 100 | 32 | 0.062 ms | 0.037 ms | 0.026 ms | 230.992 ms | 819,200 B |
| full | batched_receipts_1000 | 1M | 10 | 100 | 1,000 | 32 | 0.060 ms | 0.043 ms | 0.026 ms | 234.806 ms | 1,228,800 B |

Smoke after Q1-B: the 10K smoke matrix drops formatted as-of p95 from `7.940-9.549 ms` to `0.017-0.026 ms`, and prepared as-of p95 from `7.849-9.336 ms` to `0.013-0.022 ms`.

Q1-B verdict: the Vetch "just-written receipt -> next agent brief" point-read blocker is fixed for entity/attribute-bound Datalog as-of queries on a 1M base. A recent fact-log reader is still a possible future optimization if Vetch's agent brief path needs export/replay-style reads, but it is no longer needed to make receipt-scoped as-of Datalog reads cheap.

### Q2-A: Export Fact-Log Allocation Cleanup

Run: 2026-06-06, `cargo bench --bench agent_brief_read_path_benchmark`.

Change: `export_fact_log()` now uses an internal streaming fact visitor over committed base facts plus visible delta facts, then builds the public `Vec<FactRecord>` directly. This removes the previous intermediate `Vec<Fact>` allocation for file-backed committed records. The public API still returns `Vec<FactRecord>`, so full export/replay remains O(total facts).

| Mode | Scenario | Base facts | Facts/checkpoint | Checkpoints | Delta facts | Probes | Current p95 | As-of p95 | Prepared as-of p95 | Export recent filter p95 | File growth |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| full | single_receipt | 1M | 1 | 1 | 1 | 1 | 0.055 ms | 0.019 ms | 0.014 ms | 197.300 ms | 8,192 B |
| full | receipt_stream_100 | 1M | 1 | 100 | 100 | 32 | 0.073 ms | 0.029 ms | 0.022 ms | 245.555 ms | 819,200 B |
| full | batched_receipts_1000 | 1M | 10 | 100 | 1,000 | 32 | 0.076 ms | 0.029 ms | 0.023 ms | 218.708 ms | 1,228,800 B |

Q2-A observation: export/replay latency remains in the same broad range as Q1-B (`~197-246 ms` p95 versus `~200-235 ms` p95 before this cleanup), because the public operation still exports the full log and then filters recent records in the benchmark. The meaningful improvement is memory shape: export no longer materializes committed facts as `Vec<Fact>` before converting them to `Vec<FactRecord>`. A narrower recent fact-log reader should remain deferred until Vetch proves export/replay, not Datalog as-of, is still hot in real agent-brief construction.

### Q2-B: Recompact Input Streaming Cleanup

Run: 2026-06-07,
`/usr/bin/time -v cargo test measure_q2b_recompact_streaming_1m --lib -- --ignored --nocapture`.

Change: private `write_recompact_candidate_from_visible_facts()` now streams
visible facts through `for_each_fact()` and `PackedFactPacker` instead of first
materializing `self.storage.get_all_facts()`. Public API, file format, ledger
identity, foreground `checkpoint()` policy, and copy-on-write recompact publish
discipline are unchanged.

| Visible facts | Recompact-only wall time | Base file bytes | Recompact file bytes | Published fact pages | Candidate fact-page bytes | End-to-end max RSS |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000,001 | 11,791.318 ms | 337,833,984 | 675,770,368 | 14,275 | 58,470,400 | 2,186,428 KB |

Q2-B observation: this is a memory-shape cleanup, not a bounded-memory
recompact design. It removes the decoded committed `Vec<Fact>` allocation from
recompact input, and tests pin byte-identical packed-page output plus
fact-log/recompact ordering. Candidate fact pages and sorted EAVT/AEVT/AVET/VAET
entry buffers still remain O(total facts). The reported max RSS is for the whole
ignored test process, including 1M fixture setup; the table's wall time is
measured only around `recompact_visible_delta()`.

---

## A0: Caller-Shaped Evidence Suites (2026-07-11)

Evidence gate for the app-adoption line (`docs/APP_ADOPTION_GAP_PLAN.md`,
slice A0). Three suites shaped after the vetch-app and harrekki caller
requirement documents. All numbers: A0 environment (see Environment).

### Vetch Cadence Replay

`benches/vetch_cadence_benchmark.rs` — interleaved capture (new 4-fact card)
→ edit (retract + assert geometry) → receipt batch (5 `:bench/ref` facts) →
agent-brief reads (current + as-of point queries) → per-slice checkpoint, on
a checkpointed base. Run: `cargo bench --bench vetch_cadence_benchmark`
(full: 1M base, 100 slices) or `-- smoke` (10K base, 20 slices).

| Op (per slice) | full 1M p50 | full 1M p95 | smoke 10K p50 | smoke 10K p95 |
|---|---|---|---|---|
| capture (transact, 4 facts) | 2.02 ms | 2.29 ms | 1.97 ms | 2.25 ms |
| edit (retract + transact) | 1.89 ms | 2.15 ms | 1.89 ms | 2.04 ms |
| receipt (transact, 5 refs) | 0.96 ms | 1.13 ms | 0.93 ms | 1.14 ms |
| brief read, current | 0.055 ms | 0.065 ms | 0.045 ms | 0.095 ms |
| brief read, as-of | 0.034 ms | 0.045 ms | 0.028 ms | 0.044 ms |
| checkpoint | 3.43 ms | 5.25 ms | 2.01 ms | 2.96 ms |

File growth over the 100 full-mode slices: 1,228,800 bytes (300 pages) on a
407 MB base. Every op is independent of base size — the interactive Vetch
cadence at 1M facts costs the same as at 10K, and per-slice checkpoint stays
in the T7A delta-publish range (~5 ms p95).

### Gate D Exact Vetch Trace

Vetch's external runner
`apps/quiet-surface/scripts/bench-vicia-gate-d-exact-trace.mjs` drives the
release `minigraf --session` binary rather than calling a Rust benchmark API.
The acceptance profile starts from an immutable 1M-fact v11 base, replays
1,024 capture/edit/proposal/receipt/epistemic slices, checkpoints every slice,
discovers the current card ids through the persisted space-membership entity,
splits exact entity reads at 128, runs threshold-advised maintenance outside
the foreground cadence, explicitly checkpoints, then reopens in a fresh child
and verifies current/history/activation fingerprints. The runner rejects a
dirty Vicia checkout and emits one JSON receipt with source and binary hashes.
Vetch main `1b57689` preserves that exact receipt at
`qa/done/vicia-gate-d-full-e60a7c2.json`.

Recorded host: A0 WSL2 environment, release source
`e60a7c298a66de486fa4615a085e6aac547b0800`, 2026-07-12.

| Gate D surface | Result | Budget |
|---|---:|---:|
| append p95 (5,120 samples) | 2.378 ms | <= 10 ms |
| checkpoint p95 (1,024 samples) | 3.098 ms | <= 50 ms |
| selective current p95 | 0.259 ms | <= 10 ms |
| historical p95 | 0.214 ms | <= 10 ms |
| agent brief p95 | 0.590 ms | <= 100 ms |
| 1,024-card current-space rebuild p95 | 173.988 ms | <= 250 ms |
| fresh-child reopen | 4.357 ms | <= 500 ms |
| foreground RSS peak | 57.480 MiB | <= 512 MiB |
| reopen RSS peak | 7.902 MiB | <= 512 MiB |
| foreground file growth | 28.078 MiB | <= 64 MiB |
| idle recompact | 7.510 s / 947.512 MiB RSS | <= 60 s / 3,072 MiB |
| post-maintenance/base file ratio | 2.139 | <= 2.25 |

The first honest run exposed cumulative checkpoint work: p95 was 132.806 ms
because every publish reread all selected delta segments and rebuilt the full
committed reader. Retaining decoded segments reduced it to 58.566 ms but still
missed the product budget. The final implementation keeps shared resident
fact/index state and inserts only the new segment behind the same pending-to-
committed read barrier, producing the 3.098 ms result above. All exact query
shapes remained selective, all 1,024 cards were rediscovered after reopen, and
foreground, reopened, and expected fingerprints matched.

### Decay-Candidate Query Cost (harrekki)

`decay/` groups in `benches/minigraf_bench.rs` — "entities untouched since
T" (harrekki caller doc P1 #6): N entities with one `:touched/at` integer
each, 20% below the threshold. Run: `MINIGRAF_BENCH_MODE=full cargo bench
--bench minigraf_bench -- "decay/"`. Criterion median.

| Shape | 1K | 10K | 100K | 1M |
|---|---|---|---|---|
| `comparison_scan` — `[?e :touched/at ?t] [(< ?t T)]` | 1.6 ms | 16.1 ms | 164 ms | 407 ms |
| `not_join` — `(not-join [?e] [?e :touched/at ?t2] [(>= ?t2 T)])` | 12.0 ms | 864 ms | capped | capped |

`comparison_scan` is a full attribute scan plus in-memory filter (no index
pushdown for range predicates — gap G3): linear, 407 ms at 1M. Acceptable
for idle-window decay sweeps; not for per-tick reads. This is the A3
promotion evidence. `not_join` is superlinear (12 ms → 864 ms for 10×
facts) and explicitly capped at 10K in the bench — extending to 100K/1M
costs minutes-to-hours per iteration without adding decision information.
Use the comparison shape for decay candidates; do not use negation.

### Browser Open at Scale

`examples/browser/bench.html` + `bench-driver.cjs` — measures
`BrowserDb.open()` latency and JS-heap growth against imported fixtures.
This eager compatibility API loads **all** IndexedDB pages into memory; these
numbers quantify that shape (Vetch Gate E baseline). They are not measurements
of the newer `BrowserDb.openPaged()` path.

Runner: build the wasm pkg (`wasm-pack build --target web --out-dir
minigraf-wasm -- --features browser`), generate fixtures (`cargo run
--release --example generate_bench_fixture -- 1000000 <out.graph>`), serve
the repo root (`python3 -m http.server 8123`), then
`CHROME_PATH=<chrome> NODE_PATH=<dir with puppeteer> node
examples/browser/bench-driver.cjs /target/bench-fixtures/bench-1m.graph 3`.
The driver imports once, then measures open in a fresh browser per run
(shared user-data-dir keeps IndexedDB; fresh renderer keeps the heap
baseline clean). Chrome for Testing 150, headless,
`--enable-precise-memory-info`.

| Fixture | File size | import (fetch+IDB) | open | first query | JS heap growth on open |
|---|---|---|---|---|---|
| 100K facts | 40.1 MB | 1.0 s | 384–409 ms | 3.5 ms | +41.6 MB |
| 1M facts | 407 MB | 10.5 s | 3.19–3.24 s | 3.2–4.1 ms | +420 MB |

Open latency and heap growth are linear in file size — the browser holds
the whole graph in renderer memory. A 1M-fact authority graph costs ~3.2 s
startup and ~420 MB resident per tab: workable on desktop, but the numbers
say browser-side scale beyond that needs the page-on-demand read path noted
in the Vetch caller requirements, not a bigger tab budget. Heap figures are
`performance.memory.usedJSHeapSize` deltas (renderer JS heap; wasm linear
memory accounting may vary by Chrome version).

Re-measured 2026-07-11 on the A5-1 atomic import path (single clear+put
IndexedDB transaction, commit `13d41bc`):

| Fixture | import (fetch+IDB) | open | first query | JS heap growth on open |
|---|---|---|---|---|
| 100K facts | 0.9 s | 364–368 ms | 3.3 ms | +41.6 MB |
| 1M facts | 10.8 s | 3.44–3.55 s | 3.3–3.6 ms | +419 MB |

The 407 MB single-transaction import commits without quota aborts; numbers
match the A0 rows above — no regression from the atomicity change.

---

## A2: Incremental Fact Log (2026-07-11)

`Minigraf::export_fact_log_since(since_tx_count)` returns the fact-log tail
(`tx_count > since`) at cost proportional to the tail, not the committed
graph (`docs/APP_ADOPTION_GAP_PLAN.md` slice A2, harrekki P0 #2). Committed
packed pages hold facts in nondecreasing `tx_count` order, so the reader
binary-searches the first tail page (O(log pages) cache reads) and streams
from there; delta segments and pending facts filter in memory. Fixture:

```bash
cargo test --release --test fact_log_since_benchmark -- --ignored --nocapture
```

Gate run (A0 environment, 1,000,000 committed facts, head tx_count 1,100):

| Scenario | Tail records | Latency |
|---|---|---|
| Base tail — cursor inside the 1M base, cold cache | 100 | 90.9 µs |
| Base tail — warm cache | 100 | 32.0 µs |
| Pending tail (uncheckpointed) | 50 | 51.7 µs |
| Delta-segment tail (after checkpoint) | 50 | 31.3 µs |
| Empty poll at head | 0 | 3.2 µs |
| Full `export_fact_log()` contrast | 1,000,050 | 256.4 ms |

The cold base tail is the post-recompact daemon-tick shape — the case where
a watermark-only skip would degrade to a committed full scan; the page probe
keeps it ~2,800× cheaper than the full export. Setup cost for the fixture:
1M-fact checkpoint 7.9 s.

---

## A7: kill -9 Durability Gate (2026-07-11)

Reliability gate, not a benchmark (`docs/APP_ADOPTION_GAP_PLAN.md` slice A7,
harrekki P0 #3). `tests/kill9_durability_test.rs` SIGKILLs real
`minigraf --session --file` child processes at randomized instants —
including checkpoint-biased windows — over growing `.graph` lineages, then
reopens and audits every acknowledged transaction (an ack = a complete
`ok:true` transacted frame, which the A6 protocol only emits after WAL
fsync). Run:

```bash
cargo test --release --test kill9_durability_test -- --ignored --nocapture
# seed / scale overrides: VICIA_A7_SEED, VICIA_A7_CYCLES
```

Gate run (A0 environment, seed `0xa7a720260711`, defaults):

| Metric | Value |
|---|---|
| Kill cycles | 2,400 (random-instant / mid-checkpoint / mid-maintenance ≈ 6:3:1) |
| Acknowledged transactions | 155,699 |
| Lost acknowledged transactions | **0** |
| Unopenable files | **0** |
| Confirmed mid-checkpoint kills | 912 |
| In-flight promotions (unacked but fsynced, all-or-nothing) | 501 |
| Lineage rotations | 26 |
| Wall time | 263.5 s |

The harness found two real crash-robustness bugs before passing: a WAL
replay that reset the tx counter below the committed watermark when a kill
left a header-only WAL (acked writes then skipped on the next replay), and
a non-atomic lock-file creation whose kill window left a contentless
`.graph.lock` that blocked open until manual deletion. Both are fixed and
regression-tested (`tests/wal_test.rs`, `src/storage/backend/file.rs` unit
tests). Scope caveats: SIGKILL validates process-death durability, not
power loss; maintenance-op kills exercise the maintenance checkpoint path
only (recompact thresholds are unreachable at this scale).

---

## A8: Bulk Valid-Time Closure (2026-07-11)

`(forget ...)` closes query-selected or explicitly supplied EAV valid-time
windows as one WAL-first transaction while preserving the earlier history
(`docs/APP_ADOPTION_GAP_PLAN.md` slice A8, harrekki P1 #6). Gate commands:

```bash
cargo test --release --test forget_test \
  forget_10k_result_set_closes_in_one_transaction -- --nocapture
cargo test --release --test kill9_durability_test -- --ignored --nocapture
```

Semantic/scale gate (A0 environment):

| Metric | Value |
|---|---|
| Query-selected triples closed | 10,000 |
| Closure transaction count | **1** |
| Fact-log records | 20,000 (10k scoped retracts + 10k truncated re-asserts) |
| Release closure latency | 86.1 ms |
| History before closure / current after closure | 10,000 / 0 visible |

Crash gate (seed `0xa7a720260711`, A8-extended workload/model):

| Metric | Value |
|---|---|
| Kill cycles | 2,400 |
| Acknowledged transactions | 169,275 |
| Acknowledged forgets | 333 |
| In-flight forget promotions | 27 |
| Lost acknowledged transactions / forgets | **0 / 0** |
| Unopenable files | **0** |
| Confirmed mid-checkpoint kills | 955 |
| Lineage rotations | 29 |
| Deadline hits | 0 |
| Wall time | 505.7 s |

The first full run exposed a third real A7/A8 crash window: SIGKILL after
lazy WAL `create_new()` but before its 32-byte header write left a zero-byte
sidecar. The main `.graph` and both manifest slots were valid, but reopen
rejected the short WAL before it could audit them. A short WAL cannot contain
an acknowledged entry because the complete header is fsynced before any
append; replay now treats it as empty and the next writer reinitializes the
header. Unit coverage pins 0-, 7-, and 31-byte artifacts while full-sized
bad-magic headers remain hard errors.

---

## A5: Browser IndexedDB Growth (2026-07-11)

Long-running write growth of the browser backend (`BrowserDb`), measured for
the A5 parity-evidence gate (`docs/APP_ADOPTION_GAP_PLAN.md`). This is the
pre-A5-4 maintenance snapshot. At the time, every browser
write `execute()` runs `save()`, which appends a delta segment and rewrites
the manifest; `save()` never consults the delta growth thresholds, and the
recompact path (`run_idle_delta_maintenance`) has no browser surface — so
superseded pages accumulate in IndexedDB (and in the in-memory page buffer)
with no reclaim path. These runs quantify that shape.

Runner: `examples/browser/bench-driver.cjs growth <cycles> <factsPerCycle>
<sampleEvery> <fixture-url-path|empty>` (same serving/env as "Browser Open
at Scale"; Chrome for Testing 150, headless). Sizes are IndexedDB page count
× 4 KB; `idbCount == header page_count` held at every sample (page ids are
contiguous and never reused). Commit = one 10-fact `(transact ...)`.

Empty base (2,000- and 4,500-commit runs; the 2,000-commit prefix reproduced
exactly across both — 7,775 pages at commit 2,000 in each):

| Commits | Logical facts | IDB pages | IDB size | exec p50 | exec p95 |
|---|---|---|---|---|---|
| 100 | 1,000 | 204 | 0.8 MB | 2.0 ms | 2.9 ms |
| 1,000 | 10,000 | 2,709 | 10.6 MB | 24–25 ms | 27–29 ms |
| 2,000 | 20,000 | 7,775 | 30.4 MB | 54–56 ms | 59–62 ms |
| 3,000 | 30,000 | 15,303 | 59.8 MB | 88 ms | 101 ms |
| 4,000 | 40,000 | 25,444 | 99.4 MB | 126 ms | 138 ms |
| 4,500 | 45,000 | 31,642 | 123.6 MB | 137 ms | 150 ms |

100K-fact fixture base (9,778 pages / 38.2 MB imported), 2,000 commits:
grows to 17,684 pages / 69.1 MB. The growth delta (+7,906 pages) is
essentially the empty-base curve (+7,775) — growth is base-independent,
entirely delta-chain-shaped.

Findings:

- **Cumulative growth is quadratic in commits** (2.25× commits → 4.07×
  size): each commit appends ~1 segment page plus a full manifest rewrite
  whose page cost is linear in segment count (~13 pages/commit by commit
  4,500).
- **Per-commit latency is linear in segment count** (~0.033 ms/segment):
  p50 2 ms → 54 ms → 137 ms over 4,500 commits. At a plausible
  commit-on-gesture-end cadence of 500 commits/day this reaches ~80 MB of
  IndexedDB and >100 ms per commit within a week, for ~1.4 MB of logical
  data.
- **Thresholds fire into a void**: the soft (1,024) and hard (4,096) delta
  segment thresholds were crossed at commits ~1,025 / ~4,097 with no
  behavior change — nothing in the browser can act on them.
- **`exportGraph` → `importGraph` is NOT a remedy**: the round-trip is a
  size identity (31,642 pages before and after at commit 4,500; data
  verified by point queries). Export serialises the full `0..page_count`
  range including superseded pages; the fold-to-fresh-base path exists only
  behind native `run_idle_maintenance()`.
- **Reopen cost tracks IndexedDB size, not logical size**: after 4,500
  commits (45K facts, ~1.8 MB logical) reopen is ~1.19 s with ~136 MB heap —
  the same shape as a clean 100K-fact / 40 MB fixture (~0.37 s / ~42 MB).
- Heap sampled inside the long-lived growth page under-reports wasm linear
  memory (single-digit MB while the page buffer holds the full page set);
  the reopen heap figures are the reliable resident-cost signal.

Gate conclusion at this snapshot: browser Vicia write cadence was **not
bounded**. Without a maintenance surface, IndexedDB and reopen cost grew
without limit under exactly the cadence Vetch plans. This measurement promoted
A5-4 rather than remaining the current operating policy.

---

## A5-4: Browser Atomic Compact Maintenance (2026-07-11)

The A5 wall now has a browser-native maintenance path. At the existing delta
soft/hard threshold, `BrowserDb.runIdleMaintenance()` builds a fresh contiguous
graph from the full-history log, atomically replaces all IndexedDB page records,
and swaps the live handle only after commit. Foreground `execute()` still never
runs recompact.

Runner: build the browser package, serve the repository, then run
`bench-driver.cjs maintained-growth <cycles> <factsPerCycle> <sampleEvery>
<maintenanceEvery> <fixture|empty>`. The recorded 100K run used Chrome for
Testing 150, 4,100 ten-fact commits, and maintenance every 1,025 commits. The
100K imported base was 9,778 pages / 38.2 MB. Each maintenance call observed
exactly the soft-threshold `schedule_idle_maintenance` write advice.

| Cycle | Logical facts | Pages before | Pages after | Reclaimed | Maintenance |
|---:|---:|---:|---:|---:|---:|
| 1,025 | 110,250 | 12,650 | 10,550 | 2,100 / 8.2 MB | 2,684.3 ms |
| 2,050 | 120,500 | 13,433 | 11,326 | 2,107 / 8.2 MB | 3,055.3 ms |
| 3,075 | 130,750 | 14,209 | 12,101 | 2,108 / 8.2 MB | 2,524.4 ms |
| 4,100 | 141,000 | 14,984 | 12,876 | 2,108 / 8.2 MB | 4,176.5 ms |

`idbCount == header.page_count` held before and after every replacement, and
the post-maintenance image grew by about 775 pages per additional 10,250 facts:
physical page records now track logical history rather than lineage-square
manifest accumulation.

Write latency also resets after each fold. Pre-maintenance 512-commit windows
reached `33.9–43.1 ms` p95; the next post-maintenance windows returned to
`18.8–21.3 ms` p95. An empty-base run independently reclaimed
`8,063→1,549`, `4,428→2,322`, and `5,201→3,097` pages; its first maintenance
probe at commit 1,024 correctly no-opped because the first write created the
base and only 1,023 delta segments existed.

Correctness/failure evidence is separate from timing: the then-23-test browser WASM
suite preserves Ref assertion/retraction history, valid-time reads, exact tx
watermark, export/reopen, mutation exclusion, rejected-write rollback, poison
containment, and atomic maintenance failure.

`bench-driver.cjs worker-smoke` additionally ran the generated package in a
real module `DedicatedWorker` under Chrome 150. With `typeof window ===
"undefined"`, IndexedDB open, a published write (`tx_count = 1`), query, and a
below-threshold maintenance no-op all succeeded. This is a repeatable worker
deployment smoke; the 1M bounded-open/memory measurement remains separate.

Gate verdict:

- **Passed:** repeated browser page-record reclaim, write-latency reset,
  threshold advice, full-history identity, and atomic failure ordering.
- **Caller constraint:** maintenance is O(total history) synchronous WASM work
  (`2.5–4.2 s` at 110K–141K facts) and atomic replacement temporarily needs old
  plus candidate quota. Run it in the BrowserDb worker, never the UI thread.
- **Still open at A5-4:** eager `BrowserDb.open()` loads every IndexedDB
  page. `openPaged()` now supplies the bounded implementation path, but its 1M
  startup/query/growth matrix and the 1M maintenance peak-memory shape are
  unmeasured. Do not infer 1M browser authority readiness from the eager
  baseline or the 100K maintenance pass; A5-6d records the later measurement.

### A5-5: Gate E Tagged Portability and Corruption Matrix

Run: `cargo test --lib gate_e` for the native consumers, then
`CHROMEDRIVER=/path/to/chromedriver ./scripts/test-browser-wasm.sh` for real
headless Chrome. The browser entrypoint is also wired into `rust.yml` CI.

`tests/fixtures/gate_e/corpus.json` drives two producers (`native.graph` from
the native generator and `browser.graph` from real BrowserDb WASM) through two
consumers. All four producer→consumer cells match ten exact tagged query cases:
scalars, Ref, Keyword, null, current state, `:as-of`, valid-time,
combined-time, retraction, and VAET joins. The browser-produced fixture also
opens natively with all 13 ledger records, transaction counts, and the Ref
retraction intact. These fixtures are frozen v10 migration inputs: the first
current-library import publishes v11, and the resulting v11 image then exports
and reimports byte-for-byte with the same results.

The shared corruption matrix covers bad magic/version/header checksum,
non-empty short files, newest slot/manifest/segment fallback, missing and
partial newest pages, selected-oldest-segment and both-slot hard errors, and
unpublished full-page tails. Both backends preserve base plus the two earlier
deltas on fallback and reject base-only partial recovery. Rejected BrowserDb
imports preserve both the live sentinel and reopened durable sentinel. A full
unpublished tail is removed from IndexedDB and export; a physically incomplete
declared prefix can serve the previous manifest but remains visibly
non-exportable until repaired.

Verdict at A5-5: semantic, portability, and manifest-recovery parity pass. The
matrix exposed that v10 trusted base checksum identity when a delta was
selected, allowing an unread base-page bit flip to evade open-time detection.
A5-6b below closes that integrity half without restoring an O(base) open scan.

### A5-6b: v11 Generation-Bound Page Integrity

File format v11 stores one CRC32 per immutable base fact/index page in an
in-file catalog. Each checksum includes the base generation and absolute page
id; page 0 binds the catalog range, byte length, and CRC. Catalog size is
`40 + 4 * covered_page_count` bytes, rounded to whole 4KB pages. This is
accidental-corruption detection, not hostile-input authentication.

Evidence run on 2026-07-11:

- `cargo test --quiet`: passed the full native suite, including the v11
  migration, corruption, publication-order, and generation regressions.
- `cargo clippy --lib -- -D warnings`: passed.
- `cargo test --target wasm32-unknown-unknown --lib --features browser --no-run`: passed.
- `CHROMEDRIVER=... ./scripts/test-browser-wasm.sh`: 27 passed in real Chrome
  150, including durable v10→v11 open migration, transaction-abort byte
  preservation, and corrupt-base export rejection.
- A page-id counting backend proves v11 open reads catalog metadata but zero
  fact/index pages. Exact fact/EAVT bit flips open boundedly and fail on the
  first selective query; catalog/descriptor/truncation corruption rejects open.
- Native full-save and backup reject a corrupt selected base instead of
  checksumming and republishing/copying it. Legacy v1–v9 range/checksum/decode
  failures reject migration without changing page-0 authority; valid migration
  appends a COW base while preserving duplicate rows, v9 scoped retractions,
  and every old non-header page.

This was not the 1M Gate E performance verdict. At A5-6b, BrowserDb still
called `load_all_pages()` into a flat in-memory buffer at open. A5-6c below
adds the generation-aware on-demand source; the full 1M
startup/query/growth/maintenance peak-memory matrix still must be rerun on it.

### A5-6c: Generation-Aware Sparse IndexedDB Paging

Implementation evidence as of 2026-07-12; this section intentionally records
structure and correctness, not 1M performance numbers:

- `BrowserDb.openPaged()` reads page 0, the bounded v11 integrity catalog and
  manifest metadata, and the selected segment ranges. It does not call the
  eager full-store loader for a complete v11 image and leaves base fact/index
  pages non-resident until a deterministic query access plan demands them.
- Cold selective reads batch exact IndexedDB ranges; warm repeats reuse the
  fixed-size resident cache. A missing, malformed, or checksum-invalid demanded
  page fails closed. Explicit full scans may stage the declared base fact range
  and release clean staging afterward rather than silently turning a selective
  failure into a scan.
- Independent handles pin exact page-0 bytes. Each sparse read/write observes
  page 0 in the same IndexedDB transaction and rejects stale authority instead
  of mixing generations. No schema/metadata key was introduced; numeric page 0
  remains the compare-and-swap authority understood by existing packages.
- Writes and `forget` resolve required pages before mutation. Failed durable
  writes restore the prior sparse authority without `load_all_pages()`;
  successful writes, import, complete v10 migration, and forced maintenance
  converge back to sparse residency.
- `exportGraphAsync()` walks the complete published prefix, verifies immutable
  base pages against the v11 catalog, and rejects an authority change instead
  of combining old and new pages. Synchronous `exportGraph()` remains available
  for eager and in-memory handles.
- All 57 browser-WASM structural tests pass in the final real-Chrome run.

Gate status at A5-6c: the page-on-demand implementation existed, but no new 1M
numbers were recorded in that slice. A5-6d below supplies the cold-open/query/
warm-repeat/write/growth/export and maintenance-memory matrix; Vetch main
`6c5b1f7` later supplies the matching `openPaged()` caller adoption.

### A5-6d: 1M Paged Browser Acceptance Matrix (2026-07-12)

`examples/browser/bench-driver.cjs paged-matrix` measures the exact bounded
browser path intended for Vetch. The self-checking run imports one
1,000,000-fact v11 image, then launches a fresh renderer for each open,
verified export, 1,024 one-fact writes, five soft-threshold reopens,
maintenance, and post-maintenance reopen. It fails if the write advice does
not reach `schedule_idle_maintenance`, maintenance does not recompact and
reclaim pages, or export length differs from the declared published prefix.
The fixture is 407,580,672 bytes / 99,507 pages and uses the
`:bench/base-{i}` shape from `examples/generate_bench_fixture.rs`.

Reproduction: build `minigraf-wasm`, generate the fixture, serve the repository
root, and run:

```text
CHROME_PATH=<chrome> NODE_PATH=<dir with puppeteer-or-puppeteer-core> \
BENCH_PAGE=http://localhost:8123/examples/browser/bench.html \
BENCH_PROFILE=/tmp/vicia-gate-e-profile \
node examples/browser/bench-driver.cjs paged-matrix \
  /target/bench-fixtures/bench-1m.graph 5 1024
```

Environment: Chrome for Testing 150 headless on WSL2, AMD Ryzen 7 7800X3D,
16 logical CPUs, 32 GiB host memory, Node 23.11.1. Each memory measurement
starts after WASM initialization. Linux process-tree RSS, PSS, and private
memory were sampled every 200 ms from `/proc`; PSS is the primary process
metric because summed RSS double-counts shared mappings. These are sampled
peaks, not allocator-exact instantaneous maxima. With five runs, the harness's
nearest-rank p95 is the five-run maximum, not a production SLA estimate.

| Phase | Latency | Read/write detail | 200 ms sampled PSS peak delta | Physical result |
|---|---:|---|---:|---|
| Initial `importGraph` | 11.242 s total | 10.237 s import | +2.55 GiB | 99,507 pages / 1,000,000 facts |
| Fresh `openPaged()` + six probes (5 runs) | open 16.6 ms p50 / 17.8 ms max | cold first/middle/last max 7.4 / 1.9 / 1.9 ms; warm <= 0.2 ms | +51.1 MiB max for the whole phase | JS heap open delta 0.535 MiB |
| `exportGraphAsync()` | 5.370 s | exact verified 407,580,672-byte image | +1.04 GiB | published prefix unchanged |
| 1,024 one-fact writes | 5.121 s total | 4.9 / 8.3 / 11.9 ms p50/p95/max | +84.5 MiB | 99,507 -> 102,596 pages; soft advice emitted |
| Pre-maintenance `openPaged()` (5 runs) | 405.2 ms p50 / 428.1 ms max | three cold probes 11.8 ms max; warm 0.7 ms max | +102.2 MiB max | 1,024 visible delta segments |
| `runIdleMaintenance()` | 16.679 s | last write verified after replacement | +2.09 GiB | 102,596 -> 99,599 pages |
| Post-maintenance `openPaged()` | 16.3 ms | cold probes 9.5 ms; warm 0.6 ms | +51.0 MiB | 1,001,024 facts preserved |

Verdict: the Vicia-owned 1M foreground evidence is complete. `openPaged()`,
selective cold/warm reads, normal writes, and post-maintenance reopen do not
load the 407 MB graph into the JS heap. The soft-threshold lineage makes
metadata bootstrap slower (428.1 ms five-run maximum) but leaves selective
reads at 11.8 ms and returns to a 16.3 ms open after maintenance. The earlier
A5-4 four-cycle 100K run remains the repeated-growth evidence; this A5-6d run
adds one exact 1M threshold cycle rather than claiming a second long-duration
study.

Import, full verified export, and recompact remain explicit O(total)
operations. Their 1.04-2.55 GiB sampled PSS deltas are real process-level
costs, not RSS double-counting artifacts: export retained its full 1.04 GiB
delta when the call returned, and maintenance retained 1.27 GiB at return; the
harness then closed the browser process. Vetch must also treat a legacy v10
database's first `openPaged()` migration as O(total): A5-6d measured an already
v11 fixture, while migration temporarily loads the legacy published image.
Run these phases in a disposable DedicatedWorker under the Web Lock, emit an
outcome, terminate the worker after success or failure, and reopen through
`openPaged()`. This single 32 GiB host run does not establish a general 16 GiB
product budget. Vetch main `1b57689` consumes clean Vicia `e60a7c2`, adopts
`openPaged()` for foreground authority handles, and proves the Web-Locked
disposable migration/import/export/maintenance lifecycle, termination receipts,
and reopen behavior in real Chrome. Together with this matrix, Gate E passes;
the separate Gate A authority cutover and packaged WebView2 host smoke are not
claimed by this benchmark.

---

## Concurrency (In-Memory)

All threads operate concurrently. Throughput = aggregate ops/sec across all threads (v0.20.1).

### readers — latency (ms per Criterion iteration) / aggregate throughput (queries/sec)

| DB size | 4 threads | 8 threads | 16 threads |
|---|---|---|---|
| 10K — latency | 20.2 ms | 38.6 ms | 77.2 ms |
| 10K — throughput | 198 q/s | 207 q/s | 207 q/s |
| 100K — latency | 237 ms | 438 ms | 907 ms |
| 100K — throughput | 16.8 q/s | 18.3 q/s | 17.6 q/s |

At 10K, throughput scales nearly linearly from 4→8 threads (198→207 q/s, +4.5%), then plateaus at 16 threads — the in-memory RwLock becomes the bottleneck. At 100K, throughput stays flat across thread counts because per-query scan cost dominates lock overhead.

### readers_plus_writer — latency / aggregate throughput

| DB size | 4 threads | 8 threads | 16 threads |
|---|---|---|---|
| 10K — latency | 19.9 ms | 35.6 ms | 73.5 ms |
| 10K — throughput | 200 q/s | 225 q/s | 218 q/s |
| 100K — latency | 227 ms | 406 ms | 847 ms |
| 100K — throughput | 17.6 q/s | 19.7 q/s | 18.9 q/s |

Mixed read/write workload shows *higher* aggregate throughput than pure readers at 10K — the single writer holds the write lock only during WAL append, allowing readers to proceed concurrently most of the time.

### serialized_writers — latency / aggregate throughput

Writes are serialized by design (one writer at a time). Throughput measures total committed writes/sec across all competing threads.

| DB size | 2 threads | 4 threads | 8 threads | 16 threads |
|---|---|---|---|---|
| 10K — latency | 16.9 µs | 39.2 µs | 80.1 µs | 159.9 µs |
| 10K — throughput | 118 K/s | 102 K/s | 100 K/s | 100 K/s |
| 100K — latency | 17.2 µs | 40.5 µs | 81.4 µs | 166 µs |
| 100K — throughput | 116 K/s | 98.8 K/s | 98.3 K/s | 96.4 K/s |

Aggregate write throughput drops ~15% from 2→4 threads (lock contention overhead), then stays flat at 4–16 threads — confirms serialised writes with negligible per-thread overhead. `serialized_writers` at ≥4 threads was previously OOM-killed on this machine; v6 clearing facts from RAM after checkpoint fixed that.

---

## Concurrency (File-Backed)

File-backed DB — reads go through the LRU page cache; writes append to the WAL sidecar. Throughput = aggregate ops/sec across all threads (v0.20.1).

### readers — latency / aggregate throughput

| DB size | 4 threads | 8 threads | 16 threads |
|---|---|---|---|
| 10K — latency | 24.4 ms | 56.6 ms | 114.9 ms |
| 10K — throughput | 164 q/s | 141 q/s | 138 q/s |
| 100K — latency | 325 ms | 711 ms | 1.27 s |
| 100K — throughput | 12.3 q/s | 11.2 q/s | 12.6 q/s |

File-backed read throughput is ~15–25% lower than in-memory at equivalent thread counts, due to page-cache locking on cache misses. At 10K the 4→8 thread scaling degrades (164→141 q/s) — the page-cache RwLock becomes contended when all pages are hot and threads compete on every read. At 100K throughput stays roughly flat (page-cache warm after first scan iteration).

### readers_plus_writer — latency / aggregate throughput

| DB size | 4 threads | 8 threads | 16 threads |
|---|---|---|---|
| 10K — latency | 24.2 ms | 49.3 ms | 104.3 ms |
| 10K — throughput | 165 q/s | 164 q/s | 153 q/s |
| 100K — latency | 303 ms | 646 ms | 1.20 s |
| 100K — throughput | 13.2 q/s | 12.4 q/s | 13.4 q/s |

Mixed workload throughput at 10K stays flat 4→8 threads (165→164 q/s) vs. the degradation seen in pure-readers — the writer holding the write lock briefly gives readers a chance to be scheduled without cache contention.

### serialized_writers — latency / aggregate throughput

| DB size | 2 threads | 4 threads | 8 threads | 16 threads |
|---|---|---|---|---|
| 10K — latency | 25.9 µs | 56.7 µs | 118 µs | 235 µs |
| 10K — throughput | 77.4 K/s | 70.6 K/s | 67.7 K/s | 68.0 K/s |
| 100K — latency | 26.7 µs | 57.3 µs | 117 µs | 236 µs |
| 100K — throughput | 75.0 K/s | 69.9 K/s | 68.2 K/s | 67.7 K/s |

File-backed write throughput (~68–77 K writes/sec) is ~30% lower than in-memory (~100–118 K/s) — the WAL fsync on each commit dominates. Throughput declines ~12% from 2→4 threads then stabilises, matching the in-memory contention pattern.

---

## Negation (`not` / `not-join`)

Measures the post-filter pass overhead at different dataset sizes. 10% of entities carry a `:banned true` fact that the `not` clause filters on.

All 10K benchmarks were run with 100 samples. The O(N²) scaling is a known limitation of the current negation implementation (no hash-join in the inner filter loop).

| Benchmark | 1K | 10K |
|---|---|---|
| `not_scale` | 101.84 ms | **6.986 s** |
| `not_join_scale` | 226.82 ms | 22.898 s |
| `not_rule_body` | 172.96 ms | 16.883 s |

10K `not_scale` updated in v0.13.1 (Phase 7.4 — snapshot fix, -12.1% vs pre-fix baseline of 7.95 s). `not_join_scale` and `not_rule_body` 10K numbers are from v0.13.0 and will be updated when re-benchmarked.

`not_selectivity` — fixed 10K DB, exclusion fraction swept from 0% to 100% (100 samples each):

| Selectivity | 0% excl. | 25% excl. | 50% excl. | 75% excl. | 100% excl. |
|---|---|---|---|---|---|
| `not_selectivity` | 11.606 s | 14.793 s | 18.289 s | 21.329 s | 13.291 s |

> The non-monotonic dip at 100%: when all entities are excluded, the negation check can short-circuit as soon as a matching banned fact is found (O(1) per binding), whereas the 0%–75% cases must exhaust the entire banned-entity scan before concluding "not found".

---

## Disjunction (`or` / `or-join`)

Measures `or`-expansion and `or-join` projection overhead. 25% of entities have `:tag-a`, 25% have `:tag-b`, 50% are untagged. All disjunction benchmarks use `sample_size(10)`.

The 10K numbers reflect a known O(N²) characteristic in the current `apply_or_clauses` implementation: branches are evaluated over the full incoming binding set (seeded re-scan). `or_rule_body` avoids this because rules start from an empty binding, giving O(N) branch expansion.

| Benchmark | 1K | 10K |
|---|---|---|
| `or_scale` | 644.76 ms | 68.929 s |
| `or_join_scale` | 683.99 ms | 72.751 s |
| `or_rule_body` | 26.468 ms | 2.123 s |

10K `or_scale` updated in v0.13.1 (Phase 7.4 — change not statistically significant at p=0.36; disjunction is O(N²) and dominated by branch enumeration, not the index rebuild). Other 10K numbers are from v0.13.0.

`or_selectivity` — fixed 10K DB, fraction matching either branch swept from 0% to 100% (10 samples each):

| Selectivity | 0% match | 25% match | 50% match | 75% match | 100% match |
|---|---|---|---|---|---|
| `or_selectivity` | 44.477 s | 62.668 s | 75.393 s | 88.977 s | 104.88 s |

> Selectivity scales roughly linearly with match fraction: each additional 25% of matching entities adds ~20 s at 10K. This is consistent with the O(N × result_count) cost of branch union construction and deduplication.

---

## Aggregation

Measures aggregation post-processing overhead. `count_scale`/`sum_scale` use the value-only fixture; `grouped_count_scale`/`with_grouped_sum` use a 10-department fixture (10 groups). All aggregation benchmarks use 100 samples.

| Benchmark | 1K | 10K |
|---|---|---|
| `count_scale` (scalar `count`) | 1.770 ms | **9.720 ms** |
| `sum_scale` (scalar `sum`) | 1.881 ms | 22.745 ms |
| `grouped_count_scale` (grouped by dept, 10 groups) | 4.038 ms | 51.550 ms |
| `with_grouped_sum` (`:with` clause, grouped sum) | 670.85 ms | 67.266 s |
| `count_distinct_scale` (50% duplicates) | 3-5 ms | 30-50 ms |

10K `count_scale` updated in v0.13.1 (Phase 7.4 — snapshot fix, -64.7% vs pre-fix baseline of 27.5 ms). Other 10K numbers are from v0.13.0 and will be updated when re-benchmarked.

> `count` and `sum` are O(N). `grouped_count` is slightly higher due to the two-pattern join (`[?e :dept ?dept]` × `[?e :val ?v]`). `with_grouped_sum` at 10K shows O(N²) scaling from the same two-pattern cross-product join — the planner currently lacks a hash-join step; this is tracked as a future optimisation.

---

## Expression Clauses

Measures the expression evaluation pass overhead. `filter_scale` keeps half of entities; `binding_scale` binds a new variable for every row; `binding_into_agg` pipes the bound variable into a `sum` aggregate. All 100 samples; all show clean O(N) scaling.

| Benchmark | 1K | 10K |
|---|---|---|
| `filter_scale` (`[(< ?v N)]`) | 1.799 ms | 22.738 ms |
| `binding_scale` (`[(+ ?v 1) ?result]`) | 2.037 ms | 23.603 ms |
| `binding_into_agg` (`[(* ?v 2) ?doubled]` → `(sum ?doubled)`) | 1.935 ms | 23.294 ms |

---

## Query: Predicate Pushdown

Measures the combined cost of a multi-pattern query with a selective `Expr` predicate pushed down to the earliest binding point (#207). Fixture: N entities each with `:val` (integer) and `:name` (string). Query: `(query [:find ?e ?n :where [?e :val ?v] [?e :name ?n] [(> ?v <threshold>)]])` with threshold at the 90th percentile — ~10% of entities pass the filter.

| Benchmark | 1K | 10K | 100K |
|---|---|---|---|
| `predicate_pushdown` | 5.9 ms† | 66 ms† | 204 ms† |

† Bencher CI baseline (v1.1.1, ubuntu-latest). Local i7 re-run pending.

Predicate pushdown evaluates `(> ?v threshold)` immediately after binding `?v` from the `:val` scan, before joining `:name`. At 10K this eliminates ~90% of the `:name` lookups. Scales approximately linearly with N — the 10× cost increase from 1K to 10K reflects the full-attribute scan over N `:val` facts plus the join for the 10% that pass.

---

## Window Functions (Phase 7.7a)

Measures window function evaluation overhead (running aggregates, ranking functions). Window functions run incrementally over an ordered result set using the `AggState` accumulator path — a separate code path from batch aggregates.

| Benchmark | 1K | 10K |
|---|---|---|
| `running_sum` (sum :over order-by) | ~5-10 ms | ~50-100 ms |
| `rank` (rank :over order-by) | ~5-10 ms | ~50-100 ms |
| `row_number` (row-number :over order-by) | ~5-10 ms | ~50-100 ms |

Window functions are O(N log N) due to sorting overhead. Without an explicit `:order-by`, results are in arbitrary order and window functions may produce non-deterministic results.

---

## Temporal Metadata (Phase 7.6)

Measures pseudo-attribute binding overhead (`?tx-time`, `?valid-from`, `?valid-to`). These require extra projection work per result row.

| Benchmark | 1K | 10K |
|---|---|---|
| `tx_time` (bind :tx-time) | ~2-3 ms | ~20-30 ms |
| `valid_from` (bind :valid-from) | ~2-3 ms | ~20-30 ms |
| `valid_to` (bind :valid-to) | ~2-3 ms | ~20-30 ms |

Temporal metadata adds ~1 column of projection overhead per row — negligible compared to the underlying query cost.

---

## UDF Dispatch Overhead (Phase 7.7b)

Measures the closure dispatch overhead for user-defined aggregates and predicates vs. built-in functions.

| Benchmark | 1K | 10K |
|---|---|---|
| `aggregate_sum_dispatch` (UDF sum) | ~2-3 ms | ~20-30 ms |
| `predicate_filter_dispatch` (UDF predicate) | ~2-3 ms | ~20-30 ms |

UDF dispatch adds ~1 function pointer indirection per aggregation step or predicate evaluation. The overhead is typically negligible compared to the overall query cost.

---

## Query: Regex Filter

Measures regex evaluation overhead via the `matches?` predicate. Regexes are precompiled at parse time.

| Benchmark | 1K | 10K |
|---|---|---|
| `regex_filter` (matches? with pattern) | 2.5 ms† | 28 ms† |

† Bencher CI baseline (v1.1.1, ubuntu-latest). Local i7 re-run pending.

---

## Concurrent B+Tree Range Scans (Phase 6.5)

Measures N simultaneous EAVT range scans against a fully committed (checkpointed) B+tree — no WAL involvement. Throughput = aggregate queries/sec across all threads (v0.20.1).

| DB size | 2 threads | 4 threads | 8 threads |
|---|---|---|---|
| 10K — latency | 23.4 ms | 24.6 ms | 56.9 ms |
| 10K — throughput | 85.3 q/s | 162 q/s | 140 q/s |
| 100K — latency | 264 ms | 322 ms | 702 ms |
| 100K — throughput | 7.57 q/s | 12.4 q/s | 11.4 q/s |

At 10K, throughput nearly doubles from 2→4 threads (85→162 q/s, +90%) — strong scaling on cache-warm pages. At 8 threads it drops back to 140 q/s — the per-page read `Mutex` becomes contended when all threads hit the same B+tree nodes simultaneously. At 100K the pattern repeats: 2→4 is +64%, then 4→8 degrades slightly as cold-page I/O serialisation limits further scaling.

The backend `Mutex` is held only for the duration of a single `read_page` call on a cache miss — cache-warm reads acquire no lock, allowing true parallel reads. Remaining contention at 8 threads reflects unavoidable cold-page I/O serialisation.

---

## Memory Usage (heaptrack)

Peak heap consumption during `examples/memory_profile` (insert N facts + one query + checkpoint). Measured with [heaptrack](https://github.com/KDE/heaptrack).

| Facts | Peak Heap | Peak RSS | Runtime |
|---|---|---|---|
| 10K | 11.9 MB | 19.2 MB | 0.26 s |
| 100K | 109.4 MB | 145.7 MB | 2.44 s |
| 1M | 1.05 GB | 1.60 GB | 27.9 s |

**Phase 6.5 improvement:** v6 no longer holds the full index in RAM after checkpoint — indexes live on disk and are paged in on demand via the LRU cache. At 1M facts, peak heap dropped from **1.33 GB → 1.05 GB** (~21%). At 100K: **135.7 MB → 109.4 MB** (~19%).

---

## Phase 6.4b → Phase 6.5 Summary

| Metric | Phase 6.4b (v5) | Phase 6.5 (v6) | Change |
|---|---|---|---|
| Open 100K facts | 259 ms | 119 ms | **2.2× faster** |
| Open 1M facts | 3.14 s | 1.31 s | **2.4× faster** |
| Checkpoint 10K | 16.5 ms | 11.8 ms | 1.4× faster |
| Query 1M (point) | 4.30 s | 4.33 s | ~same |
| `serialized_writers` ≥4T | OOM-killed | 17–78 µs | fixed |
| Peak heap 1M facts | 1.33 GB | 1.05 GB | **~21% less** |
| Peak RSS 1M facts | 2.04 GB | 1.60 GB | **~22% less** |

---

## Phase 7.3 → Phase 7.4 Summary

Phase 7.4 eliminated the per-query 4-index rebuild (`load_fact` loop — BTreeMap insertions for EAVT/AEVT/AVET/VAET) in the non-rules query path. `filter_facts_for_query` now returns an `Arc<[Fact]>` slice instead of constructing a `FactStorage`; the rules path still builds a `FactStorage` for `StratifiedEvaluator`.

| Metric | Pre-fix (v0.13.0) | Post-fix (v0.13.1) | Change |
|---|---|---|---|
| `query/point_entity` at 10K | 22.1 ms | 8.6 ms | **-61.5%** |
| `aggregation/count_scale` at 10K | 27.5 ms | 9.7 ms | **-64.7%** |
| `negation/not_scale` at 10K | 7.95 s | 6.99 s | -12.1% |
| `disjunction/or_scale` at 10K | 70.9 s | 68.9 s | ~same (p=0.36) |
| Rules path | unchanged | unchanged | index rebuild still paid |

Negation and disjunction improvements are smaller because those paths are O(N²) and dominated by the inner binding-loop cost, not the index rebuild. The rules-path index rebuild is tracked in the post-1.0 backlog.

---

## Known Limitations

- **Query scan**: Queries with a concrete entity or attribute keyword in at least one pattern use selective index-backed fetches — O(k), where k = facts for that entity/attribute (#208). Queries with no bound entity or attribute fall back to a full scan — O(facts). Expression predicates are pushed down to the earliest point where their variables are bound (#207). `not` / `not-join` and `or` / `or-join` mid-query remain O(N²) in the worst case — no hash-join step yet.
- **Backend mutex held on cache-cold page reads**: Concurrent B+tree scans serialise only when a page must be loaded from disk (cache miss). Cache-warm reads are fully parallel (#279/#281). Further per-page I/O parallelism is deferred to a future release.
- **1M recursion not benchmarked**: `chain/depth_100` takes 16 s; `chain/depth_1000` was not run.

## Covering AEVT Aggregate Read (2026-07-12)

Single-pattern attribute aggregates now fold the net current view directly from
covering AEVT entries. The cursor retains one entity's temporal/retraction state
and feeds typed values into global or grouped accumulators; it does not build an
attribute-sized `Vec<Fact>` or row binding map. Sparse BrowserDb execution owns
the same cursor across IndexedDB awaits, yields every 4,096 entries, and bounds
clean unpinned staging to 192 pages.

HAL7800, release build, 1M unique Integer facts, 20 measured repetitions:

| Metric | Previous clean baseline | Covering cursor |
|---|---:|---:|
| Aggregate p50 | 1,631 ms | 330.322 ms |
| Aggregate p95 | 1,698 ms | 357.688 ms |
| Query RSS delta | 380.8 MiB | 1.375 MiB |
| Retained RSS delta | 78.9 MiB | 1.375 MiB |
| Count / checksum | 1,000,000 / 499999500000 | exact |

The Chrome 150 browser-WASM suite includes a 12K paged aggregate that crosses
multiple 4,096-entry yields, processes a scheduled browser task before query
completion, performs no full-store IndexedDB read, and releases staging after
completion.

The clean production-path reference rerun at source `aaf32a9` also measured a
0.239 ms point read and 12.164 MiB open baseline RSS. Raw cross-engine receipts
are in
`benchmarks/baselines/ref-db/2026-07-12-hal7800-v3-pending-isolation-full/`.

The review-driven v4 rerun at clean source `7cedc1b` replaces that single point
observation with one excluded warmup plus 20 retained samples per engine. On
the 1M Vicia graph, point-read p50 was `0.011 ms` and p95/max was `0.019 ms`;
aggregate p50/p95 was `320.751/341.448 ms`, with exact count/checksum. The v4
summary removes the flat `rows` collection: `groups.engineAggregate` contains
Vicia/Grafeo/Turso/Cozo, while `groups.ownedResultScan` contains only the redb
and Fjall storage floors. Markdown likewise renders separate workload and
memory tables. The retained evidence is under
`benchmarks/baselines/ref-db/2026-07-12-hal7800-v4-review-full/`.

## Unrelated Pending Aggregate Isolation and Ownership (2026-07-12)

The clean `vicia.pending-isolation.v3` rerun at source `e4ec312` replaces the
duplicated fact/hash/B-tree ownership with one canonical overlay and streams
WAL transactions during replay. At 1M unrelated pending facts, live database
RSS is 221.445 MiB (down from 1,152.316 MiB), accounted payload is 171.842 MiB,
and replay-retained RSS is 0.285 MiB. Aggregate count/checksum remain exactly
`1,000,000 / 499999500000`; p50 is 555.508 ms versus 545.916 ms at zero pending,
p95 is 573.811 ms, and query RSS delta differs by 0.250 MiB. Every unrelated
cursor records zero selected pending entries/bytes and visits. All four index
run counts remain at or below the receipt's logarithmic bound of 21. The raw
clean receipt is generated at `target/pending-isolation/full/receipt.json`.

`vicia.pending-isolation.v2` retains the v1 selected-cursor gate and adds a
separate fresh-child memory audit. The audit opens the WAL-backed variant,
accounts live ownership without cloning, samples RSS, calls glibc
`malloc_trim(0)` while the database remains live, then drops the database and
trims again. This separates live database state from allocator-retained WAL
replay memory. Non-glibc hosts preserve the ownership receipt but report the
trim step as unsupported. The default API and v11 format remain unchanged.

HAL7800, clean source `84495e6`, exact selected count/checksum
`1,000,000 / 499999500000`:

| Pending shape | Open RSS | Query RSS delta | p50 | p95 | Live DB after trim | Accounted live payload | Container/allocator residual | Replay-retained RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| unrelated 0 | 3.004 MiB | 1.125 MiB | 528.272 ms | 565.683 ms | 0.211 MiB | 0 | 0.211 MiB | 0 |
| unrelated 10K | 15.480 MiB | 0.125 MiB | 554.574 ms | 580.559 ms | 11.273 MiB | 7.711 MiB | 3.562 MiB | 1.238 MiB |
| unrelated 100K | 124.398 MiB | 0.125 MiB | 518.281 ms | 552.322 ms | 107.703 MiB | 70.770 MiB | 36.933 MiB | 13.770 MiB |
| unrelated 1M | 1,295.113 MiB | 0.125 MiB | 514.956 ms | 540.242 ms | 1,152.316 MiB | 747.949 MiB | 404.367 MiB | 139.617 MiB |
| selected control 10K | 15.480 MiB | 1.566 MiB | 546.905 ms | 572.908 ms | 11.273 MiB | 7.664 MiB | 3.610 MiB | 1.242 MiB |

The 1M unrelated live payload decomposes as follows. Owned byte counts include
inline container payload plus exact `String`/`Vec<u8>` capacities; B-tree node
headers, hash control bytes, allocation size-class rounding, and allocator
metadata remain in the explicit residual.

| Live owner | Accounted MiB | Entries | Attribute buffers | Encoded-value buffers |
|---|---:|---:|---:|---:|
| Pending `Vec<Fact>` | 127.259 | 1,000,000 | 1,000,000 | 0 |
| Duplicate-key `HashSet` | 205.842 | 1,000,000 | 1,000,000 | 1,000,000 |
| EAVT `BTreeMap` | 138.283 | 1,000,000 | 1,000,000 | 1,000,000 |
| AEVT `BTreeMap` | 138.283 | 1,000,000 | 1,000,000 | 1,000,000 |
| AVET `BTreeMap` | 138.283 | 1,000,000 | 1,000,000 | 1,000,000 |
| VAET | 0 | 0 | 0 | 0 |

The primary cause is the live pending representation: each Integer fact has
five structural owners and creates five attribute buffers plus four encoded
value buffers, or 9,000,000 small heap allocations at 1M facts. This directly
accounts for 747.949 MiB. The remaining 404.367 MiB is consistent with those
small allocations' size-class/metadata cost plus B-tree nodes and hash-table
controls; that attribution is an inference from the exact allocation counts,
not a byte-exact split inside the residual.

WAL replay is a secondary, separately measured cost. `WalReader::read_entries`
retains all 1,000 decoded transactions / 1M facts (124.665 MiB accounted) while
`load_fact` clones them into the live pending owners. After the decoded batch is
dropped, glibc still retained 139.617 MiB of RSS until `malloc_trim`. The fresh
process peak was 1,322.551 MiB. Streaming replay can remove that transient and
retained portion, but it cannot fix the dominant 1,152.316 MiB live database
shape by itself.

All unrelated variants still recorded zero selected pending snapshot
entries/bytes and zero pending visits. The full validator passed exact
count/checksum, ±2 MiB query RSS, ≤10% p50 regression, and p95 ≤115% of p50.
Raw samples, smaps, cursor diagnostics, live component accounting, allocation
counts, decoded-WAL overlap, trim deltas, and provenance are preserved in
`benchmarks/baselines/pending-isolation/2026-07-12-hal7800-memory-full/receipt.json`.

Reproduce independently of the cross-database table:

```bash
just pending-isolation-smoke
just pending-isolation-full
```

### v11 storage layout and B-tree fill isolation

`vicia.storage-layout.v2` supersedes the v1 selection contract. It uses
nearest-rank percentiles (`ceil(p * N) - 1`), reports max and MAD separately,
and rotates `75/85/90/95/100` across fresh checkpoint and query children so
candidate order and session retention cannot decide the fill policy. The
validator recomputes every summary and gate from raw samples and audits its own
rejection behavior against mutated receipts.

The clean v2 1M run at source `bd5c0a1` selected no replacement for fill 75.
Fill 85 reduced the image to 316.992 MiB and passed checkpoint, point, and RSS
gates, but its aggregate p95 was 605.201 ms against a 524.824 ms p50, narrowly
above the 115% tail gate. Fill 100 retained the 276.727 MiB layout but failed
checkpoint (4,834.386/6,407.882 ms p50/p95), point, and aggregate gates.
Production therefore remains at fill 75. The next measurement must attribute
the slow checkpoint samples to fact packing, each index sort/build, integrity
catalog publication, or sync before changing B-tree finalization policy. Raw
evidence is under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-v2-full/receipt.json`.

The follow-up clean run at source `aad95a7` adds per-sample phase timing and
uses the lazy, byte-equivalent B-tree serializer for the initial checkpoint,
matching the already-bounded recompact path. At production fill 75, checkpoint
p50 fell from 5,032.714 to 4,505.694 ms and median peak RSS delta fell from
947.625 to 744.750 MiB. Point and aggregate results remained exact. No higher
fill was selected because checkpoint p95 still exceeded 115% of p50. Comparing
the fill-75 median and p95 samples attributes the tail primarily to pending
index sort (1,089.3 vs 1,946.0 ms) and the three populated B-tree builds, not
publication sync (~1 ms). The receipt is retained under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-lazy-full/receipt.json`.

The next clean run at source `9c03d24` replaces the four simultaneous pending
typed-key vectors with one reusable fact-position sort buffer and one canonical
value encoding per fact. At fill 75, median checkpoint peak RSS falls from
744.750 to 281.250 MiB and peak typed-key ownership falls from 1,000,000 entries
to one streamed entry. In the same rotated run, fill 90 passes every receipt-owned
gate: its 301.363 MiB image is 14.6% smaller than fill 75, checkpoint p50/p95 is
5,013.869/5,717.048 ms versus 5,347.251/7,118.443 ms at fill 75, point p50 is
0.027 ms, and aggregate p50 is 489.099 ms. Production bulk-build fill therefore
moves from 75 to 90 without changing the public API or v11 page format. Raw
evidence is retained under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-reference-sort-full/receipt.json`.

The v12 format candidate at source `9099310` adds adaptive page-local prefix
leaves with restart interval 16. At production fill 90, all 20,000 EAVT leaves
and the empty VAET leaf remain raw because prefix encoding does not save bytes;
all 16,667 AEVT and 15,325 AVET leaves use the prefix codec. The published 1M
image falls from 301.363 to 269.586 MiB (-10.54%). Against the preceding clean
v11 receipt, checkpoint p50 improves from 5,013.869 to 4,914.621 ms, point p50
from 0.026979 to 0.026294 ms, aggregate p50 from 489.099 to 472.221 ms, and
checkpoint RSS p50 by 0.625 MiB. Exact count/checksum remain
1,000,000/499,999,500,000.

This receipt validates and its mutation audit rejects altered codec accounting,
but it does not authorize rollout: checkpoint p95 is 5,783.706 ms (117.68% of
p50) and aggregate p95 is 554.744 ms (117.48% of p50), both above the 115%
tail gate. The host also had two unrelated long-running CPU-bound headless
Chrome renderers, so the next evidence must be an uncontended clean rerun rather
than another selective retry on the same host. V11 remains directly readable;
foreground open and delta checkpoint do not rewrite it, and only caller-scheduled
idle COW maintenance publishes v12. Raw evidence is retained under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-v12-prefix-full/receipt.json`.

An uncontended follow-up at source `144b36b` terminated the two identified
21–24-hour stale Vetch Playwright process trees before measurement and ran no
other heavy work alongside the benchmark. Fill 90 again produced the exact
269.586 MiB image and 1,000,000/499,999,500,000 result. Against the v11
reference-sort receipt, point p50 improves 6.35% (0.026979 to 0.025266 ms),
aggregate p50 improves 9.08% (489.099 to 444.675 ms), checkpoint p50 changes
by +1.32% (5,013.869 to 5,079.803 ms), and checkpoint RSS p50 falls 0.625 MiB.
Aggregate p95/p50 is now 106.96%, but checkpoint remains outside the rollout
gate at 6,127.321/5,079.803 ms, or 120.62%.

The two slow checkpoint samples raise EAVT/AEVT collect-sort and multiple index
build phases together; they do not isolate prefix encode/decode or publication
sync as a single cause. In the same rotated run, fill 75, 85, and 100 meet the
checkpoint tail rule, while no candidate passes every receipt-owned gate.
V12 therefore remains unmerged and Vetch keeps its v11 browser package. The
canonical uncontended evidence is retained under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-v12-prefix-uncontended-full/receipt.json`.

The checkpoint construction follow-up removes three general empty-base costs:
it serializes borrowed pending keys instead of cloning attribute/value buffers,
reuses exact EAVT fact-position order for AEVT when every fact has one
attribute, and serializes standalone separator keys only for the first entry of
each leaf. At source `2526307`, clean fill-90 checkpoint p50/p95 falls to
3,633.534/3,963.193 ms, 28.5%/35.3% below the uncontended v12 receipt, and the
p95/p50 ratio is 109.07%. Checkpoint RSS p50 remains 279.750 MiB, graph bytes
remain 269.586 MiB, and count/checksum remain exactly
1,000,000/499,999,500,000.

That receipt validates and its mutation audit passes, but does not select a
fill candidate. A separately owned Vetch TypeGPU QA process began during the
run and overlapped the query phase; fill-90 point p95 rose to 0.052 ms and
aggregate p95/p50 to 119.51%, while both gates passed in the two preceding clean
v12 receipts. The checkpoint-tail objective is closed without relaxing its
rule, but package rollout still requires one uncontended full receipt with all
receipt-owned gates plus real-browser WASM execution. Evidence is retained
under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-v12-lazy-separator-full/receipt.json`.

After the unrelated QA process exited, source `65eaaad` repeated the complete
matrix without concurrent build or QA work. Fill 90 again passes checkpoint at
3,775.192/4,256.684 ms p50/p95 (112.75%), aggregate at
458.594/513.579 ms (111.99%), size, and RSS. Point read is the sole failing
gate: 0.0268/0.0571 ms p50/p95, an absolute 30-microsecond tail gap. No
candidate is selected, so the remaining evidence task is a more stable repeated
point-read contract rather than another lucky full retry. The final uncontended
receipt is retained under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-v12-final-uncontended-full/receipt.json`.

The original `vicia.storage-layout.v1` study walks the published v11 image page by page and
attributes exact payload, structural, and unused bytes to fact pages and each
EAVT/AEVT/AVET/VAET leaf/internal tree. It also reports conservative key-prefix
estimates at restart intervals 10 and 16. The diagnostic API and fill override
exist only under `bench-internals`; the public API and file format are unchanged.

The clean 1M full run used 20 fresh checkpoint builds and 20 post-warmup point
and aggregate samples per fill candidate. Fill 100 reduced the fixture from
352.742 MiB to 276.727 MiB (-21.6%) and index unused bytes from 75.811 MiB to
1.476 MiB. Its checkpoint p50 improved from 5,156.8 ms to 5,045.8 ms and
aggregate p50 changed from 532.1 ms to 535.9 ms. It was not adopted: checkpoint
p95 was 7,278.6 ms, 144% of its p50 and 28.6% above the fill-75 p95, so it
failed the tail-latency gate. Production remains at fill 75.

At fill 75, the exact allocation is 61.875 MiB fact pages, 96.551 MiB EAVT,
96.551 MiB AEVT, 97.410 MiB AVET, 0.004 MiB VAET, and 0.348 MiB of published
header/catalog pages. Restart-16 prefix encoding is estimated to save 28.264
MiB before any page-repacking effects; this is evidence for a future format
study, not a v11 saving claim. Raw samples and page accounting are preserved in
`benchmarks/baselines/storage-layout/2026-07-12-hal7800-full/receipt.json`.

```bash
just storage-layout-smoke
just storage-layout-full
```

### Bounded checkpoint index construction

`vicia.checkpoint-construction.v2` measures fresh-child delta checkpoint and
forced idle recompact over a common 1M base with 1/10/100/1K pending facts.
Candidate fact pages are now written as soon as each page fills. Recompact then
revisits those immutable pages once per EAVT/AEVT/AVET/VAET index, retaining one
typed index at a time; B-tree serialization owns only the current page
frontier. The v11 byte format, page-0 publish boundary, and public API are
unchanged.

| Pending | Checkpoint p50 / p95 | Recompact p50 / p95 | Recompact RSS delta |
|---:|---:|---:|---:|
| 1 | 1.715 / 2.630 ms | 5,210.578 / 5,807.769 ms | 177.875 MiB |
| 10 | 1.704 / 3.023 ms | 5,138.870 / 5,768.242 ms | 177.875 MiB |
| 100 | 2.087 / 3.370 ms | 5,181.993 / 5,567.445 ms | 177.750 MiB |
| 1,000 | 7.845 / 9.864 ms | 5,225.844 / 5,589.081 ms | 177.000 MiB |

The prior Vetch Gate D idle-recompact evidence was 7.510 s and 947.512 MiB
RSS. The new matrix reduces measured construction RSS by about 81% while every
variant remains below the 8.261 s p50 budget and keeps nearest-rank p95 within
115% of p50. Each checkpoint/recompact phase records baseline and post-operation
`VmHWM` alongside the 2 ms sampler; the conservative peak is never below the
kernel high-water mark, including phases shorter than one sampling interval.
Diagnostics record one fact page in memory, exactly one full typed
index, four candidate-page passes, and at most 2,888 serialized bytes in the
B-tree frontier. External sorted runs are therefore not required for the
current 640 MiB maintenance gate.

Raw samples and provenance are preserved at
`benchmarks/baselines/checkpoint-construction/2026-07-13-hal7800-full/receipt.json`.

```bash
just checkpoint-construction-smoke
just checkpoint-construction-full
```

### Restart-aware leaf read path

`vicia.leaf-read-path.v1` compares the former full-leaf materialization path
with the page-backed raw/prefix cursor on one byte-identical fill-90 v12 1M
fixture. Point samples measure warmed 200-query batches in a fresh process;
aggregate samples run in a separate fresh process. Diagnostics run as one
explicit probe query so per-entry counters do not distort the timed samples.

| Metric | Full-leaf baseline | Page-backed cursor | Gate |
|---|---:|---:|---:|
| Point batch p95 | 0.02050 ms | 0.01087 ms | pass (`<= 0.050 ms`, no regression) |
| Aggregate p50 | 432.492 ms | 419.073 ms | fail (3.10%, requires 10% or `<= 230 ms`) |
| Aggregate p95/p50 | 102.75% | 101.87% | pass (`<= 115%`) |
| Query RSS delta | 1.125 MiB | 1.125 MiB | pass (`<= baseline + 2 MiB`) |
| Peak full-leaf entries / struct bytes / payload bytes | 60 / 7,200 / 4,020 | 0 / 0 / 0 | pass |

The cursor is retained as the durable traversal boundary: it removes leaf-local
result ownership, cuts selective point latency, preserves exact
1,000,000/499,999,500,000 count/checksum, and bounds prefix resume work to a
restart block. It does not avoid deserializing all 1M `AevtKey` values for an
aggregate, so the performance receipt rejects rollout. The next measured slice
is an allocation-free AEVT projection decoder inside this cursor, not a return
to full-leaf materialization or a larger prefetch batch.

The clean canonical storage-layout rerun at source `2efa2ac` validates and
passes its mutation audit after the cursor change. Fill 90 keeps the exact
269.586 MiB image and passes size, checkpoint (3,650.368/4,112.685 ms),
aggregate (435.765/446.391 ms), and RSS gates. Its 0.01876 ms point p95 fails
the receipt-owned regression threshold, while every other fill fails at least
one size, checkpoint, or point gate. `selectedFillPercent` therefore remains
`null`; the rerun is regression evidence, not rollout authority. Its receipt is
preserved under
`benchmarks/baselines/storage-layout/2026-07-13-hal7800-leaf-cursor-full/`.

Raw receipts are preserved under
`benchmarks/baselines/leaf-read-path/2026-07-13-hal7800-full/`.

The follow-up borrowed AEVT projection keeps that cursor boundary and decodes
the existing `(AevtKey, FactRef)` postcard bytes into borrowed attribute/value
views. The clean full receipt compares against the page-backed cursor candidate,
not the older full-leaf baseline:

| Metric | Page-backed cursor | Borrowed projection | Gate |
|---|---:|---:|---:|
| Point batch p95 | 0.01087 ms | 0.01584 ms | fail (absolute `<= 0.050 ms` passes; recorded no-regression gate fails) |
| Aggregate p50 | 419.073 ms | 413.713 ms | fail (1.28%, requires 10% or `<= 230 ms`) |
| Aggregate p95/p50 | 101.87% | 104.80% | pass (`<= 115%`) |
| Query RSS delta | 1.125 MiB | 1.250 MiB | pass (`<= baseline + 2 MiB`) |
| Projected emitted / owned AEVT decode | n/a | 1,000,000 / 0 | pass |
| Peak full-leaf entries / struct bytes / payload bytes | 0 / 0 / 0 | 0 / 0 / 0 | pass |

Projection decode time in the diagnostic probe falls from 177.422 ms to
132.585 ms (25.27%), while end-to-end aggregate p50 improves only 5.360 ms.
The remaining aggregate cost is therefore outside owned AEVT leaf-key decode;
the next measurement must split reducer/entity flush and aggregate-sink time
before another implementation slice. The point workload never enters the
projection path (`aevtProjectionDecodes = 0` in its probe), so its recorded
no-regression failure is retained as host-level gate evidence rather than
attributed to this cursor. v12 rollout remains open, and no browser package or
canonical storage-layout evidence is replaced from this receipt.

The clean projection receipts are preserved under
`benchmarks/baselines/leaf-read-path/2026-07-13-hal7800-projection-full/`.

The next clean receipt adds diagnostic-only phase timing. Timers are disabled
for the 20 performance samples and enabled only for a separate probe query, so
the probe can attribute nested work without changing the candidate samples.
The 1M baseline probe records 495.124 ms total: postcard projection decode is
119.247 ms, current-entry reduction is 113.110 ms (22.84%), entity-flush
preparation is 30.738 ms, typed aggregate visitor work is 42.405 ms, and
aggregate finish/projection is 0.007 ms. Reduction therefore passes the 10%
ownership threshold for one production repair.

`CurrentAttributeValues` now retains one reusable inline value/state for the
common one-value entity and promotes to the existing hash-map reducer only when
an entity actually has multiple encoded values. This removes repeated outer
map construction and reuses the inner temporal-window allocation without
changing scoped/unscoped retraction, valid-time, Float resolution, or Ref
semantics.

| Metric | Phase-attribution baseline | Inline reducer | Gate |
|---|---:|---:|---:|
| Point batch p95 | 0.01496 ms | 0.01363 ms | pass (`<= 0.050 ms`, no regression) |
| Aggregate p50 | 355.045 ms | 282.403 ms | pass (20.46% improvement) |
| Aggregate p95/p50 | 102.45% | 102.82% | pass (`<= 115%`) |
| Query RSS delta | 1.125 MiB | 1.250 MiB | pass (`<= baseline + 2 MiB`) |
| Reducer diagnostic time | 113.110 ms | 47.350 ms | 58.14% reduction |
| Projected emitted / owned AEVT decode | 1,000,000 / 0 | 1,000,000 / 0 | pass |
| Peak full-leaf entries / struct bytes / payload bytes | 0 / 0 / 0 | 0 / 0 / 0 | pass |

The leaf-read candidate passes its performance gate and all 924 non-ignored
library tests plus the integration/doc suites, the browser WASM check, clippy,
and the real Chrome 74-test suite.
The follow-up clean `vicia.storage-layout.v2` full receipt remains valid and
mutation-audited, but selects no v12 fill. Fill 90 keeps the 269.586 MiB image
and 286.349 ms aggregate p50, while its checkpoint p95/p50 is 122.20%, point
p50/p95 is 0.01821/0.03552 ms relative to fill 75's 0.01373/0.02028 ms, and
aggregate p95/p50 is 119.61%; those receipt-owned gates fail. Every other
high-fill candidate also fails at least one size, checkpoint-tail, point, or
aggregate-tail gate. v12 rollout therefore stays open and the Vetch browser
package is not replaced.

The clean leaf receipts are preserved under
`benchmarks/baselines/leaf-read-path/2026-07-14-hal7800-inline-reducer-full/`;
the storage-layout receipt is preserved under
`benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/`.

The derived `vicia.storage-layout-variance.v1` report ties every checkpoint
p95/max observation back to its diagnostic phases, sample number, and rotated
execution position. Sync is the dominant positive phase delta for both p95 and
max at all four high-fill candidates. Their eight observations are distributed
across positions 0–3 with no position holding half, so the report classifies
the checkpoint tail as host-I/O variance rather than admitting another
construction change. This does not authorize removing or weakening fact,
data, or publish sync.

The point failure is separate and systematic. Relative to fill 75, point p50
rises 23.05% at fill 85, 32.63% at fill 90, and 49.31% at fill 100; fill 95
stays within the p50 boundary but exceeds the p95 boundary by 33.59%. The next
risk probe therefore owns point-path density attribution, not checkpoint or
current-aggregate tuning. The report, its source SHA-256, validator, and
mutation audit retain `selectedFillPercent = null` and production fill 75.

```bash
node scripts/analyze-storage-layout-variance.mjs \
  benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/receipt.json \
  benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/variance.json
node scripts/validate-storage-layout-variance.mjs \
  benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/receipt.json \
  benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/variance.json
node scripts/audit-storage-layout-variance-validator.mjs \
  benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/receipt.json \
  benchmarks/baselines/storage-layout/2026-07-14-hal7800-inline-reducer-full/variance.json
```

The clean `vicia.point-path-density.v1` follow-up builds one equivalent 1M
fixture per fill and runs 20 rotated exact EAVT point samples. Diagnostic
queries are separate from the timed samples. All candidates retain height 4,
one raw EAVT leaf visit, 5–6 leaf lower-bound comparisons, 7–8 projected
decodes, one cached fact resolution, zero prefix reconstruction, zero owned
EAVT decode, and zero full-leaf materialization.

The varying work is internal separator descent. Fill 75/85/90/95/100 performs
35/61/67/47/75 separator comparisons and decodes
2,309/4,018/4,412/3,093/4,943 separator bytes. Median descent time is
5.004/8.548/9.198/6.664/10.387 microseconds, while point p50 is
0.01029/0.01445/0.01436/0.01192/0.01565 ms. Comparison count and descent time
correlate with point p50 at 0.988 and 0.991 respectively. This admits one
production repair: replace the internal page's linear separator scan with a
binary search for the first separator greater than the requested key. It does
not admit leaf, cache, fact-resolution, format, or API work.

The clean receipt and mutation-audited analysis are preserved under
`benchmarks/baselines/point-path-density/2026-07-14-hal7800-internal-descent-full/`
from source `1b4391d`.

```bash
just leaf-read-path-smoke
just leaf-read-path-full
just leaf-read-path-compare \
  benchmarks/baselines/leaf-read-path/2026-07-14-hal7800-inline-reducer-full/baseline.json \
  benchmarks/baselines/leaf-read-path/2026-07-14-hal7800-inline-reducer-full/candidate.json
```

### H0 Vetch ledger caller contract

`vicia.vetch-ledger-caller-fixture.v1` freezes four typed caller shapes from the
current Vetch authority boundary: card move, Condense admission, proposal
verdict, and agent brief. Every sample derives fresh identities, compiles the
typed delta through the exact browser atomic-write parser/materializer, commits
under one transaction cursor, compares the exported full-history tail, and
performs one bounded proof read.

The local H0 receipts passed all 26 correctness checks at both 10K and 1M:

| 1M native metric | p95 range across four fixtures |
|---|---:|
| Caller encoding | `0.010..0.013 ms` |
| Datalog parse/materialization | `0.023..0.040 ms` |
| Atomic mutation | `1.181..2.220 ms` |
| Exact proof read | `0.052..0.070 ms` |

The real-Chrome 1M paged receipt used 20 observations per fixture:

| Browser metric | p95 range |
|---|---:|
| `executeAtomic()` total | `1.6..1.9 ms` |
| Rust mutation stage | `1 ms` |
| IndexedDB publication stage | `1 ms` |
| JSON result decode | `0..0.1 ms` |
| Exact proof read | `0.1..0.2 ms` |

The millisecond-resolution internal browser stage clock is diagnostic; the
high-resolution total is the comparison surface. H0 does not set a new absolute
budget. It rejects parse/JSON optimization and expanded commit receipts as
unmeasured public-API work, keeps atomic expectations behind the existing
serialized basis check, and admits transaction-pinned multi-query reads as the
next concrete gap.

Clean receipts from source `f1beb28` are preserved under
`benchmarks/baselines/vetch-ledger-caller/2026-07-13-local-h0/`. Both native
receipts and the real-Chrome receipt record `sourceDirty: false`; the validator
mutation audit rejects missing samples, invalid stage values, changed wasm
digests, missing scenarios, and duplicate typed changes.

```bash
VICIA_BENCH_RECEIPT=target/h0-smoke.json \
  cargo bench --features bench-internals \
  --bench vetch_ledger_caller_benchmark -- smoke

VICIA_BENCH_BASE_FIXTURE=<1m.graph> \
VICIA_BENCH_RECEIPT=target/h0-full.json \
  cargo bench --features bench-internals \
  --bench vetch_ledger_caller_benchmark -- full

wasm-pack build --target web --out-dir minigraf-wasm -- \
  --features browser,bench-internals
python3 -m http.server 8123
VICIA_BENCH_RECEIPT=target/h0-browser.json \
  node examples/browser/bench-driver.cjs ledger-caller \
  /path-served-under-repo/1m.graph 20
```

### H2 bounded typed current readers

`vicia.current-reader.v1` measures the two H2 public selection boundaries on a
checkpointed Ref-valued fixture: exact `(entity, attribute)` EAVT reads and
exact `(target, attribute)` VAET reverse-reference reads. The full profile
builds 1,000,000 facts, selects one middle fact, and records 20 observations
per reader. A separate diagnostics probe owns the structural gates.

| 1M typed read | p50 | p95 | leaf pages | projected emitted | owned key decode | full-leaf peak |
|---|---:|---:|---:|---:|---:|---:|
| `currentEntities` | 0.014407 ms | 0.050095 ms | 1 | 1 EAVT | 0 | 0 |
| `refsTo` | 0.010912 ms | 0.028309 ms | 1 | 1 VAET | 0 | 0 |

Both reads pass the 10 ms p95 foreground budget and the stricter structural
contract: at most two leaf pages, no owned projected key decode, and zero
full-leaf entries, struct bytes, or decoded payload bytes. The validator
mutation audit proves that altered fixture shape, latency, leaf scope,
materialization, owned-key count, or projected emission is rejected. Native
and real-Chrome tests separately prove exact Vetch caller-fixture equivalence,
transaction pinning, bi-temporal retractions, layered base/delta merge, and
bounded resume when pending work exhausts a step before the committed
candidate is consumed.

The clean full receipt from source `2a0ef75` is preserved at
`benchmarks/baselines/current-reader/2026-07-13-hal7800-h2-full/receipt.json`.

```bash
just current-reader-smoke
just current-reader-full
```

---

## Reproducing

```bash
# Run all Criterion benchmarks (HTML report in target/criterion/)
cargo bench

# Run a specific group
cargo bench -- "insert"
cargo bench -- "concurrent_btree_scan"

# Run heaptrack memory profile (requires heaptrack installed)
cargo build --release --example memory_profile
heaptrack ./target/release/examples/memory_profile 100000
heaptrack_print -f heaptrack.memory_profile.*.zst --merge-backtraces=0
```
