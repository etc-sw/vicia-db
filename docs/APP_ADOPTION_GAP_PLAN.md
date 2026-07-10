# App Adoption Gap Plan (vetch-app / harrekki)

Status: revised 2026-07-11 against `docs/VETCH_CALLER_REQUIREMENTS.md` and
`docs/HARREKKI_CALLER_REQUIREMENTS.md`. Caller decisions override the initial
2026-07-11 audit inferences (see Revision Note). No slice started. This line
sits after Q3-B on the Vetch delta-storage roadmap and does not modify any
delta gate. All `Fixed Invariants` in `docs/VETCH_DELTA_STORAGE_ROADMAP.md`
apply unchanged.

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

- **vetch-app** — browser-first infinite canvas, already integrated via
  `@minigraf/browser` as a migration scaffold. Governing document:
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
| G5 | Browser backend: write-through per `execute`, no WAL, no maintenance outcome parity, open loads **all** IndexedDB pages into memory, no multi-tab coordination. | `src/browser/mod.rs:36–92`, `src/browser/indexeddb.rs`. | **Vetch P0/P1** — Gate E (browser/native parity) must pass before browser Vicia replaces the legacy load path. Slice A5 (expanded). |
| G6 | `docs/BENCHMARKS.md` 100K/1M current-view rows predate v1.1.0 selective pushdown. | Query Latency section note ("unchanged from v0.8.0"). | Both callers demand caller-shaped evidence before API growth. Slice A0 (expanded). |
| G7 | History grows monotonically; no forget or erasure surface. | Full-history identity invariant. | Harrekki splits this: semantic forget = **bulk valid-time closure** (P1 #6, plannable → A8); physical erasure/vacuum = P2, opt-in, auditable (stays an open decision). |
| G8 | No long-lived session access for external (non-Rust) callers; harrekki currently spawns the CLI per call. | `~/projects/harrekki/src/harrekki/dev_system_minigraf.clj` (one-shot STDIO). | **Harrekki P0 #1** — framed pipe mode. Slice A6. |
| G9 | Crash safety is designed (WAL replay) but not demonstrated under a resident workload profile. | `tests/delta_checkpoint_crash_recovery_test.rs` covers targeted recovery, not a randomized kill loop. | **Harrekki P0 #3** — kill -9 harness. Slice A7. |
| G10 | No cheap status/telemetry surface (fact count, tx_count, WAL size, delta size, last checkpoint outcome) outside the Rust API. | `current_tx_count()` and `MaintenanceOutcome` exist; not reachable externally. | **Harrekki P0 #4**. Folded into A6 (status frames). |
| G11 | No bulk valid-time closure primitive; closing many facts requires per-fact round-trips. | `retract_batch` exists but no query-result-set atomic closure command. | **Harrekki P1 #6**. Slice A8. |
| G12 | No online snapshot/backup contract while the writer is live. | No documented checkpoint-then-copy guarantee. | **Harrekki P1 #7**. Slice A9. |
| G13 | Durability states are not classified for the caller: applied-and-visible vs durably published vs rejected vs maintenance-pending. | Native semantics exist implicitly (WAL fsync before return; `MaintenanceOutcome`); undocumented, and absent in the browser binding. | **Vetch P0** (explicit durability receipts). Folded into A5 + A6 result framing. |

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

### A6 — Framed pipe session protocol + status frames (harrekki P0 #1, #4)

Formalize the existing REPL/piped mode into a machine-parseable
request/response framing for a **caller-owned child process**: length- or
line-delimited frames, stable result encoding, explicit error frames. No
network server, no listener socket; the daemon owns the child's lifecycle.
Include a status frame exposing fact count, current `tx_count`, WAL size,
delta size, and last checkpoint time/outcome (G10), and make result frames
carry the durability classification from G13 (applied / durably published /
rejected / maintenance-pending) where the distinction exists.

- Non-goals: no network transport, no multi-writer, no auth layer.
- Gate (from the harrekki doc): an external process holds one session open,
  runs 10k mixed transact/query round-trips without respawn, and observes
  deterministic framing under malformed input.

### A7 — kill -9 durability harness (harrekki P0 #3)

A test-harness slice, not a feature: automated kill-loop in `tests/` running
tens of thousands of small transactions with periodic checkpoints, SIGKILL
at random points including mid-checkpoint, over thousands of iterations.
Seed from `tests/delta_checkpoint_crash_recovery_test.rs`.

- Gate: zero lost acknowledged transactions, zero unopenable files.

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

- Gate: at 1M base, a since-tail of ≤100 records returns without
  `stream_all()`; latency recorded in BENCHMARKS.md.

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
   `checkpoint` guarantee on return, native vs browser; failure and
   corruption classification for open / execute / checkpoint / import.
3. Parity evidence: browser open memory/startup at 1M-fact scale (from A0),
   long-running IndexedDB growth measurement, and verification that
   `importGraph` is atomic — invalid input must not partially replace the
   live database.

Facade API expansion (`prepare`, explicit tx in `BrowserDb`, maintenance
outcome parity) stays deferred until this evidence shows a measured wall.

- Gate: the Vicia-side preconditions of Vetch Gate E are documented and
  measured; import atomicity has a test.

### A8 — Bulk valid-time closure, the "forget" primitive (harrekki P1 #6)

Close `valid_to` on many facts at once as **one atomic transaction**: input
is a query result set or a supplied fact list; no per-fact round-trips over
the session boundary. This is semantic forgetting — reversible, fully
preserved in history — and is distinct from physical erasure (open decision
below). Philosophy check: batch retraction semantics already exist
(`retract_batch`); the new surface is atomic query-driven closure.

- Gate: atomicity under crash (A7 harness covers the new write path);
  closure of a 10k-fact result set in one transaction; history queries show
  the closed window correctly.

### A9 — Online snapshot/backup contract (harrekki P1 #7)

Checkpoint-then-copy contract while the writer is live: document (and
guarantee with a test) that after `checkpoint()` returns, copying the
`.graph` file yields a consistent openable snapshot even while the writer
continues. Losing the ledger is losing the being; this is the rollback and
backup story for both callers.

- Gate: concurrent copy-under-write test opens the copy successfully with
  all checkpointed transactions present.

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
A5 is documentation plus measurement. Everything that pulls toward a bigger
system (vectors, blobs, push feeds, erasure, second-process readers, query
conveniences) is parked as caller policy, a candidate, or an open decision —
consistent with the delta roadmap's rule: do not skip gates by adding a
broader engine or public surface.
