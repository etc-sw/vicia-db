# Session Protocol (A6) — Frame Reference

Version: 1 (frozen 2026-07-11 by dual caller-lane ACK — design record in
`docs/A6_SESSION_PROTOCOL_QUESTIONS.md`, answers in
`docs/A6_SESSION_PROTOCOL_ANSWERS_HARREKKI.md` and shared-memory decision
`:m/20260710T181042Z-vetch-codex-...-eba29494`).

A caller-owned child process speaking newline-delimited JSON over
stdin/stdout. No network server, no listener socket. Implementation:
`src/session.rs`; tests: `tests/session_protocol_test.rs`.

## Invocation

```
minigraf --session                 # in-memory database
minigraf --session --file <path>   # file-backed database
```

Or embed: `minigraf::session::Session::new(db).run(reader, writer)` over any
line-based transport.

File-backed CLI sessions disable threshold and drop checkpoints. Publication
is owned only by explicit `checkpoint`, `maintenance`, and `backup` requests;
acknowledged foreground writes otherwise remain under WAL replay authority.

## Framing

- One JSON object per line (UTF-8, LF) in each direction.
- Requests are processed strictly sequentially; one response frame per
  non-empty request line, in order.
- Blank/whitespace-only lines are skipped silently (no response frame).
- A line that is not a JSON object, lacks a string `"op"`, or names an
  unknown op produces an error frame with kind `protocol`; the session
  continues — resynchronization is "next newline".
- An optional `"id"` field (any JSON value) is echoed verbatim in the
  response. Reserved for future pipelining; v1 callers may omit it.

## Requests

| op | fields | notes |
| --- | --- | --- |
| `execute` | `datalog`: string | Any Datalog command text (`transact`, `retract`, `query`, `rule`, …). |
| `status` | — | Cheap telemetry snapshot; see Status fields. |
| `checkpoint` | — | Foreground checkpoint (WAL → durable image). |
| `maintenance` | — | `run_idle_maintenance()`; call in idle windows per `docs/MAINTENANCE_API_CONTRACT.md`. |
| `backup` | `destination`: non-empty string | Linearized live-writer backup to a fresh path (A9); never overwrites. |
| `export_since` | `since_tx_count`: uint | Incremental fact-log tail (A2); see export_since below. |
| `ping` | — | Liveness. |
| `shutdown` | — | Responds, then exits the loop. Equivalent to stdin EOF. |

## Responses

Success: `{"ok": true, "result": {...}, "id"?}`. Result bodies by type:

- `{"type": "transacted", "tx_id": <unix-ms>, "tx_count": <n>, "durability": "applied"|"maintenance_pending"}`
- `{"type": "retracted", ...same fields}`
- `{"type": "forgotten", "forgotten": <count>, "tx_id": <unix-ms|null>, "tx_count": <n>, "durability": "applied"|"maintenance_pending"}`
- `{"type": "query", "variables": ["?a", ...], "results": [[<value>, ...], ...]}`
- `{"type": "ok"}` — non-query, non-write commands (e.g. `rule`)
- `{"type": "status", ...}` — see Status fields
- `{"type": "checkpoint", "durability": "published"}`
- `{"type": "maintenance", "checkpoint": "noop"|"published", "delta": "noop"|"recompacted", "advice": "none"|"reduce_checkpoint_cadence"}`
- `{"type": "backup", "destination": <echo>, "tx_count": <n>, "bytes": <n>, "durability": "published"}`
- `{"type": "fact_log", ...}` — see export_since below
- `{"type": "pong"}`, `{"type": "shutdown"}`

Error: `{"ok": false, "error": {"kind": "...", "message": "..."}, "id"?}`.

## backup (A9)

Request:

```json
{"op":"backup","destination":"/rollback/before-experiment.graph","id":7}
```

The destination string is echoed exactly in the success receipt. It must name
a fresh file in an existing directory; the graph, its appended `.wal`, and its
`.graph.lock` sidecar must all be absent. Vicia never overwrites a rollback
point. Source graph/WAL/lock aliases are rejected, with conservative case
folding on Windows and Apple platforms. The caller owns this destination namespace until
the response.

