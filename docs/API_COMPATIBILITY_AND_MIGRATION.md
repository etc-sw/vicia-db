# Capability API Compatibility and Migration

Status: H5-B canonical guidance, 2026-07-14.

Capability-scoped handles are the default for new application code. They make
foreground work bounds and maintenance ownership visible in the type surface.
`Minigraf` and `BrowserDb` remain supported throughout 1.x for advanced
Datalog and compatibility workloads that do not yet have a capability-scoped
replacement.

## Choose a Handle by Responsibility

| Responsibility | Native | Browser |
| --- | --- | --- |
| Foreground transact/retract and bounded reads | `InteractiveLedger` | `BrowserInteractiveLedger` |
| Idle maintenance and portability | `MaintenanceLedger` | `BrowserMaintenanceLedger` in a disposable worker |
| Rules, prepared queries, UDF registration, semantic `forget`, REPL-style mixed commands | `Minigraf` | `BrowserDb` where the feature exists |
| Benchmarks, corruption fixtures, migration recovery | `Minigraf` | `BrowserDb` |

Do not keep an interactive and maintenance handle open on the same database
at once. Browser writers and maintenance workers must acquire the same
caller-owned Web Lock. Read views require explicit row and byte budgets and
reject incomplete results instead of returning a truncated prefix.

## Native Migration

Replace ordinary mixed `Minigraf` use with two explicit lifetimes:

```rust
use minigraf::{InteractiveLedger, MaintenanceLedger, QueryResult, ReadViewOptions};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
  {
    let ledger = InteractiveLedger::open("myapp.graph")?;
    ledger.execute_write(
        r#"(transact [[:alice :person/name "Alice"]])"#,
    )?;
    let view = ledger.read_view(ReadViewOptions::default())?;
    let names = view.query(
        r#"(query [:find ?name :where [:alice :person/name ?name]])"#,
        16,
    )?;
    match names {
        QueryResult::QueryResults { results, .. } => assert_eq!(results.len(), 1),
        _ => unreachable!("read views accept query commands only"),
    }
  }

  {
    let maintenance = MaintenanceLedger::open("myapp.graph")?;
    maintenance.run_idle_maintenance()?;
    maintenance.backup_to("myapp-backup.graph")?;
  }

  Ok(())
}
```

Keep `Minigraf` when the caller actually needs rules, prepared queries, UDFs,
semantic `forget`, or REPL-style command dispatch. Do not simulate those
features with application-side scans merely to avoid the raw handle.

## Browser Migration

Replace `BrowserDb.openPaged()` plus mixed `execute()` calls with
`BrowserInteractiveLedger`. Put maintenance, verified export, and strict import
in a disposable module worker that opens `BrowserMaintenanceLedger` under the
same Web Lock. The runnable implementation is in `examples/browser/`.

```js
await navigator.locks.request(`minigraf:${dbName}`, async () => {
  const ledger = await BrowserInteractiveLedger.open(dbName);
  try {
    await ledger.executeAtomic([
      '(transact [[:alice :person/name "Alice"]])',
    ]);
    const view = ledger.readView();
    try {
      const result = JSON.parse(await view.query(
        '(query [:find ?name :where [:alice :person/name ?name]])',
        16,
        8192,
      ));
      console.log(result.results);
    } finally {
      view.free();
    }
  } finally {
    ledger.free();
  }
});
```

The maintenance worker opens a fresh capability for one operation, posts a
structured outcome, frees the handle, and terminates. Foreground code must
reopen after a maintenance or import operation rather than reuse a stale paged
generation.

## 2.0 Compatibility Policy

- `Minigraf` and `BrowserDb` remain source-compatible and supported for all
  1.x releases.
- Capability-covered mixed-authority methods are candidates for removal in
  2.0, not scheduled removals in the current release.
- Removal requires an exact replacement, a migration example, and regression
  evidence for every affected workload.
- No removal may ship before both 2.0 and twelve months after the first
  published release containing its explicit removal notice.
- The raw types cannot be removed while rules, prepared queries, UDF
  registration, semantic `forget`, REPL execution, eager recovery, or fixture
  construction still depend on them.
- This policy does not authorize a file-format, Datalog, or bi-temporal
  compatibility break.

No raw type or method is marked deprecated by H5-B. A future 2.0 API slice must
first close the replacement gaps above; only then may it add deprecation
attributes and start a method-specific removal clock.
