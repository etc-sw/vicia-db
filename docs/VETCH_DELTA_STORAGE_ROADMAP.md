# Vetch Delta Storage Roadmap

Branch: `vetch/minigraf-refactor-plan`

Status: overall execution plan as of 2026-06-05. T7C is measured, T8A
multi-segment manifest publish is implemented on this branch, T8B mini benchmark
has passed, T8C full matrix is measured, and T9A threshold policy is the next
storage gate. This document is the single high-level plan for the Vetch-driven
Minigraf delta-storage line. The detailed storage format and test specification
remain in `docs/DELTA_INDEX_DESIGN.md`; benchmark evidence remains in
`docs/BENCHMARKS.md`.

## Scope

This roadmap covers the storage work required to make Minigraf suitable for
Vetch's expected 1M+ fact baseline:

- source import, projection versioning, local activity logs, and
  region/case-memory recalculation must not make small receipt writes expensive
- checkpoint/flush cost for small appends must be tied to pending/delta size,
  not committed graph size
- full-history ledger identity must survive base/delta/recompact transitions
- checkpoint work must be movable outside Vetch's interactive agent rhythm
- Minigraf must remain embedded, dependency-light, and single-file

This roadmap does not cover Vetch product scheduling, UI, object-store payload
layout, multimodal asset storage, vector search, BM25, or hybrid retrieval
implementation. Those remain Vetch-side projection/search concerns until a
later benchmark-backed proposal proves they belong in Minigraf core.

## Document Map

| Document | Role |
| --- | --- |
| `docs/REFACTORING_AND_ALGORITHM_PLAN.md` | Original R0-R6 cleanup, benchmark, and gate plan. Records Gate 1 and Gate 2 decisions. |
| `docs/DELTA_INDEX_REFERENCE_SURVEY.md` | Reference DB survey. Extracts portable invariants from GrafeoDB, Fjall, and redb without adopting them as dependencies. |
| `docs/DELTA_INDEX_DESIGN.md` | Detailed v10 delta format, reader semantics, crash matrix, and T0-T7 test spec. |
| `docs/BENCHMARKS.md` | Numeric evidence for R2, T6, T7A, T7B, T7C, T8B, and T8C. |
| `docs/VETCH_DELTA_STORAGE_ROADMAP.md` | This document: overall sequencing, gates, Vetch operating policy, and next-slice specs. |

## Decision Summary

Minigraf should continue the v10 in-file delta-index direction and move next to
T9A threshold and maintenance policy.

T7C showed that the current single-segment replacement path is not viable for
Vetch's long-running receipt cadence:

- 1M base + 1 fact x 1K: flush p95 `102.385 ms`, above the `50 ms` target.
- 1M base + 1 fact x 10K: flush p95 `1,051.300 ms`, max `1,559.688 ms`.
- 1M base + 1 fact x 10K: file growth `18.9 GB` for only 10K delta facts.
- 10x1K and 100x100 batching reduce file growth, but still end near 1 second
  flush p95 at 10K accumulated delta facts.
- Reopen remains acceptable in the measured matrix: p95 <= `29.157 ms`.
- Immediate current-query reads after writes remain sub-millisecond.
- As-of/replay receipt reads remain around seconds and are a separate
  agent-brief read-path blocker.

T8A now stops rewriting accumulated delta facts on every checkpoint. T8B
confirms the mini gate:

- 1M base + 1 fact x 1K: flush p95 `11.679 ms`, max `15.874 ms`,
  reopen p95 `6.290 ms`, file growth `12,234,752 B`, segment count `1,000`.
- 1M base + 10 facts x 100: flush p95 `6.882 ms`, max `7.233 ms`,
  reopen p95 `2.644 ms`, file growth `1,228,800 B`, segment count `100`.
- Corrupt latest segment fallback remains `true`.
- Immediate current-query reads remain sub-millisecond.
- As-of/replay receipt reads remain around `1.45 s` p95 and are still a
  separate Q1 agent-brief read-path blocker.

