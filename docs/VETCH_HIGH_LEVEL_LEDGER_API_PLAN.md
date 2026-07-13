# Vetch-Oriented High-Level Ledger API Plan

Status: H0 measured and H2 bounded transaction-pinned current reads completed
on 2026-07-13. The typed commit facade remains deferred because it is not a
current Vetch blocker. H3 interactive/maintenance capability separation is the
next open slice.

Related authority:

- `docs/VETCH_CALLER_REQUIREMENTS.md`
- `docs/DURABILITY_AND_CALLER_RULES.md`
- `docs/VETCH_DELTA_STORAGE_ROADMAP.md`
- `docs/BENCHMARKS.md`
- Vetch Quiet Surface canonical migration plan:
  `/mnt/e/projects/obsidian/vetch/40-plans/2026-07-13-quiet-surface-canonical-architecture-migration-plan.md`

## Recommendation

Add one safe, high-level ledger facade above the existing Vicia storage and
Datalog engine. The normal API should make a receipt-sized atomic commit,
bounded current read, and consistent read view easy. Full scans, portability,
and recompact must remain explicit maintenance or raw operations.

The facade is generic. It does not know about cards, Condense, proposals,
verdicts, leases, or Vetch projection policy. Vetch compiles those product
transitions into typed fact changes and consumes the exact Vicia durability
receipt through an application-owned port.

This plan does not authorize a file-format change, a second query language, a
new authority store, or a compatibility break. The first implementation must
reuse the current fact identity, Datalog engine, delta publication, and
`BrowserDb.openPaged()` storage path.

## Why This API Exists

The current browser boundary already provides the important storage behavior:

- `executeAtomic()` preflights one mixed transact/retract batch;
- one successful call receives one `tx_id` and `tx_count`;
- one IndexedDB transaction publishes the dirty page set;
- the result reports durability and maintenance advice;
- `openPaged()` keeps foreground open and selective reads bounded;
- maintenance is explicit and runs outside the foreground authority handle.

The remaining problem is the caller shape. A caller can still:

- construct repeated Datalog strings for ordinary fact mutations;
- parse JSON string results into large JavaScript object graphs;
- reread a broad durable projection after a successful append;
- use whole-state snapshots as conflict or checkpoint authority;
- invoke an unbounded query where an exact entity read was intended;
- treat foreground and maintenance operations as one undifferentiated API.

The high-level facade removes those default footguns. It does not promise that
an arbitrary recursive or unbounded query becomes cheap. It makes expensive
work explicit and keeps the ordinary path proportional to the requested delta
or selected result.

## Philosophy Check

The proposal preserves Vicia's governing constraints:

- embedded library, not a server;
- one portable `.graph` image;
- Datalog remains the expressive query language;
- bi-temporal fact identity remains unchanged;
- no required external runtime or service;
- additive compatibility first;
- public API growth follows caller-shaped measurement.

The facade belongs above the engine. Product vocabulary, relation admission,
search, ranking, blob storage, model orchestration, and UI projection remain
outside Vicia.

## Ownership Boundary

```text
Vetch application command / admission policy
-> Vetch fact vocabulary and transition compiler
-> Vicia high-level ledger request
-> atomic fact publication
-> Vicia durability receipt
-> Vetch incremental projection patch
```

Vicia owns:

- fact encoding and full-history identity;
- atomicity, transaction ordering, and read-your-writes;
- durable publication classification;
- exact committed transaction cursor;
- bounded indexed reads and consistent temporal views;
- maintenance pressure classification;
- recovery and portability invariants.

Vetch owns:

- which source, proposal, relation, verdict, lease, or canvas change is
  admissible;
- product command and revision identity;
- product receipt facts and idempotency facts;
- semantic conflict policy;
- pending versus accepted UI state;
- projection patching, search indexes, and context packing;
- worker scheduling and user-facing recovery.

## Normative API Shape

The TypeScript below is the normative caller sketch for the browser binding.
Rust and session-protocol surfaces may use idiomatic representations but must
preserve the same semantics and receipt fields.

Names may change during H0 only when the semantic boundary remains the same.

### Value and change types

