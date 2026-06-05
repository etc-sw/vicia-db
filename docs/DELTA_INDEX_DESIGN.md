# Delta Index Design and Test Spec

Branch: `vetch/minigraf-refactor-plan`

Status: living design and test specification. T0-T9C-C guardrails are implemented on this branch; T8A replaces accumulated single-segment replacement with multi-segment manifest append, T8B confirms the mini accumulation gate, T8C routes long-tail segment growth to T9 thresholds, T9A documents the threshold policy, T9B implements private threshold decisions, T9C-A adds a private explicit recompact primitive, T9C-B makes recompact publish copy-on-write, and T9C-C adds a private idle/background maintenance caller.

Roadmap: see `docs/VETCH_DELTA_STORAGE_ROADMAP.md` for the post-T7C execution plan and gate sequence.

## Decision

Minigraf should add an append-friendly delta index layer inside the `.graph` file, then merge it with the current checkpointed base indexes at read time. The first implementation must keep the current full checkpoint/rebuild path as repair and `recompact()` fallback.

The design target is:

- Small append flush cost scales with pending/delta size, not committed graph size.
- Full-history identity survives across base and delta.
- Current-view queries remain equivalent to the existing full-rebuild path.
- Crash recovery never publishes a root or manifest that points to incomplete pages.
- Vetch can push expensive recompact work outside the interactive agent rhythm.

## Non-goals

- No external database backend.
- No sidecar index file as the primary design.
- No immediate incremental mutation of existing B+tree pages.
- No vector, BM25, HNSW, hybrid search, columnar, or CSR storage work in Minigraf core.
- No removal of the current WAL before delta crash semantics are proven.

## Current Storage Boundary

The current v10 file has page 0 with the legacy 84-byte header plus the delta manifest extension area. The base header still carries four committed index roots and checksums. `PersistentFactStorage::save()` can currently take three internal paths:

1. No-op when there is no dirty state.
2. Delta segment publish when a clean v10 base has pending facts and no visible delta manifest.
3. Multi-segment delta publish when a visible delta has new pending facts: the new segment contains only the pending facts, and the newly published manifest preserves previous segment descriptors plus the appended segment.
4. Full rebuild from base-plus-pending when the delta path is not available, and copy-on-write recompact from base-plus-delta facts when visible delta maintenance or repair is required.

Every durable path still follows the same discipline:

1. Reads the current header.
2. Writes and syncs fact/index/manifest pages before publication.
3. Writes page 0 as the commit point.
4. Wires `CommittedFactReader` and `CommittedIndexReader`.
5. Clears pending facts and allows WAL retire only after the durable publish is known.

This design preserves that discipline. It changes what a checkpoint/flush writes, not the fact identity model.

## Format Direction

Use a v10 format extension in page 0. The existing 84-byte v9 header remains the base section. v10 adds a double-buffered delta manifest pointer area in the unused space of page 0.

The v10 header extension should contain:

| Field | Purpose |
| --- | --- |
| `delta_manifest_slot0_generation` / `slot1_generation` | Monotonic manifest generation. Reopen chooses the highest valid generation. `0` means the slot is empty. |
| `delta_manifest_slot0_page` / `slot1_page` | Manifest payload start page for each slot. |
| `delta_manifest_slot0_page_count` / `slot1_page_count` | Number of pages occupied by the manifest payload. |
| `delta_manifest_slot0_len` / `slot1_len` | Number of bytes in the manifest payload. |
| `delta_manifest_slot0_checksum` / `slot1_checksum` | Checksum of the manifest payload. |
| `delta_manifest_slot0_descriptor_checksum` / `slot1_descriptor_checksum` | Checksum of the slot descriptor fields. A corrupt slot is ignored if the alternate slot is valid. |
| `base_fact_page_start` | First packed fact page of the currently published base. Defaults to page `1` for older v10 extension tails. Copy-on-write recompact publishes a later start page. |
| `base_layout_checksum` | Checksum over `base_fact_page_start`, so a corrupt base-start pointer rejects reopen instead of silently reading page `1`. |

Publish rule:

