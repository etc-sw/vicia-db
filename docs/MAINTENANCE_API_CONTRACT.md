# Maintenance API Contract

Status: Q3-B native contract, A5-4 browser atomic compact maintenance, and the
A5-6d 1M disposable-worker lifecycle boundary.

`Minigraf::run_idle_maintenance()` and browser
`BrowserDb.runIdleMaintenance()` are the public maintenance hooks in this
line. They exist so an embedding application can move recompact work out of
foreground receipt capture without depending on `PersistentFactStorage`,
`CheckpointOutcome`, or raw recompact internals.

This document is caller guidance, not a new storage algorithm. It does not add
background threads, sidecar files, a server mode, a new query surface, or a raw
public `recompact()` API.

## Scope

The native maintenance hook may:

- checkpoint pending WAL-backed writes
- fold visible delta segments into a fresh copy-on-write base when private
  threshold policy says maintenance is needed
- return caller advice about checkpoint cadence pressure

The browser hook may:

- no-op while selected delta growth remains below threshold
- stream the complete fact log into a fresh contiguous graph image at threshold
- atomically replace IndexedDB before swapping the live handle
- return native-compatible outcome vocabulary plus before/after page counts

The maintenance hook must not:

- run automatically from foreground `checkpoint()`
- block human capture or receipt append paths as a normal operating mode
- define Vetch replay eligibility or strict-before event boundaries
- make recompact bounded-memory; Q2-B only removed one intermediate decoded
  fact buffer
- reclaim old ignored pages on the native file backend; native file-space
  reclamation remains a separate future phase

Browser maintenance intentionally does reclaim obsolete IndexedDB records.
Native copy-on-write recompact cannot do this because it appends its new base
after old pages; BrowserDb instead builds a fresh page-1 image and commits one
atomic `clear`+`put` replacement.

## Caller Windows

Vetch should call `run_idle_maintenance()` only from windows where a full
copy-on-write recompact would be acceptable if thresholds are crossed:

| Window | Recommended use |
| --- | --- |
| Startup after opening the graph | Optional, if previous session ended with visible delta growth. |
| Agent slice boundary | Recommended when no write transaction is active and user capture is not waiting. |
| Import/projection rebuild completion | Recommended after bulk source or projection writes. |
| Shutdown before process exit | Optional best effort; never required for durability. |
| Idle/background tick | Recommended default for long-running daemons. |

Do not call the hook while a `WriteTransaction` is active on the same thread.
The API rejects that case to avoid deadlock. Cross-thread callers serialize
behind the normal write lock.

For BrowserDb, keep foreground writes behind the caller-owned Web Lock, but do
not run O(total) maintenance inside a long-lived authority worker. After write
advice requests maintenance, end use of the foreground handle. A disposable
DedicatedWorker acquires the same lock, opens through `openPaged()`, runs
maintenance, posts the outcome, and terminates after either success or failure;
the next foreground operation reopens a fresh paged handle. The same-handle
guard still rejects every read, export, or second mutation while a durability
mutation is in flight; cross-handle and cross-tab writer ownership remains
Vetch policy.

## Outcome Semantics

`run_idle_maintenance()` returns `Result<MaintenanceOutcome>`.

| Field | Meaning | Caller action |
| --- | --- | --- |
| `checkpoint = Noop` | No durable page-0 publish was needed before maintenance. | No action. |
| `checkpoint = Published` | Pending or replayed WAL-backed writes were durably published. | WAL-backed writes covered by the checkpoint can be treated as durable. |
| `delta = Noop` | No delta recompact ran. | Continue normal cadence. |
| `delta = Recompacted` | Visible delta segments were folded into a fresh base. | Continue normal cadence; expect old delta pages to remain as ignored file growth. |
| `advice = None` | Private policy saw no hard cadence pressure before maintenance. | No cadence change needed. |
| `advice = ReduceCheckpointCadence` | Private policy saw hard threshold pressure before maintenance. | Batch checkpoints more aggressively and prioritize later idle maintenance. |

`ReduceCheckpointCadence` can co-occur with `delta = Recompacted`. Advice
describes the pre-maintenance delta state that triggered the fold, not
necessarily the post-call state.