```ts
type ViciaEntityId = string;
type ViciaAttribute = `:${string}/${string}`;
type ViciaTxCursor = bigint;

type ViciaValue =
  | { type: "string"; value: string }
  | { type: "integer"; value: bigint }
  | { type: "float"; value: number }
  | { type: "boolean"; value: boolean }
  | { type: "ref"; value: ViciaEntityId }
  | { type: "keyword"; value: string }
  | { type: "null" };

interface ViciaValidTime {
  from?: bigint;
  to?: bigint;
}

type ViciaFactChange =
  | {
      operation: "assert";
      entity: ViciaEntityId;
      attribute: ViciaAttribute;
      value: ViciaValue;
      validTime?: ViciaValidTime;
    }
  | {
      operation: "retract";
      entity: ViciaEntityId;
      attribute: ViciaAttribute;
      value: ViciaValue;
      validTime?: ViciaValidTime;
    };
```

There is no `save(state)` operation. A request contains fact changes, never an
opaque application snapshot. A client helper may expose `replaceExact(from,
to)`, but it must compile to an exact retraction plus assertion and must not
invent last-write-wins semantics.

### Atomic expectations

```ts
type ViciaCommitExpectation =
  | {
      condition: "present";
      entity: ViciaEntityId;
      attribute: ViciaAttribute;
      value: ViciaValue;
    }
  | {
      condition: "absent";
      entity: ViciaEntityId;
      attribute: ViciaAttribute;
      value?: ViciaValue;
    };
```

Expectations, if admitted by H0 evidence, are checked in the same mutation
boundary as the changes. A failed expectation publishes no fact and consumes
no transaction identity. Expectations are generic compare-and-set guards, not
Vetch conflict policy.

If H0 shows that the existing exact-read plus serialized single-writer path is
sufficient for the first Vetch slice, expectations remain an internal design
candidate rather than blocking the initial facade.

### Commit request

```ts
interface ViciaCommitRequest {
  changes: readonly ViciaFactChange[];
  expect?: readonly ViciaCommitExpectation[];
  diagnostics?: {
    operationKind?: string;
    operationId?: string;
  };
}

interface ViciaCommittedFact {
  entity: ViciaEntityId;
  attribute: ViciaAttribute;
  value: ViciaValue;
  validFrom: bigint;
  validTo: bigint;
  txId: bigint;
  txCursor: ViciaTxCursor;
  asserted: boolean;
}

interface ViciaCommitReceipt {
  outcome: "committed";
  transaction: {
    id: bigint;
    cursor: ViciaTxCursor;
  };
  durability: "applied" | "published" | "memory";
  counts: {
    assertions: number;
    retractions: number;
  };
  committedFacts: readonly ViciaCommittedFact[];
  touched: {
    entities: readonly ViciaEntityId[];
    attributes: readonly ViciaAttribute[];
  };
  maintenance:
    | { state: "healthy" }
    | { state: "schedule-idle" }
    | { state: "backpressure" };
}
```

The foreground interactive batch limit must keep `committedFacts` receipt-sized.
Bulk import, migration, and other large operations use the maintenance or
portability surface and do not return an interactive receipt.

`diagnostics` is opaque attribution only. Vicia must not interpret an
`operationKind` such as `canvas.card.move` or `proposal.admit`, and it must not
use the field as hidden product authority. Durable product idempotency remains
an explicit fact plus, where needed, an atomic absence expectation.

### Interactive handle

```ts
interface ViciaInteractiveLedger {
  commit(request: ViciaCommitRequest): Promise<ViciaCommitReceipt>;
  current: ViciaCurrentReader;
  readView(options?: ViciaReadViewOptions): ViciaReadView;
  prepare(definition: ViciaPreparedQueryDefinition): ViciaPreparedQuery;
  close(): Promise<void>;
}

const ledger = await ViciaLedger.openInteractive({
  name: "vetch.authority",
  lockName: "vetch.authority.writer",
  budgets: "interactive",
});
```

The browser facade owns one-in-flight mutation ordering for its handle. The
configured Web Lock remains the cross-tab authority boundary. Opening an
interactive handle must select the measured paged path; it must not silently
fall back to eager full-image residency.

### Bounded current reads

```ts
interface ViciaCurrentReader {
  entities(request: {
    ids: readonly ViciaEntityId[];
    attributes: readonly ViciaAttribute[];
  }): Promise<readonly ViciaCurrentFact[]>;

  refsTo(request: {
    attribute: ViciaAttribute;
    value: ViciaEntityId;
    limit: number;
  }): Promise<readonly ViciaEntityId[]>;
}
```