1. Write and sync all new fact pages, delta pages, and manifest payload pages. FileBackend append writes update only the in-memory page count; disk page 0 is not modified here.
2. Fill the inactive manifest slot with generation, page range, length, payload checksum, and descriptor checksum.
3. Write and sync page 0 as the publish point.
4. Only after this succeeds, wire readers and retire WAL entries covered by the checkpoint.

Recovery rule:

- Evaluate both header slots independently.
- For each descriptor-valid slot, verify the manifest payload checksum and decode the manifest.
- For each manifest-valid slot, verify every referenced delta segment before making it visible.
- Use the highest generation that passes all of those checks.
- If a newer slot, manifest payload, or delta segment is corrupt but an older slot passes all checks, fall back to the older slot.
- If no usable slot remains for a committed delta state, report file corruption. Do not silently open base-only.
- Ignore corrupt trailing delta/manifest pages that were never published by page 0.

This borrows redb's root-publish discipline without importing redb's allocator or MVCC page model.

## Delta Manifest

A manifest is an ordered list of delta segments visible on top of one base checkpoint.

Manifest fields:

| Field | Purpose |
| --- | --- |
| `format_version` | Manifest payload format. Starts at `1` under Minigraf file format v10. |
| `base_checkpoint_tx_count` | `last_checkpointed_tx_count` of the base the manifest overlays. |
| `base_fact_page_count` | Fact pages in the base view used to create segment `FactRef`s. |
| `base_eavt_root`, `base_aevt_root`, `base_avet_root`, `base_vaet_root` | Roots that deltas overlay. These must match the page 0 base roots unless the manifest represents an in-progress recompact candidate. |
| `segments` | Sorted by `low_tx_count`, then `segment_id`. |
| `manifest_checksum` | Checksum of the manifest payload with this field zeroed. |

Manifest invariants:

- Segments are ordered and non-overlapping by committed write `tx_count`.
- Segment `low_tx_count` must be greater than the base checkpoint `tx_count`.
- Manifest `high_tx_count` equals the max segment high tx.
- Removing a segment is allowed only after a new base root has been published that covers its high tx.

## Delta Segment

A delta segment stores sorted index entries for facts newer than the base checkpoint. It is the durable equivalent of Grafeo's mutable overlay, but fact-oriented rather than object-oriented.

Segment metadata:

| Field | Purpose |
| --- | --- |
| `segment_id` | Monotonic id within the file. |
| `base_checkpoint_tx_count` | Base checkpoint this segment overlays. |
| `low_tx_count`, `high_tx_count` | Inclusive tx range covered by this segment. |
| `fact_page_start`, `fact_page_count` | Packed fact pages appended for this segment. |
| `eavt_page_start`, `aevt_page_start`, `avet_page_start`, `vaet_page_start` | Sorted delta index entry regions. |
| `*_entry_count` | Entry count per covering index. |
| `*_min_key`, `*_max_key` | Segment-level skip bounds. |
| `segment_checksum` | Checksum over metadata plus all referenced fact and index pages. |
| `commit_marker` | Fixed marker at the end of the segment payload. Missing marker means incomplete segment. |

Fact identity:

All delta index entries preserve the existing full-history key identity:

`entity`, `attribute`, encoded `value`, `valid_from`, `valid_to`, `tx_count`, `tx_id`, `asserted`.

VAET contains only `Value::Ref` facts and preserves `tx_id` plus `asserted`, so ref-like edges cannot collapse across assert/retract or same-E/A/V writes.

## Reader Semantics

The first code slice should be a non-persistent `LayeredIndexReader`.

Inputs:

- A base `CommittedIndexReader`.
- Zero or more in-memory sorted delta entry sets.

Behavior:

- Each range scan returns base `FactRef`s plus matching delta `FactRef`s in index order.
- Delta-only facts are visible even when no matching base fact exists.
- Tombstone/retraction facts are returned for full-history surfaces; current-view filtering remains the query executor's net-asserted projection responsibility.
- Candidate deduplication must use full-history identity, not just E/A/V.
- The reader may return a `Vec<FactRef>` initially because the current trait returns vectors. A later streaming trait can be designed only after correctness is locked.

The first reader implementation proved merge semantics against the existing index key types and test fixtures. The current branch writes v10 files with one or more visible delta segments. A checkpoint over a visible delta appends only the pending facts as a new segment and publishes a manifest list through the inactive slot; it does not rewrite previously selected delta facts.

