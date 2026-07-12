# Vicia Performance TODO

Temporary checklist based on the 1M reference DB benchmark.

## P0 — Aggregate memory

- [ ] Attribute allocation sources: cloned facts, materialized bindings, snapshots, reducer state, and page buffers.
- [ ] Add counters for allocation count/bytes, cloned facts, result rows, and peak reducer state.
- [ ] Reduce 1M Integer aggregate RSS delta from 381 MiB to 128 MiB, then 64 MiB.
- [ ] Verify unrelated pending attributes do not increase selected-attribute memory.

## P1 — Aggregate latency

- [ ] Remove intermediate binding and current-view materialization.
- [ ] Feed typed entity/value batches directly from the cursor into aggregate sinks.
- [ ] Reduce 1M aggregate p50 from 1.63 s to 800 ms, then 400 ms.
- [ ] Keep p95 within 15% of p50.

## P2 — Retained memory

- [ ] Compare retained RSS after 1 and 20 repeated aggregates.
- [ ] Distinguish live session/cache state from allocator retention or leaks.
- [ ] Reduce retained heap from 79 MiB to 32 MiB, then 16 MiB.

## P3 — Storage and maintenance

- [ ] Attribute bytes to fact pages and each EAVT/AEVT/AVET/VAET index.
- [ ] Record B-tree fill ratio and repeated attribute/entity encoding cost.
- [ ] Reduce the 1M fixture from 338 MiB without changing the public API or v11 format.
- [ ] Measure `1M base + 1/10/100/1K pending` checkpoint latency and peak RSS.

## Regression gates

- [ ] Preserve 1M point-read latency near the current 0.24 ms baseline.
- [ ] Preserve open baseline RSS near 12 MiB.
- [ ] Require exact count `1,000,000` and checksum `499999500000` for every run.
- [ ] Keep engine aggregate and KV owned-scan results in separate comparison groups.