`BrowserDb.runIdleMaintenance()` resolves to JSON with lower-case equivalents:

```json
{
  "checkpoint": "noop",
  "delta": "noop|recompacted",
  "advice": "none|reduce_checkpoint_cadence",
  "before_pages": 14984,
  "after_pages": 12876
}
```

Browser writes are already write-through, so normal browser maintenance reports
`checkpoint = "noop"`. Replacement failure changes neither the live handle nor
IndexedDB and can be retried later.

## Error Semantics

Maintenance errors are visible failures, not silent drift.

- If checkpointing fails, the caller should treat the write as not durably
  checkpointed and retry later or surface the storage error.
- If checkpointing succeeds and later delta maintenance fails, the checkpoint
  remains durable and the WAL is not restored. The caller should retry
  maintenance on a later idle tick; the error does not imply data loss.
- A crash before recompact page-0 publish leaves the previous base plus selected
  delta manifest visible. Candidate pages may remain as ignored file growth.
- A crash after recompact page-0 publish leaves the new base visible and old
  delta pages ignored.

The caller must not try to repair files by deleting WAL or truncating graph
pages after a maintenance error. File repair belongs to explicit storage
recovery logic.

## Vetch Scheduling Policy

Recommended Vetch policy:

1. Append receipts immediately through normal writes.
2. Batch `checkpoint()` by receipt group, agent slice, or import batch when
   product latency allows it.
3. Call `run_idle_maintenance()` only from idle/background/slice-boundary
   windows.
4. If `ReduceCheckpointCadence` appears, increase checkpoint batching and
   schedule another idle maintenance pass.
5. If maintenance returns an error, keep capture available, record the error,
   and retry from a later idle tick.
6. In the browser, run the O(total-history) rebuild in a disposable worker,
   reserve quota headroom for the old and candidate images during atomic
   replacement, post an outcome, terminate after either result, and reopen
   through `openPaged()`.

Forbidden policy:

- Do not run maintenance synchronously inside the human capture path.
- Do not make Vetch correctness depend on maintenance completing.
- Do not interpret stale briefs as storage data loss; brief freshness remains a
  Vetch projection/index concern.

## Verification Surface

The Q3-A implementation pins this contract with focused coverage:

- in-memory maintenance returns no-op
- file-backed maintenance checkpoints pending writes and retires WAL after
  durable publish
- threshold-crossed visible delta can checkpoint pending writes and recompact
  in one idle call
- a second idle call converges to no-op
- same-thread active `WriteTransaction` is rejected
- foreground `checkpoint()` does not run hidden recompact
- fault-injected phase-2 recompact failure preserves the previous visible delta
  and reopen ignores unpublished candidate pages

The A5-4 browser implementation additionally pins:

- compact copies preserve all history identity fields, Ref values,
  retractions, and a tx watermark newer than the last fact
- replacement removes stale page records and exports one contiguous image
- current and valid-time history survive replacement and reopen
- replacement failure preserves the previous live and durable graph
- same-handle reads, exports, and second mutations cannot overlap maintenance
- unreadable failed-write handles cannot promote uncertain memory through
  maintenance or export
- four 100K-base soft-threshold cycles reclaim pages and reset write latency

A5-6d adds one self-checking 1M threshold cycle on Chrome 150. Foreground
`openPaged()` measured a 17.8 ms five-run maximum and 51.1 MiB maximum sampled
PSS delta; maintenance took 16.679 s, peaked at a 2.09 GiB sampled PSS delta,
and retained 1.27 GiB when the call returned. This is evidence for the
disposable-worker lifetime, not proof that a particular runtime has reclaimed
memory after termination and not a general device-memory floor.

Future caller integration should add Vetch-side evidence for:

- daemon idle tick invokes the hook outside capture
- `ReduceCheckpointCadence` changes batching/backoff policy
- maintenance errors are surfaced and retried without losing writes
- startup/shutdown/import windows do not block interactive receipt capture
- the browser worker acquires the same Web Lock, posts a success or failure
  outcome, terminates, and a fresh `openPaged()` handle reopens the authority