## Flush Semantics

After `wal_write_stamped_batch` applies facts to `FactStorage`, checkpoint/flush can choose one of two paths:

1. **Delta flush**: write only pending facts plus sorted delta index entries, publish manifest, keep WAL entries until the manifest is durable.
2. **Full rebuild**: current `save()` behavior, used for recompact, repair, migration, or when delta thresholds are exceeded.

Initial policy:

- Keep public `checkpoint()` as the API name.
- Internally prefer delta flush when the existing file is v10-capable and the delta segment count/bytes are below thresholds.
- Fall back to full rebuild if manifest validation fails, if there are too many segments, or if a format upgrade is required.

T7C policy update for Vetch:

- Durable append/receipt stays immediate through the WAL path.
- Segment checkpoint/flush may be batched by receipt or slice, but it must not rewrite all accumulated delta facts on every checkpoint.
- Recompact should be idle/background/scheduled maintenance.
- Full rebuild is import/maintenance only and must not be a foreground path in normal Vetch work.
- The next storage slice should replace single-segment replacement with a multi-segment manifest. Internal/background recompact thresholds still matter, but they do not remove the need for append-only segment publish.
- Agent-brief read latency must be measured separately from publish latency. T7C shows immediate current queries stay sub-millisecond, while as-of/replay queries over a 1M base stay around seconds and need a separate read/query-path improvement.

T8A implementation update:

- A visible-delta checkpoint now appends one new segment for pending facts and publishes a manifest containing all selected previous segment descriptors plus the new descriptor.
- Reopen loads every segment referenced by the selected manifest before wiring `LayeredFactLoaderImpl` and `LayeredIndexReader`.
- Manifest validation rejects out-of-order segments, overlapping tx ranges, and overlapping page ranges.
- Corrupt newest segment/manifest/slot still falls back to the previous valid slot; corrupt older segment referenced by the selected multi-segment manifest makes that selected manifest invalid.

### Checkpoint Outcome And WAL Retire Policy

`PersistentFactStorage::save()` reports an internal `CheckpointOutcome`:

| Outcome | Durable publish evidence | WAL retire allowed |
| --- | --- | --- |
| `Noop` | No page 0 publish happened. | No |
| `FullRebuild` | Base fact pages, indexes, checksum, and page 0 were synced. | Yes |
| `FullRebuildFromVisibleDelta` | The visible base-plus-delta view was folded into a fresh base and page 0 was synced. | Yes |
| `DeltaSegment` | Delta segment pages, manifest pages, v10 header extension, and page 0 were synced. | Yes |

`Minigraf::checkpoint()` must not delete the WAL if replayed or newly written WAL entries remain and `save()` returns `Noop`. In normal operation `force_dirty()` prevents that path during WAL replay, but the guard is deliberate: WAL retire is allowed only when the storage layer reports a durable publish boundary, not merely `Ok(())`.

T9A threshold policy for the first implementation:

| Metric | Soft threshold | Hard threshold |
| --- | ---:| ---:|
| Visible delta segments | `1,024` | `4,096` |
| Visible delta page growth | `16,384` pages (`64 MiB`) | `65,536` pages (`256 MiB`) |
| Delta/base page ratio | `0.10` | `0.25` |
| Delta/base fact ratio | `0.10` | `0.25` |

Soft threshold returns `ScheduleBackgroundRecompact`; hard threshold returns
`MaintenanceBackpressure`. Neither decision may run a foreground full rebuild
inside normal `checkpoint()`. The fact-ratio threshold is a secondary broad
import signal; T8C shows tiny receipt cadence must be bounded primarily by
segment count and page/file growth.

Ratio thresholds are guarded by absolute floors. Page ratio applies only after
at least `1,024` visible delta pages (`4 MiB`), and fact ratio applies only when
exact base/delta fact counts are available with at least `1,000` visible delta
facts. The v10 manifest descriptors store page ranges and tx ranges, not exact
fact counts, so T9B's manifest-derived metric must ignore fact-ratio checks
unless an internal caller supplies exact counts.

## Recompact Semantics

`recompact()` is a full rebuild from base plus all visible delta segments.

