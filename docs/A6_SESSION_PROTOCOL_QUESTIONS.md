# A6 Framed Pipe Session Protocol — Design Questions

Status: awaiting caller-lane answers, 2026-07-11. A6 implementation starts
after Q1–Q5 are answered (ACK of the recommended default is enough). The
protocol is a long-term contract with the harrekki JVM adapter (the
`xtdb_ledger.clj` seat) and possibly other non-Rust callers, so caller
lanes get a veto before the first byte is frozen.

Context: `docs/APP_ADOPTION_GAP_PLAN.md` slice A6;
`docs/HARREKKI_CALLER_REQUIREMENTS.md` P0 #1 (framed pipe mode) and #4
(status surface); `docs/VETCH_CALLER_REQUIREMENTS.md` P0 durability
receipts. Scope pins: caller-owned child process over stdin/stdout, no
network server, no listener socket, session survives malformed input.

## Q1 — Framing

One JSON object per line (NDJSON, UTF-8, LF), or length-prefixed frames?

**Recommended: NDJSON.** Trivial from JVM/Clojure (`BufferedReader.readLine`
+ any JSON lib), human-debuggable with a shell pipe, and resync after a
malformed frame is "skip to next newline". JSON escaping already removes
embedded-newline risk. Length-prefix is stronger for binary payloads, but
the protocol carries Datalog text and JSON results — no binary planned
(blobs are pinned outside the graph by both caller docs).

## Q2 — Value encoding fidelity (the important one)

The existing browser `execute()` JSON is **lossy**: `Value::Ref(uuid)` and
`Value::Keyword` both flatten to plain JSON strings
(`src/browser/mod.rs:350–364`). A ledger adapter cannot reconstruct
`Value::Ref` vs a string that happens to contain a UUID — and the delta
roadmap's Gate 1 pins `Value::Ref` as identity that matters.

Options:

- (a) Reuse the browser encoding as-is — parity across surfaces, but lossy.
- (b) Tagged encoding for the pipe protocol: scalars stay plain JSON;
  `{"$ref": "<uuid>"}`, `{"$kw": ":status/active"}` for the two ambiguous
  types. Lossless round-trip; slightly more adapter code.

**Recommended: (b) tagged.** The pipe protocol is the ledger surface;
losing ref/keyword typing there poisons every downstream replay. Follow-up
for the Vetch lane: should browser `execute()` adopt the same tagged
encoding in a later major (breaking the current scaffold parser), or keep
the lossy form as a browser-only legacy? A6 does not need this answered,
but the answer decides whether A5 documents one encoding or two.

## Q3 — Concurrency model

Strictly sequential request→response (one in-flight), or request-id
pipelining from day one?

**Recommended: sequential v0**, with a reserved optional `"id"` field
echoed back in responses so pipelining can be added compatibly. Question
to harrekki: does the tick loop ever need overlapping requests (e.g.
status polling during a slow query), or is one-at-a-time fine for v0?

## Q4 — Ops surface

Raw Datalog only, or semantic ledger ops in the protocol?

**Recommended: raw v0.** Ops: `execute` (any Datalog command text),
`status`, `checkpoint`, `maintenance` (`run_idle_maintenance`), `ping`,
`shutdown`. Semantic ledger verbs (append / supersede / as-of read) stay in
the Clojure adapter as Datalog generators — the protocol stays thin and the
`TemporalLedger` protocol shape remains harrekki-owned. Objection wanted if
harrekki would rather push supersede semantics into Vicia now.

## Q5 — Lifecycle and shutdown

What does stdin EOF mean?

**Recommended:** EOF = graceful close: finish the in-flight request, exit 0.
No implicit checkpoint on exit — WAL is already fsynced per commit, replay
on next open is the durability story, and hiding a checkpoint in shutdown
would blur the maintenance contract (`docs/MAINTENANCE_API_CONTRACT.md`).
Callers that want a checkpointed file send `checkpoint` (or `maintenance`)
before closing. SIGKILL at any point is covered by the A7 harness.

## Name-review only (objection window, no discussion needed)

- Status frame fields: `fact_count`, `tx_count`, `wal_bytes`,
  `delta_segments`, `delta_pages`, `last_checkpoint_unix_ms`,
  `last_checkpoint_outcome`.
- Durability classification on write responses: `applied` (in memory +
  WAL-fsynced), `published` (in checkpointed image), `rejected` (failed
  before application), `maintenance_pending` (applied; delta thresholds
  advise maintenance).
- Error frame: `{"error": {"kind": "parse|execution|storage|protocol",
  "message": "..."}}`; session stays alive after `parse`/`execution`,
  exits after unrecoverable `storage`.
