# Vicia Performance TODO

Temporary checklist based on the 1M reference DB benchmark.

## P0 — Aggregate memory

- [x] Attribute allocation sources: the dominant costs were the attribute-sized `Vec<Fact>` snapshot and row bindings before the sink.
- [ ] Add counters for allocation count/bytes, cloned facts, result rows, and peak reducer state.
- [x] Reduce 1M Integer aggregate RSS delta from 381 MiB to 128 MiB, then 64 MiB. Current 20-run delta: 1.25 MiB.
- [ ] Verify unrelated pending attributes do not increase selected-attribute memory.

## P1 — Aggregate latency

- [x] Remove intermediate binding and current-view materialization.
- [x] Feed typed entity/value views directly from the cursor into aggregate sinks.
- [x] Reduce 1M aggregate p50 from 1.63 s to 800 ms, then 400 ms. Current p50: about 343 ms.
- [x] Keep p95 within 15% of p50. Current elapsed p95: about 356 ms.

## P2 — Retained memory

- [ ] Compare retained RSS after 1 and 20 repeated aggregates.
- [ ] Distinguish live session/cache state from allocator retention or leaks.
- [x] Reduce retained heap from 79 MiB to 32 MiB, then 16 MiB. Current retained RSS delta: 1.25 MiB.

## P3 — Storage and maintenance

- [ ] Attribute bytes to fact pages and each EAVT/AEVT/AVET/VAET index.
- [ ] Record B-tree fill ratio and repeated attribute/entity encoding cost.
- [ ] Reduce the 1M fixture from 338 MiB without changing the public API or v11 format.
- [ ] Measure `1M base + 1/10/100/1K pending` checkpoint latency and peak RSS.

## Regression gates

- [x] Preserve 1M point-read latency near the current 0.24 ms baseline. Current final run: 0.277 ms (+14.8%, inside the 20% regression gate).
- [x] Preserve open baseline RSS near 12 MiB. Current baseline: 12.29 MiB.
- [x] Require exact count `1,000,000` and checksum `499999500000` for every run.
- [x] Keep engine aggregate and KV owned-scan results in separate comparison groups. The v3 summarizer separates `engineAggregate` from `ownedResultScan` and treats redb/Fjall as storage floors.

## Next task

- Verify that growing unrelated pending attributes does not increase selected-attribute aggregate snapshot memory. Add a controlled `1M committed selected attribute + 0/10K/100K/1M unrelated pending facts` workload, record cursor snapshot bytes and peak reducer state, and require selected count/checksum plus snapshot memory to remain constant within allocator-page noise.
