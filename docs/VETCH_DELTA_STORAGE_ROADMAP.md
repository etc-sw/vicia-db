# Vetch Delta Storage Roadmap

Current line: merged on `main`. Use a fresh worktree and slice branch for new
storage cleanup, rename, benchmark, or public API work.

Status: overall execution plan as of 2026-06-07. T7C is measured, T8A
multi-segment manifest publish is implemented on the current line, T8B mini benchmark
has passed, T8C full matrix is measured, T9A threshold policy is documented,
T9B private threshold metrics pass, T9C-A adds a private explicit recompact
primitive, T9C-B makes recompact publish copy-on-write, T9C-C adds a private
idle/background maintenance caller, and Q1-A adds a dedicated agent-brief
read-path benchmark harness. Q1-B resolves the entity/attribute-bound as-of
agent-brief point-read blocker with selective index pushdown. Q2-A removes the
intermediate committed `Vec<Fact>` allocation from `export_fact_log()` without
changing its public `Vec<FactRecord>` API. S1 rechecked the Q1-B/Q2-A surface,
and Q2-B removes the intermediate `Vec<Fact>` allocation from the private
recompact candidate writer. Q2-B is still only a memory-shape cleanup: candidate
packed pages and the four sorted index-entry buffers remain O(total facts).
Q3-A adds `Minigraf::run_idle_maintenance()` as the embedder scheduling surface:
it checkpoints pending WAL-backed writes, then runs private delta maintenance
under the same write lock, and reports a stable public outcome. Raw recompact is
still private, automatic/background scheduling remains a caller-policy decision,
and maintenance is not wired into foreground `checkpoint()`. Q3-B records the
Vetch caller contract for when to invoke the hook, how to interpret outcomes,
and how to retry visible maintenance errors.
This document is the single high-level plan for the Vetch-driven Minigraf /
Vicia DB delta-storage line. The detailed storage format and test specification
remain in `docs/DELTA_INDEX_DESIGN.md`; benchmark evidence remains in
`docs/BENCHMARKS.md`; rename sequencing remains in
`docs/VICIA_DB_RENAME_PLAN.md`; maintenance caller guidance remains in
`docs/MAINTENANCE_API_CONTRACT.md`.

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
| `docs/MAINTENANCE_API_CONTRACT.md` | Q3-B caller contract for `run_idle_maintenance()`: safe windows, outcome semantics, retry/error policy, and Vetch scheduling guidance. |
| `docs/BENCHMARKS.md` | Numeric evidence for R2, T6, T7A, T7B, T7C, T8B, T8C, Q1-A, Q1-B, Q2-A, and Q2-B. |
| `docs/VETCH_DELTA_STORAGE_ROADMAP.md` | This document: overall sequencing, gates, Vetch operating policy, and next-slice specs. |
| `docs/VICIA_DB_RENAME_PLAN.md` | Staged Vicia DB successor rename plan, compatibility policy, attribution checklist, and `vicia-db-decision-gate` skill shape. |

## Decision Summary

Minigraf should keep the v10 in-file delta-index direction. T9 threshold and
recompact policy, Q1-B agent-brief as-of pushdown, Q2-A export allocation
cleanup, Q2-B private recompact input streaming, Q3-A public idle maintenance,
and Q3-B maintenance caller contract are complete. Q2-B confirms the current
locking model can stream visible facts into the recompact candidate without a
cursor redesign, but it does not make recompact bounded-memory. Q3-A closes the
embedder reachability gap without making maintenance automatic. Q3-B keeps the
next implementation decision outside the database core until Vetch has caller
evidence. The next storage decision should be based on Vetch's actual
maintenance-caller experience or a deeper index/page streaming cleanup, not
another hot-path checkpoint change.

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
- As-of/replay receipt reads remained around seconds before Q1-B and were
  tracked as a separate agent-brief read-path blocker.

T9 then bounded long-term segment/file growth with private threshold decisions,
copy-on-write recompact, and an idle maintenance caller outside foreground
`checkpoint()`. Q1-B then fixed the Vetch receipt-scoped as-of point-read path:
on a 1M base, formatted as-of p95 drops from `1,257.698-1,499.003 ms` to
`0.017-0.043 ms`, and prepared as-of p95 drops from `1,260.495-1,623.022 ms` to
`0.013-0.026 ms`.