The common current-read API requires an exact entity set or another indexed
seed plus explicit result bounds. It returns structured values, not a JSON
string that every caller reparses through a generic `readRows()` helper.

There is no `current.all()` or implicit full-scan method.

### Consistent read views and prepared Datalog

```ts
interface ViciaReadViewOptions {
  asOf?: ViciaTxCursor;
  validAt?: bigint | "now" | "any";
}

interface ViciaPreparedQueryDefinition {
  id: string;
  datalog: string;
  budget: {
    maxRows: number;
    maxBytes: number;
    requireIndexedSeed: boolean;
  };
}

interface ViciaReadView {
  current: ViciaCurrentReader;
  query<TBindings, TRow>(
    query: ViciaPreparedQuery<TBindings, TRow>,
    bindings: TBindings,
  ): Promise<readonly TRow[]>;
}
```

One read view pins transaction and valid-time selection across all reads used
to build a canvas projection, agent brief, or decision context. Repeated query
definitions prepare and cache their parsed plan by stable id plus source hash.

Budget enforcement must reject a result or plan that exceeds the admitted
foreground boundary. It must not silently truncate a query whose completeness
affects authority. Unbounded expressive Datalog remains available only through
the explicit raw surface.

### Incremental changes

```ts
interface ViciaChangePage {
  fromCursor: ViciaTxCursor;
  nextCursor: ViciaTxCursor;
  hasMore: boolean;
  facts: readonly ViciaCommittedFact[];
}

interface ViciaChangeRequest {
  after: ViciaTxCursor;
  attributes?: readonly ViciaAttribute[];
  entities?: readonly ViciaEntityId[];
  limit: number;
}
```

`changesSince(request)` is a conditional H4 surface, not an automatic H1
addition. First benchmark Vetch's existing transaction/event facts and indexed
Datalog cursor path. Promote the API only if the exact caller measurement shows
that the existing path cannot satisfy latency or allocation budgets.

If promoted, it must preserve the exact `export_fact_log_since()` identity and
ordering, include assertions and retractions, page results, and never scan the
whole committed ledger for a small tail.

Push subscriptions and database-owned projection callbacks remain rejected.
Polling or caller-owned wakeup plus a durable cursor is the first contract.

### Maintenance and portability handle

```ts
interface ViciaMaintenanceLedger {
  runIdleMaintenance(): Promise<ViciaMaintenanceReceipt>;
  verify(): Promise<ViciaVerificationReceipt>;
  exportGraph(): Promise<Uint8Array>;
  importGraph(graph: Uint8Array): Promise<ViciaImportReceipt>;
  close(): Promise<void>;
}
```

`openMaintenance()` is a separate capability surface. Browser use requires a
disposable DedicatedWorker and the same configured Web Lock. Interactive
handles do not expose recompact, import, full export, or full verification.

### Raw compatibility surface

The existing `BrowserDb`, `execute`, `executeAtomic`, and unrestricted Datalog
entrypoints remain supported during the additive adoption window. New
documentation and examples route ordinary callers through `ViciaLedger`.

If later removal is justified, it requires a separate major-version plan and
the repository's compatibility policy. This plan does not rename a stable
method to `unsafe` or remove an existing API.

## Required Semantics

Every implementation slice preserves:

1. Full-history identity: entity, attribute, encoded value, valid interval,
   `tx_count`, `tx_id`, and asserted state.
2. `Value::Ref` parity through base, delta, recompact, export/import, native,
   browser, and session surfaces.
3. One successful commit equals one transaction identity and one durable
   publication outcome.
4. A rejected request or failed expectation publishes no prefix.
5. A failed IndexedDB publication restores the previous durable image or
   poisons the handle; it never acknowledges uncertain state.
6. Foreground commit cost scales with request facts and dirty delta pages, not
   total graph size.
7. Foreground reads scale with selected facts and explicit bounds.
8. No foreground call hides checkpoint, full rebuild, recompact, import, or
   export.
9. Projection caches are caller-owned and rebuildable; they are not a second
   Vicia authority.

## Expected Vetch Paths

