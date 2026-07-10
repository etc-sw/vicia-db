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
| `ping` | — | Liveness. |
| `shutdown` | — | Responds, then exits the loop. Equivalent to stdin EOF. |

## Responses

Success: `{"ok": true, "result": {...}, "id"?}`. Result bodies by type:

- `{"type": "transacted", "tx_id": <unix-ms>, "tx_count": <n>, "durability": "applied"|"maintenance_pending"}`
- `{"type": "retracted", ...same fields}`
- `{"type": "query", "variables": ["?a", ...], "results": [[<value>, ...], ...]}`
- `{"type": "ok"}` — non-query, non-write commands (e.g. `rule`)
- `{"type": "status", ...}` — see Status fields
- `{"type": "checkpoint", "durability": "published"}`
- `{"type": "maintenance", "checkpoint": "noop"|"published", "delta": "noop"|"recompacted", "advice": "none"|"reduce_checkpoint_cadence"}`
- `{"type": "pong"}`, `{"type": "shutdown"}`

Error: `{"ok": false, "error": {"kind": "...", "message": "..."}, "id"?}`.

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