T8C then confirms the real boundary:

- Multi-segment publish is the correct default delta checkpoint path.
- 1M base + 1 fact x 1K: flush p95 `12.318 ms`, max `46.607 ms`,
  reopen p95 `6.589 ms`, file growth `12,234,752 B`.
- 1M base + 1 fact x 10K: flush p95 `99.818 ms`, max `133.904 ms`,
  reopen p95 `67.537 ms`, file growth `662,257,664 B`.
- 10K delta facts with only 1K or 100 segments stay below the hot flush target:
  10x1K p95 `36.821 ms`, 100x100 p95 `38.347 ms`.
- Corrupt latest segment fallback remains `true` across the matrix.
- Immediate current-query reads remain sub-millisecond.
- As-of/replay receipt reads remain around seconds and are still a separate Q1
  agent-brief read-path blocker.

The next step is therefore not another checkpoint algorithm change. It is T9A:
bound segment count and long-term file/manifest growth through an internal
threshold and idle/background recompact policy.

## Evidence Trail

The storage direction is not based on a broad desire to optimize Minigraf. It is
the result of progressively narrower gates:

| Step | Result | Decision |
| --- | --- | --- |
| R0/R1 | Vetch ledger identity and matcher/index boundaries were locked. `PatternMatcher` is not the storage identity surface. | Preserve full-history identity before touching checkpoint algorithms. |
| Gate 1 | Current-view selectors and full-history rows have different identity contracts. `Value::Ref`, `tx_id`, `tx_count`, and `asserted` matter for Vetch. | Treat identity invariants as fixed constraints. |
| R2 | 1 pending fact on a 1M committed graph took `4,829.691 ms` with full rebuild. | Current rebuild cost is tied to committed graph size. |
| Gate 2 | Vetch's 1M baseline makes batching alone insufficient as the long-term answer. | Write a delta/index design note; avoid immediate B+tree mutation. |
| Reference survey | Grafeo/Fjall/redb provide useful invariants: layered compact store, sorted durable batches, and double-buffered root publish. | Borrow discipline, not dependencies or storage engines. |
| T1-T5 | Layered reader, segment codec, manifest recovery, integration, and recovery policy guardrails were built. | v10 delta path is feasible inside the single `.graph` file. |
| T6 | First 1M base + 1 fact delta checkpoint reduced full rebuild cost, but checksum scope still carried too much base cost. | Scope validation to base identity plus delta bytes. |
| T7A | 1M base + 1 fact checkpoint reached `5.266 ms`; reopen reached `0.114 ms`. | Delta publish can be pending-sized for one segment. |
| T7B | Double-buffered manifest slots became the real publish boundary; second rotated delta flush was `2.852 ms`. | Crash-safe slot publish works for replacement single-segment deltas. |
| T7C | Accumulated single-segment replacement failed: 1K one-fact checkpoints p95 `102.385 ms`; 10K p95 `1,051.300 ms`; file growth `18.9 GB`. | Replace single-segment replacement with multi-segment append before tuning. |
| T8A | Visible-delta checkpoint now appends one pending-only segment and publishes an expanded manifest list. Integration covers multi-segment Ref edges, retractions, export order, and corrupt-segment fallback. | Run T8B mini benchmark before broader tuning or recompact thresholds. |
| T8B | Multi-segment mini gate passed: 1K one-fact checkpoints p95 `11.679 ms`, max `15.874 ms`, reopen p95 `6.290 ms`; 10x100 p95 `6.882 ms`; fallback remains true. | Continue to T8C full accumulation matrix. |
| T8C | Full matrix passed for the default path but exposed the long-tail limit: 1x10K p95 `99.818 ms` and file growth `662,257,664 B`; 10K facts batched into 1K/100 segments stay under `50 ms` p95. | Keep multi-segment publish; add T9A segment/file-growth thresholds. |

