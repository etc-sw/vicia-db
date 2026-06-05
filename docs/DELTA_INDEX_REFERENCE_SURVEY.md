# Delta Index Reference Survey

Branch: `vetch/minigraf-refactor-plan`

Status: research artifact only. No storage algorithm or file-format change is proposed here.

Reference snapshots:

- GrafeoDB: `/home/upopo/projects/reference/grafeo` at `4ebae02f`
- Fjall: `/home/upopo/projects/reference/fjall` at `fb57152`
- redb: `/home/upopo/projects/reference/redb` at `76e0e07`

## Philosophy Fit

The usable pieces fit Minigraf only if they are translated into the existing embedded, single-file, dependency-light storage model. This survey does not recommend adopting GrafeoDB, Fjall, redb, LSM machinery, vector indexes, or a new database backend. The useful direction is narrower:

- Keep a committed base and append-friendly delta/index segments inside Minigraf's storage boundary.
- Keep full-history fact identity across base and delta.
- Make small append checkpoint/flush cost scale with pending/delta size rather than total committed graph size.
- Preserve crash recovery by publishing new roots only after all referenced pages or segments are durable.
- Keep a full-rebuild checkpoint/recompact path as the simple fallback and migration escape hatch.

## Executive Verdict

The easiest pieces to port are not data structures; they are invariants.

| Source | Portable to Minigraf | Do not port |
| --- | --- | --- |
| GrafeoDB compact store | Base plus mutable overlay lifecycle; dirty/deleted state as first-class metadata; overlay-only visibility tests | Columnar property tables, CSR adjacency, vector/search stack, LPG object-copy semantics |
| Fjall | Batch framing, journal recovery, persisted watermarks, oldest-to-newest replay, snapshot-safe compaction watermarks | Full LSM tree, keyspace framework, background worker stack, compression/filter dependencies |
| redb | Double-buffered root publish discipline, per-root/slot checksums, recovery fallback to previous valid root, optional 2-phase mode for stricter durability | Page allocator, full MVCC page model, redb as a dependency |

The next Minigraf design note should specify a delta index layer, not an external storage replacement.

## 1. Grafeo Compact Store Concept

Reference:

- `/home/upopo/projects/reference/grafeo/docs/user-guide/compact-store.md:13`
- `/home/upopo/projects/reference/grafeo/docs/user-guide/compact-store.md:138`

GrafeoDB's user guide frames `compact()` as a conversion from a mutable graph store into an immutable query-optimized base plus mutable overlay. New inserts and property updates land in the overlay, and `recompact()` folds the overlay into a fresh compact base.

Minigraf translation:

- The "base" is the current checkpointed fact pages plus four committed B+tree indexes.
- The "overlay" should be append-friendly delta index segments that cover facts newer than the base root.
- `recompact()` should mean "merge base plus delta segments into a fresh base, then publish the new base root and retire covered deltas."
- Query-visible semantics must be a merge of base and delta, not "base unless dirty."

This is conceptually easy to import because it preserves the existing checkpoint/rebuild fallback. It does not require GrafeoDB's columnar layout or graph-specific CSR adjacency.

## 2. Grafeo Layered State

Reference:

- `/home/upopo/projects/reference/grafeo/crates/grafeo-core/src/graph/compact/layered.rs:1`
- `/home/upopo/projects/reference/grafeo/crates/grafeo-core/src/graph/compact/layered.rs:27`
- `/home/upopo/projects/reference/grafeo/crates/grafeo-core/src/graph/compact/layered.rs:365`
- `/home/upopo/projects/reference/grafeo/crates/grafeo-core/src/graph/compact/layered.rs:1120`
- `/home/upopo/projects/reference/grafeo/crates/grafeo-core/src/graph/compact/layered.rs:1290`

Grafeo's `LayeredStore` keeps a read-only base, a mutable overlay, dirty node/edge sets, deleted-from-base sets, a deletion dirty bit, and a merge guard. Reads first account for deletion/dirty metadata, then combine base and overlay where needed. Writes and deletes update overlay state and explicit deletion metadata.

Minigraf translation:

- Delta state must distinguish base facts, delta-only facts, and tombstone/retraction facts.
- Deletion/retraction visibility cannot be reconstructed from absence after reopen. It must be persisted in delta entries or segment metadata.
- Fact identity must remain full-history identity across segments: `entity`, `attribute`, encoded `value`, `valid_from`, `valid_to`, `tx_count`, `tx_id`, and `asserted`.
- A compact/merge guard is still useful, but it can be a narrow write-lock discipline rather than ArcSwap/LpgStore machinery.
- Overlay promotion should not copy whole objects as Grafeo does; Minigraf already has immutable EAV facts and explicit retractions.

Easy implementation candidate: a `DeltaIndexReader` that implements the same committed range-scan shape as the current B+tree reader and merges base B+tree ranges with sorted delta ranges.

## 3. Grafeo Failure Modes

Reference:

- `/home/upopo/projects/reference/grafeo/crates/grafeo-engine/tests/post_compact_overlay_visibility.rs:1`
- `/home/upopo/projects/reference/grafeo/crates/grafeo-engine/tests/compact_store_integration.rs:240`
- `/home/upopo/projects/reference/grafeo/crates/grafeo-engine/tests/grafeo_file.rs:1320`

The important failure mode is overlay invisibility after compaction. Grafeo had a path where post-compact overlay-only nodes were not dirty base nodes, so a reader could fall through to the compact base and treat them as missing. Related tests cover overlay-only nodes, overlay edges, base-to-overlay edges, overlay properties, recompact visibility, and deleted base nodes staying deleted after reopen.

Minigraf guardrails:

- Delta-only facts must be visible even when no base fact has the same E/A/V.
- `Value::Ref` edges must work when either side of the edge is introduced in delta.
- Retractions/tombstones must survive reopen and must not be inferred only from missing base rows.
- Recompact must preserve facts from both the previous base and all covered deltas.
- Writes after recompact must land in a fresh delta and remain visible.
- Crash or concurrent compaction tests should prove that writes are never lost across publish boundaries.

These tests are more portable than the implementation. They should drive Minigraf's first delta-index test plan.

## 4. Fjall Journal, Flush, and Compaction

Reference:

- `/home/upopo/projects/reference/fjall/src/journal/entry.rs:13`
- `/home/upopo/projects/reference/fjall/src/journal/batch_reader.rs:26`
- `/home/upopo/projects/reference/fjall/src/journal/writer.rs:33`
- `/home/upopo/projects/reference/fjall/src/batch/mod.rs:100`
- `/home/upopo/projects/reference/fjall/src/recovery.rs:120`
- `/home/upopo/projects/reference/fjall/src/journal/manager.rs:39`
- `/home/upopo/projects/reference/fjall/src/snapshot_tracker.rs:124`
- `/home/upopo/projects/reference/fjall/src/flush/worker.rs:20`
- `/home/upopo/projects/reference/fjall/src/compaction/worker.rs:34`

Fjall frames every journal batch as start marker, fixed item count, batch seqno, item records, and end marker with checksum/trailer. Recovery keeps `last_valid_pos`; an incomplete final batch is truncated, while checksum mismatch or item-count mismatch is a recovery error.

The write path is also useful: append batch to journal, optionally persist it, apply it to mutable state, then publish the visible seqno. Recovery replays sealed journals oldest-to-newest, tracks per-keyspace watermarks, and only deletes old journals once persisted table seqnos cover the journal watermarks. Flush and compaction use a snapshot-safe GC watermark.

Minigraf translation:

- A delta segment needs explicit batch framing: segment id, base root/epoch, tx range, entry counts, per-index counts, checksum, and committed terminator.
- On open, only complete committed delta segments enter the visible manifest. An incomplete tail is ignored or truncated to the last committed segment boundary.
- Reader visibility should be published only after the delta segment is durable.
- Segment cleanup must be gated by a persisted base root that covers the segment's high `tx_count`.
- Segment cleanup must also respect open read/as-of snapshots if Minigraf later supports long-lived snapshot handles.
- Recovery should replay base plus delta manifests in tx order; never assume directory/file order if the single-file metadata contains explicit segment ids.

Useful but deferred: Bloom filters can be segment-level skip metadata for E/A/V range scans. They should not become a general query-engine rewrite.

## 5. redb Commit, Checksum, and Root Publish

Reference:

