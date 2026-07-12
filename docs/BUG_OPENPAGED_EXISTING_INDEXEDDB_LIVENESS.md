# P0 Bug Request: `BrowserDb.openPaged()` Must Terminate on Existing IndexedDB State

## Status

- Priority: P0 reliability and browser adoption blocker
- Affected API: `BrowserDb.openPaged(database_name)`
- Observed consumer: Vetch quiet-surface
- Observed Vetch commit: `0992a44`
- Vendored Vicia source commit: `e60a7c298a66de486fa4615a085e6aac547b0800`
- Environment: Chrome 150 on Windows, Vite-hosted browser WASM

This is a request to repair an existing storage liveness contract. It does not
request a new public API, file-format change, server process, or Vetch-specific
behavior in Vicia.

## Summary

One preserved, previously usable Vetch authority database causes
`BrowserDb.openPaged("vetch.quiet-surface.authority.v2")` to consume CPU
indefinitely without resolving or rejecting its Promise.

The Vetch application awaits this open in a disposable lifecycle worker before
mounting its workspace. Because Vicia never returns, the worker retains its Web
Lock, the application root remains empty, and no storage error can be shown
until the caller's five-minute watchdog terminates the worker.

The required Vicia contract is:

```text
valid recoverable image
  -> open succeeds with the selected committed state

invalid, incomplete, cyclic, or unsupported selected image
  -> open rejects in bounded time without modifying IndexedDB
```

Returning a better message is not sufficient. The defect is non-termination.

## Confirmed Evidence

The following facts were observed against the preserved failing browser
profile:

1. The Vetch HTML, lifecycle worker module, JavaScript glue, and WASM asset load.
2. Startup stops before workspace/store construction, at the lifecycle prepare
   operation that calls `BrowserDb.openPaged()`.
3. The affected Chrome renderer accumulates more than 100 seconds of CPU and
   does not answer a DevTools `Runtime.evaluate` request within 15 seconds.
4. Clearing normal HTTP cache and JavaScript code cache does not change the
   failure.
5. Removing only the `http://127.0.0.1:8782` IndexedDB authority state makes
   the same profile open Vetch normally.
6. A fresh profile running the same Vetch and Vicia builds reaches a ready
   TypeGPU canvas with no long tasks; measured `DOMContentLoaded` was about
   121-124 ms.
7. The affected origin exposes these IndexedDB databases:
   - `vetch.quiet-surface.authority.v2`
   - `vetch.quiet-surface.canvas-persistence-journal`
8. No `authority.v1` database was present in this reproduced failure. A
   simultaneous v1/v2 database-name migration is therefore not required to
   trigger the bug.
9. The authority object store contains numeric 4 KiB page records. Page 0
   begins with `MGRF` and declares file-format version 11.
10. The separate Vetch canvas journal contains pending/conflict intents, but it
    cannot be the direct cause of this pre-mount stall: Vetch constructs and
    recovers that store only after `prepareViciaAuthorityPagedStorage()` has
    completed.

The preserved profile may contain private Vetch content. Do not commit it or
attach it to a public issue. Extract the smallest sanitized page-map fixture
that still reproduces the open behavior before implementation.

## What Is Not Yet Proven

The exact internal non-terminating stage has not been identified. Plausible
locations include:

- recovery-candidate or manifest selection;
- delta/segment chain traversal;
- base-page catalog traversal;
- page-reference or free-list traversal;
- v11 checkpoint reconstruction;
- a retry loop around an IndexedDB or integrity outcome.

These are investigation targets, not findings. Do not describe the bug as a
v1-to-v2 migration failure without new evidence.

## Required Investigation

Add bounded stage evidence around the existing open path. At minimum,
distinguish:

```text
IndexedDB connection opened
page 0 read and decoded
published prefix validated
catalog/manifest candidates enumerated
recovery candidate selected
base and selected deltas accepted
paged handle returned
```