## Philosophy Fit

The plan stays inside Minigraf's SQLite-like constraints:

- Keep the database embedded and zero-configuration.
- Keep one `.graph` file as the primary durable database image.
- Keep the fact-level WAL until delta crash semantics are proven.
- Avoid external database dependencies, sidecar index files, client/server
  architecture, and alternate query languages.
- Prefer crash-safe root/manifest publish discipline over broad storage rewrites.
- Keep full rebuild as the repair, migration, import, and maintenance fallback.

## Fixed Invariants

These invariants are not tuning knobs:

| Surface | Invariant |
| --- | --- |
| Current-view query | Equivalent to full-rebuild current projection after base + deltas are merged. |
| Full-history identity | Preserve `entity`, `attribute`, encoded `value`, `valid_from`, `valid_to`, `tx_count`, `tx_id`, and `asserted`. |
| Ref edges | `Value::Ref` rows must survive across base/delta and segment/segment boundaries, including VAET. |
| WAL retire | WAL deletion is allowed only after a durable publish outcome, never after a no-op save. |
| Publish boundary | Segment pages and manifest payload are written and synced before page 0 selects the manifest slot. |
| Recovery | Corrupt newer slot/manifest/segment falls back to the previous valid committed state; no silent base-only loss. |
| Recompact | Full rebuild from visible base + deltas, scheduled outside the interactive Vetch rhythm. |

## Vetch Operating Policy

Vetch should use Minigraf with this cadence:

| Work type | Policy |
| --- | --- |
| Durable append / receipt | Immediate through WAL. |
| Segment checkpoint / flush | Batchable by receipt or slice; should be pending-sized. |
| Recompact | Idle, background, or scheduled maintenance. |
| Full rebuild | Import or maintenance only; not a foreground normal-work path. |
| Broad import / projection rebuild | Background job; may end with explicit recompact. |
| Multimodal/object graph growth | Requires the same small-write cadence; do not rely on foreground rebuild. |

## Ownership Split

| Concern | Minigraf responsibility | Vetch responsibility |
| --- | --- | --- |
| Durable graph/ledger facts | Persist EAV facts, bi-temporal metadata, full-history rows, and `Value::Ref` edges. | Decide what receipts, imports, projections, and local activity records become facts. |
| Small write durability | WAL append immediately; delta checkpoint pending-sized when checkpointed. | Batch checkpoint calls by receipt/slice when safe for the agent workflow. |
| Compaction | Provide internal/full-rebuild fallback and later recompact thresholds. | Schedule recompact during idle/background/maintenance windows. |
| Multimodal payloads | Store pointers, hashes, metadata, relations, and authority graph edges. | Own blob/object stores, OCR/transcript/chunk payloads, embedding stores, and rebuildable search projections. |
| Retrieval/search | Keep Datalog and graph facts correct; optimize receipt/as-of paths only when measured. | Compose graph, vector, text, and object projections for agent briefs. |

## Overall Phase Map