| Vetch transition | High-level Vicia use |
| --- | --- |
| Card create/edit/move/remove | Commit exact card and membership fact changes plus a Vetch command receipt fact. |
| Edge or group admission/removal | Commit relation or membership assertions/retractions atomically with the product receipt. |
| Condense landing | Commit packed card positions, group membership, run admission, and receipt after Vetch has shown a non-authoritative pending projection. |
| Source or packet capture | Append immutable source metadata and refs; keep large payloads outside Vicia. |
| Proposal creation | Append proposal facts without accepted status. |
| Proposal verdict/admission | Check the expected open state, replace status facts, append admitted product facts, and append the verdict receipt in one commit. |
| CapabilityLease issue/revoke | Commit lease scope, actor refs, validity, and explicit revocation facts. |
| Actor action and observed outcome | Commit the action receipt and outcome facts under Vetch-owned lease checks. |
| Agent brief/context | Build all source, decision, and receipt reads from one consistent read view. |
| Rebuildable projection refresh | Apply the commit receipt locally; use a measured cursor path after reopen or cross-runtime wakeup. |
| Backup, migration, recompact | Use the maintenance/portability handle outside foreground work. |

## Implementation Slices

### H0 — Exact caller contract and benchmark freeze

Status: complete as a measurement gate. No public API was added.

- Record Vetch `cards.move`, Condense admission, proposal verdict, and
  agent-brief read requests as typed fixture data.
- Hold changed facts constant while varying the committed baseline through the
  current 1M profile.
- Measure caller-side encoding, Datalog parse/materialization, Vicia mutation,
  IndexedDB publication, result decoding, exact proof read, and allocation
  separately.
- Decide whether atomic expectations are required for correctness or can remain
  behind Vetch's existing serialized conflict path in H1.
- Decide whether structured browser results need a binding change or an
  additive package facade can meet the allocation budget.
- Freeze the H1 types and budgets from evidence.

Gate:

- one fixture and one machine-readable result distinguish Vetch preprocessing,
  Vicia execution, browser publication, and Vetch projection work;
- the benchmark identifies a concrete public-surface gap rather than treating
  all caller cost as database cost.

#### H0 receipt and verdict — 2026-07-13

The durable fixture `vicia.vetch-ledger-caller-fixture.v1` records normalized
`cards.move`, Condense admission, proposal verdict, and agent-brief shapes from
the current Vetch authority adapter and Gate D trace. Each measured sample
derives fresh operation/entity identities, compiles the typed changes into the
current Datalog boundary, commits assertions and retractions under one
transaction cursor, and checks exact full-history identity plus a bounded proof
read.

The native smoke and full receipts each passed 26 correctness checks. On the
1M v12 fixture, caller-shaped mutation p95 was `1.181..2.220 ms`, Datalog
parse/materialization p95 was `0.023..0.040 ms`, and exact proof-read p95 was
`0.052..0.070 ms`. These paths remained delta/selection-sized relative to the
10K smoke profile.

The real-Chrome 1M paged run used 20 observations per scenario and separated
caller encoding, Rust preparation, mutation, IndexedDB publication, result
decode, and exact proof read. `executeAtomic()` p95 was `1.6..1.9 ms`; exact
proof-read p95 was `0.1..0.2 ms`; JSON decode p95 was at most `0.1 ms`.

H0 decisions:

- Keep atomic expectations out of H1. The current serialized writer can recheck
  the exact basis inside its lock, reject a stale second verdict, and consume no
  transaction identity. Reopen this only for a caller that cannot use that
  boundary.
- Keep the existing compact durability receipt. The transaction cursor plus
  caller-owned typed delta identifies and patches the accepted projection;
  echoing `committedFacts` and `touched` is not justified by latency,
  allocation, or correctness evidence.
- Do not change the wasm binding merely to avoid Datalog parsing or JSON
  decoding. Neither is a measured foreground bottleneck.
- Admit the consistent read view as the next public-surface candidate. The H0
  interleaving probe proves that separate agent-brief reads can observe
  different transaction cursors. H2 must pin one cursor and reject incomplete
  results; a facade that only wraps current raw calls does not solve this gap.

The H0 milestone is characterization evidence, not an absolute performance
budget. Numeric H1 budgets remain unset because H1 has no measured blocker.
The next implementation slice should define and verify the smallest pinned
read-view boundary before adding a broader `ViciaLedger` facade.

Canonical clean receipts are stored at
`benchmarks/baselines/vetch-ledger-caller/2026-07-13-local-h0/` from source
`f1beb28`.

### H1 — Typed commit facade over existing atomic publication

