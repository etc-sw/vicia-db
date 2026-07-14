# Vicia DB Performance Roadmap

Status: canonical high-level performance direction as of 2026-07-14.

This document owns the whole performance shape, priority order, admission
rules, and stop conditions. It does not replace the evidence or implementation
documents:

- `docs/BENCHMARKS.md` owns measured results and receipt provenance.
- `docs/REF_DB_PERFORMANCE_TODO.md` owns the current executable checklist.
- `docs/VETCH_DELTA_STORAGE_ROADMAP.md` and `docs/DELTA_INDEX_DESIGN.md` own
  base/delta/recompact durability semantics.
- `docs/CROSS_DB_STRESS_BENCHMARK.md` owns cross-engine workload contracts.

When these documents disagree about performance priority, refresh this roadmap
from the latest clean receipt before admitting implementation. Live code and
validated receipts remain stronger evidence than prose.

## 1. Product and System Shape

Vicia DB is an embedded, single-file, bi-temporal Datalog ledger. Performance
work must preserve that noun and its authority boundary:

```text
append-only full-history ledger
  -> WAL-backed pending facts
  -> in-file sorted delta segments
  -> immutable compact base
  -> derived current and query projections
```

The full-history ledger is authority. Current views, aggregate summaries,
statistics, caches, and compact layouts are derived and rebuildable. A faster
projection must never collapse or redefine full-history identity:

```text
entity + attribute + encoded value
+ valid_from + valid_to
+ tx_count + tx_id + asserted
```

`Value::Ref`, scoped and unscoped retractions, arbitrary `:as-of`, and valid
time remain mandatory correctness dimensions.

## 2. Performance Model

Vicia has four different operating rhythms. They must not be optimized as one
undifferentiated benchmark:

| Rhythm | Expected cost owner | Policy |
|---|---|---|
| Durable append/receipt | changed facts and WAL bytes | immediate foreground |
| Delta checkpoint | pending/delta size | bounded foreground or short worker task |
| Current/selective read | selected index range and result budget | bounded foreground |
| Recompact/full rebuild | total visible history | explicit idle maintenance/import |

The central rule is that ordinary Vetch interaction must not pay total committed
graph size. O(total-history) work is acceptable only for explicit maintenance,
verified export, import, repair, or a query whose declared semantics truly
require the full history.

## 3. Non-Negotiable Constraints

Every performance slice must preserve:

- embedded library execution; no required server or external service;
- one logical portable `.graph` image;
- Datalog as the query language;
- v1-v12 readable compatibility and explicit migration policy;
- append-only bi-temporal history and exact retraction semantics;
- page-0 publication, manifest fallback, WAL-retire, and corruption rules;
- native, browser/WASM, mobile, and FFI viability;
- dependency-light core and bounded binary growth;
- public API compatibility unless a separate breaking-change plan is approved.

The following are outside the core roadmap without new benchmark-backed
authority:

- external storage-engine adoption;
- a second authoritative sidecar index;
- vector, BM25, multimodal blob, GPU, or distributed query infrastructure;
- weakening `fsync`, integrity checks, or receipt gates to improve a number;
- hidden O(total-history) work in interactive writes or reads;
- a browser-only file/schema authority that diverges from `.graph` bytes.

## 4. Current Baseline

The completed line has already removed the largest avoidable costs:

- multi-segment delta checkpoints scale with pending/delta work rather than the
  committed base;
- current attribute aggregates stream from covering AEVT without an
  attribute-sized `Vec<Fact>` or binding map;
- restart-aware page-backed cursors materialize no full leaf;
- borrowed AEVT projections emit 1M entries with zero owned `AevtKey` decode;
- the common one-value entity reducer uses reusable inline state;
- internal separators use binary search;
- pending indexes own each fact/value once and isolate unrelated attributes;
- repeated 1M aggregates retain about 1.125 MiB with zero median growth.

The storage-layout frontier receipt selected fill 87 under its 20-run matrix:

| 1M v12 fill-87 metric | Result |
|---|---:|
| Graph size | 276.590 MiB |
| Size reduction vs fill 75 | 11.93% |
| Full checkpoint p50 / p95 | 3,274.761 / 3,496.123 ms |
| Point p95 | 0.01420 ms |
| Aggregate p50 / p95 | 283.743 / 304.362 ms |
| Query RSS p95 | 0.125 MiB |