| Phase | Status | Purpose | Gate |
| --- | --- | --- | --- |
| R0-R1 | Done | Lock ledger identity and clarify matcher hint semantics. | Identity tests and query behavior stable. |
| R2 | Done | Measure current checkpoint/index rebuild cost. | 1M base + small pending append proves full rebuild is too expensive. |
| Gate 2 | Done | Choose storage direction. | Delta/index design required; no external DB or immediate B+tree mutation. |
| T1-T3 | Done | Prove layered reader, segment codec, and manifest recovery in isolation. | Pure storage semantics pass before integration. |
| T4-T5 | Done | Integrate checkpoint/reopen/crash policy around v10 manifest slots. | Recovery policy distinguishes fallback-safe states from corruption. |
| T6 | Done | Benchmark first delta flush against rebuild baseline. | One small append no longer pays O(1M) rebuild. |
| T7A | Done | Scope checksum/validation cost to base identity plus delta bytes. | First delta flush and reopen are pending-sized. |
| T7B | Done | Use double-buffered manifest slots as publish boundary. | Corrupt newer slot can fall back to previous valid slot. |
| T7C | Done | Measure accumulated single-segment replacement cadence. | Single-segment replacement fails; multi-segment append is required. |
| T8A | Done | Append a new delta segment per checkpoint and publish a multi-segment manifest. | Integration and corrupt-segment fallback tests pass. |
| T8B | Done | Mini benchmark gate after T8A. | Flush/reopen/file growth meet near-term Vetch targets. |
| T8C | Done | Full accumulated benchmark matrix. | Multi-segment is the default path; tiny-segment accumulation needs thresholds. |
| T9A | Planned next | Segment/file-growth threshold policy. | Long-term segment/file growth is bounded outside hot path. |
| T9B | Planned | Recompact implementation and maintenance path. | Threshold-triggered maintenance preserves visible semantics and crash guarantees. |
| Q1 | Planned separate lane | Agent-brief receipt/as-of read-path improvement. | Just-written receipt can be read cheaply on a 1M base. |
| Q2 | Planned cleanup lane | Streaming/allocation cleanup after correctness shape stabilizes. | Export/checkpoint/recompact memory improves without semantic drift. |

## Gate Protocol

Each gate has one owner decision:

| Gate | Decide | Continue if | Reassess if |
| --- | --- | --- | --- |
| T8A correctness | Can multi-segment manifest preserve exact visible semantics? | Base + multiple deltas, retractions, `Value::Ref`, export, reopen, and corruption tests pass. | Reader merge collapses history identity or corrupt middle segment can silently drop facts. |
| T8B mini benchmark | Is the new algorithm likely enough? | Passed: 1K one-fact accumulated delta flush p95 `11.679 ms`, max `15.874 ms`, reopen p95 `6.290 ms`. | Flush still scales with accumulated facts or manifest rewrite dominates. |
| T8C full matrix | Is multi-segment publish the default path? | Default path accepted; 1K segment p95 is `12.318 ms`, and batched 10K facts stay under `50 ms`. | 10K tiny segments reach p95 `99.818 ms` and file growth `662,257,664 B`. |
| T9 threshold gate | Are internal thresholds enough? | Recompact bounds segment/file growth without entering Vetch foreground work. | Thresholds fire too often, or recompact publish weakens crash guarantees. |
| Q1 read gate | Is the next-agent brief cheap enough? | Receipt/as-of reads avoid whole-base scans for Vetch-shaped reads. | Query optimization risks Datalog semantics or requires broad public API churn. |

Do not skip gates by adding a broader storage engine, sidecar index, vector
stack, or public API. If a gate fails, fix the narrow failing invariant first.

## Execution Phases

### Phase T8A: Multi-Segment Manifest Publish

Goal: checkpoint over a visible delta appends a new segment instead of replacing
the selected delta with a copy of all accumulated delta facts.

Implementation shape:

- Keep the existing v10 double-buffered manifest slots.
- Extend the active manifest with one new segment descriptor on checkpoint.
- Write only pending fact/index pages for the new segment.
- Publish the new manifest through the inactive slot.
- Keep previous valid slot as fallback until the new slot fully verifies.
- Load all visible segments on reopen, in tx/segment order.
- Merge base + all delta segment readers for current queries and export.

Tests first:

- visible delta + pending write appends a segment instead of rewriting the old
  delta facts
- base + segment1 + segment2 delta-only facts visible after reopen
- base-to-segment and segment-to-segment `Value::Ref` edges visible
- retraction in a later segment hides an earlier assertion in current view
- `export_fact_log()` preserves base + multiple deltas in deterministic tx order
- corrupt latest segment falls back to previous valid manifest slot
- corrupt middle selected segment rejects the selected manifest unless an
  alternate slot is valid
