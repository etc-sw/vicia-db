# Durability and Caller Rules

Per-backend durability semantics (gap G13) and the browser caller rules from
slice A5 of `docs/APP_ADOPTION_GAP_PLAN.md`. This is the authority for what
`execute` / `checkpoint` / `importGraph` guarantee **at the moment they
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
  IndexedDB-committed, the browser's strongest available tier.
  `maintenance_pending` is currently **unreachable**: the delta growth
  thresholds are computed but nothing in the browser can act on them
  (measured: `docs/BENCHMARKS.md` "A5: Browser IndexedDB Growth").
- **Flush-failure rule (load-bearing)**: if a write `execute()` or
  `checkpoint()` rejects on the IndexedDB flush, the in-memory handle is
  *ahead* of durable state, and the unflushed pages have already left the
  dirty set (`apply_write` drains `take_dirty()` before the await). A
  *later successful* write on the same handle would commit a manifest that
  references pages never durably written — torn durable state discovered
  only at next open. **Discard the handle and reopen from IndexedDB after
  any flush error.** Reads on the poisoned handle remain consistent
  (memory is a superset); writes are what must stop.
- `importGraph` is atomic: the blob is validated and built into a
  replacement store first, the durable replacement commits as a single
  IndexedDB `clear`+`put` transaction, and only then does the live handle
  swap. On any failure (invalid blob, quota abort, IndexedDB error) both
  the queryable and durable state remain the old database. Locked by six
  wasm tests in `src/browser/`.

## 2. Failure and corruption classification

Native error message catalog: `docs/ERROR_REFERENCE.md` (browser binding
errors are out of its scope — this table is the browser classification).

| Op | Failure | Backend | State afterwards |
| --- | --- | --- | --- |
| `open` | Lock held by a live process | native | No handle. `.graph.lock` sidecar is hard-link-atomic; stale locks (dead PID, empty artifact) are removed automatically (`FileLock::acquire`, `src/storage/backend/file.rs`). |
| `open` | Header checksum mismatch / bad magic / unsupported version | both | No handle; detected **at open**, not lazily (`src/storage/persistent_facts.rs` load path). The file is not modified. Browser reaches the same validation via `PersistentFactStorage::new` over the loaded pages. |
| `open` | WAL header invalid | native | No handle; the main file is untouched. |
| `open` | WAL entry CRC mismatch | native | Opens; replay **stops silently at the first bad entry** (`src/wal.rs` replay loop). Only never-acknowledged work is absent. |
| `open` | IndexedDB unavailable / blocked | browser | No handle; nothing modified. |
| `execute` | Parse / execution error | both | Rejected — nothing applied, nothing flushed. |
| `execute` | Fact exceeds `MAX_FACT_BYTES` (4080 B) | both | Rejected at serialization; store payloads externally, keep pointers (gap G4 policy). |
| `execute` | WAL append fails (I/O) | native | Rejected — memory unchanged, database consistent. |
| `execute` / `checkpoint` | IndexedDB flush fails (quota, I/O) | browser | Durable state = old; **handle poisoned for writes** — apply the flush-failure rule above. Quota aborts reject the promise (the transaction `onabort` hook) instead of hanging. |
| `checkpoint` | I/O failure | native | WAL retained, pending facts retained; safe to retry. Lock-poisoned errors indicate a panicked writer thread — treat the process as needing restart. |
| `importGraph` | Empty blob / length not a `PAGE_SIZE` multiple / unparseable | browser | Rejected before any durable or live change. |
| `importGraph` | IndexedDB replace fails | browser | The single replace transaction rolls back; memory and IndexedDB both still the old database. |

## 3. Two value encodings — one canonical

Two JSON encodings of `Value` exist today; they are **not** interchangeable:

| `Value` | Tagged (canonical, A6 session protocol) | Browser `execute()` JSON (temporary) |
| --- | --- | --- |
| `String` | string | string |
| `Integer` | number | number |
| `Float` (finite) | number | number |
| `Float` (non-finite) | `{"$float": "nan"\|"inf"\|"-inf"}` | **null** (lossy) |
| `Boolean` | bool | bool |
| `Null` | null | null |
| `Ref(uuid)` | `{"$ref": "<uuid>"}` | **plain string** (tag lost) |
| `Keyword` | `{"$kw": ":a/b"}` | **plain string** (tag lost) |

The tagged encoding (`docs/SESSION_PROTOCOL.md` "Value encoding") is the
long-term canonical form per the vetch-lane A6 Q2 decision. The browser
`execute()` JSON (`value_to_json`, `src/browser/mod.rs`) is an explicitly
named **temporary compatibility surface**: it cannot distinguish a `Ref` or
`Keyword` from a `String`, and it maps non-finite floats to `null`. Browser
`execute()` will converge on the tagged encoding in a planned **breaking**
transition. Callers must not build logic that depends on distinguishing
those types from the browser JSON until then — pin the schema knowledge
app-side (you know which attributes hold refs) or wait for the transition.

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

2. **Batch, then debounce.** Every write `execute()` appends a delta
   segment and rewrites the manifest — cumulative IndexedDB growth is
   quadratic in commit count and per-commit latency is linear in segment
   count (~0.033 ms/segment; measured 2 ms → 137 ms p50 over 4,500
   ten-fact commits, 123.6 MB of IndexedDB for ~1.8 MB of logical data).
   Therefore: put multi-statement work in **one** `execute` (one
   `tx_count`, one segment, one IndexedDB transaction), and debounce
   high-frequency sources app-side — commit on gesture end, never per
   frame (the gap-plan caller policy table).

3. **Treat browser Vicia as read-mostly until a maintenance surface
   lands.** There is currently **no reclaim path in the browser**: the
   growth thresholds fire into a void, and `exportGraph` → `importGraph`
   is a size identity, not a compaction (measured). Bulk data should enter
   via `importGraph` of a natively maintained `.graph` file; live browser
   writes should be bounded in count. Reopen cost tracks IndexedDB size,
   not logical size — an unbounded write cadence degrades startup without
   limit. This is the measured wall behind the A5 decision to defer
   browser facade expansion.
