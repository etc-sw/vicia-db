# Vicia Performance TODO

Temporary checklist based on the 1M reference DB benchmark.

## P0 — Aggregate memory

- [x] Attribute allocation sources: the dominant costs were the attribute-sized `Vec<Fact>` snapshot and row bindings before the sink.
- [x] Add cursor counters for selected pending entries/bytes, committed/pending visits, exact fact resolutions, emitted rows, peak entity values/windows, and yield/resume count.
- [x] Reduce 1M Integer aggregate RSS delta from 381 MiB to 128 MiB, then 64 MiB. Current production-path 20-run delta: 1.375 MiB.
- [x] Verify unrelated pending attributes do not increase selected-attribute memory. The clean `vicia.pending-isolation.v2` full receipt keeps selected pending entries/bytes and pending visits at zero through 1M unrelated WAL facts; query RSS delta differs from zero-pending by exactly 1.000 MiB in the accepted run.

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

- [x] Decompose WAL-backed pending open RSS. At 1M Integer facts the live database is 1,152.316 MiB after allocator trim: 747.949 MiB directly accounted across `Vec<Fact>`, duplicate keys, EAVT/AEVT/AVET, and 9M owned small buffers; 404.367 MiB remains container/allocator residual. WAL batch replay adds 124.665 MiB of overlapping decoded ownership and leaves 139.617 MiB allocator-retained until trim.
- [ ] Attribute bytes to fact pages and each EAVT/AEVT/AVET/VAET index.
- [ ] Record B-tree fill ratio and repeated attribute/entity encoding cost.
- [ ] Reduce the 1M fixture from 338 MiB without changing the public API or v11 format.
- [ ] Measure `1M base + 1/10/100/1K pending` checkpoint latency and peak RSS.

## Regression gates

- [x] Preserve 1M point-read latency near the current 0.24 ms baseline. Current final run: 0.239 ms.
- [x] Preserve open baseline RSS near 12 MiB. Current baseline: 12.164 MiB.
- [x] Require exact count `1,000,000` and checksum `499999500000` for every run.
- [x] Keep engine aggregate and KV owned-scan results in separate comparison groups. The v3 summarizer separates `engineAggregate` from `ownedResultScan` and treats redb/Fjall as storage floors.

## Next task

- Design the pending overlay around one canonical fact/value owner plus lightweight index references. Treat streaming WAL replay as a separate bounded improvement: it can remove about 140 MiB of retained replay RSS at 1M, but it does not address the measured 1,152 MiB live shape. Preserve full ledger identity and current range semantics before changing ownership.