Status: deferred. H0 found no current correctness, latency, or allocation gap
that requires this facade before the pinned read view.

- Add internal typed fact-change and commit-receipt types shared by native core
  and browser binding code where practical.
- Implement the facade on the existing `executeAtomic` publication invariant;
  do not create another transaction engine.
- Return a structured receipt sufficient for caller projection patching without
  a durable-state reread.
- Keep the existing raw APIs unchanged.
- Bound interactive request and returned committed-fact sizes.

Gate:

- typed and raw equivalent commits produce identical fact-log records and
  current/as-of/valid-at results;
- parse/materialization and result-decoding allocations meet the H0 budget;
- browser failure injection preserves the existing restore-or-poison rule;
- native/browser tagged values and transaction identity remain equivalent.

### H2 — Bounded current reads and consistent read views

Status: complete. Native and browser read views pin one transaction and
valid-time selection across bounded Datalog, typed entity reads, and typed
reverse-reference reads.

The implemented native `ReadView` captures its cursor while serialized with
batch publication, then injects that cursor and one valid-time selection into
every query. Browser `BrowserReadView` provides current-time, explicit
`readViewAt`, and any-valid-time constructors over the same rule. Both paths
require an indexed seed and explicit row budgets; browser results also require
an explicit byte budget. Temporal or result-limit clauses inside the Datalog
source are rejected because the view owns those authority choices. Oversized
results fail as incomplete rather than being truncated.

This closes the H0 interleaving defect without introducing the broader
`ViciaLedger` facade or a second query engine. The real-Chrome regression proves
that a paged exact-entity view performs demand reads without a full-store read.
`ReadView::current_entities` and browser `BrowserReadView.currentEntities()` now
scan exact EAVT `(entity, attribute)` ranges with borrowed raw/prefix-leaf
projection, merge the resident delta in index order, and preserve scoped and
unscoped retract, `asOf`, and `validAt` semantics. Requests are bounded to 128
ids, 32 attributes, 256 distinct pairs, 10,000 complete rows, and 65,536
historical entries; browser results also retain the 8 MiB structured-result
ceiling. Exceeding any bound rejects without truncation.

`ReadView::refs_to` and browser `BrowserReadView.refsTo()` scan the exact VAET
`(target, attribute)` range through the same borrowed raw/prefix-leaf boundary.
They merge resident delta history without owned on-disk `VaetKey` decoding,
return source UUIDs in deterministic order, and preserve scoped/unscoped
retractions plus `asOf` and `validAt` selection. The Vetch current-card,
current-space membership, proposal status/verdict, receipt, and agent-brief
fixture produces the same results through typed readers and raw Datalog.

- Expose exact entity/attribute reads through the indexed current-view path. ✅
- Add browser prepared/bound query parity if H0 proves repeated parsing is
  material. Not admitted by H0.
- Add consistent `asOf` and `validAt` read-view selection. ✅
- Return structured bounded results. ✅
- Reject incomplete result truncation and unindexed foreground plans. ✅

Gate:

- current-card, current-space membership, proposal status, receipt, and
  agent-brief fixtures match raw Datalog results; ✅
- exact reads remain selective at the 1M baseline; ✅
- one multi-query read view cannot mix transaction cursors. ✅

#### H2 receipt and verdict — 2026-07-13

The clean `vicia.current-reader.v1` full receipt builds a checkpointed 1M-Ref
fixture and samples each typed reader 20 times. `currentEntities` records
`0.014/0.050 ms` p50/p95; `refsTo` records `0.011/0.028 ms`. Each exact read
visits one leaf and emits one projected entry. Owned EAVT/VAET key decodes and
all three full-leaf materialization metrics remain zero. The validator rejects
latency, leaf-scope, materialization, owned-key, emission, fixture-shape, and
dirty-source mutations.

The same fixture contract passes native raw-Datalog equivalence and the real
Chrome paged suite (71/71). Canonical evidence is stored at
`benchmarks/baselines/current-reader/2026-07-13-hal7800-h2-full/` from source
`2a0ef75`.

### H3 — Interactive and maintenance capability split

- Publish separate interactive and maintenance constructors/facades.
- Keep paged open mandatory for the browser interactive path.
- Route maintenance, verified export/import, and full integrity work through
  the disposable-worker contract.
- Preserve the existing low-level methods for compatibility.

Gate:

- ordinary examples cannot accidentally call recompact or full export through
  the interactive type;
- maintenance remains atomic and previous-state safe under fault injection;
- no foreground benchmark invokes O(total) work.

### H4 — Incremental change pages, only if measured

Promotion condition:

- a real Vetch projection refresh or cross-runtime continuation misses its
  latency or allocation budget using indexed Datalog/event facts; and
- `export_fact_log_since()` can supply the required tail without weakening
  history identity or result bounds.

If the condition is absent, close H4 as rejected and keep the public API
smaller.

### H5 — Adoption, documentation, and compatibility decision

- Update browser/native examples to use the high-level facade for ordinary
  work.
- Publish exact migration examples from raw calls.
- Record which raw APIs remain supported and for how long.
- Update `README.md`, `CHANGELOG.md`, `ROADMAP.md`, `docs/TEST_COVERAGE.md`, and
  API references only when an implementation phase actually lands.
- Do not couple this adoption to the Vicia rename or file-format work.

## Vetch Integration Sequence

Vetch Phase 0B.1 is independent and proceeds first. It owns checkpoint outcome,
dirty revision, retry, and conflict observation and must not wait for this API.

Vetch Phase 0B.2 defines an application-owned semantic delta checkpoint port
whose request and receipt intentionally match this plan's `commit()` boundary.
Its first adapter may translate that port to the existing `executeAtomic()`
API and normalize the current JSON receipt.

The Vicia implementation becomes a blocking dependency only when the isolated
caller benchmark or correctness proof demonstrates one of these gaps:

- atomic expectations are required but cannot be expressed safely;
- the receipt cannot identify the matching durable operation without a broad
  readback;
- raw Datalog parse/materialization or JSON decoding misses the foreground
  budget;
- an exact bounded read cannot use the existing indexed path;
- a required incremental tail cannot meet its budget through existing facts
  and queries.

When the high-level facade lands, Vetch replaces only the adapter translation.
Application checkpoint types, pending projections, conflict policy, and
renderer behavior do not change.

## Verification

Each implementation slice runs the focused invariant tests plus:

```bash
cargo test
cargo fmt -- --check
cargo clippy --lib -- -D warnings
git diff --check
```

Browser API changes additionally run:

```bash
CHROMEDRIVER=/usr/local/bin/chromedriver ./scripts/test-browser-wasm.sh
```

Performance acceptance uses the H0 exact-caller fixtures at small and 1M
baselines. It reports p50/p95/max latency, allocation, dirty pages, and result
bytes for encoding, commit, publication, decode, and any proof read separately.
No performance claim is accepted from a combined wall-clock number that hides
caller whole-state work.

The Vetch integration gate additionally requires its cloned-profile real
Windows Chrome/WebGPU motion evidence. Vicia's browser benchmark does not
replace product runtime evidence.

## Risk

The main risks are:

- moving Vetch schema or admission policy into Vicia;
- adding public APIs before the exact caller gap is measured;
- implementing a second mutation engine instead of wrapping the current atomic
  publication path;
- returning a large committed-fact array for bulk work;
- silently truncating authority-relevant queries;
- hiding O(total) maintenance behind an ergonomic foreground method;
- breaking stable raw APIs merely to make the new facade look canonical.

## Rejected

- `saveCanvas(state)` or another product-specific database method;
- automatic whole-projection readback after commit;
- a Vicia-owned canvas/search/context projection cache;
- database-owned push subscriptions before cursor polling is measured;
- foreground recompact, export, import, or full verification;
- replacing Datalog with a second general query language;
- a broad public change-feed API without an exact caller gate;
- a raw-API removal in the same slice as the additive facade.

## First Slice

Slice name:

```text
vicia.h0-vetch-ledger-api-caller-contract
```

First proof:

```text
The same fixed card-move and Condense deltas are replayed against small and 1M
Vicia authorities, and the result separates Vetch encoding/readback cost from
Vicia atomic publication cost while freezing the smallest typed commit receipt
that eliminates the broad readback.
```

Stop conditions:

- stop if the proposed facade requires a file-format change;
- stop if a generic expectation cannot preserve full-history identity;
- stop if the H0 evidence shows the measured wall is entirely Vetch-owned and
  the existing receipt already satisfies the adapter contract;
- stop and keep H4 private if indexed Datalog or the existing since-tail path
  already meets the projection-refresh budget.
