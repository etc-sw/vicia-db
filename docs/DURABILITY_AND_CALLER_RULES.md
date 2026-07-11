# Durability and Caller Rules

Per-backend durability semantics (gap G13) and the browser caller rules from
slice A5 of `docs/APP_ADOPTION_GAP_PLAN.md`. This is the authority for what
`execute` / `checkpoint` / `backup_to` / `importGraph` / `runIdleMaintenance` guarantee **at the moment they
return**, per backend, and for the rules a browser caller must follow because
of those semantics. The session-protocol view of the same facts (the
`durability` field on result frames) is `docs/SESSION_PROTOCOL.md`
"Durability classification"; the two documents must not diverge.

Backends covered:

- **Native file-backed** — `Minigraf::open("path.graph")`: single `.graph`
  file + WAL sidecar. What harrekki runs.
- **Browser** — `BrowserDb.open(dbName)` (wasm): IndexedDB-backed,
  write-through, no WAL. What Vetch would run.
- In-memory databases (native `Minigraf::new()`, browser
  `BrowserDb.openInMemory()`) have **no durability**; nothing below
  applies to them.

## 1. What returns mean, per backend

| Event | Native file-backed | Browser (IndexedDB) |
| --- | --- | --- |
| `execute` (write) returns Ok | Facts are in the WAL (**fsynced**) and in memory. Survives kill -9 / power loss via replay. = `applied`. | All dirty pages committed in **one** IndexedDB readwrite transaction. Survives tab close and browser crash; **not** guaranteed against power loss / OS crash (see below). No WAL tier exists. |
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

| Browser write result | Meaning | Caller action |
| --- | --- | --- |
| `maintenance_pending = false`, `advice = "none"` | Delta growth is healthy. | Continue normal batching. |
| `maintenance_pending = true`, `advice = "schedule_idle_maintenance"` | Soft threshold crossed. | Schedule `runIdleMaintenance()` in the next worker/idle window while retaining the Web Lock. |
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
  wasm tests in `src/browser/`.
- `runIdleMaintenance` follows the same durable-replace ordering as import:
  build a fresh compact image from the complete fact log, commit one
  IndexedDB `clear`+`put` transaction, then swap the live PFS. A rejected
  replacement leaves the previous live and durable graph untouched.

## 2. Failure and corruption classification

Native error message catalog: `docs/ERROR_REFERENCE.md` (browser binding
errors are out of its scope — this table is the browser classification).

| Op | Failure | Backend | State afterwards |
| --- | --- | --- | --- |
| `open` | Lock held by a live process | native | No handle. `.graph.lock` sidecar is hard-link-atomic; stale locks (dead PID, empty artifact) are removed automatically (`FileLock::acquire`, `src/storage/backend/file.rs`). |
| `open` | Header checksum mismatch / bad magic / unsupported version | both | No handle; detected **at open**, not lazily (`src/storage/persistent_facts.rs` load path). The file is not modified. Browser reaches the same validation via `PersistentFactStorage::new` over the loaded pages. |
| `open` | Non-empty file shorter than one page | native | No handle and no rewrite. A zero-byte path remains an intentional new-database creation surface; 1–4095 bytes are a visible truncation error. |
| `open` / `importGraph` | Newest slot, manifest, or segment is corrupt while the previous manifest is valid | both | Opens on the previous complete manifest. The shared Gate E corpus verifies that base plus both earlier deltas remain visible and only the newest retraction is absent. |
| `open` / `importGraph` | A selected older segment or both manifest slots are corrupt | both | No handle/replacement; base-only or plausible partial fallback is forbidden. |
| `open` | WAL header invalid | native | No handle; the main file is untouched. |
| `open` | WAL entry CRC mismatch | native | Opens; replay **stops silently at the first bad entry** (`src/wal.rs` replay loop). Only never-acknowledged work is absent. |
| `open` | IndexedDB unavailable / blocked | browser | No handle; nothing modified. |
| `execute` | Parse / execution error | both | Rejected — nothing applied, nothing flushed. |
| `execute` | Fact exceeds `MAX_FACT_BYTES` (4080 B) | both | Rejected at serialization; store payloads externally, keep pointers (gap G4 policy). |
| `execute` | WAL append fails (I/O) | native | Rejected — memory unchanged, database consistent. |
| `execute` / `checkpoint` | IndexedDB flush fails (quota, I/O) | browser | Durable state = old. Successful durable reload restores the live handle and rejects the operation; unreadable durable state poisons the whole handle until reopen. |
| `checkpoint` | I/O failure | native | WAL retained, pending facts retained; safe to retry. Lock-poisoned errors indicate a panicked writer thread — treat the process as needing restart. |
| `importGraph` | Blob shorter than page 0 / unparseable with no valid predecessor | browser | Rejected before any durable or live change. A trailing partial page is treated like native open: only complete pages enter recovery, so an interrupted newest candidate may fall back to the previous manifest. A physically missing page inside the declared prefix keeps `exportGraph` unavailable until a clean repair/maintenance image exists. |
| `importGraph` | IndexedDB replace fails | browser | The single replace transaction rolls back; memory and IndexedDB both still the old database. |
| `runIdleMaintenance` | Compact build or IndexedDB replace fails | browser | Rejected; memory and IndexedDB both remain the previous graph. Retry in a later worker/idle window. |

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
   independent in-memory stores write-through-flushing into one IndexedDB
   store — last flush wins, manifests interleave, corruption follows.
   `BrowserDb` does not (and by design will not) coordinate this;
   single-writer discipline is caller policy. Wrap every writing handle's
   lifetime in a Web Lock, which the browser releases automatically when
   the tab dies (unlike a lock file):

   ```js
   await navigator.locks.request(`vicia:${dbName}`, async (lock) => {
     const db = await BrowserDb.open(dbName);
     // ... entire writing session while the lock is held ...
   });
   ```

   Read-only tabs that never call `execute` with writes / `importGraph`
   can open without the lock, but see a snapshot loaded at open time.

2. **Batch, then debounce.** Between maintenance windows every write
   `execute()` appends a delta segment and rewrites the manifest, so
   per-commit latency rises with segment count. Therefore: put
   multi-statement work in **one** `execute` (one
   `tx_count`, one segment, one IndexedDB transaction), and debounce
   high-frequency sources app-side — commit on gesture end, never per
   frame. Successful write results expose `maintenance_pending` and `advice`;
   use those fields instead of guessing from commit count.

3. **Schedule browser maintenance in a worker.** Call
   `runIdleMaintenance()` at startup/import/slice/idle boundaries while the
   caller-owned Web Lock is held. It no-ops below threshold and atomically
   reclaims superseded page records after soft/hard pressure. The rebuild is
   O(total history), synchronous WASM work; run the writing BrowserDb inside
   a dedicated worker so maintenance cannot block the main UI. The 100K
   maintained-growth gate proves repeated reclaim and latency reset. The
   binding now discovers IndexedDB through `globalThis`; the repeatable
   `bench-driver.cjs worker-smoke` gate passes open/write/query/maintenance in
   a real module DedicatedWorker. The
   existing 1M full-load open shape (~420 MB per tab), page-local base
   integrity, and 1M maintenance peak-memory proof remain Gate E blockers; do
   not claim browser authority readiness until those bounded-storage gates are
   complete.
