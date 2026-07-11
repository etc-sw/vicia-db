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
- `{"type": "fact_log", ...}` — see export_since below
- `{"type": "pong"}`, `{"type": "shutdown"}`

Error: `{"ok": false, "error": {"kind": "...", "message": "..."}, "id"?}`.

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

This is the canonical long-term encoding (vetch lane decision); the browser
`execute()` JSON, which flattens `Ref`/`Keyword` to plain strings, is an
explicitly named temporary compatibility surface until its planned breaking
transition.

## Durability classification

| value | meaning |
| --- | --- |
| `applied` | In memory and WAL-fsynced — survives kill -9 via replay. The level harrekki treats as "remembered". |
| `maintenance_pending` | `applied`, and delta-growth thresholds currently advise maintenance — schedule a `maintenance` call. |
| `published` | In the checkpointed durable image (checkpoint/maintenance responses). |
| rejected | Not a field: rejection is the error frame (`parse`/`execution`) — nothing was applied. |

Per-backend semantics behind these values — what `execute`/`checkpoint`
guarantee at return on native vs browser, and the browser caller rules —
are in `docs/DURABILITY_AND_CALLER_RULES.md`.

## Error kinds

| kind | meaning | session |
| --- | --- | --- |
| `protocol` | Malformed frame, missing/unknown op or field | continues |
| `parse` | `datalog` text failed to parse | continues |
| `execution` | Command parsed but failed to execute | continues |
| `storage` | Checkpoint/maintenance/status I-O failure | continues; caller should treat repeated `storage` errors as child-restart grounds |

Transport failure (broken pipe) exits the process; the daemon's restart +
WAL replay is the recovery path either way.

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
  checkpoint **this session** performed via `checkpoint` or `maintenance`
  ops (`null` before the first one). Auto-checkpoints triggered by the WAL
  threshold inside `execute` are not currently reported here.

## Lifecycle

- stdin EOF or `shutdown` op → finish in-flight work, exit 0. **No implicit
  checkpoint** — WAL replay on next open is the durability contract; send
  `checkpoint` (or `maintenance`) first if you want a checkpointed file.
- SIGKILL at any point: acknowledged (`applied`) writes survive via WAL
  replay. Verified continuously by the A7 harness (planned).
- One process per `.graph` file (advisory lock). All other access goes
  through the daemon that owns this child.
