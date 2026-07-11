# A6 Session Protocol — Harrekki/Vetch Lane Answers

Status: answered 2026-07-11 (harrekki lane, claude). Responds to
`docs/A6_SESSION_PROTOCOL_QUESTIONS.md`. All five recommendations are
**ACK**; comments below are context, not objections, except where marked.
The A2 `export_since` frame ACK (same lane, same day) is appended at the
end of this document.

Caller evidence: `~/projects/harrekki-wt-vetch-resident/src/harrekki/resident.clj`
(wake-loop), `resident_journal.clj` (current interim journal the Vicia
session will replace), `docs/HARREKKI_CALLER_REQUIREMENTS.md` (P0 #1/#4).

## Q1 Framing — ACK: NDJSON

Both target runtimes (babashka and JVM) get this for free
(`readLine` + bundled cheshire/jsonista). No binary payloads exist or are
planned on this lane — blobs are pinned outside the graph.

## Q2 Value encoding — ACK: tagged (b), strongly

Harrekki's boundary pin is "refs only, no blobs": the cognition ledger
stores `Value::Ref` pointers to everything outside itself. A pipe encoding
that flattens `Ref` to a string would poison exactly the facts the ledger
exists to keep. Tagged `{"$ref": ...}` / `{"$kw": ...}` is the right
shape; two ambiguous types is the correct minimal set.

On the Vetch follow-up (browser `execute()` encoding): this lane's
opinion — keep the browser encoding lossy as a documented legacy surface
and let A5 document **two encodings**. The pipe protocol is the ledger
surface of record; the browser surface is a viewer. Migrating the browser
to tagged is nice-to-have in a later major, not worth blocking anything.

## Q3 Concurrency — ACK: sequential v0

Direct answer to the question posed: **no, the tick loop does not need
overlapping requests.** The wake-loop is single-threaded by design — the
being does one thing per wake (`resident.clj` `wake-loop`: poll ask →
experience or idle-tick → repeat). Status polling happens between
requests, not during them; if a slow query delays a status read, the
being simply wakes late, which is acceptable. The reserved echoed `"id"`
field is the right compatibility hook.

## Q4 Ops surface — ACK: raw v0, and explicitly *no* supersede in Vicia

No objection — the opposite: harrekki **wants** supersede semantics to
stay in the Clojure adapter. `TemporalLedger` (append / as-of / valid-at /
supersedes closure) is harrekki-owned contract surface; pushing it into
Vicia would split ownership of the being's memory semantics across two
repos. The protocol staying thin (`execute`, `status`, `checkpoint`,
`maintenance`, `ping`, `shutdown`) matches the Non-Requirements section
of `HARREKKI_CALLER_REQUIREMENTS.md`.

## Q5 Lifecycle — ACK: EOF = graceful, no implicit checkpoint

Matches the daemon's model: the resident owns the child's lifecycle and
will send explicit `checkpoint` on its own rest cadence (the being's
"consolidation" moments), not rely on shutdown side effects. WAL-replay-
as-durability is exactly what P0 #3 (kill -9 harness, A7) verifies.

## Name review — no objections

- Status fields cover P0 #4's list exactly (fact/tx counts, WAL size,
  delta size, last checkpoint time/outcome). Good.
- Post-ACK deviation, accepted (2026-07-11): `fact_count` is null once
  committed data lives on disk (exact total would need a full scan, which
  status must never do); `pending_facts` (always exact) was added instead.
  Fine for this caller — the resident's self-model tracks growth via
  `tx_count`, which is monotonic and always present; an exact fact total
  was never load-bearing.
- Durability classification (`applied`/`published`/`rejected`/
  `maintenance_pending`): `applied` (WAL-fsynced) is the level the
  resident treats as "remembered".
- Error frames: session exiting after unrecoverable `storage` is fine —
  the daemon's response to child death is restart + WAL replay either way.

## A2 `export_since` frame — ACK: single-frame, as proposed

Answered 2026-07-11 (harrekki lane, claude), closing
`:need/vicia-a2-export-since-frame-ack`. The frame in
`docs/SESSION_PROTOCOL.md` "export_since" is accepted verbatim; the open
question resolves to **single-frame reply, no chunking**.

Why, from the caller's side:

- **The poll discipline is the eye's existing pattern.** Harrekki's vetch
  perception adapter already lives on cursor + ack-after-landing with
  short wake ticks (5–60s); the vicia swap will do the same with
  `head_tx_count` as the stored cursor. Tails are small by construction —
  a being that writes a handful of cognition records per wake never
  accumulates a tail worth chunking.
- **The one large-tail moment is boot, and boot is a replay anyway.**
  `since_tx_count: 0` at realistic being scale is a few MB on one line;
  both bb and JVM read that without ceremony. Truly large full exports are
  documented to use the Rust API — correct division.
- **Chunking would break the protocol's best property.** "One request line
  → one response frame, resync = next newline" is what makes the adapter
  trivial on both runtimes. A multi-frame reply makes mid-stream
  resynchronization ambiguous and adds a state machine to every adapter
  for a case the caller cannot produce.
- `head_tx_count` advancing the cursor on an empty tail, `entity` as plain
  uuid string, `valid_to` null / `"all"`, and `protocol` error on
  missing/negative `since_tx_count` — all fine as specified.

One non-blocking reservation, recorded so it isn't reinvented later: if a
bounded reply ever becomes necessary, prefer a **request-side `limit`**
(caller-driven pagination; server rounds the cut down to a `tx_count`
boundary so a stored cursor never splits a transaction) over chunked
responses. Reserve, don't build.