- `/home/upopo/projects/reference/redb/docs/design.md:70`
- `/home/upopo/projects/reference/redb/docs/design.md:313`
- `/home/upopo/projects/reference/redb/docs/design.md:451`
- `/home/upopo/projects/reference/redb/src/tree_store/page_store/header.rs:87`
- `/home/upopo/projects/reference/redb/src/tree_store/page_store/header.rs:195`
- `/home/upopo/projects/reference/redb/src/tree_store/page_store/header.rs:294`
- `/home/upopo/projects/reference/redb/src/tree_store/page_store/page_manager.rs:767`
- `/home/upopo/projects/reference/redb/src/tree_store/page_store/header.rs:603`

redb stores transaction roots in double-buffered commit slots. A single primary bit selects the newest slot. Each slot carries a transaction id and checksum; tree pages carry checksums in a Merkle-like chain. The default durable path writes all data/checksums and the secondary slot, flips the primary bit, then fsyncs. On recovery, if the selected primary is corrupt or older, redb falls back to the previous valid slot. Its tests simulate a failed commit where the primary bit reaches disk but the new primary slot is corrupt.

Minigraf translation:

- Current Minigraf checkpoint already treats the header write as the commit point after fact and index pages are synced (`src/storage/persistent_facts.rs:911` and `src/storage/persistent_facts.rs:922`).
- A delta-index design should keep that discipline: write all delta pages/segment metadata, verify checksums, sync, then publish a small root/manifest pointer.
- If a file-format change is needed, consider a double-buffered manifest/root area rather than a single mutable root field. A corrupt new manifest should fall back to the previous valid manifest.
- Keep `header_checksum` and `index_checksum` semantics, but make them cover base plus visible delta metadata if deltas become part of the committed view.
- A stricter two-phase publish mode is worth documenting for migration/checkpoint paths, but the default should stay minimal and embedded-friendly.

This is the most directly relevant redb idea because Minigraf already has page 0 root fields and header checksums. The gap is that v9 has one active root set, not double-buffered roots.

## Candidate Minigraf Shape

The easiest design to implement incrementally:

1. Keep the current full checkpoint/rebuild path.
2. Add a design-only `DeltaIndexSegment` concept:
   - segment id
   - base root id or base checkpoint `tx_count`
   - inclusive/exclusive tx range
   - sorted EAVT, AEVT, AVET, and VAET entries
   - tombstone/retraction entries as first-class rows
   - per-index counts and checksums
3. Add a visible manifest concept:
   - current base header/root
   - ordered delta segment list
   - manifest checksum
   - previous valid manifest for recovery if the format changes
4. Query/index readers merge base range scans plus delta range scans and deduplicate only after preserving full-history identity.
5. `recompact()` builds a new base from base plus visible deltas, writes and syncs it, publishes the new root/manifest, then retires covered deltas after watermarks allow it.

Minimal acceptance criteria for the future implementation:

- A one-fact append on a 1M-fact graph avoids O(total committed facts) index rebuild during the append flush.
- Current-view query results match the existing full-rebuild path.
- Full-history export preserves assert/retract rows and `Value::Ref` identity across base and delta.
- Crash tests cover incomplete delta segment, corrupt manifest, successful segment before crash, and recompact crash before/after publish.
- The full-rebuild checkpoint path remains available as repair and migration fallback.

## Explicit Non-port List

Do not pull these into Minigraf core as part of the delta-index roadmap:

- GrafeoDB `Value::Vector`, HNSW, BM25, RRF, vector quantization, hybrid graph/vector planning.
- GrafeoDB columnar property storage or CSR adjacency as a replacement for Minigraf's EAV facts.
- Fjall's full LSM/keyspace/background-worker implementation.
- redb's page allocator, table layer, and full MVCC page model.
- Any external DB dependency.
- Sidecar index files unless a later design proves the single-file constraint cannot meet Vetch's baseline.

## Next Design Questions

- Is the first delta layer stored as pages inside the `.graph` file, as a WAL-like region folded at checkpoint, or both?
- Does v10 need double-buffered manifest slots, or can v9 header fields be extended without weakening crash recovery?
- What is the reader API for merging base and delta without allocating all candidate entries?
- What segment-level skip metadata is worthwhile before Bloom filters?
- Which benchmark target defines "small write stays cheap" for Vetch's 1M baseline: append flush latency, checkpoint latency, reopen recovery latency, or all three?