The next step is therefore not another checkpoint algorithm change and not a new
public receipt API. After Q3-B, Vetch should validate the maintenance caller
contract in a real daemon or application loop. Only then should Minigraf/Vicia
choose between deeper bounded-memory recompact work, file-space reclamation, or
a narrower recent fact-log reader.

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
| T9 | Threshold decisions, explicit private recompact, copy-on-write publish, and idle/background maintenance caller are implemented. | Keep foreground checkpoint on delta append; Vetch schedules idle maintenance. |
| Q1-A | Full 1M agent-brief benchmark split current, formatted as-of, prepared as-of, and export/replay paths. | Optimize as-of point reads first; do not add a public receipt API yet. |
| Q1-B | Entity/attribute-bound as-of p95 dropped from `1,257.698-1,499.003 ms` to `0.017-0.043 ms` on the 1M matrix. | Treat the receipt-scoped Datalog read blocker as fixed for Vetch-shaped point reads. |
| Q2-A | `export_fact_log()` streams committed facts into `FactRecord`s instead of first collecting committed `Vec<Fact>`. | Keep export as full-log audit API; defer narrower recent-log reads until Vetch proves that path is hot. |
| S1 | Q1-B/Q2-A review and stability checks passed; all-target clippy still fails on pre-existing test-lint debt. | Constrained Q2-B to a bounded cleanup spike. |
| Q2-B | `write_recompact_candidate_from_visible_facts()` streams visible facts through `FactStorage::for_each_fact()` instead of materializing a committed `Vec<Fact>` first. | Recompact input has a better memory shape, but candidate pages and index-entry buffers remain O(total facts). |
| Q3-A | Public `run_idle_maintenance()` exposes checkpoint/delta/advice outcome while keeping raw recompact private. | Vetch can schedule maintenance without owning storage internals. |
| Q3-B | `docs/MAINTENANCE_API_CONTRACT.md` records caller windows, outcome semantics, error handling, and Vetch scheduling policy. | Vetch can validate maintenance adoption without another Vicia storage algorithm change. |

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
| Compaction | Provide internal/full-rebuild fallback, private recompact thresholds, and a public idle-maintenance entry point. | Call `run_idle_maintenance()` during idle/background/maintenance windows and own cadence/backoff policy. |
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
| T9A | Done | Segment/file-growth threshold policy. | Long-term segment/file growth is bounded outside hot path. |
| T9B | Done | Private threshold metrics and decision tests. | The storage layer can classify visible delta growth without doing foreground full rebuild. |
| T9C-A | Done | Private explicit recompact primitive. | Manual/internal recompact preserves visible semantics when it completes successfully and is guarded against pending writes. |
| T9C-B | Done | Crash-safe recompact publish gate. | Recompact writes a copy-on-write base after the current image and publishes it through page 0 with a protected base-start pointer. |
| T9C-C | Done | Background/idle scheduling policy gate. | Threshold-triggered maintenance has a private idle/background caller and remains out of foreground checkpoint. |
| Q1-A | Done | Agent-brief read-path benchmark/spec gate. | Current, as-of, prepared as-of, and export/replay surfaces are measured separately. |
| Q1-B | Done | Agent-brief read strategy decision. | Entity/attribute-bound as-of selective pushdown fixes the receipt-scoped point-read blocker without public API changes. |
| Q2-A | Done | Export fact-log allocation cleanup. | `export_fact_log()` uses a streaming committed fact visitor and avoids an intermediate `Vec<Fact>`. |
| S1 | Done | Stability/code-quality review before Q2-B. | Q1-B/Q2-A surfaces pass targeted tests, full tests, fmt, lib clippy, and diff whitespace checks. |
| Q2-B | Done | Private recompact input streaming cleanup. | Recompact candidate writing avoids an intermediate `Vec<Fact>` without semantic drift; index/page buffers remain O(total facts). |
| Q3-A | Done | Public idle maintenance API contract. | `Minigraf::run_idle_maintenance()` exposes checkpoint/delta/advice outcome without exposing raw `CheckpointOutcome` or invoking recompact from `checkpoint()`. |
| Q3-B | Done | Maintenance caller adoption contract. | Caller windows, outcome semantics, retry policy, and Vetch scheduling guidance are documented without adding another API or storage algorithm. |

