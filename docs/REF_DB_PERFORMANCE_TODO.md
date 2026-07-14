# Vicia Performance TODO

Temporary checklist based on the 1M reference DB benchmark.
The whole-shape direction, ordering, and admission rules live in
[`PERFORMANCE_ROADMAP.md`](PERFORMANCE_ROADMAP.md); this file remains the
current executable checklist.

## P0 — Aggregate memory

- [x] Attribute allocation sources: the dominant costs were the attribute-sized `Vec<Fact>` snapshot and row bindings before the sink.
- [x] Add cursor counters for selected pending entries/bytes, committed/pending visits, exact fact resolutions, emitted rows, peak entity values/windows, and yield/resume count.
- [x] Reduce 1M Integer aggregate RSS delta from 381 MiB to 128 MiB, then 64 MiB. Current clean 20-run retained delta: 1.125 MiB.
- [x] Verify unrelated pending attributes do not increase selected-attribute memory. The clean `vicia.pending-isolation.v3` full receipt keeps selected pending entries/bytes and pending visits at zero through 1M unrelated WAL facts; query RSS delta stays within 0.5 MiB of zero-pending.

## P1 — Aggregate latency

- [x] Remove intermediate binding and current-view materialization.
- [x] Feed typed entity/value views directly from the cursor into aggregate sinks.
- [x] Reduce 1M aggregate p50 from 1.63 s to 800 ms, then 400 ms. Current production-path p50: 330.322 ms.
- [x] Keep p95 within 15% of p50. Current production-path p95: 357.688 ms.

## P2 — Retained memory

- [x] Compare retained RSS after 1 and 20 repeated aggregates. The clean five-pair `vicia.aggregate-retention.v1` receipt records the same 1.125 MiB post-trim median at both endpoints, zero median growth, and no positive first-five/last-five RSS trend.
- [x] Distinguish live session/cache state from allocator retention or leaks. Fresh children record open, per-query, pre-trim, live-trim, and post-drop RSS plus smaps ownership. Twenty runs retain 1.078–1.227 MiB of live database state, `malloc_trim(0)` exposes no additional repeated-query growth, and drop/trim leaves no work-proportional residue.
- [x] Reduce retained heap from 79 MiB to 32 MiB, then 16 MiB. Current retained RSS delta: 1.125 MiB.

## P3 — Storage and maintenance

