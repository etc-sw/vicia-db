# Vicia Performance TODO

Temporary checklist based on the 1M reference DB benchmark.

## P0 — Aggregate memory

- [x] Attribute allocation sources: the dominant costs were the attribute-sized `Vec<Fact>` snapshot and row bindings before the sink.
- [x] Add cursor counters for selected pending entries/bytes, committed/pending visits, exact fact resolutions, emitted rows, peak entity values/windows, and yield/resume count.
- [x] Reduce 1M Integer aggregate RSS delta from 381 MiB to 128 MiB, then 64 MiB. Current production-path 20-run delta: 1.375 MiB.
- [x] Verify unrelated pending attributes do not increase selected-attribute memory. The clean `vicia.pending-isolation.v3` full receipt keeps selected pending entries/bytes and pending visits at zero through 1M unrelated WAL facts; query RSS delta stays within 0.5 MiB of zero-pending.

## P1 — Aggregate latency

- [x] Remove intermediate binding and current-view materialization.
- [x] Feed typed entity/value views directly from the cursor into aggregate sinks.
- [x] Reduce 1M aggregate p50 from 1.63 s to 800 ms, then 400 ms. Current production-path p50: 330.322 ms.
- [x] Keep p95 within 15% of p50. Current production-path p95: 357.688 ms.

## P2 — Retained memory

- [ ] Compare retained RSS after 1 and 20 repeated aggregates.
- [ ] Distinguish live session/cache state from allocator retention or leaks.
- [x] Reduce retained heap from 79 MiB to 32 MiB, then 16 MiB. Current retained RSS delta: 1.375 MiB.

## P3 — Storage and maintenance

- [x] Decompose and reduce WAL-backed pending open RSS. The canonical overlay owns each fact/value once and indexes `PendingFactId` runs; at 1M Integer facts the clean v3 full receipt measures 221.445 MiB live RSS and 171.842 MiB accounted payload. Sequential WAL replay peaks at one 1K-fact batch and leaves only 0.285 MiB retained RSS.
- [x] Attribute bytes to fact pages and each EAVT/AEVT/AVET/VAET index. The clean `vicia.storage-layout.v1` full receipt accounts for every published v11 page; at production fill 75 the 1M fixture is 61.875 MiB facts, 96.551 MiB EAVT, 96.551 MiB AEVT, 97.410 MiB AVET, and 0.004 MiB VAET.
- [x] Record B-tree fill ratio and repeated attribute/entity encoding cost. The receipt retains exact payload/structural/unused bytes and conservative restart-10/16 prefix estimates for every index and fill candidate.
- [x] Reduce the 1M fixture from 338 MiB without changing the public API or v11 format. Reference-sorted checkpoint construction made fill 90 pass every v2 gate and reduced the fixture to 301.363 MiB.
- [x] Correct the fill-selection evidence contract. The clean `vicia.storage-layout.v2` full receipt uses rotated fresh children, nearest-rank p95 with separate max/MAD, receipt-owned gates, and mutation-audited validation; no fill candidate passed every gate, so production remains at 75.
- [x] Bound initial checkpoint serialized-index ownership. Reusing the lazy B-tree serializer reduces fill-75 checkpoint p50 from 5,032.714 to 4,505.694 ms and median peak RSS delta from 947.625 to 744.750 MiB without changing v11 bytes; phase timing attributes remaining tail to pending index sort and EAVT/AEVT/AVET builds rather than sync.
- [x] Bound pending index sort ownership and stabilize high-fill construction. One reusable 1M-entry fact-position buffer plus one canonical value encoding replaces four owned typed-key vectors; fill-75 median peak RSS falls from 744.750 to 281.250 MiB. The clean full receipt selects fill 90 with a 301.363 MiB image and a 5,013.869/5,717.048 ms checkpoint p50/p95.
- [x] Measure `1M base + 1/10/100/1K pending` checkpoint latency and peak RSS. The clean `vicia.checkpoint-construction.v2` receipt records 20 interleaved fresh samples per variant; checkpoint p95 is 2.630/3.023/3.370/9.864 ms and HWM-backed recompact RSS delta stays at 177.000–177.875 MiB.
- [ ] Roll out file format v12 adaptive prefix leaves. The implementation keeps raw leaves when compression loses, uses restart-16 prefix leaves for repeated AEVT/AVET keys, reads v11 without rewriting it, preserves v11 foreground delta checkpoints, and upgrades only through idle COW maintenance. Borrowed initial-build keys, uniform-attribute EAVT/AEVT order reuse, and leaf-first-key-only separator serialization reduce clean fill-90 checkpoint p50/p95 from 5,079.803/6,127.321 ms to 3,633.534/3,963.193 ms; the 109.07% tail now passes the 115% gate while the image remains 269.586 MiB and exact count/checksum remain 1,000,000/499,999,500,000. Rollout remains open because a concurrent Vetch TypeGPU QA run overlapped the query phase and the same receipt missed the point/aggregate tail gates; no candidate was selected.

## Regression gates

- [x] Preserve 1M point-read latency with repeated evidence. The clean v4 full run records 20 post-warmup samples: p50 `0.011 ms`, p95/max `0.019 ms` (raw samples retained in the Vicia receipt).
- [x] Preserve open baseline RSS near 12 MiB. Current baseline: 12.164 MiB.
- [x] Require exact count `1,000,000` and checksum `499999500000` for every run.
- [x] Keep engine aggregate and KV owned-scan results in separate comparison groups. The v4 summary has no flat `rows`; JSON stores distinct `groups.engineAggregate` and `groups.ownedResultScan` arrays, and Markdown emits separate workload and memory tables.

## Next task

- Re-establish one uncontended full receipt whose point and aggregate tails pass alongside the now-passing fill-90 checkpoint gate; do not select a retry that overlaps the active Vetch TypeGPU QA process. Then run the real-browser WASM suite with a compatible `CHROMEDRIVER` and replace Vetch's complete browser package, never only the `.wasm` binary.