Adoption caveat:

- Q3-A makes maintenance reachable through `Minigraf`/`ViciaDb` via
  `run_idle_maintenance()`. It is still explicit caller-driven maintenance:
  foreground `checkpoint()` never runs recompact, raw `recompact()` remains
  private, and Vetch must decide when to call the idle hook.
- Q3-B records that decision boundary. Production adoption still needs
  Vetch-side evidence that the hook is invoked outside capture/write foreground
  paths and that `ReduceCheckpointCadence` changes batching/backoff policy.

## Gate Protocol

Each gate has one owner decision:

| Gate | Decide | Continue if | Reassess if |
| --- | --- | --- | --- |
| T8A correctness | Can multi-segment manifest preserve exact visible semantics? | Base + multiple deltas, retractions, `Value::Ref`, export, reopen, and corruption tests pass. | Reader merge collapses history identity or corrupt middle segment can silently drop facts. |
| T8B mini benchmark | Is the new algorithm likely enough? | Passed: 1K one-fact accumulated delta flush p95 `11.679 ms`, max `15.874 ms`, reopen p95 `6.290 ms`. | Flush still scales with accumulated facts or manifest rewrite dominates. |
| T8C full matrix | Is multi-segment publish the default path? | Default path accepted; 1K segment p95 is `12.318 ms`, and batched 10K facts stay under `50 ms`. | 10K tiny segments reach p95 `99.818 ms` and file growth `662,257,664 B`. |
| T9 threshold gate | Are internal thresholds enough? | Recompact bounds segment/file growth without entering Vetch foreground work. | Thresholds fire too often, or recompact publish weakens crash guarantees. |
| Q1 read gate | Is the next-agent brief cheap enough? | Receipt/as-of reads avoid whole-base scans for Vetch-shaped reads. | Query optimization risks Datalog semantics or requires broad public API churn. |
| S1 stability gate | Are Q1-B/Q2-A safe enough before Q2-B? | Review finds no semantic blocker and targeted recovery/export/query tests plus broad gates pass. | Visitor order, rule/as-of semantics, crash fallback, or ledger identity regress. |
| Q3 maintenance API gate | Can Vetch schedule storage maintenance without owning storage internals? | A single public call checkpoints pending writes, runs private delta maintenance, returns stable outcome/advice, and leaves foreground `checkpoint()` unchanged. | The public outcome leaks internal storage enums, phase-2 failure can imply data loss, or checkpoint starts doing hidden recompact work. |
| Q3-B adoption gate | Is the caller contract explicit enough for Vetch implementation? | Safe call windows, outcome handling, retry/error policy, and forbidden foreground behavior are written down. | Vetch needs a stronger API, raw recompact, automatic scheduling, or file-space reclamation to use the hook safely. |

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

### Phase T9A: Segment/File-Growth Threshold Policy

Goal: define the private policy that keeps long-term delta growth from hurting
flush, reopen, read/query, and file growth while preserving cheap hot writes.

Policy inputs:

- `visible_delta_segment_count`
- `visible_delta_page_growth` and derived bytes
- `visible_delta_base_page_ratio`
- exact `visible_delta_fact_count` and `visible_delta_fact_base_ratio` when
  available
- optional manifest payload byte count if T9B can expose it cheaply

Initial decision surface:

| Decision | Meaning | Allowed foreground behavior |
| --- | --- | --- |
| `ContinueDeltaAppend` | Delta growth is healthy. | Keep checkpoint on the pending-sized delta append path. |
| `ScheduleBackgroundRecompact` | Soft threshold crossed. | Keep appending deltas, but surface an internal maintenance recommendation. |
| `MaintenanceBackpressure` | Hard threshold crossed. | Do not run foreground full rebuild inside `checkpoint()`; Vetch should batch further checkpoints and prioritize background maintenance. |

Initial threshold candidates, derived from T8C:

| Threshold | Soft | Hard | Rationale |
| --- | ---:| ---:| --- |
| Delta segment count | `1,024` | `4,096` | T8C shows `1K` segments healthy (`12.318 ms` p95) and `10K` tiny segments above target (`99.818 ms` p95). |
| Delta page growth | `16,384` pages (`64 MiB`) | `65,536` pages (`256 MiB`) | T8C `1x10K` reaches `161,684` pages / `662,257,664 B`; this should be caught before hot-path cost returns. |
| Delta/base page ratio | `0.10` | `0.25` | Keeps delta growth meaningfully smaller than the checkpointed base without overreacting to tiny bases. |
| Delta fact/base ratio | `0.10` | `0.25` | Secondary signal for broad imports; T8C shows tiny receipt cadence needs segment/page thresholds because fact ratio alone stays low. |

Policy rules:

- A soft threshold returns `ScheduleBackgroundRecompact`, not a foreground
  rebuild.
- A hard threshold returns `MaintenanceBackpressure`; the storage layer may keep
  appending deltas for correctness, but Vetch should stop treating per-receipt
  checkpoint cadence as healthy until background maintenance runs.
- Ratio thresholds have absolute floors. Page ratio applies only after at least
  `1,024` delta pages (`4 MiB`), and fact ratio applies only after exact delta
  and base fact counts are available with at least `1,000` delta facts. This
  prevents tiny base files from scheduling maintenance after a few small writes.
- The current manifest descriptor does not store exact fact counts. T9B should
  use segment/page metrics from the selected manifest and treat fact-ratio
  checks as unavailable unless exact fact counts are supplied by a future
  internal caller.
- Threshold checks must be based on the selected visible manifest and base page
  identity, not on unpublished trailing bytes.
- Full rebuild/recompact remains the repair and maintenance mechanism, not a
  normal `checkpoint()` side effect.
- No public `recompact()` API is added in T9A. A public maintenance surface
  requires a concrete Vetch scheduling caller.

Acceptance:

- T8C numbers justify the default thresholds.
- The policy has no dependency, file-format, or public API change.
- The policy prevents unbounded per-receipt checkpoint cadence from being called
  production-ready without background maintenance.
- T9B has a concrete tests-first implementation plan.

### Phase T9B: Private Threshold Metrics and Decision Tests

Goal: implement the private metric and decision surface without triggering
recompact yet.

Tests first:

- empty/no-manifest database returns `ContinueDeltaAppend`
- manifest with `1,000` tiny segments and `12 MiB` growth returns
  `ContinueDeltaAppend`
- manifest with `1,024` tiny segments returns `ScheduleBackgroundRecompact`
- manifest crossing soft segment threshold returns `ScheduleBackgroundRecompact`
- manifest crossing hard segment threshold returns `MaintenanceBackpressure`
- manifest crossing soft/hard page-growth thresholds returns the matching
  decision
- small-base ratio cases below the absolute floor return
  `ContinueDeltaAppend`
- fact-ratio threshold is optional, secondary, and does not mask segment/page
  thresholds
- threshold metrics ignore unpublished trailing delta/manifest pages

Implementation shape:

- Add a private `DeltaGrowthMetrics` struct derived from the selected manifest,
  base page count, optional exact fact count, and page growth.
- Add a private `DeltaMaintenanceDecision` enum.
- Keep `checkpoint()` on the delta append path; do not execute recompact in T9B.
- Wire no public API unless the Vetch scheduling contract exists.
- Record the decision only where existing internal code can observe it safely,
  or keep the first implementation as a pure function with unit tests.

### Phase T9C-A: Private Recompact Primitive

Goal: provide a private maintenance primitive that can fold the selected visible
delta into a fresh base without adding a public API or running from foreground
`checkpoint()`.

Result:

- `PersistentFactStorage::recompact_visible_delta()` is private/internal and is
  not called by `checkpoint()`.
- The primitive rejects uncheckpointed pending facts; Vetch receipt cadence must
  checkpoint writes before maintenance.
- Successful recompact preserves full-history rows, scoped retractions, and
  `Value::Ref` edges.
- Successful recompact removes the selected visible delta manifest, so reopened
  readers use the rebuilt base.
- `delta_maintenance_decision()` exposes the selected-manifest threshold
  decision internally without executing maintenance.

New fact found during T9C-A:

- The current full-rebuild writer rewrites fact/index pages in place starting at
  page 1. That is fine for explicit success-path maintenance, but it cannot yet
  prove the T9C crash invariant "crash before recompact header publish preserves
  previous base + manifest." A crash-safe background recompact needs a separate
  publish design before threshold-triggered scheduling is allowed.