- [x] Decompose and reduce WAL-backed pending open RSS. The canonical overlay owns each fact/value once and indexes `PendingFactId` runs; at 1M Integer facts the clean v3 full receipt measures 221.445 MiB live RSS and 171.842 MiB accounted payload. Sequential WAL replay peaks at one 1K-fact batch and leaves only 0.285 MiB retained RSS.
- [x] Attribute bytes to fact pages and each EAVT/AEVT/AVET/VAET index. The clean `vicia.storage-layout.v1` full receipt accounts for every published v11 page; at production fill 75 the 1M fixture is 61.875 MiB facts, 96.551 MiB EAVT, 96.551 MiB AEVT, 97.410 MiB AVET, and 0.004 MiB VAET.
- [x] Record B-tree fill ratio and repeated attribute/entity encoding cost. The receipt retains exact payload/structural/unused bytes and conservative restart-10/16 prefix estimates for every index and fill candidate.
- [x] Reduce the 1M fixture from 338 MiB without changing the public API or v11 format. Reference-sorted checkpoint construction made fill 90 pass every v2 gate and reduced the fixture to 301.363 MiB.
- [x] Correct the fill-selection evidence contract. The clean `vicia.storage-layout.v2` full receipt uses rotated fresh children, nearest-rank p95 with separate max/MAD, receipt-owned gates, and mutation-audited validation; no fill candidate passed every gate, so production remains at 75.
- [x] Bound initial checkpoint serialized-index ownership. Reusing the lazy B-tree serializer reduces fill-75 checkpoint p50 from 5,032.714 to 4,505.694 ms and median peak RSS delta from 947.625 to 744.750 MiB without changing v11 bytes; phase timing attributes remaining tail to pending index sort and EAVT/AEVT/AVET builds rather than sync.
- [x] Bound pending index sort ownership and stabilize high-fill construction. One reusable 1M-entry fact-position buffer plus one canonical value encoding replaces four owned typed-key vectors; fill-75 median peak RSS falls from 744.750 to 281.250 MiB. The clean full receipt selects fill 90 with a 301.363 MiB image and a 5,013.869/5,717.048 ms checkpoint p50/p95.
- [x] Measure `1M base + 1/10/100/1K pending` checkpoint latency and peak RSS. The clean `vicia.checkpoint-construction.v2` receipt records 20 interleaved fresh samples per variant; checkpoint p95 is 2.630/3.023/3.370/9.864 ms and HWM-backed recompact RSS delta stays at 177.000–177.875 MiB.
- [ ] Roll out file format v12 adaptive prefix leaves. The implementation keeps raw leaves when compression loses, uses restart-16 prefix leaves for repeated AEVT/AVET keys, reads v11 without rewriting it, preserves v11 foreground delta checkpoints, and upgrades only through idle COW maintenance. Borrowed initial-build keys, uniform-attribute EAVT/AEVT order reuse, and leaf-first-key-only separator serialization reduce the final uncontended fill-90 checkpoint to 3,775.192/4,256.684 ms p50/p95; its 112.75% tail and the 111.99% aggregate tail pass while the image remains 269.586 MiB and exact count/checksum remain 1,000,000/499,999,500,000. Rollout remains open because the 0.0268/0.0571 ms point-read p50/p95 fails its gate; no candidate was selected.
- [x] Replace full-leaf read materialization with a restart-aware page-backed cursor. Raw leaves use slot-directory binary search; prefix leaves binary-search restart-16 records and reconstruct only the selected block before continuing one entry at a time. The clean 1M receipt reduces point batch p95 from 0.02050 to 0.01087 ms, keeps RSS delta unchanged at 1.125 MiB, and records zero full-leaf `Vec` entries/bytes. Aggregate p50 improves only 3.10%, from 432.492 to 419.073 ms, so this slice does not authorize v12 rollout.
- [x] Decode current-attribute AEVT entries as borrowed postcard projections. The clean 1M receipt emits exactly 1,000,000 projected entries, decodes zero owned `AevtKey`s in the projected stream, keeps all full-leaf materialization metrics at zero, and holds RSS delta to 1.250 MiB. Diagnostic projection decode time improves 25.27% (177.422 to 132.585 ms), but aggregate p50 improves only 1.28% (419.073 to 413.713 ms), below the 10%/230 ms gate. Point p95 is 0.01584 ms: below the absolute 0.050 ms limit but above the recorded 0.01087 ms cursor receipt even though the point probe never enters the projection path. Retain the durable projection; keep v12 rollout open.
- [x] Attribute current-read phases and repair the measured reducer. The diagnostic-only 1M probe assigns 22.84% of query time to `reduce_current_entry`; the accepted repair reuses one inline value/window state and promotes to the existing map only for multi-value entities. The clean same-fixture receipt reduces aggregate p50 from 355.045 to 282.403 ms (20.46%), keeps p95/p50 at 102.82%, improves point p95 from 0.01496 to 0.01363 ms, holds query RSS to 1.250 MiB, and retains exactly 1,000,000 projected entries with zero owned AEVT decode or full-leaf materialization. The clean storage-layout rerun and mutation audit pass structurally, but no high-fill candidate passes every rollout gate, so v12 and Vetch package rollout remain open.
- [x] Isolate the remaining v12 rollout variance. The reproducible `vicia.storage-layout-variance.v1` report maps checkpoint p95/max samples to phase medians and rotated order. Sync owns both observations for all four high-fill candidates without a fixed-position bias, so checkpoint construction is not admitted for another repair and durability sync remains unchanged. Point p50 exceeds fill 75 by 23.05%/32.63%/49.31% at fill 85/90/100, while fill 95 is a p95-only failure; the next risk probe is point-path density attribution.
- [x] Attribute point-path density cost. The clean `vicia.point-path-density.v1` full receipt keeps tree height, raw leaf codec, leaf comparisons/decodes, and cached fact resolution effectively fixed across fills. Internal separator comparisons grow from 35 at fill 75 to 61/67/75 at fill 85/90/100; internal descent median grows from 5.004 microseconds to 8.548/9.198/10.387 microseconds and correlates with point p50 at 0.991. This admits only an internal separator binary-search repair.
- [x] Binary-search internal separators. The repair preserves the first-separator-greater-than-key child rule and reduces full-receipt comparisons from 35/61/67/47/75 to 16/16/14/14/15 at fill 75/85/90/95/100. Point p50 becomes 0.00787/0.00776/0.00752/0.00739/0.00760 ms and candidate p95 is at most 0.01039 ms; all high fills pass the 20% relative gate with unchanged v12 bytes and exact result diagnostics.
- [x] Rerun the complete v12 acceptance matrix from the binary-search source. Every fill passes point, aggregate, and RSS gates. Fill 85 passes checkpoint but misses the size gate; size-valid fills 90/95/100 fail only the checkpoint-tail gate. The mutation-audited variance report attributes the common tail to sync without fixed-position bias and admits no implementation. `selectedFillPercent` remains `null`, production remains at fill 75, and the Vetch browser package remains unchanged.
- [x] Measure the exact fill frontier instead of rerunning the coarse matrix. The clean `vicia.storage-layout.v3` full receipt adds fill 86/87/88/89 under the same 20-run rotated contract and selects fill 87. Its 276.590 MiB image is 11.93% smaller than fill 75; checkpoint p50/p95 is 3,274.761/3,496.123 ms, point p95 is 0.01420 ms, aggregate p50/p95 is 283.743/304.362 ms, and query RSS p95 is 0.125 MiB. Receipt and variance mutation audits pass; this measurement leaves the current source default at fill 90 and does not replace the Vetch package.