The evidence mechanism may be test-only or internal. Avoid adding a public
progress API before the failing internal stage and required caller contract are
proven.

Audit every loop or graph walk reachable from `openPaged()` for:

- a visited-page/segment/candidate set;
- a maximum bounded by the declared and physically available page count;
- checked arithmetic for page/range calculations;
- explicit missing-page and wrong-page-type errors;
- cycle detection;
- monotonic progress across fallback candidates;
- no retry of an identical IndexedDB read or recovery candidate without a
  state change.

## Required Recovery Semantics

Preserve the current recovery policy:

- A corrupt newest candidate may fall back to a previous valid committed
  state.
- A selected corrupt or physically incomplete state with no valid predecessor
  must reject.
- Open failure must not clear, rewrite, migrate, compact, or silently replace
  the source with an empty database.
- An automatic migration may publish only after the complete candidate is
  validated and its IndexedDB transaction commits atomically.
- Full-history identity and `Value::Ref` behavior must remain unchanged.

If the preserved image is valid according to the existing v11 format, the fix
must open it successfully. If it is invalid, the fix must identify the first
violated invariant and reject without mutation.

## Acceptance Tests

Create a real-browser regression using a sanitized derivative of the failing
numeric page map.

Required cases:

1. The exact minimized failure fixture causes the old build to exceed the test
   deadline or enter the identified loop.
2. The fixed build resolves or rejects within a strict bounded deadline.
3. A rejection reports the violated storage invariant and leaves every
   IndexedDB key/value byte-identical.
4. If a previous valid committed candidate exists, open selects it and exposes
   the expected exact ledger rows.
5. If no valid committed candidate exists, open rejects rather than returning
   a partial or empty graph.
6. Repeated open attempts produce the same outcome and do not grow or rewrite
   IndexedDB.
7. The existing current-format sparse-open benchmark and 1M acceptance remain
   within their recorded bounds.
8. Native/browser corruption parity and tagged `Ref`/history fixtures remain
   green.

The test deadline is a harness guard, not the implementation. The implementation
must terminate through structural bounds and invariant checks rather than an
internal wall-clock timeout.

## Vetch Caller Work, Not Closure for This Bug

Vetch should independently:

- mount a startup shell before authority preparation;
- terminate its disposable worker after a short product deadline;
- show a visible failure/recovery surface;
- preserve or quarantine raw IndexedDB pages before an explicit reset.

Those measures protect product availability. They do not close this Vicia bug,
because a valid embedded database call must not consume CPU forever or retain a
caller lock indefinitely.

## Recommendation

Treat this as a narrow browser storage liveness slice before further browser
release-readiness claims.

## Risk

High data-integrity risk if fixed by broad fallback, database deletion, or
silent partial recovery. Medium compatibility risk if the repair changes v11
candidate selection. Low API risk if kept internal.

## Required Gate

Recovery gate plus the existing real-Chrome browser corruption and sparse-open
measurement gates.

## First Slice

Minimize and sanitize the preserved page map, locate the first non-progressing
stage, and add a red real-browser liveness regression before changing recovery
logic.

## Verification

- focused real-Chrome fixture regression;
- browser WASM test suite;
- native/browser Gate E corruption corpus;
- current `openPaged()` sparse-open benchmark;
- `cargo test`;
- `cargo fmt -- --check`;
- `cargo clippy --lib -- -D warnings`;
- `git diff --check`.

## Rejected

- treating a shorter Vetch timeout as the database fix;
- clearing the caller's IndexedDB automatically;
- parsing or repairing Vicia page semantics in Vetch;
- assuming v1/v2 database coexistence without evidence;
- adding a public progress API before the internal failing stage is known.

## Stop Conditions

Stop and re-scope before implementation if:

- the failure cannot be reproduced from a sanitized fixture;
- the only proposed repair drops selected committed facts;
- the repair requires a file-format change or public API change;
- the change widens into browser storage redesign rather than bounded
  `openPaged()` recovery.