Rules:

- Build the new base from the current visible view: base fact pages plus delta fact pages in tx order.
- Preserve full-history rows, including retractions.
- Build new EAVT, AEVT, AVET, and VAET B+trees.
- Write the new base fact and index pages after the currently published image, and sync them before publishing page 0.
- Publish a new base header with no delta manifest, or with a fresh empty manifest.
- Record the new base fact page start in the v10 header extension.
- Retire covered delta segments only after the new base header is durable.
- T9C-B result: `recompact_visible_delta()` now writes a copy-on-write base
  candidate first and changes visibility only when page 0 is published. The
  older in-place full-rebuild helper remains private fallback/test support and
  is not the recompact publish path.

This keeps the current full-rebuild implementation as the safety net.

## Crash Matrix

| Crash point | Expected recovery |
| --- | --- |
| WAL append fails before facts are applied | Existing behavior: no facts are visible in-process and no delta is written. |
| Facts applied in memory, before delta flush | WAL replay restores facts on reopen. |
| Delta fact pages partially written | Manifest is not published; ignore partial pages. |
| Delta index pages partially written | Manifest is not published; ignore partial pages. |
| Manifest payload written but header slot not flipped | Previous manifest remains visible; WAL may replay uncheckpointed facts. |
| Header primary slot flipped but new manifest corrupt | Fall back to a secondary valid manifest if it covers the durable view; otherwise replay WAL or report corruption. |
| Recompact pages written but header not published | Previous base plus manifest remains visible. |
| Recompact header published but delta retire interrupted | New base is visible; old delta pages are garbage but ignored by manifest. |

State selection summary:

| Disk state on reopen | Expected behavior |
| --- | --- |
| Extra unpublished bytes after page 0 | Ignore them; they are outside the selected base or manifest. |
| Selected manifest slot is valid | Load base plus selected manifest segments. |
| Selected manifest slot is corrupt but alternate slot is valid | Fall back to the alternate valid slot. |
| Selected manifest references truncated or out-of-bounds pages | Reject that manifest; use alternate valid slot or WAL repair/error policy. |
| No valid manifest and WAL covers newer txs | Replay WAL and force a durable checkpoint. |
| No valid manifest and WAL was already retired past base | Report corruption rather than silently dropping delta-covered writes. |

## Test Spec

### T0: Existing Guardrails Stay Green

Run before and after every implementation slice:

- `cargo test --test fact_log_export_test`
- `cargo test --test multivalue_index_test`
- `cargo test --test retract_valid_time_test`
- `cargo test --lib storage::index`

Purpose: ensure full-history identity, `Value::Ref`, `tx_id`, and `asserted` invariants stay fixed.

### T1: Non-persistent Layered Reader

New test module: `src/storage/delta_index.rs` or `tests/delta_index_reader_test.rs`.

Cases:

- `delta_only_fact_visible_in_eavt_aevt_avet`
- `delta_only_ref_edge_visible_in_vaet`
- `base_and_delta_ref_edge_range_scan_merges_both`
- `same_ref_eav_different_tx_id_not_collapsed`
- `same_ref_eav_assert_and_retract_not_collapsed`
- `range_scan_respects_start_end_across_base_and_delta`
- `empty_delta_delegates_to_base_reader`

This is the first implementation gate. It requires no file-format change.

### T2: Segment Codec

New test module: `tests/delta_index_segment_test.rs` after codec implementation.

Cases:

- segment round-trip preserves all index counts and tx range
- missing commit marker rejects segment
- checksum mismatch rejects segment
- out-of-bounds page reference rejects manifest
- segment min/max keys skip irrelevant range
- VAET segment contains only `Value::Ref` rows

### T3: Manifest Recovery

New test module: `tests/delta_manifest_recovery_test.rs`.

Cases:

- primary valid and newer wins
- primary corrupt falls back to secondary
- secondary newer but corrupt is ignored
- both corrupt requires repair/full rebuild path
- header extension checksum mismatch rejects extension
- manifest high tx lower than segment high tx rejects manifest

### T4: Public API Integration

New test module: `tests/delta_checkpoint_integration_test.rs`.

Cases:

- checkpoint after one pending write on checkpointed base does not rebuild all base index pages
- reopen after delta checkpoint sees delta-only fact
- reopen after delta checkpoint sees base-to-delta `Value::Ref` edge
- second delta checkpoint appends only pending segment pages instead of rewriting accumulated delta facts
- reopen after two delta checkpoints sees segment-to-segment `Value::Ref` edge
- later delta segment retraction hides earlier delta segment assertion in current view
- `export_fact_log()` includes base and multiple delta records in deterministic tx order
- current-view query matches full-rebuild checkpoint result
- retraction in delta hides base assertion in current-view query but remains in export log
- recompact removes visible delta manifest and preserves results

### T5: Crash Simulation

New test module: `tests/delta_checkpoint_crash_recovery_test.rs`.

Cases:

- corrupt partial delta pages before manifest publish
- corrupt manifest payload after write but before slot flip
- corrupt primary manifest after slot flip
- interrupt recompact before header publish
- interrupt delta retire after recompact header publish

Use deterministic file mutation helpers. Do not use debug formatting of `Result`, `Fact`, `Value`, or `Uuid` in assertion messages.

T5B adds the recovery-policy surface:

- clean save returns `CheckpointOutcome::Noop`
- first base checkpoint returns `CheckpointOutcome::FullRebuild`
- small append on a clean base returns `CheckpointOutcome::DeltaSegment`
- pending facts on a visible delta return `CheckpointOutcome::DeltaSegment` by appending a new segment and publishing the expanded manifest through the inactive manifest slot
- `checkpoint()` treats `Noop` as insufficient evidence for deleting a non-empty WAL

### T6: Benchmark Gate

Extend `tests/checkpoint_rebuild_benchmark.rs` or add an ignored delta benchmark:

- committed base: 10K, 100K, 1M
- pending facts: 1, 10, 100, 1K
- include `Value::Ref` and retractions
- measure first delta flush wall time, reopen recovery time, second rotated delta flush wall time, and final file page count

Acceptance:

- 1 pending fact on a 1M base must not perform an O(1M) committed-index rebuild during delta flush.
- Delta flush should scale primarily with pending fact count and segment metadata size.
- Full rebuild/recompact may remain O(total facts), because it is explicitly scheduled outside Vetch's interactive work rhythm.

Current T7A result, recorded in `docs/BENCHMARKS.md`: v10 single-segment delta plus scoped checksum validation reduces the 1M base plus one pending fact checkpoint from the R2 full-rebuild baseline of 4,829.691 ms and the T6 delta baseline of 512.109 ms to 5.266 ms. Reopen of that selected delta view drops from 307.388 ms to 0.114 ms. Delta publish/reopen now validates base identity plus new delta segment/manifest bytes instead of checksumming all committed pages. Full rebuild, repair, and recompact continue to use full-file checksums.

Current T7B result, recorded in `docs/BENCHMARKS.md`: v10 now uses both manifest slots as the real publish boundary. A second small write over a visible delta publishes a replacement single-segment delta through the inactive slot instead of forcing an interactive full rebuild. The 1M base plus one pending fact gate remains pending-sized at 3.336 ms for first delta flush and 0.088 ms for reopen; the second rotated delta flush is 2.852 ms.

Current T7C result, recorded in `docs/BENCHMARKS.md`: single-segment replacement fails the accumulated Vetch receipt gate. With a 1M base and 1 fact per checkpoint, 1K accumulated delta facts have flush p95 102.385 ms, above the 50 ms target. At 10K accumulated delta facts, flush p95 is 1,051.300 ms and file growth is 18.9 GB. Batching reduces file growth but not the hot flush problem: 10 facts x 1K checkpoints and 100 facts x 100 checkpoints both end near 1 second p95 at 10K accumulated delta facts. Reopen remains under 30 ms p95 and immediate current-query reads remain sub-millisecond, but as-of/replay queries remain about 1.75-2.72 s p95 and must be treated as a separate agent-brief read-path blocker.