- manifest segment ordering and tx-range overlap checks reject bad payloads

Acceptance:

- Existing T0-T7 tests stay green.
- `checkpoint()` over a visible delta returns `CheckpointOutcome::DeltaSegment`.
- New checkpoint writes are proportional to pending facts plus manifest metadata,
  not accumulated delta facts.
- T8A does not add a public API or new dependency.

### Phase T8B: Mini Benchmark Gate

Goal: prove the multi-segment path fixes the T7C failure before broad tuning.

Run a bounded benchmark before the full T7C matrix:

| Scenario | Required measurement |
| --- | --- |
| 1M base + 1 fact x 1K | flush p50/p95/max, reopen p50/p95/max, current query, as-of query, file/page growth, segment count |
| 1M base + 10 facts x 100 | same measurements |
| corrupt latest segment | previous valid slot fallback still works |

Target:

- 1K accumulated delta facts: hot flush p95 <= `50 ms`.
- Max should stay near <= `200 ms`.
- Reopen should stay <= `250-500 ms`.
- File/page growth should be roughly proportional to actual segment bytes plus
  manifest metadata, not repeated accumulated-delta rewrites.

Decision after T8B:

| Result | Decision |
| --- | --- |
| Flush target passes, reopen/file growth acceptable | Continue to T8C full T7C matrix. |
| Flush passes, reopen/file growth grows with segment count | Add recompact threshold policy before full matrix. |
| Flush still misses at 100-1K accumulated facts | Inspect manifest rewrite/checksum cost before adding more features. |
| Current query regresses | Fix reader merge path before proceeding. |

T8B result: the target passed. The 1M base + 1 fact x 1K scenario measured
flush p95 `11.679 ms`, max `15.874 ms`, reopen p95 `6.290 ms`, file growth
`12,234,752 B`, segment count `1,000`, and corrupt fallback `true`. The
1M base + 10 facts x 100 scenario measured flush p95 `6.882 ms`, reopen p95
`2.644 ms`, file growth `1,228,800 B`, segment count `100`, and corrupt
fallback `true`. Proceed to T8C.

### Phase T8C: Full Accumulation Matrix Re-run

Goal: re-run the full T7C matrix against multi-segment publish.

Required scenarios:

- 1M base + 1 fact checkpoint x 10 / 100 / 1K / 10K
- 1M base + 10 facts checkpoint x 100 / 1K
- 1M base + 100 facts checkpoint x 100

Measure:

- flush p50/p95/max
- reopen p50/p95/max
- immediate current-query latency after writes
- as-of/replay query latency
- file/page growth
- delta fact count growth
- segment count growth
- manifest/file growth pressure, inferred from file/page growth and segment
  count; exact manifest payload decomposition belongs in T9A if needed
- crash/corruption fallback

Decision:

- If flush remains pending-sized and reopen remains acceptable, multi-segment
  publish becomes the default delta checkpoint path.
- If reopen or query latency grows with segment count, proceed to T9 recompact
  thresholds.
- If as-of/replay remains seconds-level while current reads stay sub-ms, proceed
  to Q1 read-path work as a separate lane.

T8C result: multi-segment publish remains the default path, but T9 thresholds
are needed before production use with unbounded per-receipt checkpoint cadence.
The 1x10K scenario improves the T7C p95 from `1,051.300 ms` to `99.818 ms`
and cuts file growth from `18.9 GB` to `662,257,664 B`, but this still shows
segment-count/manifest accumulation entering the hot path. The batching rows
make the threshold shape clear: 10K delta facts with 1K segments have flush p95
`36.821 ms`, and 10K delta facts with 100 segments have p95 `38.347 ms`.
Current reads remain sub-millisecond, reopen remains below the `250-500 ms`
gate, fallback remains true, and as-of reads remain a Q1 lane.

### Phase T9: Recompact Thresholds and Maintenance Path

