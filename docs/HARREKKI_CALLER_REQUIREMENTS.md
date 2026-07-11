# Harrekki Caller Requirements for Vicia DB

Status: caller requirements derived from the Harrekki resident-intelligence
design (2026-07-10 session, upopo + claude). Harrekki is a planned fork of
`~/projects/harrekki` into a long-running daemon whose private cognition
ledger is a Vicia `.graph` file.

Date: 2026-07-11

## Recommendation

Vicia DB should serve as the durable bi-temporal cognition ledger for a
long-lived single-writer daemon process (JVM/Clojure). It should not become
Harrekki's scheduler, salience engine, packet/blob store, or transport layer.

The caller shape differs from Vetch's in one important way: Vetch is an
interactive app session; Harrekki is a **resident daemon** that ticks
continuously, is killed without warning, and must never lose its memory.
Durability, incremental reads, and non-blocking maintenance dominate over
query richness.

Boundary pins (things Vicia must NOT take on for this caller):

- Embeddings, soft-prefix `.pt` packets, and other blobs stay outside the
  graph; the ledger stores content-hash / path `Ref` pointers only.
- The Harrekki cognition ledger is a separate `.graph` file. It is not
  `shared.graph` (orchestra memory) and not the Vetch product authority store.
  One daemon = one writer = one file.
- Forgetting is valid-time closure, not deletion. Physical erasure is a
  separate, explicit compaction concern (P2).

## Evidence Read

- Harrekki ledger contract: `~/projects/harrekki/src/harrekki/ledger.clj`
  (`TemporalLedger` protocol — append, as-of / valid-at visibility,
  supersedes), `xtdb_ledger.clj` (the adapter seat Vicia would take).
- Current JVM access pattern: `~/projects/harrekki/src/harrekki/dev_system_minigraf.clj`
  (spawns the CLI per call, one-shot STDIO — too slow for a tick loop).
- Nucleus loop and receipts: `~/projects/harrekki/src/harrekki/loop.clj`,
  `promotion.clj`, `dataset_sink.clj`.
- Resident/DMN design constraints: LFM research pins in
  `/mnt/e/projects/obsidian/vetch/10-research/` (multi-sample verdicts,
  Tier-1 deterministic verification, external-knowledge architecture).
- Vicia storage direction already covering part of this:
  `docs/VETCH_DELTA_STORAGE_ROADMAP.md`, `docs/DELTA_INDEX_DESIGN.md`,
  `docs/MAINTENANCE_API_CONTRACT.md`.

## P0 — blocks resident daemon v0

### 1. Long-lived session access for an external (JVM) caller

Today Harrekki spawns the `minigraf` CLI per call. A daemon ticking many
times per minute needs a persistent conversation with one open database.

Preferred, philosophy-aligned shape: a **caller-owned child process in
framed pipe mode** — the existing REPL/piped mode formalized into a
machine-parseable request/response framing (length- or line-delimited,
stable result encoding, explicit error frames). No network server, no
listener socket required; the daemon owns the child's lifecycle. A
fork-maintained JVM binding is an acceptable alternative but carries more
maintenance weight.

Acceptance: a Clojure process can hold one session open, run 10k mixed
transact/query round-trips without respawn, and observe deterministic
framing under malformed input.

### 2. Incremental "facts since tx_count N" read

The daemon's perception is "what changed since my last tick", and its audit
export is "what changed since the last receipt". Snapshot queries via
`:as-of` exist; what is needed is a cheap first-class delta read:

- query or export primitive returning all facts (asserted and retracted)
  with `tx_count > N`, in tx order, including valid-time scope;
- proportional cost to the delta size, not the committed graph size.

`export_fact_log()` is close; the requirement is the *since-N windowed*
form usable over the session protocol.

### 3. kill -9 durability under resident workload

WAL replay is designed to be crash-safe; this caller needs it demonstrated
under its specific profile: tens of thousands of small transactions with
periodic checkpoints, killed with SIGKILL at random points (including
mid-checkpoint), thousands of iterations. This is a test-harness
requirement, not a feature. A resident that forgets on crash is not a
resident.

Acceptance: an automated kill-loop harness in `tests/` passes with zero
lost acknowledged transactions and zero unopenable files.

### 4. Status / telemetry surface

The daemon's self-model and its checkpoint scheduling need cheap
introspection: fact count, current `tx_count`, WAL size, delta size, last
checkpoint time/outcome. `docs/MAINTENANCE_API_CONTRACT.md` and the idle
maintenance outcome work already cover part of this — the requirement is
that the same numbers are reachable over the session protocol (#1), not
only via the Rust API.

## P1 — needed as the resident grows

### 5. Maintenance outside the interactive path

Already the active Vicia line (delta storage roadmap, Q2-B streaming
recompact). Restated from this caller's angle: checkpoint/recompact must
not stall reads/writes in ways that break tick cadence; cost tied to delta
size, not committed size. No new asks beyond the existing roadmap — this
document just registers that the resident daemon is a second caller
depending on it.

### 6. Bulk valid-time closure (the "forget" primitive)

DMN-style forgetting closes `valid_to` on many facts at once, reversibly.
Needed: closing the valid-time window for the result set of a query (or a
supplied fact list) as **one atomic transaction**, without per-fact
round-trips over the session boundary. Decay-candidate queries ("entities
untouched since T") should be benchmarked; if tx-time indexes do not cover
them cheaply, that becomes a follow-up requirement.

### 7. Online snapshot / backup

A consistent copy of the `.graph` file while the writer is live. A9 provides
`Minigraf::backup_to()` and the session `backup` op: one write lock spans source
checkpoint through destination fsync and atomic publish, and the receipt names
the exact included `tx_count`. Callers must use this operation rather than
calling `checkpoint()` and copying afterward, because the writer can advance
page 0/EOF once `checkpoint()` releases its lock. Destinations are fresh,
single-file rollback points; existing graph/WAL/lock paths are never
overwritten. Losing the ledger is losing the being.

### 8. Cross-process read policy (decision, not feature)

SWMR is in-process. While the daemon holds the file, other tools (human
inspection, other sessions) need reads. Preferred resolution: **all access
goes through the daemon**, reusing the session protocol — no Vicia change.
The alternative (read-only open of the committed base while a writer is
live) is a real feature request; only pursue it if daemon-mediated access
proves insufficient. Decide early; retrofitting is messy.

## P2 — known, not yet blocking

- **Bounded traversal**: associative walks over the cognition graph will hit
  recursion limits (observed: depth-100 chain ~140s; 7.6M-tuple OOM on
  w5_d5 fan-out). Depth/result-limited recursion or an iterative expansion
  primitive will eventually be needed; until then the caller walks hop by
  hop.
- **Salience helpers as built-ins**: UDFs are Rust closures and cannot cross
  the session boundary. If time-decay weighting / recency ranking in-query
  proves valuable, promote a small fixed set to built-ins rather than
  exposing UDF registration.
- **Physical erasure (vacuum)**: bi-temporal history grows forever by
  design. An explicit compaction option that tombstone-exports and then
  physically removes long-dead facts will eventually matter for privacy and
  size. Must remain opt-in and auditable.

## Non-Requirements

To keep the philosophy check easy: this caller does **not** ask for a
network server mode, multi-writer concurrency, replication, another query
language, in-graph vector search, or blob storage. Where a need can be met
in the Harrekki daemon layer instead of Vicia, that is the default answer.