The operation holds the source writer lock across checkpoint, exact published-
page copy, candidate fsync, and atomic publish. `tx_count` is the source
watermark contained in the backup, not a later status sample. `bytes` is the
fsynced checkpointed prefix. The source remains open and can accept later
writes after the response; those writes are absent from this backup.
This assumes the documented one-owner model: the session's handle and its
clones are the only writers for the source pathname.

Schema errors are `protocol`; target validation, conflicts, copy, and
destination-sync errors are recoverable `storage` errors. A source checkpoint
failure is fatal to the session because the current storage authority can no
longer be trusted. A failure after a successful source checkpoint can leave the
source newly checkpointed. A late directory-sync failure can also leave a
complete but unacknowledged destination; inspect or remove that fresh path
before retrying, never assume an error authorizes overwrite.

## forget (A8)

`execute` accepts two bulk valid-time closure forms:

```clojure
(forget [:find ?e ?a ?v :where [?e :session/expired true] [?e ?a ?v]])
(forget {:valid-to "2026-07-01T00:00:00Z"}
        [[:session-1 :session/state :expired]])
```

The query `:find` must contain exactly three plain variables in EAV order.
`:as-of` and `:valid-at` are rejected because the command always resolves
the current transaction-time state at the closure time. Every matching
valid-time window is replaced by an exact scoped retract and, when
non-empty, a re-assertion truncated to `valid_to = T`. All records share one
`tx_count` and one WAL entry. A no-match result reports `forgotten: 0` and
`tx_id: null` without consuming a transaction count.

## export_since (A2) — status: frozen (caller-lane ACK 2026-07-11)

