# Cross-Database Stress Benchmark

This harness compares Vicia with separately classified embedded engines under
one deterministic EAV-shaped workload. It is an external development tool, not
a Vicia storage dependency.

```bash
./scripts/run-cross-db-stress.sh smoke
./scripts/run-cross-db-stress.sh full
```

Each engine runs in a fresh process and receives the same base fact count,
durable append batches, deterministic point reads, and close/reopen cadence.
The receipt records build, append, read, reopen, and full-scan latency; Linux
peak RSS; primary and total storage bytes; and bytes per fact. Exact final count
and arithmetic checksum are mandatory. Source commit, dirty state, named
testbed, OS, architecture, CPU, logical CPU count, and host memory travel with
the receipt. The summary rejects mixed-host or mixed-source rows and recomputes
percentiles and correctness from raw samples.

## Comparison roles

| Engine | Role | Interpretation |
|---|---|---|
| Vicia | Product bi-temporal Datalog engine | The behavior being developed |
| CozoDB 0.7.6 SQLite backend | Embedded Datalog/graph peer | Closest semantic peer in this first matrix |
| SQLite WAL + `synchronous=FULL` | Embedded relational EAV baseline | Durable single-file relational baseline |
| redb 4.1 | Embedded key-value storage floor | Storage lower bound, not a graph/Datalog competitor |

Do not turn the table into one overall ranking. Cozo and Vicia can be compared
as Datalog-facing embedded products for this narrow EAV shape. SQLite shows the
cost of a hand-shaped relational representation. redb shows how much work is
left after query, temporal, and graph semantics are removed.

## Stability scope

The current gate performs repeated process-local close/reopen cycles and then
requires an exact full scan plus each engine's available integrity boundary.
It then starts a continuous writer, waits for five acknowledged durable batches,
sends `SIGKILL`, and requires the reopened database to contain at least that
announced contiguous prefix with its exact arithmetic checksum. This detects
reopen failures, missing/duplicate committed records, checksum drift, torn
transactions, and unbounded file/RSS growth. Vicia's deeper randomized kill-9
gate remains authoritative for format-specific fallback and WAL retirement.

## Licenses and isolation

The tool crate is excluded from the main Cargo workspace. Its dependencies do
not enter Vicia's library or published dependency graph. CozoDB is MPL-2.0,
the sqlite Rust wrapper is MIT, and redb is MIT OR Apache-2.0; they are used only to build this
local benchmark executable.

## Upstream references

- CozoDB embedded Datalog and time-travel documentation: <https://docs.cozodb.org/en/latest/>
- SQLite WAL and synchronous durability rules: <https://www.sqlite.org/pragma.html#pragma_synchronous>
- redb embedded ACID key-value scope: <https://github.com/cberner/redb>