## Regression gates

- [x] Preserve 1M point-read latency with repeated evidence. The clean v4 full run records 20 post-warmup samples: p50 `0.011 ms`, p95/max `0.019 ms` (raw samples retained in the Vicia receipt).
- [x] Preserve open baseline RSS near 12 MiB. Current baseline: 12.164 MiB.
- [x] Require exact count `1,000,000` and checksum `499999500000` for every run.
- [x] Keep engine aggregate and KV owned-scan results in separate comparison groups. The v4 summary has no flat `rows`; JSON stores distinct `groups.engineAggregate` and `groups.ownedResultScan` arrays, and Markdown emits separate workload and memory tables.
- [x] Promote the cross-engine receipt to v5 and add SQLite as the direct embedded SQL reference. Five clean 1M trials rotate all seven engines, close/reopen every freshly built database, adaptively batch hot/distributed/missing point reads, validate every result, retain host/source/durability provenance, and pass a 5% trial-MAD gate plus an eight-case mutation audit. Vicia records 0.00420/0.00459 ms hot point p50/p95, 171.455/177.240 ms aggregate p50/p95, and 1.625 MiB aggregate RSS delta; SQLite records 0.00942/0.01038 ms hot point and 29.530/32.769 ms aggregate.

## Next task

### Status: fill 87 is selected; production promotion is the next slice

- Promote the B-tree bulk-build default from the current fill 90 to the
  receipt-selected fill 87. This changes packing policy only; v12 bytes remain
  readable by the existing raw/prefix readers and require no migration.
- Run the full Rust suite, fmt, Clippy, WASM browser build, canonical receipt
  validation/mutation audits, and the real-Chrome suite after the production
  constant changes.
- Replace the complete Vetch browser package only after those gates pass. Sync
  JavaScript glue, typings, manifest, `.wasm`, and provenance together; do not
  replace the `.wasm` alone.
- Keep durability sync and the receipt-owned gates unchanged. The frontier
  receipt admits fill 87; it does not admit checkpoint construction work for
  the sync-owned tails of the other candidates.