New facts fixed during T9C-B:

- `FileBackend::write_page()` used to rewrite disk page 0 when appending a
  non-header page in order to bump `page_count`. That weakened the intended
  "page 0 is the only publish boundary" rule. It now updates only the handle's
  cached `page_count`; durable page 0 changes happen only through explicit
  header writes.
- v10 header extension now records checksum-protected `base_fact_page_start`.
  Older v10 extension tails with zeroed base-start fields default to page 1.
  Copy-on-write recompact publishes a later base start and leaves old base/delta
  pages as ignored garbage.

### Phase T9C-B: Crash-Safe Recompact Publish

Goal: make internal/background recompact safe to schedule automatically once
T9B can identify when it is needed.

Design requirement:

- Do not overwrite pages reachable from the currently published header before a
  durable publish marker can recover or ignore the in-progress recompact.
- Keep the single-file invariant. If scratch/journal pages are needed, they must
  live inside the `.graph` file and have clear recovery semantics.
- Do not call recompact from foreground `checkpoint()` unless a separate
  scheduling policy explicitly allows it.

Acceptance:

- Done: recompact preserves full-history rows, retractions, and `Value::Ref`
  edges.
- Done: recompact removes the visible delta manifest by publishing an empty
  manifest extension plus a new base-start pointer.
- Done: crash before recompact header publish preserves previous base +
  manifest; the unit test writes candidate pages without page 0 publish and
  reopens the previous manifest.
- Done: crash after recompact header publish leaves the new base visible.
- Done: old delta pages may remain as garbage but are ignored by selected
  manifest/base-start state.

Decision after T9:

| Result | Decision |
| --- | --- |
| Hot flush stays cheap and thresholded recompact bounds growth | Keep recompact internal and let Vetch schedule it in background. |
| Recompact is needed by Vetch callers but cannot be scheduled implicitly | Done in Q3-A/Q3-B: expose `run_idle_maintenance()` and record the caller contract; keep raw recompact private. |
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

- Done: just-written receipt as-of reads on a 1M base no longer scan the whole
  base when the query is entity/attribute-bound.
- Done: query semantics remain Datalog-first; no alternate query language was
  added.
- Done: the public API remains unchanged.

### Phase Q2: Streaming and Allocation Cleanup

Goal: reduce memory and buffering costs on export, checkpoint, and recompact
paths after the correctness-critical storage shape is stable.

Candidate work:

- Done in Q2-A: streaming fact-log export over committed base + visible deltas
  before constructing the public `Vec<FactRecord>`.
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
- T9A threshold policy is complete.
- T9B private threshold metrics and decision tests are complete.
- T9C-A private explicit recompact primitive is complete.
- T9C-B crash-safe recompact publish is complete.
- T9C-C background/idle scheduling policy is complete. It adds a private
  `run_idle_delta_maintenance()` caller that executes recompact only for
  scheduled/backpressure decisions, noops for healthy deltas, and preserves the
  pending-facts guard.
- Q3-A public idle maintenance API is complete. It exposes
  `Minigraf::run_idle_maintenance()` as the single embedder call for
  checkpoint-then-delta-maintenance, while keeping raw recompact private and
  foreground `checkpoint()` unchanged.
- Q3-B maintenance caller contract is complete. It adds
  `docs/MAINTENANCE_API_CONTRACT.md` and updates this roadmap so Vetch adoption
  can proceed without another Vicia storage algorithm change.
- Q1-A agent-brief read-path benchmark/spec is complete. It adds
  `benches/agent_brief_read_path_benchmark.rs` and separates current point,
  as-of point, prepared as-of, and export/recent-filter timing.

Known verification caveat:

- `cargo clippy --all-targets -- -D warnings` currently fails on pre-existing
  test/bench `unwrap`, `expect`, `panic`, and indexing lints. Use lib clippy as
  the storage implementation gate unless the all-target lint cleanup lane is
  explicitly active.

## Completed Slice: T9C-C Background Recompact Scheduling Policy Gate

Name: T9C-C background recompact scheduling policy gate.

Objective:

- Give the private threshold decision surface an internal/background caller
  without adding a public API or running recompact in foreground checkpoint.

Scope:

- Use T9C-B crash tests as the safety precondition.
- No broad storage engine dependency.
- No new dependency.
- No public recompact API unless a Vetch scheduling contract is explicit.
- No as-of query optimization in T9C-C; keep that in Q1.
- No foreground `checkpoint()` threshold-triggered recompact.

Done:

- Done: threshold-triggered maintenance moves to a clearly idle/background-only
  private caller.
- Done: healthy deltas return `Noop`.
- Done: scheduled and backpressure decisions call the crash-safe copy-on-write
  recompact primitive.
- Done: pending facts still block maintenance, so Vetch must checkpoint receipt
  writes before idle recompact.
- Done: no public `recompact()` API and no new dependency.
- Done: foreground checkpoint remains on the delta append path.

Stop conditions:

- If a threshold requires foreground full rebuild during normal Vetch work,
  reject it.
- If the policy cannot be expressed without a public API, write the Vetch
  scheduling contract first.
- If crash-safe recompact requires changing where base fact pages start, add a
  file-format design note and migration test before implementation.

## Completed Slice: Q1-A Agent-Brief Read-Path Benchmark Gate

Name: Q1-A agent-brief read-path benchmark/spec gate.

Objective:

- Fix the Vetch agent-brief read surfaces before optimizing them: current point
  query, as-of point query, prepared as-of point query, and full export plus
  recent tx filter.

Scope:

- Add a dedicated benchmark harness only; do not optimize query execution in
  Q1-A.
- Keep the 1M base assumption.
- Keep receipt-like writes as `Value::Ref` facts.
- Keep public API unchanged.
- Include a smoke mode for quick local verification.

Done:

- Done: `cargo bench --bench agent_brief_read_path_benchmark` builds a 1M base
  and measures the Q1 read surfaces.
- Done: `MINIGRAF_AGENT_BRIEF_BENCH_MODE=smoke cargo bench --bench
  agent_brief_read_path_benchmark` runs a 10K-base smoke matrix.
- Done: smoke evidence shows prepared as-of is not meaningfully faster than
  formatted as-of, so parse overhead is not the primary blocker.
- Done: Q1-B should choose between as-of selective pushdown, a recent fact-log
  reader, or a small prepared helper based on full 1M data.

## Completed Slice: Q1-B Agent-Brief Read Strategy

Name: Q1-B agent-brief read strategy decision.

Objective:

- Use Q1-A evidence to pick and implement the smallest change that makes
  just-written receipt reads cheap on a 1M base.

Chosen path:

- Done: entity/attribute-bound `:as-of` queries use the existing selective
  committed-index fetch before temporal filtering.
- Done: rule-using queries remain on the full fact base.
- Done: no public API, file-format, checkpoint, or recompact policy changed.
- Rejected: prepared helper first, because the full 1M Q1-A run showed prepared
  as-of was not materially faster than formatted as-of.
- Deferred: recent fact-log reader, because Q1-B fixed receipt-scoped Datalog
  as-of reads; export/replay can be revisited only if Vetch's brief path still
  needs it.

Evidence:

- Full 1M `single_receipt` as-of p95: `1,257.698 ms` -> `0.017 ms`.
- Full 1M `receipt_stream_100` as-of p95: `1,499.003 ms` -> `0.037 ms`.
- Full 1M `batched_receipts_1000` as-of p95: `1,456.026 ms` -> `0.043 ms`.
- Regression tests fail if entity-bound or attribute-bound as-of queries call a
  committed full scan.

## Completed Slice: Q2-A Export Fact-Log Allocation Cleanup

Name: Q2-A export fact-log allocation cleanup.

Objective:

- Remove the intermediate committed `Vec<Fact>` allocation from
  `export_fact_log()` while preserving the existing public `Vec<FactRecord>`
  API and deterministic base-then-delta ordering.

Done:

- Done: `CommittedFactReader` has an object-safe `for_each_fact` visitor with a
  default `stream_all()` fallback.
- Done: file-backed committed readers stream packed pages through the visitor.
- Done: layered base+delta readers stream base facts first and visible delta
  facts after them.
- Done: `Minigraf::export_fact_log()` builds `FactRecord`s directly from the
  visitor.