Current T8B result, recorded in `docs/BENCHMARKS.md`: multi-segment append passes the mini accumulation gate. With a 1M base and 1 fact per checkpoint, 1K accumulated delta facts have flush p95 11.679 ms, max 15.874 ms, reopen p95 6.290 ms, file growth 12,234,752 B, segment count 1,000, and corrupt-latest fallback remains true. With 10 facts x 100 checkpoints, flush p95 is 6.882 ms, reopen p95 is 2.644 ms, file growth is 1,228,800 B, segment count is 100, and fallback remains true. Immediate current-query reads remain sub-millisecond. As-of/replay queries still take about 1.45 s p95 and remain a separate Q1 read-path lane.

Current T8C result, recorded in `docs/BENCHMARKS.md`: multi-segment append is the right default path but needs T9 thresholds for unbounded tiny-segment growth. The 1M base plus 1 fact x 10K case drops from T7C's 1,051.300 ms flush p95 and 18.9 GB file growth to 99.818 ms p95 and 662,257,664 B growth, but that is still above the hot flush target. The batching rows show the dominant pressure is segment count and manifest/file growth rather than delta fact count alone: 10K delta facts in 1K segments have flush p95 36.821 ms, and 10K facts in 100 segments have flush p95 38.347 ms. Current reads stay sub-millisecond, reopen stays below the 250-500 ms gate, corrupt-latest fallback remains true, and as-of/replay remains Q1 read-path work.

Current T9A/T9B/T9C policy: keep multi-segment publish as the default delta checkpoint path, but classify visible delta growth with a private decision surface. Healthy growth returns `ContinueDeltaAppend`; soft threshold growth returns `ScheduleBackgroundRecompact`; hard threshold growth returns `MaintenanceBackpressure`. T9B implements the pure/private metrics and decision tests. T9C-A adds an explicit private recompact primitive, T9C-B gives that primitive a copy-on-write publish path, and T9C-C adds `run_idle_delta_maintenance()` as the private idle/background caller. Threshold-triggered execution is still not wired into foreground `checkpoint()` and no public `recompact()` API exists; Vetch must schedule the idle caller after pending receipt writes are durably checkpointed.

## Implementation Order

1. Add `LayeredIndexReader` and in-memory delta entry fixtures.
2. Add tests T1.
3. Add segment metadata and codec structs behind private storage module APIs.
4. Add tests T2.
5. Add manifest structs and recovery selection logic in memory.
6. Add tests T3.
7. Implement v10 header extension read/write.
8. Wire first delta flush into `checkpoint()` with full rebuild fallback.
9. Add T4/T5 integration and crash tests.
10. Scope selected-delta checksum validation to base identity plus delta bytes.
11. Use double-buffered manifest slots as the publish boundary for replacement single-segment deltas. Done in T7B; retained as fallback evidence for T8A.
12. Re-run T6/T7 benchmark gates and update `docs/BENCHMARKS.md`.
13. Implement multi-segment manifest publish so small checkpoints append one new segment instead of rewriting all accumulated delta facts. Done in T8A.
14. Run the T8B mini benchmark and T8C full accumulation matrix. Done; proceed to T9 thresholds.
15. Document internal/background recompact thresholds for segment count, delta bytes, and long-term file growth. Done in T9A.
16. Implement private threshold metrics and decision tests. Done in T9B.
17. Add a private explicit recompact primitive. Done in T9C-A.
18. Add crash-safe recompact publish before any threshold-triggered internal/background scheduling. Done in T9C-B with a v10 `base_fact_page_start` extension field and copy-on-write base publish.
19. Add a private idle/background maintenance caller that runs recompact only for scheduled/backpressure decisions and keeps foreground checkpoint on the delta append path. Done in T9C-C.
20. Add a separate read-path gate for Vetch agent briefs, especially as-of/replay query latency after receipt writes.

## Open Questions

- Should `CommittedIndexReader` grow a streaming range-scan trait before persistent delta lands, or should the first implementation keep `Vec<FactRef>` to reduce blast radius?
- Should `export_fact_log()` read through the same base-plus-delta manifest, or should it keep a dedicated fact-log stream path?
- Is a sync-data mode enough for delta segment publish on all supported platforms, or should v10 use full sync for the first release?
- Should `recompact()` become public later, or stay internal until Vetch has a real scheduling caller?
- Which query executor path should make as-of receipt reads cheap enough for Vetch agent briefs: tighter index pushdown, a fact-log replay reader, or a prepared current/as-of receipt API?