Goal: keep long-term delta growth from hurting reopen, read/query, and file
growth while preserving cheap hot writes.

Implementation shape:

- Add internal thresholds for segment count, delta bytes, and delta/base ratio.
- Keep threshold values conservative and documented.
- Use existing full rebuild from visible base + deltas as `recompact()`.
- Do not expose a public `recompact()` API until Vetch has a real scheduling
  caller or a clear operator story.
- Ensure recompact publish follows the same crash-safe page 0 commit discipline.

Initial threshold candidates:

- `max_delta_segments_before_recompact = 32`
- `max_delta_bytes_before_recompact = 64 MiB`
- `max_delta_fact_ratio_before_recompact = 0.25`

Acceptance:

- Recompact preserves full-history rows, retractions, and `Value::Ref` edges.
- Recompact removes visible delta manifest or publishes an empty fresh manifest.
- Crash before recompact header publish preserves previous base + manifest.
- Crash after recompact header publish leaves new base visible.
- Old delta pages may remain as garbage but are ignored by selected manifest.

Decision after T9:

| Result | Decision |
| --- | --- |
| Hot flush stays cheap and thresholded recompact bounds growth | Keep recompact internal and let Vetch schedule it in background. |
| Recompact is needed by Vetch callers but cannot be scheduled implicitly | Consider a small public maintenance API only after Vetch provides a concrete scheduling caller. |
| File garbage becomes unacceptable | Design file-space reclamation as a separate phase; do not mix it into T9 correctness work. |

### Phase Q1: Agent-Brief Read Path

Goal: make "write receipt, then read it into the next agent brief" cheap for
Vetch, especially for as-of/replay reads.

T7C proved current reads after writes are cheap, but as-of/replay query reads are
not. This should not block T8A, because storage publish must first stop rewriting
delta facts. It should start after T8B or T8C confirms multi-segment publish.

Candidates:

- Push bound entity/attribute/value lookups deeper into as-of query execution.
- Add a fact-log replay reader for receipt-shaped reads.
- Add prepared receipt/as-of query patterns if a public API shape is justified.
- Add segment-level tx bounds and key bounds to skip irrelevant delta segments.

Acceptance:

- Just-written receipt as-of read on a 1M base should not scan the whole base.
- Query semantics remain Datalog-first; no alternate query language.
- The public API remains minimal unless Vetch usage proves a small helper is
  worth the surface area.

### Phase Q2: Streaming and Allocation Cleanup

Goal: reduce memory and buffering costs on export, checkpoint, and recompact
paths after the correctness-critical storage shape is stable.

Candidate work:

- Streaming fact-log export over base + deltas.
- Streaming recompact input instead of materializing all facts where possible.
- Optional streaming range-scan trait only after current `Vec<FactRef>` behavior
  is fully locked.
- Remove misleading optimization surfaces before adding new ones.

Acceptance:

- No behavior change without tests.
- No broad module split unless it removes real complexity.
- No new dependency.

## Merge And Release Readiness

The delta-storage line can be considered ready for merge into main only after:

- T8A correctness tests pass and the multi-segment manifest path is the default
  delta checkpoint path.
- T8B and T8C benchmark results are recorded in `docs/BENCHMARKS.md`.
- T9 is either implemented or explicitly deferred with measured segment-count,
  reopen, and file-growth evidence showing deferral is acceptable.
- WAL retire remains gated by `CheckpointOutcome`.
- Full rebuild/recompact remains available as repair and migration fallback.
- No public API or dependency has been added without a separate philosophy
  review and explicit approval.
- `cargo fmt --check`, `cargo test`, `cargo clippy --lib -- -D warnings`, and
  `git diff --check` pass.

Vetch can adopt the line experimentally earlier if T8A plus T8B pass, but
production use should now wait for a documented T9 threshold decision.

## Reference Database Lessons