The incremental "facts since tx_count N" read (harrekki P0 #2). The Rust API
(`Minigraf::export_fact_log_since`) is frozen; the frame shape was ACKed
verbatim by the harrekki lane per the A6 precedent (record:
`docs/A6_SESSION_PROTOCOL_ANSWERS_HARREKKI.md` "A2 export_since frame",
shared-memory need `:need/vicia-a2-export-since-frame-ack` resolved).

Request: `{"op": "export_since", "since_tx_count": <uint>, "id"?}`

Response result body:

```json
{"type": "fact_log", "since_tx_count": 42, "head_tx_count": 45,
 "records": [
   {"entity": "550e8400-e29b-41d4-a716-446655440000",
    "attribute": ":person/name",
    "value": "Alice",
    "tx_id": 1767052800000,
    "tx_count": 43,
    "valid_time": {"valid_from": 1767052800000, "valid_to": null},
    "asserted": true}
 ]}
```

- `records` — every fact-log record with `tx_count > since_tx_count`
  (assertions **and** retractions), in the same deterministic storage order
  as `export_fact_log()`. Cost is proportional to the tail (tx-ordered page
  probe + in-memory delta/pending filter), never a committed full scan.
- `head_tx_count` — the current head; an empty tail still advances the
  caller's stored cursor. Poll discipline: store `head_tx_count`, pass it
  back as the next `since_tx_count`.
- `entity` — plain UUID string (an entity id is always a UUID; no type
  ambiguity, so the `$ref` tag is unnecessary). `value` — tagged encoding.
- `valid_time` — `{"valid_from": <ms>, "valid_to": <ms>|null}`; `null` means
  open-ended (the `i64::MAX` sentinel does not survive an f64 round-trip, so
  it never crosses the wire). The string `"all"` marks a legacy unscoped
  retraction cancelling every valid-time window of its EAV triple.
- Missing or negative `since_tx_count` → `protocol` error; the session
  continues. `since_tx_count: 0` is the full export — intended for small
  tails; full exports of large databases should use the Rust API.
- Chunking: **decided no** (harrekki-lane ACK) — the daemon polls small
  tails by design; the boot-time full export is a replay the caller reads
  in one line; and a multi-frame reply would break "resync = next newline".
  Reserved escape hatch if a bounded reply ever becomes necessary: a
  request-side `limit` with the cut rounded down to a `tx_count` boundary
  (caller-driven pagination) — not chunked responses. Not built.

## Value encoding (tagged, lossless)

| Vicia `Value` | JSON |
| --- | --- |
| `String` | string |
| `Integer` | number |
| `Float` (finite) | number |
| `Float` (non-finite) | `{"$float": "nan"|"inf"|"-inf"}` |
| `Boolean` | bool |
| `Null` | null |
| `Ref(uuid)` | `{"$ref": "<uuid>"}` |
| `Keyword` | `{"$kw": ":a/b"}` |

This is the canonical encoding shared by the session protocol and BrowserDb
query results. The browser transition landed with the Gate E portable corpus;
both surfaces call the same encoder and preserve `Ref`, `Keyword`, and
non-finite float identity.

## Durability classification

| value | meaning |
| --- | --- |
| `applied` | In memory and WAL-fsynced — survives kill -9 via replay. The level harrekki treats as "remembered". |
| `maintenance_pending` | `applied`, and delta-growth thresholds currently advise maintenance — schedule a `maintenance` call. |
| `published` | In the checkpointed durable image (checkpoint/maintenance) or in a fsynced, atomically published independent backup. |
| rejected | Not a field: rejection is an error frame (`parse`, `execution`, or a recoverable pre-apply `storage` failure) — nothing was applied. |

Per-backend semantics behind these values — what `execute`/`checkpoint`
guarantee at return on native vs browser, and the browser caller rules —
are in `docs/DURABILITY_AND_CALLER_RULES.md`.

## Error kinds

| kind | meaning | session |
| --- | --- | --- |
| `protocol` | Malformed frame, missing/unknown op or field | continues |
| `parse` | `datalog` text failed to parse | continues |
| `execution` | Command parsed but failed to execute | continues |
| `storage` | Storage validation, I/O, integrity, or synchronization failure | Pre-apply WAL validation/write failures and backup destination validation/copy/sync failures continue. Committed reads, status, checkpoint, maintenance, `export_since`, backup source checkpoint, lock poison, and any post-WAL apply/checkpoint failure emit this frame once and then exit nonzero. |

The caller may keep using the child only after a documented recoverable
`storage` frame. EOF after a `storage` frame requires restart. Transport
failure (broken pipe) also exits; restart + WAL replay is the recovery path.

## Status fields

```json
{"type": "status", "fact_count": 2, "pending_facts": 2, "tx_count": 1,
 "wal_bytes": null, "delta_segments": null, "delta_pages": null,
 "last_checkpoint_unix_ms": null, "last_checkpoint_outcome": null}
```

- `fact_count` — exact total **when cheaply knowable** (all facts in
  memory: in-memory databases, or file-backed before the first committed
  image exists). `null` once committed data lives on disk — an exact total
  would require a committed full scan, which `status` must never do.
  Deviation from the ACKed name-review list, in the caller's favor:
  `pending_facts` (always exact, always cheap) was added alongside, and
  `tx_count` remains the recommended self-model number.
- `pending_facts` — in-memory records not yet checkpointed. Always exact.
- `wal_bytes`, `delta_segments`, `delta_pages` — `null` for in-memory
  databases.
- `last_checkpoint_unix_ms` / `last_checkpoint_outcome` — the last
  checkpoint **this session** performed via `checkpoint`, `maintenance`, or `backup`
  ops (`null` before the first one). File-backed CLI sessions do not perform
  threshold or drop checkpoints.

## Lifecycle

- stdin EOF or `shutdown` op → finish in-flight work, exit 0. **No implicit
  checkpoint** — WAL replay on next open is the durability contract; send
  `checkpoint` (or `maintenance`) first if you want the source checkpointed;
  use `backup` for an independent live-writer rollback point.
- A fatal storage failure → emit exactly one `storage` error frame with the
  request id, then exit nonzero without an implicit checkpoint. A restarted
  child replays every previously acknowledged WAL-backed write.
- SIGKILL at any point: acknowledged (`applied`) writes survive via WAL
  replay. Verified continuously by the A7 harness (planned).
- One process per `.graph` file (advisory lock). All other access goes
  through the daemon that owns this child.
