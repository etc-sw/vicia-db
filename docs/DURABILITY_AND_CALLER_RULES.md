# Durability and Caller Rules

Per-backend durability semantics (gap G13) and the browser caller rules from
slice A5 of `docs/APP_ADOPTION_GAP_PLAN.md`. This is the authority for what
`execute` / `executeAtomic` / `checkpoint` / `backup_to` /
`exportGraphAsync` / `importGraph` / `importGraphForPagedAccess` /
`runIdleMaintenance` guarantee **at the moment they return**, per backend,
and for the rules a browser caller must follow because of those semantics. The
session-protocol view of the same facts (the
`durability` field on result frames) is `docs/SESSION_PROTOCOL.md`
"Durability classification"; the two documents must not diverge.

Backends covered:

- **Native file-backed** — `Minigraf::open("path.graph")`: single `.graph`
  file + WAL sidecar. What harrekki runs.
- **Browser** — eager-compatible `BrowserDb.open(dbName)` or bounded-v11
  `BrowserDb.openPaged(dbName)` (wasm): IndexedDB-backed, write-through, no
  WAL. Vetch main `1b57689` uses the paged path for foreground authority.
- In-memory databases (native `Minigraf::new()`, browser
  `BrowserDb.openInMemory()`) have **no durability**; nothing below
  applies to them.

## 1. What returns mean, per backend

| Event | Native file-backed | Browser (IndexedDB) |
| --- | --- | --- |
| `execute` (write) returns Ok | Facts are in the WAL (**fsynced**) and in memory. Survives kill -9 / power loss via replay. = `applied`. | All dirty pages committed in **one** IndexedDB readwrite transaction. Survives tab close and browser crash; **not** guaranteed against power loss / OS crash (see below). No WAL tier exists. |
| `BrowserDb.executeAtomic(commands)` returns Ok | Not exposed; native callers use one `transact` or `retract` command per WAL transaction. | Every command was parsed and materialized before mutation; all facts share one `tx_id` / `tx_count`, and the exact dirty-page set committed in **one** IndexedDB readwrite transaction. |
| `checkpoint()` returns Ok | Committed image durable (data synced before the header publish), WAL retired. = `published`. | Same flush as `execute`'s write-through; only needed after bulk operations. |
| `backup_to()` / session `backup` returns Ok | Source checkpoint complete; a fresh independent destination contains exactly returned `tx_count`, is fsynced, and was atomically published without overwrite while the same write lock remained held. | Not applicable; browser portability uses atomic export/import. |
| idle maintenance returns Ok | Pending writes are checkpointed first; threshold delta may be copy-on-write recompacted. | Threshold delta was either healthy (`noop`) or rebuilt as a fresh contiguous graph and atomically replaced in IndexedDB. |
| Handle drop / tab close | Best-effort checkpoint on `Drop` (errors swallowed). Un-checkpointed WAL replays on next open. | Whatever the last committed IndexedDB transaction wrote. Nothing in flight survives partially (single tx). |
| Crash mid-write | The in-flight entry has a bad CRC32 and is discarded on replay; every *acknowledged* write survives. | The in-flight IndexedDB transaction rolls back whole; reopen shows the previous consistent state. |

### Native: WAL-first

- A write `execute()` appends the transaction to the WAL sidecar and
  **fsyncs on every append** (`WalWriter::append_entry`, `src/wal.rs`)
  *before* facts apply to the in-memory store (`Minigraf::execute`,
  `src/db.rs`). If the WAL write fails, nothing was applied — the database
  is unchanged and consistent. A partial entry from a crash mid-append
  fails its CRC32 and replay stops at it: only unacknowledged work is lost.
- `checkpoint()` publishes the committed image with data pages synced
  before the header referencing them (publish/recovery ordering:
  `docs/DELTA_INDEX_DESIGN.md`). The WAL is deleted only after an outcome
  that actually published (`CheckpointOutcome::permits_wal_retire`,
  `src/storage/persistent_facts.rs`).
- Verified continuously by the A7 kill -9 harness
  (`docs/BENCHMARKS.md` "A7: Crash Safety Under kill -9").
- `backup_to()` is stronger than `checkpoint()` followed by caller-side copy:
  it retains the same write lock through an exact published-prefix copy,
  destination fsync, no-clobber publish, and Unix parent-directory fsync.
  The WAL and unpublished physical tail pages are excluded. A failure after
  source checkpoint may leave that checkpoint durable, but never returns a
  published backup receipt; existing destination/sidecar paths are untouched.
  The linearization domain is one daemon-owned `Minigraf` handle and its
  clones. Independently opening the same source pathname is not a second
  writer mode; all access must route through the owner.

### Browser: write-through, no WAL