A subsequent 40-pair direct risk probe rejected that promotion. Fill 90 was
smaller (269.586 versus 276.590 MiB), won 28 of 40 paired checkpoints, and was
faster at checkpoint p50/p95 (3,198.195/3,748.090 versus
3,248.581/4,090.620 ms), point p50/p95, and aggregate p50/p95. Production
therefore retains fill 90 and exact fill tuning ends. Neither fill passed the
existing checkpoint `p95 <= 115% of p50` rollout gate, so this decision does
not authorize replacing the Vetch browser package.

Different benchmark families use different workload contracts; their absolute
numbers must not be mixed. The cross-engine v5 receipt, for example, records a
171.455/177.240 ms Vicia aggregate and a 29.530/32.769 ms SQLite aggregate
under its own exact engine-aggregate contract.

## 5. Execution Order

The roadmap is deliberately sequential at authority boundaries. Measurement
may proceed independently, but production changes advance only after the prior
gate closes.

```text
R0 close v12 fill policy (fill 90 retained)
  -> R1 current-projection feasibility
  -> R2 in-file current projection, if admitted
  -> R3 bounded-memory recompact
  -> R4 general Datalog execution
  -> R5 cold-page I/O and cache concurrency
  -> R6 next-format density study, only if still justified
```

Benchmark infrastructure, browser parity, recovery testing, and package
provenance run across every stage rather than appearing as a final cleanup.

## R0. Close the v12 Fill Policy (Complete)

### Outcome

End exact fill tuning with one direct comparison and leave production on the
better supported packing policy.

### Slice

- compare fill 87 and fill 90 over 40 alternating paired 1M runs with explicit
  fill overrides;
- retain production fill 90: it is smaller, wins 28/40 paired checkpoints, and
  has better checkpoint, point, and aggregate medians;
- preserve exact count/checksum `1,000,000/499,999,500,000` for every query
  sample;
- stop fine-grained fill search because it is no longer an admitted structural
  optimization.

### Stop conditions

- do not merge the rejected fill-87 production change;
- do not weaken sync or acceptance thresholds;
- do not treat a closed fill decision as v12/Vetch package authorization;
- do not continue directly into another file-format packing change.

## R1. Current-Projection Feasibility Gate

### Why

The current aggregate path is allocation-light but still folds 1M history
entries for every exact current aggregate. Decoder and reducer tuning have
already delivered diminishing returns. A larger improvement requires avoiding
repeated temporal folding, not another small callback optimization.

### Larger shape

Add a rebuildable in-file current projection whose authority is explicitly
subordinate to the full-history ledger:

```text
base + visible delta history
  -> deterministic current-state projection
  -> selective current reads and admitted aggregates
```

Arbitrary `:as-of` and historical export continue to read the ledger. The
projection serves only semantics it can prove exactly.

### First proof

Build a `bench-internals` risk probe, not a public API or format. Materialize a
compact typed current-state candidate from the existing exact cursor, then
measure:

- current count/sum latency and p95 stability;
- candidate bytes and rebuild time;
- incremental assert, scoped retract, unscoped retract, and valid-window update
  cost;
- base+delta visibility and publication-generation invalidation;
- Integer, Float, Boolean, Ref, Keyword, String, and Null correctness.

### Admission gate

Proceed to R2 only when the same-source 1M receipt shows:

- aggregate p50 `<= 150 ms` or at least 35% below the current exact path;
- aggregate p95 `<= 115%` of p50;
- query RSS no more than current baseline + 2 MiB;
- projection size no more than 15% of the selected v12 image;
- no foreground delta-checkpoint regression above 10%;
- exact current results across retractions, `Value::Ref`, and valid-time cases;
- deterministic rebuild from the ledger after projection loss/corruption.

If the probe misses the latency gate, delete the prototype and proceed to R3.
Do not ship a second current-state authority for a marginal improvement.

### Measured outcome

R1 passes on the clean 1M full receipt from source `853a800`:

| Metric | Ledger fold | Projection candidate |
|---|---:|---:|
| Aggregate p50 | 264.261 ms | 4.033 ms |
| Aggregate p95 | 269.842 ms | 4.244 ms |
| Query RSS delta | — | 0 MiB |
| Accounted projection bytes | — | 29.000 MiB (11.46% of image) |
| Deterministic rebuild | — | pass |
| One-fact ledger-tail refresh | — | 0.105 ms |

The candidate preserves exact count/checksum, all `Value` variants,
`Value::Ref`, scoped and unscoped retractions, overlapping valid windows,
pending/base precedence, and checkpoint-generation invalidation. It is not
installed in production routing and changes no public API or persisted bytes.
This admits R2 design work; it does not authorize persisting the fixed-time
candidate unchanged.

### Reference implementations

- Turso's DBSP-style incremental aggregate operator:
  `/home/upopo/db-ref/turso/core/incremental/aggregate_operator.rs`;
