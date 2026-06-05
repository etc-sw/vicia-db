# Delta Index Design and Test Spec

Branch: `vetch/minigraf-refactor-plan`

Status: living design and test specification. T0-T5A guardrails are partially implemented on this branch; T5B records the checkpoint outcome and recovery policy before multi-segment work.

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
2. Single-segment delta publish when a clean v10 base has pending facts and no visible delta manifest.
3. Full rebuild from either base-plus-pending facts or the current visible base-plus-delta view.

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
| `delta_manifest_primary_slot` | Selects slot 0 or slot 1 as the visible manifest pointer. |
| `delta_manifest_slot0_page` / `slot1_page` | Root page for each manifest slot. `0` means no delta manifest. |
| `delta_manifest_slot0_len` / `slot1_len` | Number of bytes in the manifest payload. |
| `delta_manifest_slot0_high_tx_count` / `slot1_high_tx_count` | Highest `tx_count` covered by that manifest. Used to choose the newer valid slot after crash. |
| `delta_manifest_slot0_checksum` / `slot1_checksum` | Checksum of the manifest payload. |
| `delta_manifest_header_checksum` | Checksum of the v10 extension fields with this checksum field zeroed. |

Publish rule:

1. Write and sync all new fact pages, delta pages, and manifest payload pages.
2. Fill the inactive manifest slot with page, length, high tx, and checksum.
3. Write and sync the header with the inactive slot still secondary if using a two-phase publish.
4. Flip `delta_manifest_primary_slot`.
5. Write and sync page 0.
6. Only after this succeeds, wire readers and retire WAL entries covered by the manifest high tx.

Recovery rule:

- If the primary manifest slot has a valid header checksum, valid manifest checksum, and a `high_tx_count` at least as new as the secondary, use it.
- If the primary slot is corrupt or points outside `page_count`, try the secondary.
- If both slots are invalid and the WAL still covers txs newer than the base checkpoint, replay the WAL and force a full rebuild/repair.
- If both slots are invalid and the WAL has already been retired past the base checkpoint, report file corruption. Do not silently drop delta-covered writes.
- Never use a manifest whose segment list references incomplete or out-of-bounds pages.

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

The first reader implementation proved merge semantics against the existing index key types and test fixtures. The current branch now writes v10 files with a single visible delta segment and falls back to full rebuild when a delta already exists.

## Flush Semantics

After `wal_write_stamped_batch` applies facts to `FactStorage`, checkpoint/flush can choose one of two paths:

1. **Delta flush**: write only pending facts plus sorted delta index entries, publish manifest, keep WAL entries until the manifest is durable.
2. **Full rebuild**: current `save()` behavior, used for recompact, repair, migration, or when delta thresholds are exceeded.

Initial policy:

- Keep public `checkpoint()` as the API name.
- Internally prefer delta flush when the existing file is v10-capable and the delta segment count/bytes are below thresholds.
- Fall back to full rebuild if manifest validation fails, if there are too many segments, or if a format upgrade is required.

### Checkpoint Outcome And WAL Retire Policy

`PersistentFactStorage::save()` reports an internal `CheckpointOutcome`:

| Outcome | Durable publish evidence | WAL retire allowed |
| --- | --- | --- |
| `Noop` | No page 0 publish happened. | No |
| `FullRebuild` | Base fact pages, indexes, checksum, and page 0 were synced. | Yes |
| `FullRebuildFromVisibleDelta` | The visible base-plus-delta view was folded into a fresh base and page 0 was synced. | Yes |
| `DeltaSegment` | Delta segment pages, manifest pages, v10 header extension, and page 0 were synced. | Yes |

`Minigraf::checkpoint()` must not delete the WAL if replayed or newly written WAL entries remain and `save()` returns `Noop`. In normal operation `force_dirty()` prevents that path during WAL replay, but the guard is deliberate: WAL retire is allowed only when the storage layer reports a durable publish boundary, not merely `Ok(())`.

Suggested thresholds for the first implementation:

- `max_delta_segments_before_recompact = 32`
- `max_delta_bytes_before_recompact = 64 MiB`
- `max_delta_fact_ratio_before_recompact = 0.25`

These are conservative defaults, not final performance tuning.

## Recompact Semantics

`recompact()` is a full rebuild from base plus all visible delta segments.

Rules:

- Build the new base from the current visible view: base fact pages plus delta fact pages in tx order.
- Preserve full-history rows, including retractions.
- Build new EAVT, AEVT, AVET, and VAET B+trees.
- Sync all new pages before publishing page 0.
- Publish a new base header with no delta manifest, or with a fresh empty manifest.
- Retire covered delta segments only after the new base header is durable.

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
- `export_fact_log()` includes base and delta records in deterministic order
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
- pending facts on a visible delta return `CheckpointOutcome::FullRebuildFromVisibleDelta`
- `checkpoint()` treats `Noop` as insufficient evidence for deleting a non-empty WAL

### T6: Benchmark Gate

Extend `tests/checkpoint_rebuild_benchmark.rs` or add an ignored delta benchmark:

- committed base: 10K, 100K, 1M
- pending facts: 1, 10, 100, 1K
- include `Value::Ref` and retractions
- measure delta flush wall time, recompact wall time, reopen recovery time, and final file page count

Acceptance:

- 1 pending fact on a 1M base must not perform an O(1M) committed-index rebuild during delta flush.
- Delta flush should scale primarily with pending fact count and segment metadata size.
- Recompact may remain O(total facts), because it is explicitly scheduled outside Vetch's interactive work rhythm.

## Implementation Order

1. Add `LayeredIndexReader` and in-memory delta entry fixtures.
2. Add tests T1.
3. Add segment metadata and codec structs behind private storage module APIs.
4. Add tests T2.
5. Add manifest structs and recovery selection logic in memory.
6. Add tests T3.
7. Implement v10 header extension read/write.
8. Wire delta flush into `checkpoint()` with full rebuild fallback.
9. Add T4/T5 integration and crash tests.
10. Re-run T6 benchmark gate and update `docs/BENCHMARKS.md`.

## Open Questions

- Should `CommittedIndexReader` grow a streaming range-scan trait before persistent delta lands, or should the first implementation keep `Vec<FactRef>` to reduce blast radius?
- Should `export_fact_log()` read through the same base-plus-delta manifest, or should it keep a dedicated fact-log stream path?
- Is a sync-data mode enough for delta segment publish on all supported platforms, or should v10 use full sync for the first release?
- Should `recompact()` be public immediately, or stay internal until Vetch has a real scheduling caller?
