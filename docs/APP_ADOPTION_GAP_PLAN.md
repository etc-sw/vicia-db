# App Adoption Gap Plan (vetch-app / harrekki)

Status: revised 2026-07-11 against `docs/VETCH_CALLER_REQUIREMENTS.md` and
`docs/HARREKKI_CALLER_REQUIREMENTS.md`. Caller decisions override the initial
2026-07-11 audit inferences (see Revision Note). A0, A6, A7, A2, and A5
landed 2026-07-11 (A2's `export_since` frame frozen after harrekki-lane
ACK; A5 evidence gate and A5-4 browser maintenance passed — see the A5 block),
and A8/A9 landed. The shared native/browser tagged, portability, and corruption
corpus and A5-6a/b fail-closed page integrity work have also landed. Remaining
known implementation work is bounded browser open; remaining acceptance proof includes the real
Gate D Vetch trace and a 1M browser maintenance peak-memory run. This line sits
after Q3-B on the Vetch delta-storage roadmap and does not modify any delta
gate. All `Fixed Invariants` in `docs/VETCH_DELTA_STORAGE_ROADMAP.md` apply
unchanged.

## Revision Note

The first draft of this plan led with query-surface completeness
(`:limit`/`:order-by`, range-predicate pushdown). Neither caller requirement
document asks for those as P0; both mandate evidence-first API growth
("add public APIs only for measured gaps that indexed Datalog cannot solve
cleanly"). The callers' actual P0s are elsewhere: a long-lived session
protocol, demonstrated kill -9 durability, incremental since-N reads, and
browser/native parity evidence for the Vetch authority cutover. This revision
demotes A1/A3/A4 to measured-need candidates and adds A6–A9.

## Scope

Two embedders want Vicia/Minigraf as their primary store:

- **vetch-app** — browser-first infinite canvas, already integrated via the
  local `@vicia-db/browser` package boundary as a migration scaffold. Governing document:
  `docs/VETCH_CALLER_REQUIREMENTS.md` (authority cutover, Gates A–E).
- **harrekki** — resident JVM/Clojure daemon; its private cognition ledger is
  a Vicia `.graph` file. Governing document:
  `docs/HARREKKI_CALLER_REQUIREMENTS.md` (P0/P1/P2).

This plan sequences only Vicia-side work. Vetch-side integration gates
(replayable transaction model, projection parity, cutover) live in the Vetch
caller document and are not duplicated here. App-side responsibilities follow
the Ownership Split in `docs/VETCH_DELTA_STORAGE_ROADMAP.md`.

## Gap Inventory (evidence + caller verdict)

| # | Gap | Evidence | Caller verdict |
| --- | --- | --- | --- |
| G1 | No `:limit` / `:offset` / global `:order-by`. | `DatalogQuery` has no limit/order fields (`src/query/datalog/types.rs`). | Neither caller asks. Sorting/pagination live in app projections. Demoted to candidate (A1). |
| G2 | No incremental change surface; `export_fact_log()` is full-export only (`src/db.rs:761`). | Public API audit. | **Harrekki P0 #2** ("what changed since my last tick"). Vetch consumes via stored cursor. Slice A2. |
| G3 | Value-range predicates evaluate post-scan in memory; AVET/VAET range scans exist at storage layer but executor never pushes comparisons into them. | `eval_binop` (`src/query/datalog/executor.rs` ~2200); `threshold_filter` 57.8 ms at 10K (`docs/BENCHMARKS.md`). | Neither caller asks now. Vetch viewport culling is Vetch-owned UI projection; harrekki decay-candidate queries are benchmark-first (P1 #6 note). Demoted to candidate (A3). |
| G4 | Single-fact cap `MAX_FACT_BYTES` = 4080 bytes (`src/storage/packed_pages.rs:47`); no documented chunking convention. | Insert-time validation. | Both callers pin payloads (harrekki: blobs/packets; Vetch: even note text) **outside** the graph — pointers/hashes only. Guard-rail doc only (A4). |
| G5 | Browser backend is write-through with no WAL; open still loads **all** IndexedDB pages into memory and cross-tab coordination remains caller-owned. | `src/browser/mod.rs`, `src/browser/maintenance.rs`, `src/browser/indexeddb.rs`. | **Vetch P0/P1** — A5-4 closes maintenance/failure behavior, A5-5 closes tagged/portable/corruption parity, and A5-6b closes page-local base integrity plus durable v10 migration. Remaining Gate E implementation blocker is bounded 1M open; 1M maintenance peak memory still needs evidence. |
| G6 | `docs/BENCHMARKS.md` 100K/1M current-view rows predate v1.1.0 selective pushdown. | Query Latency section note ("unchanged from v0.8.0"). | Both callers demand caller-shaped evidence before API growth. Slice A0 (expanded). |
| G7 | History grows monotonically; no forget or erasure surface. | Full-history identity invariant. | Harrekki splits this: semantic forget = **bulk valid-time closure** (P1 #6, plannable → A8); physical erasure/vacuum = P2, opt-in, auditable (stays an open decision). |
| G8 | No long-lived session access for external (non-Rust) callers; harrekki currently spawns the CLI per call. | `~/projects/harrekki/src/harrekki/dev_system_minigraf.clj` (one-shot STDIO). | **Harrekki P0 #1** — framed pipe mode. Slice A6. |
| G9 | Crash safety is designed (WAL replay) but not demonstrated under a resident workload profile. | `tests/delta_checkpoint_crash_recovery_test.rs` covers targeted recovery, not a randomized kill loop. | **Harrekki P0 #3** — kill -9 harness. Slice A7. |
| G10 | No cheap status/telemetry surface (fact count, tx_count, WAL size, delta size, last checkpoint outcome) outside the Rust API. | `current_tx_count()` and `MaintenanceOutcome` exist; not reachable externally. | **Harrekki P0 #4**. Folded into A6 (status frames). |
| G11 | No bulk valid-time closure primitive; closing many facts requires per-fact round-trips. | `retract_batch` exists but no query-result-set atomic closure command. | **Harrekki P1 #6**. Slice A8. |
| G12 | A caller needs an openable rollback point while the writer remains live. | `Minigraf::backup_to()` and the session `backup` op now hold one write lock across checkpoint, published-prefix copy, fsync, and atomic no-clobber publish. | **Harrekki P1 #7 — CLOSED by A9.** External `checkpoint(); copy` is explicitly not the contract. |
| G13 | Durability states were not classified for the caller: applied-and-visible vs durably published vs rejected vs maintenance-pending. | Native session frames and BrowserDb write results now expose ordered transaction/durability fields; the backend-specific contract and failure states live in `docs/DURABILITY_AND_CALLER_RULES.md`. | **Vetch P0** (explicit durability receipts) — CLOSED by A5-3/A5-4/A6. Session frames and browser writes both report durability, while browser writes additionally report maintenance pressure/advice. |

## Slice Plan

Naming: A-series (adoption); R/T/Q/S stay reserved for the delta-storage
line. A1/A3/A4 keep their IDs but are demoted to candidates. Implementation
slices use isolated worktrees per repo policy; each lands with tests, doc
sync, and — where a gate has a number — a `docs/BENCHMARKS.md` entry.

### A0 — Caller-shaped evidence gate (DONE 2026-07-11)

Landed. Evidence lives in `docs/BENCHMARKS.md` ("A0: Caller-Shaped Evidence
Suites" plus the re-measured Query Latency / Time-Travel tables). Suites:

- Stale-table refresh: `MINIGRAF_BENCH_MODE=full` extends `query/` scales to
  100K/1M in `benches/minigraf_bench.rs`. Headline: `point_entity` and both
  time-travel reads are flat ~4 µs from 1K to 1M (selective index path);
  the v0.8.0-era 266 ms / 4.33 s rows are gone.
- Vetch cadence replay: `benches/vetch_cadence_benchmark.rs` (full 1M /
  smoke 10K). Headline: every interactive op ≤ ~2 ms p50 and independent of
  base size; per-slice checkpoint ~5 ms p95 at 1M.
- Decay-candidate cost: `decay/` groups in `benches/minigraf_bench.rs`.
  Headline: comparison scan is linear (407 ms at 1M — idle-window OK,
  per-tick no) — this is the A3 promotion evidence; the not-join shape is
  superlinear and capped at 10K by design.
- Browser open at scale: `examples/browser/bench.html` + `bench-driver.cjs`
  + `examples/generate_bench_fixture.rs`. Headline: open latency and heap
  are linear in file size — 1M facts = 3.2 s open, +420 MB per tab.

- Gate: PASSED — no adoption-relevant table carries v0.8.0-era numbers; all
  three caller-shaped suites exist with documented local runners.

### A6 — Framed pipe session protocol + status frames (DONE 2026-07-11)

Landed. Frame reference: `docs/SESSION_PROTOCOL.md`; implementation:
`src/session.rs` (+ `minigraf --session [--file <path>]`); tests:
`tests/session_protocol_test.rs` (17 tests, including real child-process
runs). Design as frozen by the dual-lane ACK: NDJSON framing; tagged
`$ref`/`$kw` encoding; sequential v0 with echoed `id`; raw ops
(`execute`/`status`/`checkpoint`/`maintenance`/`ping`/`shutdown`); EOF =
graceful, no implicit checkpoint; durability classification on write
frames (`applied`/`maintenance_pending`; `published` on checkpoint;
rejection = error frame).

One caller-favoring deviation from the name-review list, documented in the
protocol doc: status `fact_count` is exact only when cheaply knowable and
`null` once committed data lives on disk (an exact total would need a
committed full scan); always-exact `pending_facts` was added alongside.

- Gate: PASSED — `gate_10k_mixed_round_trips_single_session` holds one
  child session for 10k mixed transact/query round-trips (1.2 s, zero
  failures); `malformed_input_over_real_pipe` proves deterministic framing
  and survival under garbage input.

### A7 — kill -9 durability harness (DONE 2026-07-11)

Landed. Harness: `tests/kill9_durability_test.rs` — SIGKILLs real
`minigraf --session --file` children (the A6 protocol is the ack boundary:
a complete `ok:true` transacted frame implies WAL fsync) at randomized
instants over growing `.graph` lineages, three kill modes (random-instant,
mid-checkpoint biased, mid-maintenance), per-cycle audit of every
acknowledged transaction plus atomicity / duplicate / phantom / tx-count
monotonicity checks and a functional-after-recovery probe. Deterministic
schedule via seeded SplitMix64 (`VICIA_A7_SEED`, `VICIA_A7_CYCLES`);
failure artifacts preserved. Smoke (24 cycles) runs in the default suite;
the full gate is `#[ignore]`d
(`cargo test --release --test kill9_durability_test -- --ignored`).

The harness found and drove the fix of two real crash-robustness bugs:
WAL replay resetting the tx counter below the committed watermark on a
header-only WAL (acked writes then skipped on the next replay — lost), and
non-atomic `.graph.lock` creation leaving a contentless lock after a kill
that blocked open until manual deletion. Regression tests live in
`tests/wal_test.rs` and `src/storage/backend/file.rs`.

- Gate: PASSED — 2,400 kill cycles, 155,699 acknowledged transactions,
  zero lost, zero unopenable files, 912 confirmed mid-checkpoint kills,
  263.5 s wall. Evidence: `docs/BENCHMARKS.md` "A7: kill -9 Durability
  Gate". Caveats: process-death durability, not power loss; recompact
  thresholds unreachable at harness scale (maintenance checkpoint path
  only). A8 extends the harness op mix and closed-aware audit model (weight
  table in `gen_stream_op`).

### A2 — Incremental fact log: `export_fact_log_since(tx_count)` (harrekki P0 #2)

Same `FactRecord` shape and (tx_count, entity, attribute) ordering as
`export_fact_log()`, returning records with `tx_count > since`. Per the
harrekki requirement: includes asserted **and** retracted facts with their
valid-time scope, in tx order, at cost proportional to the tail/delta size —
never a committed full scan for small tails (reuse the `NoFullScanFactReader`
test discipline). Must be reachable over the A6 session protocol.

Vetch note: the Vetch doc prefers stored-cursor Datalog for projection
refresh and wants public change-feed APIs only for measured gaps. A2 is
justified by the harrekki P0 alone; Vetch may adopt it via measurement. A
push/listener API stays deferred (both callers agree polling/cursor first).

Landed 2026-07-11. `Minigraf::export_fact_log_since(since_tx_count)`
returns exactly the `tx_count > since` subsequence of `export_fact_log()`.
Committed packed pages are tx-nondecreasing (append-order checkpoints,
order-preserving recompact), so the reader binary-searches the first tail
page in O(log pages) cache reads — the tail stays cheap even after a
recompact folds it into the base, the case a watermark-only skip would
miss; delta and pending layers filter in memory. The no-full-scan
discipline is regression-locked twice: a `SinceOnlyLoader` reader double
whose full-stream entry points bail (`src/graph/storage.rs`), and a
counting-backend page-read bound (`src/storage/persistent_facts.rs`).

Session exposure: `export_since` op implemented and tested; the frame shape
is **frozen after harrekki-lane ACK** (`docs/SESSION_PROTOCOL.md`
"export_since"). Chunking decided no — a request-side `limit` escape hatch
is reserved but not built.

- Gate: PASSED — at a 1M-fact committed base, a 100-record base tail
  returns in 91 µs cold / 32 µs warm (full-export contrast 256 ms,
  ~2,800×); pending / delta-segment 50-record tails 52 µs / 31 µs; empty
  head poll 3 µs. Evidence: `docs/BENCHMARKS.md` "A2: Incremental Fact
  Log"; fixture `tests/fact_log_since_benchmark.rs` (`--ignored`).

### A5 — Browser parity evidence + adoption policy (expanded per Vetch Gate E)

Caller decision overrides the earlier "documentation only" compression: the
Vetch authority cutover requires either semantic parity or a documented
browser policy proving bounded behavior, before browser Vicia replaces the
legacy load path. Scope:

1. Caller rules doc: single-writer per DB name via Web Locks API (no browser
   `.graph.lock` analogue), app-side debounce for high-frequency writes
   (write-through IndexedDB, no WAL), batch multi-statement work into one
   `execute`.
2. Documented durability semantics per backend (G13): what `execute` /
   `checkpoint` guarantee on return, native vs browser; use the same lossless
   tagged value encoding in both session and BrowserDb query results; classify
   failure and corruption behavior for open / execute / checkpoint / import.
3. Parity evidence: browser open memory/startup at 1M-fact scale (from A0),
   long-running IndexedDB growth measurement, and verification that
   `importGraph` is atomic — invalid input must not partially replace the
   live database.

Facade API expansion for `prepare` and explicit transactions stays deferred
until caller evidence shows a measured wall. Browser maintenance outcome and
write durability fields landed later in A5-4.

- A5-3 evidence gate: PASSED — caller rules and browser durability semantics
  are documented, the 1M full-load cost is measured, and import atomicity has
  tests. This is evidence for Gate E, not completion of Gate E. Evidence:
  `docs/DURABILITY_AND_CALLER_RULES.md` (caller rules + G13 durability
  semantics + failure classification + canonical tagged encoding),
  `docs/BENCHMARKS.md` "Browser Open at Scale" re-measure and "A5: Browser
  IndexedDB Growth", six wasm atomicity tests in `src/browser/`.
  A5-6b later closed page-local integrity; Vicia still owns bounded 1M open
  before the final browser cutover decision can move to vetch-app.
- Progress (2026-07-11): **import atomicity LANDED** — `importGraph` now
  commits the durable replacement in a single IndexedDB `clear`+`put`
  transaction *before* the live handle switches (was: swap-then-flush, which
  left memory on the new DB when the flush failed, tearing durable state via
  later write-through flushes; shrinking imports also leaked stale trailing
  pages in IndexedDB forever). The shared IDB transaction promise now hooks
  `onabort`, so a non-request abort (e.g. quota exhaustion at commit) errors
  instead of hanging. Locked by six wasm tests in `src/browser/` — the
  flush-failure-ordering and shrinking-import stale-page tests are red on the
  old code.
- Progress (2026-07-11): **growth measured** (evidence part 2/3, `docs/
  BENCHMARKS.md` "A5: Browser IndexedDB Growth") — browser write cadence is
  **not bounded**: cumulative IndexedDB growth is quadratic in commits (2,000
  ten-fact commits → 30.4 MB, 4,500 → 123.6 MB for ~1.8 MB logical), per-
  commit latency linear in segment count (2 ms → 137 ms), soft/hard delta
  thresholds crossed with nothing able to act, and the `exportGraph` →
  `importGraph` round-trip measured as a size identity — **no caller-side
  remedy exists**. Reopen tracks IndexedDB size, not logical size. 1M open/
  import re-measured on the atomic import path: no regression, 407 MB
  single-transaction import commits cleanly. This is the measured wall for
  the deferred facade decision: browser Vicia is read-mostly (bulk import +
  bounded writes) until a maintenance surface lands. Remaining for the gate:
  caller rules doc + durability semantics (G13) — A5-3.
- Progress (2026-07-11): **caller rules + durability semantics documented**
  (part 3/3) — `docs/DURABILITY_AND_CALLER_RULES.md`: per-backend
  return-time guarantees (native WAL-fsync `applied` tier vs browser
  single-IndexedDB-transaction commit with the Chrome 121+ `relaxed`
  default caveat), the browser flush-failure handle-poisoning rule,
  failure/corruption classification for open/execute/checkpoint/import,
  the tagged value contract and the three browser caller rules (Web Locks
  single-writer, batch+debounce, read-mostly adoption policy). This closes
  A5-3.

#### A5-4 — Browser atomic compact maintenance (DONE 2026-07-11)

The A5 growth measurement promoted the previously deferred browser facade
decision. `BrowserDb.runIdleMaintenance()` now applies the existing soft/hard
delta policy outside foreground writes. At threshold it streams the complete
fact log into a fresh contiguous page-1 image, atomically replaces IndexedDB
with one `clear`+`put` transaction, and swaps live state only after that commit.
This preserves full-history identity while actually removing superseded page
records; merely exposing native copy-on-write recompact would not reclaim them.

The same slice closes the browser write-result and failure boundary:

- successful writes return `tx_id`, deterministic `tx_count`,
  `durability`, `maintenance_pending`, and `advice` while retaining the legacy
  `transacted` / `retracted` fields
- an aborted IndexedDB commit reloads the previous durable graph, so the
  rejected operation is absent from subsequent live queries and reopen
- if durable reload is impossible, the whole handle is poisoned and rejects
  query/write/export/import/checkpoint/maintenance until reopen
- a same-handle operation guard prevents query/export observation and second
  mutation overlap while an async IndexedDB commit is unresolved
- IndexedDB discovery uses `globalThis` instead of `window`, and the repeatable
  bench-driver smoke passes open/write/query/maintenance in a real module
  DedicatedWorker

Gate: PASSED for functional correctness and 100K maintained growth. The
browser WASM tests cover identity, temporal Ref history, rollback/poison,
replacement failure, stale-page removal, and reopen. Four consecutive
100K-base soft-threshold cycles each compacted successfully; page records
dropped from `12,650→10,550`, `13,433→11,326`, `14,209→12,101`, and
`14,984→12,876`, while post-maintenance write p95 returned to roughly
`19–21 ms` from `34–43 ms` pre-maintenance windows. Maintenance remained
O(total history) at `2.5–4.2 s`, so worker scheduling and quota reserve are
load-bearing.

#### A5-5 — Shared tagged portability and corruption corpus (DONE 2026-07-11)

One declarative corpus now generates both a native v10 graph and a graph from
the real Chrome BrowserDb facade. Both consumers run both fixtures (2×2) and
compare exact tagged scalar/Ref/Keyword/null results across current, `:as-of`,
valid-time, combined-time, retraction, and VAET joins. Native also opens the
browser-produced ledger and verifies all 13 history records and tx ordering.

The same bytes drive slot, manifest, segment, header, truncation, and
unpublished-tail mutations. Both backends recover only through the previous
valid manifest, reject selected-older-segment or both-slot corruption, preserve
the old live/durable browser state on rejected import, and omit unpublished
tail pages from export/backup. A complete fallback image remains portable; a
physically incomplete prefix stays queryable through fallback but export fails
visibly until repair. Browser and session results share `src/json_value.rs`.
The repeatable CI entrypoint now runs 27 browser-WASM tests in headless Chrome;
the three A5-6b additions cover durable migration success, atomic abort, and
verified export.

This still does **not** close Vetch Gate E. Browser open loads every page into
renderer/worker memory; the recorded 1M shape remains about 420 MB per handle.
A5-6b now detects an unread base-fact/index-page bit flip through a
generation-bound in-file catalog even when a selected delta is present. Next
browser storage work is a page-on-demand IndexedDB source using that verifier,
followed by the 1M open/query/growth/maintenance peak-memory matrix.

#### A5-6a — Fail-closed query access boundary (DONE 2026-07-11)

The executor now derives a deterministic `QueryAccessPlan` before touching
storage. Queries with a bounded set of entity/attribute lookups use that exact
selective plan; rules, unbound patterns, or more than four distinct lookups use
an explicit full scan. An index-page or fact-page read failure during a
selective plan remains an error instead of being reclassified as permission to
full-scan the store. Nested `not`, `not-join`, `or`, and `or-join` patterns are
part of the same plan, and candidate deduplication retains all eight ledger
identity fields.

Declared packed-fact page ranges also reject short or wrong-type pages instead
of silently omitting their facts. This is the reusable query/storage boundary
for the sparse browser source and page verifier; it does not itself make
BrowserDb page-on-demand or close Gate E.

#### A5-6b — Generation-bound base-page integrity (DONE 2026-07-11)

File format v11 keeps the 84-byte legacy header and both v10 manifest slots,
then adds a checksummed descriptor for one in-file `MGPGC001` catalog. The
catalog stores one CRC32 for every immutable base fact/index page and binds it
to the base generation plus absolute page id. Open reads page 0 and catalog
metadata but no base pages; selective, temporal, full-history, export, and
backup paths verify a base page when they actually read it. CRC32 is accidental
corruption detection, not authentication.

Fresh/full/COW publishers sync and read back the catalog before publishing page
0. Valid v1–v9 graphs append a COW v11 base after the complete legacy image,
preserving duplicate rows and v9 scoped retractions; a complete v10 graph
appends only the catalog without rewriting base, delta, or manifest bytes.
Corrupt legacy base data fails before page 0 changes. BrowserDb
commits migration dirty pages and page 0 in one IndexedDB transaction before
returning a handle, and an injected transaction abort preserves the exact v10
image. The native suite is green, and real Chrome passes 27 browser-WASM tests.
Gate E remains open because the IndexedDB source is still
eager/full-load and the 1M bounded-memory matrix has not run on a sparse source.

### A8 — Bulk valid-time closure, the "forget" primitive (DONE 2026-07-11)

Close `valid_to` on many facts at once as **one atomic transaction**: input
is a query result set or a supplied fact list; no per-fact round-trips over
the session boundary. This is semantic forgetting — reversible, fully
preserved in history — and is distinct from physical erasure (open decision
below). Philosophy check: batch retraction semantics already exist
(`retract_batch`); the new surface is atomic query-driven closure.

- Gate: atomicity under crash (A7 harness covers the new write path);
  closure of a 10k-fact result set in one transaction; history queries show
  the closed window correctly.

Landed. `(forget ...)` accepts either a three-column EAV query result set or
an explicit fact list, with optional `{:valid-to ...}` closure time. It
resolves and materializes under the write lock, then writes every scoped
retract plus truncated re-assert in one WAL-first transaction. A no-match
closure is idempotent and consumes neither a `tx_count` nor a WAL entry.
Native session and browser façades expose the same command; explicit
`WriteTransaction` staging rejects it because closure discovery must see a
stable committed valid-time view.

- Gate: PASSED — `tests/forget_test.rs` closes 10,000 query-selected facts
  in one transaction (20,000 fact-log records, 86.1 ms release), pins current/history/
  `:as-of` semantics and checkpoint/reopen durability. The extended A7
  harness passed 2,400 SIGKILL cycles with 169,275 acknowledged transactions,
  333 acknowledged forgets, 27 promoted in-flight forgets, zero lost, zero
  unopenable files, and zero deadline hits. It found one additional crash
  window: SIGKILL after lazy WAL
  creation but before its 32-byte header left a zero/short sidecar that
  reopen rejected. Short WAL headers now recover as an empty pre-append
  state and are reinitialized before the next write; unit coverage pins
  0-, 7-, and 31-byte cases.

### A9 — Online snapshot/backup contract (DONE 2026-07-11)

`Minigraf::backup_to(destination)` is the linearization boundary. It validates
that the target graph and its WAL/lock sidecars are unoccupied, then holds the
source write lock across checkpoint, exact page-0-published prefix copy,
candidate fsync, and atomic no-overwrite publish. It copies neither the WAL nor
unpublished copy-on-write tail pages. The returned `BackupOutcome { tx_count,
bytes }` names the exact source watermark and checkpointed byte count in the
independent backup.

The A6 session exposes the same operation as `{"op":"backup",
"destination":"..."}` with a `published` durability receipt. Existing targets
are never overwritten; stale destination WAL/lock sidecars, source aliases,
conservatively case-folded Windows/Apple aliases, in-memory databases, and
missing parents reject without corrupting source or target. A source checkpoint
may already have succeeded if a later copy/fsync/publish step fails.

Gate: PASSED. A deterministic clone-writer test blocks a post-copy writer until
atomic publish, then proves the backup contains exactly returned `tx_count = 1`
while the source continues to `tx_count = 2`. Full-history Ref/scoped-retraction
identity, pending-WAL checkpointing, publish-conflict cleanup, and live child-
session ordering are separately covered. Public `checkpoint(); fs::copy()` is
rejected as a guarantee because the next writer can mutate page 0/EOF after the
checkpoint lock is released.

## Candidates (demoted — promote only on measured evidence)

| ID | Candidate | Promotion trigger |
| --- | --- | --- |
| A1 | `:limit` / `:offset` / `:order-by` | A real caller query, measured via A0 suites, that indexed Datalog plus app-side projection cannot meet. Harrekki's bounded-traversal need (P2) may re-surface this as depth/result-limited recursion instead — treat that as its own design question. |
| A3 | Range-predicate pushdown to AVET | A0 shows decay-candidate or other range-shaped caller reads are too costly and tx-time/valid-time indexes do not cover them (per harrekki P1 #6 note). Viewport culling is explicitly Vetch-owned UI projection and does not justify this. |
| A4 | Oversized-value chunking convention doc | Low priority guard-rail: both callers store pointers/hashes only. Keep the insert-rejection error message pointing at a short convention note; schedule anytime. |

## Ordering

A0 first — every later gate and both promotion decisions cite it. A6 and A7
are independent of each other and of A0; A6 unblocks harrekki v0 and should
start early. A2 depends on A6 only for its exposure surface (the Rust API
part can land first). A5's evidence part consumes A0's browser suite. A8 and
A9 are P1, after the P0 set. Candidates have no schedule.

## Caller Policy (app-side, not Vicia slices)

| Concern | Owner | Policy |
| --- | --- | --- |
| Payloads (note text, blobs, embeddings, `.pt` packets) | both | Outside the graph; Vicia holds content-hash / path `Ref` pointers only. Both caller docs pin this. |
| Viewport spatial index / geometry | vetch-app | Vetch-owned UI projection (TypeGPU/Rete); DB stays source of truth. Not a Vicia range-query problem. |
| Write debounce | vetch-app | Commit note position on gesture end, not per frame. |
| Browser maintenance | vetch-app | Hold the Web Lock, run `runIdleMaintenance()` in the BrowserDb worker at idle/slice/import boundaries, and react to write-result advice. Never rebuild on the UI capture path. |
| Authority cutover sequencing | vetch-app | Governed by `docs/VETCH_CALLER_REQUIREMENTS.md` work order and Gates A–E; Vicia-side prerequisites are A0/A5. |
| Maintenance cadence | harrekki | Schedule `run_idle_maintenance()` in idle windows per `docs/MAINTENANCE_API_CONTRACT.md`; hard-threshold recompact is seconds-scale at 1M and must not sit in the tick loop. |
| Shared access | harrekki | One daemon = one writer = one `.graph` file; all other access (human inspection, other sessions) goes through the harrekki daemon over the A6 protocol. Read-only open of a live file is a fallback feature only if daemon mediation proves insufficient (harrekki P1 #8). |

## Open Decisions (need owner call, not scheduled)

| Decision | Tension | Default until decided |
| --- | --- | --- |
| Physical erasure / vacuum (G7 residue) | Full-history identity is a fixed invariant; privacy and file size eventually demand true deletion. Harrekki P2 pins the shape: opt-in, auditable, tombstone-export-then-remove. | No erasure. Semantic forgetting via A8 valid-time closure; revisit with measured growth from real resident usage. |
| `Value::Bytes` variant | File-format bump; tempts in-core blob storage. | Rejected-by-default, strengthened: both callers pin payloads outside the graph. |
| Push change-feed / tx listener | New public surface with lifetime/backpressure questions. | A2 polling + stored cursor. Both caller docs agree. Revisit only on measured cost. |
| Read-only open of a live `.graph` | SWMR is in-process; a second-process reader is a real feature with recovery/locking implications. | Daemon-mediated access (A6). Decide early if daemon mediation fails — retrofitting is messy (harrekki P1 #8). |
| Salience/decay built-ins | UDFs are Rust closures and cannot cross the A6 session boundary; in-query recency ranking may prove valuable. | Caller-side computation. If measured, promote a small fixed set to built-ins rather than exposing UDF registration over the protocol (harrekki P2). |

Resolved since the first draft: **Node/Python bindings** — answered by the
framed pipe protocol (A6, caller-owned child process); no napi/UniFFI
commitment. **Semantic forgetting** — split out of the erasure decision and
planned as A8.

## Philosophy Fit

A6 formalizes an existing surface (piped REPL) without adding a server,
socket, or wire dependency — the daemon owns the child. A7/A9 are
reliability proofs, the core of the SQLite posture. A2/A8 are small,
ledger-shaped primitives consistent with bi-temporal first-class semantics.
A5 adds only the browser durability, maintenance, and parity boundaries needed
by the embedded ledger. Everything that pulls toward a bigger system (vectors,
blobs, push feeds, erasure, second-process readers, query conveniences) is
parked as caller policy, a candidate, or an open decision — consistent with
the delta roadmap's rule: do not skip gates by adding a broader engine or
public surface.
