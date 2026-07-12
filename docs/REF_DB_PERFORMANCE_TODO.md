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
- [ ] Attribute bytes to fact pages and each EAVT/AEVT/AVET/VAET index.
- [ ] Record B-tree fill ratio and repeated attribute/entity encoding cost.
- [ ] Reduce the 1M fixture from 338 MiB without changing the public API or v11 format.
- [ ] Measure `1M base + 1/10/100/1K pending` checkpoint latency and peak RSS.

## Regression gates

- [x] Preserve 1M point-read latency with repeated evidence. The clean v4 full run records 20 post-warmup samples: p50 `0.011 ms`, p95/max `0.019 ms` (raw samples retained in the Vicia receipt).
- [x] Preserve open baseline RSS near 12 MiB. Current baseline: 12.164 MiB.
- [x] Require exact count `1,000,000` and checksum `499999500000` for every run.
- [x] Keep engine aggregate and KV owned-scan results in separate comparison groups. The v4 summary has no flat `rows`; JSON stores distinct `groups.engineAggregate` and `groups.ownedResultScan` arrays, and Markdown emits separate workload and memory tables.

## Next task

- Stream checkpoint construction from the canonical overlay so maintenance peak RSS no longer requires a full temporary `Vec<Fact>` plus complete in-memory index-entry sets.