- Grafeo's immutable columnar base plus mutable overlay:
  `/home/upopo/db-ref/grafeo/crates/grafeo-core/src/graph/compact/layered.rs`;
- Grafeo's compact columns and factorized execution chunks:
  `graph/compact/column.rs` and `execution/factorized_chunk.rs`.

Borrow incremental-state, deletion, overlay precedence, and rebuild
invariants. Do not import either engine or adopt a general columnar backend.

## R2. In-File Current Projection

This stage exists only if R1 passes.

### First slice: temporal projection layout

Before assigning a page root, extend the compact candidate with the valid-time
information required to answer a moving current read without a hidden rebuild.
The fixed-time R1 candidate becomes stale when wall clock time crosses a
`valid_from` or `valid_to` boundary even if the ledger generation is unchanged.
R2-A must therefore choose and measure one durable representation:

- compact surviving `(valid_from, valid_to)` interval columns filtered at read
  time; or
- an equivalent boundary schedule that updates only rows crossing the current
  time and remains exactly rebuildable from the ledger.

The representation must retain R1 latency/RSS gates, stay within 15% of the
fill-90 image, and prove clock-boundary transitions, overlapping windows,
retractions, and deterministic rebuild. Do not create persisted projection
pages until this temporal identity is green.

### Storage boundary

- projection pages live inside the logical `.graph` image;
- projection generation is bound to the exact base generation and visible
  manifest generation;
- incomplete or corrupt newer projection publication falls back to the last
  projection matching the selected ledger state, or rebuilds explicitly;
- projection loss never makes ledger history unreadable;
- WAL retirement remains tied to durable ledger publication, not projection
  publication;
- foreground writes update a bounded delta projection or mark it stale; they
  never rebuild total history.

### Query boundary

- specialize exact current selectors and admitted count/sum operations first;
- keep arbitrary Datalog, historical, recursive, grouped, distinct, window,
  and UDF paths on the existing executor until separately measured;
- expose no public projection-management API unless a real caller cannot use
  existing maintenance/read surfaces.

### Verification

- differential current results against the ledger fold;
- base-only, delta-only, base+delta, same-key, retract, and valid-window cases;
- crash before/after projection publish;
- missing/corrupt projection rebuild;
- long-lived read-view generation pinning;
- native and real-Chrome package parity.

## R3. Bounded-Memory Recompact and Full Build

### Why

Foreground checkpoints are already delta-sized. The remaining O(total-history)
cost belongs to import, repair, and idle recompact. Current candidate fact
pages and sorted index buffers still scale with total facts.

### Target shape

Use a bounded-memory bulk builder:

```text
visible history stream
  -> bounded sorted runs
  -> k-way merge per index
  -> sequential fact/leaf/internal page emission
  -> verify candidate
  -> page-0 or atomic-image publication
```

### Durable slices

1. Add phase and ownership diagnostics for run creation, merge fan-in,
   candidate pages, temporary bytes, and peak live entries.
2. Stream one index build through bounded runs while retaining the current
   builder as the byte/semantic oracle.
3. Generalize to EAVT/AEVT/AVET/VAET with one canonical value encoding and
   reusable buffers.
4. Publish a complete candidate only after integrity verification.
5. Reclaim obsolete COW lineage through the existing explicit maintenance
   boundary.

Temporary run files are allowed only as crash-cleanable construction scratch;
they never become a second database authority or a required deployment
artifact.

### Gate

- peak recompact RSS `<= 128 MiB` at the 1M fixture;
- no work-proportional retained RSS after drop/trim;
- p50 and p95 no more than 15% slower than the current same-source builder;
- exact fact-log identity and current/historical query equivalence;
- candidate failure leaves the selected publication and WAL authority intact;
- successful native/browser maintenance reclaims obsolete physical pages.

### References

- Fjall flush/compaction worker, sorted runs, restart policy, and snapshot
  tracking under `/home/upopo/db-ref/fjall/src/`;
- redb page-backed tree building, page manager, COW publication, and cache under
  `/home/upopo/db-ref/redb/src/tree_store/`;
- Grafeo compact builder and atomic compact-base swap under
  `/home/upopo/db-ref/grafeo/crates/grafeo-core/src/graph/compact/`.

## R4. General Datalog Execution

### Why

Selective point/current paths are fast, but general joins, mid-query negation,
disjunction, and some grouped aggregates still have O(N²) behavior. This is a
query-execution problem, not a storage-format problem.

### Order

1. Establish scaling receipts for general pattern join, `not`/`not-join`,
   `or`/`or-join`, grouped count/sum, and rules.