Reference repos reinforce the direction but do not replace Minigraf-specific
proof:

- Grafeo-style writable layered compact store maps well to base + mutable delta
  segments + explicit recompact.
- Fjall/LSM suggests sorted append segments and compaction policy, but Minigraf
  should not adopt an external LSM or multi-file layout.
- redb suggests double-buffered/root-publish discipline for crash-safe metadata
  selection.
- Small smoke measurements from reference repos show that small durable writes
  can be ms-scale or below; Minigraf's T7C failure is therefore more likely the
  single-segment replacement algorithm than unavoidable append cost.

## Risk Register

| Risk | Mitigation |
| --- | --- |
| Manifest/file growth grows with segment count | T8B/T8C measure segment count and file/page growth; T9 adds recompact thresholds and can decompose manifest payload if needed. |
| Reader merge becomes complex | Keep `Vec<FactRef>` trait initially; add tests for base + multiple segments before optimizing. |
| Corrupt middle segment silently drops facts | Reject selected manifest unless an alternate valid slot exists. |
| WAL is retired too early | Keep `CheckpointOutcome` as the WAL retire gate. |
| Full rebuild sneaks into foreground Vetch work | Keep full rebuild/recompact as explicit maintenance only. |
| As-of reads stay seconds-level | Treat Q1 as a separate read-path lane after T8 storage publish is fixed. |
| File format churn | Stay within v10 header extension and manifest payload versioning; keep migration fallback. |

## Verification Cadence

Run before and after every implementation slice:

- `cargo test --test fact_log_export_test`
- `cargo test --test multivalue_index_test`
- `cargo test --test retract_valid_time_test`
- `cargo test --lib storage::index`

Run for storage publish/recovery slices:

- `cargo test --test delta_checkpoint_integration_test -- --nocapture`
- `cargo test --test delta_checkpoint_crash_recovery_test -- --nocapture`
- `cargo test`
- `cargo clippy --lib -- -D warnings`
- `cargo fmt -- --check`
- `git diff --check`

Run benchmark gates only when the relevant slice is ready:

- T8B mini benchmark is complete.
- T8C full `cargo bench --bench delta_accumulation_benchmark` is complete.
- T9A threshold policy is the next storage gate.

Known verification caveat:

- `cargo clippy --all-targets -- -D warnings` currently fails on pre-existing
  test/bench `unwrap`, `expect`, `panic`, and indexing lints. Use lib clippy as
  the storage implementation gate unless the all-target lint cleanup lane is
  explicitly active.

## Next Slice Goal Spec

Name: T9A segment/file-growth threshold policy.

Objective:

- Define the internal thresholds and maintenance decision surface that keep
  multi-segment delta growth out of Vetch's foreground work rhythm.

Scope:

- Design and documentation first; implementation should wait for the next slice
  unless the policy requires a tiny private constant/test fixture.
- No broad storage algorithm change.
- No new dependency.
- No public recompact API unless a Vetch scheduling contract is explicit.
- No as-of query optimization in T9A; keep that in Q1.

Done:

- Threshold inputs are documented: segment count, delta bytes/page growth,
  delta/base ratio, and manifest/file growth pressure.
- Hot-path threshold behavior is decided: continue, request/schedule background
  maintenance, or perform internal recompact only outside foreground work.
- Candidate default thresholds are justified from T8C numbers, especially:
  `1K` segments is healthy, `10K` tiny segments is not.
- Crash/recovery rules for threshold-triggered recompact are linked back to the
  existing double-buffered manifest and full-rebuild recovery policy.
- Next implementation slice T9B is specified with tests before code.

Stop conditions:

- If a threshold requires foreground full rebuild during normal Vetch work,
  reject it.
- If the policy cannot be expressed without a public API, write the Vetch
  scheduling contract first.
- If thresholds alone cannot bound the 10K tiny-segment case, inspect manifest
  payload serialization/checksum cost before implementing broader features.