- A write `execute()` applies facts in memory, runs the internal page-level
  save (delta segment + manifest rewrite), then flushes all dirty pages in
  a **single IndexedDB readwrite transaction** and awaits its `complete`
  event before resolving (`BrowserDb::apply_write`, `src/browser/mod.rs` →
  `IndexedDbBackend::write_pages`, `src/browser/indexeddb.rs`). One
  `execute` = one atomic durable step; there is no torn page set from a tab
  dying mid-write.
- **What "committed" buys you**: Vicia passes no durability hint, so the
  transaction uses the browser default. Since Chrome 121 (Jan 2024) that
  default is `relaxed` (matching Firefox/Safari behavior): `complete` fires
  once data reaches the OS write buffer, not the disk platter. Tab close
  and browser crash are safe; power failure or OS crash within the OS flush
  window (seconds) can lose the latest transactions. See
  [the Chrome announcement](https://developer.chrome.com/blog/indexeddb-durability-mode-now-defaults-to-relaxed).
  There is no browser equivalent of the native WAL fsync tier.
- Mapping to the session-protocol classification: browser has **no
  `applied` tier** (no WAL) — a resolved `execute` is directly at
  IndexedDB-committed, the browser's strongest available tier. Successful
  write JSON now includes `tx_id`, deterministic `tx_count`,
  `durability = "published"`, `maintenance_pending`, and `advice`.
- `executeAtomic(commands)` is the browser boundary for one logical write
  that needs both `transact` and `retract`. It accepts 1–256 write-only
  commands, preflights at most 262,144 facts and 64 MiB of Datalog source,
  rejects query/rule/forget commands and repeated entity/attribute/value
  facts, then assigns one transaction identity to the fully materialized
  batch. A parse, preflight, materialization, resource-limit, or duplicate
  error occurs before mutation. An IndexedDB abort follows the same durable
  reload-or-poison rule as `execute()`; no command prefix is published.

| Browser write result | Meaning | Caller action |
| --- | --- | --- |
| `maintenance_pending = false`, `advice = "none"` | Delta growth is healthy. | Continue normal batching. |
| `maintenance_pending = true`, `advice = "schedule_idle_maintenance"` | Soft threshold crossed. | End the foreground handle scope and schedule a disposable worker that acquires the same Web Lock, runs maintenance, reports, and terminates. |
| `maintenance_pending = true`, `advice = "reduce_checkpoint_cadence"` | Hard threshold crossed. | Increase batch size/backoff and run maintenance before resuming the prior cadence. |
| `durability = "noop"`, null `tx_id`/`tx_count` | A `forget` matched no open fact and consumed no transaction. | Treat as successful idempotent no-op; no maintenance action. |

- **Flush-failure rule (load-bearing)**: a failed IndexedDB readwrite
  transaction is followed by a reload of the previous durable page image.
  If that reload succeeds, the rejected operation is absent from both live
  queries and reopen, and the handle remains usable. If IndexedDB itself can
  no longer be read (closed/broken connection), the handle becomes explicitly
  poisoned: query, write, export, import, checkpoint, and maintenance reject
  until the caller discards the handle and reopens. No later operation can
  promote the uncertain in-memory image.
- While a mutation is awaiting its IndexedDB outcome, that same handle rejects
  queries and exports as well as other mutations. An unacknowledged write can
  therefore never become an observable read or portable graph image before
  the commit succeeds.
- `importGraph` is atomic: the blob is validated and built into a
  replacement store first, the durable replacement commits as a single
  IndexedDB `clear`+`put` transaction, and only then does the live handle
  swap. On any failure (invalid blob, quota abort, IndexedDB error) both
  the queryable and durable state remain the old database. Locked by six
  wasm tests in `src/browser/`. It intentionally retains the native-compatible
  policy where a physically truncated newest candidate may select an older
  valid manifest, even when the recovered physical image is non-exportable.
- `importGraphForPagedAccess` uses the same construction, migration, atomic
  replace, and live-swap pipeline, then adds a pre-publish gate: the resulting
  candidate must be complete current-format v11 authority accepted by the
  sparse planner. A complete v10 blob migrates and succeeds; non-exportable
  previous-manifest recovery rejects while the exact live and IndexedDB state
  remains unchanged. Use this method for a cutover that will next call
  `openPaged()`.
- `runIdleMaintenance` follows the same durable-replace ordering as import:
  build a fresh compact image from the complete fact log, commit one
  IndexedDB `clear`+`put` transaction, then swap the live PFS. Paged handles
  return to sparse residency after the swap. A rejected replacement leaves
  the previous live and durable graph untouched.
- Every independent IndexedDB handle pins the exact page-0 bytes observed at
  open. Sparse reads observe page 0 in the same readonly transaction as their
  requested pages; writes and complete replacements compare page 0 before
  queueing mutations in one readwrite transaction. A newer publication makes
  the older handle fail with a reopen error instead of mixing generations.
  Cheap clones share their own successful authority advances. Page 0 is the
  compare-and-swap authority; no browser-only schema/metadata record exists.
- `open()` eagerly retains the complete published image so synchronous
  `exportGraph()` remains compatible. `openPaged()` starts from bounded v11
  metadata and demand-loads verified pages. Its portability API is
  `exportGraphAsync()`, which walks the complete published prefix through the
  verifier without requiring every page to remain resident.

## 2. Failure and corruption classification

Native error message catalog: `docs/ERROR_REFERENCE.md` (browser binding
errors are out of its scope — this table is the browser classification).

| Op | Failure | Backend | State afterwards |
| --- | --- | --- | --- |
| `open` | Lock held by a live process | native | No handle. `.graph.lock` sidecar is hard-link-atomic; stale locks (dead PID, empty artifact) are removed automatically (`FileLock::acquire`, `src/storage/backend/file.rs`). |
| `open` | Header checksum mismatch / bad magic / unsupported version | both | No handle; detected **at open**, not lazily (`src/storage/persistent_facts.rs` load path). The file is not modified. Browser reaches the same validation via `PersistentFactStorage::new` over the loaded pages. |
| `open` / `openPaged` / first committed read | v11 catalog/descriptor corruption, or a base fact/index page checksum mismatch | both | Catalog metadata corruption rejects open without rewriting the image. Base pages are verified lazily against their generation and absolute page id, so `openPaged` can succeed and the first read touching a corrupt page returns an error; eager `open` may encounter the same error during prefetch. Browser asynchronous export and native backup use the same verified boundary. CRC32 detects accidental corruption; it does not authenticate hostile bytes. |
| `open` / `openPaged` | Automatic v10→v11 migration cannot commit | browser | No handle. Catalog pages and page 0 share one IndexedDB transaction; abort preserves the exact v10 image for retry. |
| `open` | Non-empty file shorter than one page | native | No handle and no rewrite. A zero-byte path remains an intentional new-database creation surface; 1–4095 bytes are a visible truncation error. |
| `open` / `importGraph` | Newest slot, manifest, or segment is corrupt while the previous manifest is valid | both | Opens on the previous complete manifest. The shared Gate E corpus verifies that base plus both earlier deltas remain visible and only the newest retraction is absent. |
| `open` / `importGraph` | A selected older segment or both manifest slots are corrupt | both | No handle/replacement; base-only or plausible partial fallback is forbidden. |
| `open` | WAL header invalid | native | No handle; the main file is untouched. |
| `open` | WAL entry CRC mismatch | native | Opens; replay **stops silently at the first bad entry** (`src/wal.rs` replay loop). Only never-acknowledged work is absent. |
| `open` | IndexedDB unavailable / blocked | browser | No handle; nothing modified. |
| paged read / write / import / maintenance / export | Another independent handle published a different page 0 | browser | The stale operation rejects with a reopen error before returning mixed-generation data or committing bytes. Already resident old pages remain one old snapshot; a later IndexedDB demand cannot cross into the new image. Discard and reopen the handle. |
| `execute` | Parse / execution error | both | Rejected — nothing applied, nothing flushed. |
| `executeAtomic` | Empty/oversized batch, non-write command, parse/materialization error, or duplicate EAV fact | browser | Rejected during preflight — no transaction identity allocated, no live mutation, and no IndexedDB publication. |
| `execute` | Fact exceeds `MAX_FACT_BYTES` (4080 B) | both | Rejected at serialization; store payloads externally, keep pointers (gap G4 policy). |
| `execute` | WAL append fails (I/O) | native | Rejected — memory unchanged, database consistent. |
| `execute` / `executeAtomic` / `checkpoint` | IndexedDB flush fails (quota, I/O) | browser | Durable state = old. Successful durable reload restores the live handle and rejects the operation; unreadable durable state poisons the whole handle until reopen. |
| `checkpoint` | I/O failure | native | WAL retained, pending facts retained; safe to retry. Lock-poisoned errors indicate a panicked writer thread — treat the process as needing restart. |
| `importGraph` | Blob shorter than page 0 / unparseable with no valid predecessor | browser | Rejected before any durable or live change. A trailing partial page is treated like native open: only complete pages enter recovery, so an interrupted newest candidate may fall back to the previous manifest. A physically missing page inside the declared prefix keeps `exportGraph` unavailable until a clean repair/maintenance image exists. |
| `importGraphForPagedAccess` | Recovery selects a legacy or physically incomplete image | browser | Rejected before the IndexedDB transaction and before the live swap. Complete legacy input may migrate to v11, but the post-construction image must pass the bounded sparse-authority planner. The prior live handle and exact durable page map remain unchanged. |
| `importGraph` | IndexedDB replace fails | browser | The single replace transaction rolls back; memory and IndexedDB both still the old database. |
| `runIdleMaintenance` | Compact build or IndexedDB replace fails | browser | Rejected; memory and IndexedDB both remain the previous graph. Retry in a later worker/idle window. |
| `exportGraph` | Called on a sparse handle without a fully resident published prefix | browser | Rejects visibly; call `await exportGraphAsync()` for the supported verified sparse export. No durable state changes. |

## 3. Canonical value encoding

Native session frames and BrowserDb query results now use one shared lossless
encoder (`src/json_value.rs`):

| `Value` | JSON |
| --- | --- |
| `String` | string |
| `Integer` | number |
| `Float` (finite) | number |
| `Float` (non-finite) | `{"$float": "nan"\|"inf"\|"-inf"}` |
| `Boolean` | bool |
| `Null` | null |
| `Ref(uuid)` | `{"$ref": "<uuid>"}` |
| `Keyword` | `{"$kw": ":a/b"}` |

The planned browser transition is complete. `Ref` and `Keyword` no longer
flatten into ambiguous strings, and non-finite floats no longer become `null`.
The Gate E 2×2 producer/consumer matrix runs native- and Chrome-generated v10
fixtures through both native and BrowserDb, comparing exact tagged current,
`:as-of`, valid-time, combined-time, retraction, and VAET-join results.

## 4. Browser caller rules

Derived from the semantics above plus the A5 growth measurements
(`docs/BENCHMARKS.md` "A5: Browser IndexedDB Growth").

1. **One writer per DB name, via Web Locks.** There is no browser analogue
   of the native `.graph.lock`; two tabs opening the same DB name are two
   independent in-memory stores. Exact page-0 comparison now makes the stale
   writer reject instead of interleaving generations, but it is a safety
   boundary, not a work scheduler or retry protocol. `BrowserDb` does not (and
   by design will not) choose which tab owns writes; single-writer discipline
   remains caller policy. Wrap every writing handle's
   lifetime in a Web Lock, which the browser releases automatically when
   the tab dies (unlike a lock file):

   ```js
   await navigator.locks.request(`vicia:${dbName}`, async (lock) => {
     const db = await BrowserDb.openPaged(dbName);
     // ... entire writing session while the lock is held ...
   });
   ```

   Read-only tabs that never call `execute` with writes / `importGraph`
   can open without the lock. Eager `open()` sees the snapshot loaded at open
   time. `openPaged()` keeps already resident pages on that old snapshot and
   rejects the next IndexedDB miss after another handle publishes, so it never
   combines generations; reopen on that stale-authority error.

2. **Batch, then debounce.** Between maintenance windows every write
   `execute()` appends a delta segment and rewrites the manifest, so
   per-commit latency rises with segment count. Therefore: put same-kind facts
   in one `transact` or `retract` command; when one authority transition
   needs both kinds, use one bounded `executeAtomic` call. Both forms produce
   one `tx_count`, one segment, and one IndexedDB transaction. Debounce
   high-frequency sources app-side — commit on gesture end, never per frame.
   Successful write results expose `maintenance_pending` and `advice`; use
   those fields instead of guessing from commit count.

3. **Give O(total) browser work a disposable worker lifetime.** React to write
   advice at startup/import/slice/idle boundaries. End use of the foreground
   handle, launch a DedicatedWorker that acquires the same caller-owned Web
   Lock, opens with `openPaged()`, calls `runIdleMaintenance()`, posts its
   outcome, and terminates after either success or failure. The next foreground
   operation reopens through `openPaged()`. Maintenance no-ops below threshold
   and atomically reclaims superseded page records after soft/hard pressure.
   Initial import, `exportGraphAsync()`, and the first `openPaged()` of a legacy
   v10 database use the same disposable-worker rule. That migration temporarily
   loads the legacy published image before committing v11.
   The 100K maintained-growth gate proves repeated reclaim and latency reset;
   `bench-driver.cjs worker-smoke` proves the binding works in a real module
   worker. A5-6d completes the recorded-host 1M matrix: foreground open/query/
   write stays sparse, but import/export/maintenance add 2.55/1.04/2.09 GiB of
   200 ms sampled process-tree PSS. Export retains 1.04 GiB and maintenance
   retains 1.27 GiB when the call returns; the harness then closes the browser
   process. A long-lived authority worker is therefore not the intended
   reclamation boundary. Vetch main `1b57689` implements this exact boundary
   against clean Vicia `e60a7c2`: foreground `openPaged()`, one shared Web
   Lock, bounded `executeAtomic` authority publications, disposable
   migration/import/export/maintenance, outcome receipts, worker termination,
   and fresh reopen. Its real-Chrome caller smoke closes Gate E.