2. Replace nested membership scans with hash/semi-join structures where input
   bindings are stable.
3. Preserve dependency-safe predicate pushdown and add cardinality feedback
   only where the receipt proves join order matters.
4. Introduce block/value-vector execution for aggregate input only after a
   live-path phase receipt proves per-row dispatch dominates.
5. Add parallel native execution only after the serial block path is correct
   and memory-bounded.

### Gates

- demonstrate near-linear scaling on the admitted 1K/10K/100K workload;
- no regression on selective point/current paths;
- bound intermediate rows and respect foreground work/result budgets during
  generation, not after materialization;
- preserve stratification, recursive fixpoint, retraction, and temporal
  semantics;
- keep deterministic results across native and WASM.

### SIMD policy

The existing SIMD study found explicit temporal filters slower than optimized
scalar code. Integer horizontal sum was faster only after obtaining a
contiguous typed slice. Therefore:

- do not add explicit SIMD to row-oriented temporal filtering;
- revisit SIMD only inside an admitted typed block/current-projection layout;
- require end-to-end query improvement, not a synthetic kernel win.

### References

- Cozo query evaluation, magic sets, join reorder, and temporary relations
  under `/home/upopo/db-ref/cozo/cozo-core/src/query/` and `runtime/temp_store.rs`;
- Grafeo vector/factorized/parallel pipeline code under
  `/home/upopo/db-ref/grafeo/crates/grafeo-core/src/execution/`;
- Turso translation, grouping, aggregation, and resumable VDBE execution under
  `/home/upopo/db-ref/turso/core/`.

## R5. Cold-Page I/O and Cache Concurrency

### Why

Cache-warm reads are already highly competitive. Remaining concurrency and
tail risk is concentrated in cold page loads, scan pollution, browser page
demand, and host sync variance.

### Measurement first

Separate these workloads:

- cold point lookup;
- warm point lookup;
- short selective range;
- full sequential aggregate scan;
- concurrent readers with independent misses;
- reader plus WAL writer;
- browser IndexedDB demand read;
- checkpoint/recompact sync phases.

### Candidate work

- move independent cold reads outside a global backend mutex where the page
  authority permits;
- coalesce duplicate concurrent misses for the same page;
- add sequential-leaf readahead that remains within the declared read budget;
- use scan-resistant cache admission so one aggregate does not evict hot point
  pages;
- evaluate native read-only `mmap` views only behind the existing page and
  checksum boundary;
- tune browser access-shaped fetch windows from measured page demand, not a
  larger global batch constant.

### Gate

- improve the targeted cold/concurrent p95 by at least 20%;
- no warm point p95 regression beyond 10%;
- no cache, RSS, open-handle, or browser staging growth beyond existing bounds;
- no weakening of page verification, generation checks, or durability sync.

redb's cached file, LRU, page manager, and borrowed cursor are the primary
reference. Larger browser prefetch alone is explicitly rejected because the
previous aggregate bottleneck was query restart and per-entry work, not only
batch size.

## R6. Next-Format Density Study

This stage is conditional. v12 fill 87 already passes the current size gate.
Open a new format only when product distribution, browser package size, import
time, or real user files prove the remaining 276.590 MiB shape is a blocker.

### Candidate studies

- abbreviated/prefix-compressed internal separator keys;
- page-local dictionaries for repeated attributes and keywords;
- index-specific restart policies rather than one fixed interval;
- page-adaptive restart intervals selected from encoded cost;
- compact UUID/time/value suffix encodings;
- index-specific covering payloads that preserve each admitted query contract;
- contiguous-image reclamation of obsolete COW lineage.

### Gate

A v13 proposal must demonstrate on the full 1M matrix:

- at least 10% additional image reduction versus selected v12 fill 87;
- checkpoint, point, aggregate, and RSS within 110% of the v12 baseline;
- p95 no more than 115% of p50;
- v11/v12 readable compatibility and explicit migration behavior;
- malformed dictionary/prefix/restart/separator corruption tests;
- no dependency or binary-size cost disproportionate to the saving.

Page size changes, whole-page general compression, and dropping an index are
not default candidates. Each changes more than density and needs its own
workload and compatibility case.

## Cross-Cutting: Browser and Vetch Adoption Gate

Every production performance change that reaches the browser must close this
sequence:

```text
clean native receipt
  -> full Rust/fmt/Clippy
  -> wasm32 browser build
  -> real Chrome correctness and bounded-memory suite
  -> complete package assembly
  -> Vetch vendor sync
  -> Vetch consumer smoke
```

Foreground APIs remain capability-scoped:

- interactive handles own bounded reads and atomic writes;
- maintenance handles own checkpoint advice, compact maintenance, backup,
  verified export, and strict import;
- no package change may reintroduce hidden checkpoint/recompact work into an
  interactive lifetime.

GPU/WebGPU analytics, if ever useful, belong first in a Vetch-derived analytic
projection. Vicia core should remain the CPU-portable authority unless a
separate end-to-end proof shows that transfer, layout conversion, WASM/browser
support, dependency size, and fallback behavior all earn inclusion.

## Cross-Cutting: Benchmark and Receipt Contract

Every performance decision uses a clean, reproducible receipt:

- source commit and tracked-clean state;
- host, filesystem, runtime, durability mode, and feature provenance;
- warmup policy and raw samples;
- fresh child processes where allocator/build history would contaminate query
  results;
- rotated candidate order;
- p50, nearest-rank p95, max, and MAD;
- exact count/checksum or semantic fingerprint;
- RSS baseline, peak, retained, and post-drop ownership where relevant;
- repository-only structural diagnostics;
- validator and mutation audit;
- distinct contracts for engine aggregate, owned-result scan, point, current,
  historical, checkpoint, and maintenance work.

The preferred local full-run environment is the WSL ext4 filesystem with low
I/O pressure and no competing build, model, browser, or benchmark work. Windows
or a dual-boot environment is not required merely to obtain valid storage
evidence. Real Windows/WebView2 remains a separate product packaging/runtime
gate when that platform surface changes.

## Cross-Cutting: Correctness Matrix

Every admitted storage/read/query optimization must cover the applicable rows:

- raw and prefix leaves;
- v11 read and v12 read/write;
- base-only, delta-only, and base+delta merge;
- same-key base/delta ordering;
- String, Integer, Float, Boolean, Ref, Keyword, and Null;
- negative and maximal valid/transaction times;
- scoped and unscoped retraction;
- `:as-of`, `:valid-at`, and any-valid-time;
- single and multiple values per entity/attribute;
- bounded resume/yield with no duplicate or missing row;
- publication change during a long read;
- malformed bytes, UTF-8, varints, lengths, slots, restarts, and checksums;
- crash before data sync, before publish, after publish, and before WAL retire;
- native/WASM result parity.

## Decision Rules

### Admit implementation when

- a clean receipt assigns at least 10% of end-to-end cost or a clear complexity
  slope to one production-owned phase;
- the proposed slice remains useful inside the larger architecture;
- success and failure both change the next decision;
- the public API and file format can remain stable, or a separate migration
  plan has been approved.

### Retain a durable slice when

- it improves structural ownership, corruption safety, or boundedness even if
  one performance threshold narrowly misses;
- it remains the correct boundary for the next optimization;
- point, RSS, semantic, and recovery gates remain green.

### Stop or revert when

- semantics, recovery, corruption handling, point latency, or RSS regress;
- improvement exists only in a synthetic kernel and disappears end to end;
- a supposedly bounded foreground path pays total-history work;
- a derived projection becomes a second authority;
- the change requires weakening durability or hiding incompatible workloads in
  one comparison row.

## Reference Repository Map

Use local clones as implementation references, not dependencies:

| Repository | Primary use |
|---|---|
| `/home/upopo/db-ref/turso` | incremental aggregate state, pager/VACUUM, resumable execution |
| `/home/upopo/db-ref/grafeo` | compact columnar base, mutable overlay, factorized/vector execution |
| `/home/upopo/db-ref/fjall` | sorted runs, compaction, restart policy, snapshot lifetime |
| `/home/upopo/db-ref/redb` | page-backed cursor, COW publication, allocator/cache invariants |
| `/home/upopo/db-ref/cozo` | Datalog join/reorder/magic/temp-relation execution |
| `/home/upopo/Raphtory` | temporal indexes and parallel temporal graph analysis |

The `/home/upopo/db-ref` redb and Fjall checkouts are newer than the duplicate
copies under this repository's untracked `reference/` directory. Grafeo is at
the same inspected commit in both locations.

## Immediate Next Slice

R0 and R1 are closed. The only active slice is R2-A temporal projection layout:

```text
carry exact valid-time intervals or boundary transitions in compact columns
measure moving-time reads, rebuild, update, bytes, latency, and RSS
assign no in-file root until temporal identity passes
```

R2-A is a durable risk probe inside the admitted in-file projection shape. It
must stay behind `bench-internals`, preserve ledger authority, and avoid public
API or persisted-byte changes. If exact temporal state exceeds the size or
latency gate, stop R2 and proceed to R3 rather than persisting the fixed-time R1
snapshot. R3-R6 remain parked behind their respective evidence gates.