- Done: regression coverage fails if the streaming visitor falls back to
  `stream_all()` for the dedicated streaming-only fixture.

Evidence:

- Full 1M export/replay p95 remains latency-neutral in broad terms:
  `197.300-245.555 ms` after Q2-A versus `199.950-234.806 ms` after Q1-B.
- Q2-A is therefore a memory-shape cleanup, not a query-latency fix.

## Completed Gate: S1 Stability And Code-Quality Review

Name: S1 Q1-B/Q2-A stability and code-quality gate.

Objective:

- Recheck the merged Q1-B/Q2-A surface before Q2-B touches recompact/checkpoint
  streaming internals.

Done:

- Reviewed selective `:as-of` pushdown for rule, `not`, `not-join`, `or`, and
  `or-join` surfaces.
- Reviewed committed fact visitor ordering and export semantics.
- Confirmed Q2-B remains a cleanup spike, not a public API or checkpoint
  algorithm change.

Evidence:

- Passed: `cargo test as_of_ --lib`.
- Passed: `cargo test --test fact_log_export_test`.
- Passed: `cargo test --test delta_checkpoint_integration_test`.
- Passed: `cargo test --test delta_checkpoint_crash_recovery_test`.
- Passed: `cargo test --test retraction_test`.
- Passed: `cargo test --test not_join_test`.
- Passed: `cargo test --test disjunction_test`.
- Passed: `cargo test --test production_patterns_test`.
- Passed: `cargo test test_for_each_fact_streams_committed_without_stream_all --lib`.
- Passed: `cargo fmt -- --check`.
- Passed: `cargo test`.
- Passed: `cargo clippy --lib -- -D warnings`.
- Passed: `git diff --check HEAD~1..HEAD`.
- Audit-only: `cargo clippy --all-targets -- -D warnings` still fails on
  pre-existing test-lint debt such as `unwrap/expect/panic/indexing` in tests
  and a pre-existing test-helper `type_complexity` lint.

## Completed Slice: Q2-B Recompact Input Streaming

Name: Q2-B recompact input streaming spike.

Objective:

- Determine whether private recompact can stream visible facts into candidate
  pages and index entries without first materializing
  `self.storage.get_all_facts()`.

Done:

- Done: added `PackedFactPacker`, a streaming packed-page builder that preserves
  the byte layout produced by `pack_facts()`.
- Done: changed `write_recompact_candidate_from_visible_facts()` to use
  `FactStorage::for_each_fact()` instead of `get_all_facts()`.
- Done: recompact builds candidate packed pages and index entries from the
  visitor while keeping copy-on-write publish discipline unchanged: candidate
  pages and indexes first, page 0 publish last.
- Done: full-history identity coverage now includes `valid_from` and `valid_to`
  in the recompact projection.
- Done: added a storage-order fact-log canary for recompact before/after/reopen.
- Done: added a rich packed-page byte-equivalence fixture covering `Value::Ref`,
  scoped retraction, and same-EAV assert/retract shape.

Evidence:

- Passed: `cargo test test_streaming_packer_matches_pack_facts_layout --lib`.
- Passed: `cargo test recompact --lib`.
- Passed: `cargo test --test delta_checkpoint_crash_recovery_test`.
- Passed: `cargo test measure_q2b_recompact_streaming_1m --lib`.
- Passed 1M manual measurement:
  `/usr/bin/time -v cargo test measure_q2b_recompact_streaming_1m --lib -- --ignored --nocapture`.
  The printed recompact-only wall time was `11791.318 ms` for `1,000,001`
  visible facts. End-to-end test process max RSS was `2,186,428 KB`; this
  includes fixture setup, not only the recompact call.
- Peak-memory proxy from the 1M run: candidate fact pages were `14,275` pages,
  or `58,470,400` bytes. The implementation still keeps candidate fact pages
  and sorted EAVT/AEVT/AVET/VAET entry buffers in memory.

Stop conditions:

- No public API change.
- No file-format change.
- No ledger identity, retraction, base/delta/recompact visibility, or foreground
  checkpoint policy change.
- No new dependency.
- No Vetch production-readiness claim: recompact remains private/internal and
  still needs an explicit embedder scheduling contract before production use.

Result:

- Q2-B succeeds as a first cleanup cut. It removes the committed `Vec<Fact>`
  materialization from private recompact input, so the writer no longer holds a
  decoded full fact log solely to pack candidate pages.
- Q2-B does not make recompact bounded-memory. The next deeper cleanup, if
  needed, is a separate page/index streaming writer that writes candidate fact
  pages as they fill and bounds or externalizes sorted index-entry buffers.

## Completed Slice: Q3-A Public Idle Maintenance API

Name: Q3-A public idle maintenance API contract.

Objective:

- Let Vetch schedule storage maintenance without depending on
  `PersistentFactStorage`, `CheckpointOutcome`, or raw recompact internals.

Done:

- Done: added `Minigraf::run_idle_maintenance()`; the `ViciaDb` alias inherits
  the same method.
- Done: the method takes the existing write lock once, checkpoints pending
  WAL-backed writes first, then runs private delta maintenance under the same
  lock.
- Done: public `MaintenanceOutcome` reports stable `checkpoint`, `delta`, and
  `advice` effects without exposing internal `CheckpointOutcome`.
- Done: public maintenance enums are `#[non_exhaustive]`, so later advice or
  effect variants can be added without breaking embedders.
- Done: in-memory databases return a no-op outcome.
- Done: same-thread active `WriteTransaction` returns an error instead of
  deadlocking on the write lock.
- Done: foreground `checkpoint()` still never runs threshold-triggered
  recompact.
- Done: raw `recompact()` remains private; Vetch owns scheduling cadence.

Failure semantics:

- If checkpointing succeeds and later delta maintenance fails, the checkpoint
  remains durable and the WAL is not restored. The error means maintenance
  should be retried on a later idle tick; it does not imply data loss.
- Crash before recompact page-0 publish keeps the previous base plus selected
  delta manifest visible. Candidate pages may remain as ignored file growth.
- `MaintenanceAdvice::ReduceCheckpointCadence` can co-occur with
  `delta = Recompacted` because advice describes the pre-maintenance delta
  state that triggered the fold.

Evidence:

- Added public API tests for in-memory no-op, pending file-write checkpoint,
  checkpoint-then-recompact on threshold delta, convergence on the second idle
  call, and same-thread write-transaction rejection.
- Added a policy guard that a threshold-crossed foreground `checkpoint()` leaves
  the delta manifest for explicit idle maintenance instead of recompact.
- Added fault-injection coverage that a phase-2 recompact failure preserves the
  previously visible delta state and reopen ignores unpublished recompact pages.

Stop conditions:

- Do not add an automatic background thread in Vicia DB. Scheduling belongs to
  the embedder.
- Do not expose raw recompact as a public API until a caller needs that sharper
  lever and can own its failure mode.
- Do not claim recompact is bounded-memory; Q2-B only removed one intermediate
  decoded fact buffer.

## Completed Slice: Q3-B Maintenance Caller Contract

Name: Q3-B maintenance caller adoption contract.

Objective:

- Make the Q3-A public hook actionable for Vetch without adding another storage
  algorithm, background thread, or public raw recompact API.

Done:

- Done: added `docs/MAINTENANCE_API_CONTRACT.md`.
- Done: documented safe caller windows: startup, agent slice boundary,
  import/projection completion, shutdown best effort, and idle/background tick.
- Done: documented forbidden caller behavior: no foreground capture blocking,
  no correctness dependency on maintenance completion, no app-level WAL/page
  repair after maintenance errors.
- Done: documented outcome semantics for checkpoint, delta, and advice fields.
- Done: documented phase-2 error semantics: a checkpoint can remain durable even
  if later delta maintenance fails; callers should retry maintenance later.
- Done: updated stale roadmap language that still treated a public maintenance
  API as a future consideration.

Stop conditions:

- Do not implement Vetch/vetch-memoryd caller code in this repository.
- Do not add automatic scheduling to Vicia DB core.
- Do not add raw public `recompact()`.
- Do not claim file-space reclamation or bounded-memory recompact is solved.

Next gate:

- Vetch should prove a real caller: invoke `run_idle_maintenance()` outside the
  capture/write foreground path, record `ReduceCheckpointCadence`, change
  batching/backoff policy, and retry visible maintenance errors from a later
  idle tick.
