# Vetch Caller Requirements for Vicia DB

Status: product-caller requirements derived from the live Vetch integration and
the current Vetch epistemic-state and Braindump-to-GO plans.

Date: 2026-07-12

## Recommendation

Vicia DB should become Vetch's durable bi-temporal ledger and the source from
which current product projections can be rebuilt. It should not become Vetch's
semantic policy engine, search engine, blob store, or agent orchestrator.

The most important next outcome is not another write spike. It is a proven
authority cutover:

```text
Vetch command / source / receipt
-> atomic Vicia transaction
-> durable history
-> deterministic current projection
-> canvas, briefs, Explore, GO, and Learn
```

After that cutover, browser `localStorage` may remain an import source or an
explicit disposable cache. It must not remain an independent co-authority that
can disagree with Vicia.

## Evidence Read

This requirement set was derived from these live surfaces rather than from the
database roadmap alone:

- Vetch canvas persistence:
  `apps/quiet-surface/src/adapters/vicia-db/viciaCanvasPersistence.ts`
- Vetch persistence composition:
  `apps/quiet-surface/src/workspace/workspaceStore.svelte.ts`
- Legacy load/write path:
  `apps/quiet-surface/src/adapters/local-storage/canvasPersistence.ts`
- Browser contract and dual-write smoke tests:
  `apps/quiet-surface/scripts/smoke-vicia-authority-contract.mjs` and
  `apps/quiet-surface/scripts/smoke-vicia-canvas-dual-write.mjs`
- Vetch product model:
  `VETCH_EPISTEMIC_STATE.md` and `VETCH_BRAINDUMP_TO_GO.md` in the Vetch vault
- Vicia storage direction:
  `docs/VETCH_DELTA_STORAGE_ROADMAP.md`, `docs/DELTA_INDEX_DESIGN.md`, and
  `docs/MAINTENANCE_API_CONTRACT.md`

## Current Vetch Use

The default quiet-surface store uses `viciaBackedCanvasPersistence`, backed by
Vetch's local `@vicia-db/browser` build. The adapter opens the IndexedDB database
`vetch.quiet-surface.authority.v2` and writes Datalog facts for canvas command
events, cards, source references, edges, spaces, and manual groups.
Vetch main `6c5b1f7` vendors the clean `@vicia-db/browser` build from Vicia
`9c8ae60`. The adapter opens foreground authority handles through
`BrowserDb.openPaged()`, preflights any legacy v10 Vicia image before mount,
serializes the paged one-in-flight boundary, and runs strict import, verified
export, and maintenance in disposable workers under one shared Web Lock. The
Vicia-owned 1M matrix supplies the foreground and O(total) process evidence;
the Vetch Chrome acceptance supplies the caller lifecycle proof.

The integration has already proved useful primitives:

- browser IndexedDB persistence and reopen
- native-compatible `.graph` export/import
- atomic multi-fact Datalog transactions
- entity references and graph joins
- assertion, retraction, current-view, `:as-of`, and `:valid-at` semantics
- cards, edges, proposals, and receipts in one graph
- browser-side contract and real UI dual-write smoke coverage

However, Vicia is not yet the product authority:

1. Every persistence call writes the full `localStorage` snapshot first.
2. Canvas loading reads only that legacy snapshot.
3. Calls without a command skip Vicia entirely.
4. The Vicia facts are not sufficient to rebuild the complete current canvas.
5. A Vicia write failure can occur after the legacy snapshot has already
   succeeded, leaving the stores divergent.
6. Removal commands currently record removal-event entities rather than
   retracting or superseding every current-state fact they invalidate.
7. The separate canvas-graph Datalog synchronizer is not wired into the live
   store.

The current implementation is therefore a migration scaffold: Vicia receives
authority-shaped history, while `localStorage` still owns recovery and current
state.

## The Product Shape Vicia Must Support

Vetch is moving beyond canvas persistence. Its durable model includes:

- immutable source spans and source-backed assertion events
- propositions separated from the event that reported them
- support, challenge, dependency, derivation, and supersession relations
- epistemic assessments such as working, supported, conditional, contested,
  stale, and invalidated
- validity conditions and review triggers
- possible-world branches and their premises, lineage, and verification
- decisions, conditional plans, capability leases, actions, receipts, and
  verdicts
- observed outcomes that update later epistemic state without rewriting source

These objects share one storage need: append evidence and state transitions
immediately, retain the prior history, and cheaply ask both “what is usable
now?” and “why did it look different then?”

Vicia's fact identity and bi-temporal model are a strong fit. Vetch should encode
product meaning as ordinary entities, attributes, refs, assertions, and
retractions. Vicia should guarantee their durable and query-correct behavior.

## Required Capabilities

### P0 — Authority Cutover

#### Complete replayable writes

One accepted Vetch command must map to one atomic Vicia transaction containing
enough information to rebuild its affected state. Create, replace, move,
relation admission/removal, grouping, space changes, proposal verdicts, and
receipt updates all need explicit assertion/retraction or supersession
semantics. Event facts alone are insufficient unless replay is complete and
deterministic.

#### Deterministic current projections

Vetch must be able to query or rebuild:

- the current canvas for one space
- current cards, geometry, source refs, tags, groups, and accepted relations
- the latest accepted state of a proposal, receipt, plan, or epistemic
  assessment
- the source and transition chain explaining each current value

The result must not depend on browser session order, in-memory residue, or a
second snapshot store.

#### Read-your-writes and atomicity

After a successful transaction, the next query in that handle must see the
whole transaction. No reader may observe half of a command. Transaction order
and `:as-of` identity must be deterministic even when multiple writes share the
same wall-clock millisecond.

#### Explicit durability receipts

The caller needs to distinguish:

- applied and query-visible
- durably published
- rejected before application
- applied but maintenance still pending

Vetch should create its product receipt as facts, but the database result must
make these storage states unambiguous. Success must never mean that only the
legacy fallback was written.

#### Safe migration and fallback

The `localStorage` snapshot must be importable idempotently into an empty Vicia
graph. Cutover needs a parity check that rebuilds the projection from Vicia and
compares it with the legacy state before Vicia becomes authoritative.

After cutover, an automatic fallback may select only the previous valid
committed Vicia state. It must not silently select a newer divergent
`localStorage` snapshot. Corruption with no valid committed state must be a
visible error.

### P0 — Temporal and Graph Correctness

#### Preserve full-history identity

Base, delta, checkpoint, reopen, export/import, and recompact must preserve:

```text
entity
attribute
encoded value
valid_from
valid_to
tx_count
tx_id
asserted
```

`Value::Ref` is mandatory. Retractions must remain visible to full-history
surfaces while disappearing correctly from current projections.

#### Bi-temporal queries

The same query shape must correctly support:

- current transaction state at current valid time
- transaction-time `:as-of`
- valid-time `:valid-at`
- `:any-valid-time` history inspection
- combined transaction-time and valid-time selection

This is required for claims such as “A asserted B on T,” “B was considered
supported under condition C,” and “what did the plan depend on when GO was
issued?”

#### Stable references and joins

Vetch must safely link source spans, assertions, propositions, evidence,
branches, decisions, tasks, actions, and receipts with refs. VAET-style reverse
lookup and multi-hop Datalog joins must remain correct across retractions and
time travel.

### P0 — Interactive Write Cadence and Recovery

#### Receipt-sized append cost

Foreground append and checkpoint work must scale with the pending or delta
facts, not the total committed graph. The current roadmap's 1M-fact receipt
benchmarks are the governing evidence surface. A normal capture or receipt must
never trigger foreground full rebuild or recompact.

#### Crash-safe publish

WAL entries may retire only after durable publish. A failed or corrupt newest
candidate may fall back only to the previous valid committed manifest. If the
selected committed state itself is corrupt and no valid predecessor exists,
open must fail visibly rather than return a plausible partial graph.

#### Caller-scheduled maintenance

Native Vetch needs the existing `run_idle_maintenance()` contract: append now,
checkpoint in receipt/slice/import batches, and recompact only during idle,
background, startup, shutdown, or explicit maintenance windows. Maintenance
failure must not erase already durable writes.

The browser binding now exposes `runIdleMaintenance()`: it consumes the same
delta threshold policy, builds a fresh full-history image, atomically replaces
IndexedDB, and returns outcome/advice plus page counts. The 100K repeated-growth
gate proves page-record reclaim and write-latency reset. This closes the
maintenance-surface gap, not the full browser gate: maintenance is O(total
history) and must run in a disposable BrowserDb worker. `openPaged()` now avoids
the eager full-image startup shape for v11. Its 1M matrix measures a 17.8 ms
five-run maximum open, <= 7.4 ms selective cold point reads, 8.3 ms p95
one-fact writes, and 51.1 MiB maximum sampled PSS growth across open plus six
point probes. Recompact
remains explicit disposable-worker work: 16.679 s and a 2.09 GiB sampled PSS
delta in the same run.

### P1 — Efficient Product Reads

#### Bound query latency by selectivity

The hot reads are not whole-ledger scans. They are usually bound by space,
entity, attribute, source, status, task, or recent decision scope. Current and
historical point reads must scale primarily with matching facts. The existing
agent-brief and selective-index benchmarks should be extended with Vetch's
actual query shapes before adding a new public API.

#### Prepared parameterized queries in bindings

Native `Minigraf::prepare()` already moves parse work out of repeated query
execution. Vetch will repeatedly issue the same current-card, source-context,
dependency, freshness, and receipt queries with different bindings. Browser and
other Vetch bindings should support equivalent prepared/bound execution if
measurement shows Datalog string construction or parsing is material.

#### Incremental projection refresh

After a transaction, Vetch should refresh only affected canvas, brief, search,
and epistemic projections. First attempt this with normal indexed Datalog using
transaction/event facts and a stored cursor. Add a public transaction-range or
change-feed API only if a measured real caller query cannot meet the latency or
allocation gate through Datalog.

#### Bounded open and memory shape

Desktop and browser open, current-space reconstruction, and agent-brief queries
must remain usable at the expected 1M+ fact baseline. `BrowserDb.open()` keeps
the original eager behavior for compatibility. `BrowserDb.openPaged()` is the
v11 page-on-demand path: it reads bounded authority/catalog/manifest metadata,
fetches verified fact/index pages on deterministic query demand, and evicts
clean staging through the existing fixed-size cache boundary. Vetch `6c5b1f7`
adopts that exact measured path rather than a shadow database. Five real-Chrome
1M runs measured 16.6 ms p50 / 17.8 ms maximum open and 51.1 MiB maximum sampled
PSS growth across open plus six point probes; see `docs/BENCHMARKS.md` A5-6d. A
legacy v10 database's first paged open is a separate O(total) migration and
belongs in the disposable-worker cutover.

### P1 — Portability and Operations

- Native and browser must agree on file format, temporal semantics, refs,
  retractions, and query results.
- Export must represent a durable checkpointed graph, and import must be
  atomic: invalid input must not partially replace the live database. A Vetch
  authority cutover uses `importGraphForPagedAccess()`, which shares normal
  import migration and atomic replacement but rejects a recovery result that a
  fresh bounded `openPaged()` cannot own. General `importGraph()` retains the
  broader native-compatible recovery policy.
- Vetch needs integrity verification and actionable error classification for
  open, execute, checkpoint, import, and maintenance failures.
- Backup/restore must remain compatible with the single-file promise. Browser
  IndexedDB is an implementation detail; export/import is the portability
  boundary. Paged handles use `await exportGraphAsync()` so every published page
  crosses the v11 verifier without requiring the full image to remain resident;
  synchronous `exportGraph()` remains the eager/in-memory compatibility API.
- Native live-writer rollback points use `backup_to` (or session `backup`),
  which returns the exact included `tx_count` only after the checkpointed
  destination is fsynced and atomically published. External
  `checkpoint(); copy` is not a supported concurrency guarantee.
- Schema evolution should be represented by Vetch facts and migrations unless
  a database-format change is truly required.

## What Must Remain Vetch-Owned

The following are not Vicia core requirements:

- proposition extraction, claim classification, hidden-assumption detection,
  trust judgment, and epistemic-state policy
- branch generation, salience, verification strategy, decision ranking, and
  GO authorization
- relation admission and product authority rules
- BM25, vector search, embeddings, hybrid retrieval, reranking, and context
  packing
- note text, images, multimodal blobs, and large source payload storage
- TypeGPU/Rete geometry policy and UI projections
- automatic background scheduling, resource leases, agent orchestration, and
  external effect execution
- a scalar confidence feature or domain-specific validity-expression language

Vetch may store the evidence, classifications, conditions, and outcomes of
these processes in Vicia. Their meaning and transition rules remain Vetch
domain/application responsibilities.

## Acceptance Gates

Track boundary: Gate E blocks the browser authority cutover, not use of an
exact Vicia Git revision by the Windows-native desktop app. Native desktop
adoption is governed by Gates A–D; browser adoption additionally requires
Gate E.

### Gate A — Current-state reconstruction

Given a fixture covering every live canvas command, rebuild the complete state
from an empty Vicia projection and match the expected Vetch state without
reading `localStorage`. Include replace/remove, multi-value tags/group members,
relation admission/removal, source refs, and reopen.

### Gate B — Temporal epistemic ledger

Store one source assertion, one world proposition, support and counterevidence,
a conditional assessment, invalidation, decision, action receipt, and later
revalidation. Prove current, `:as-of`, `:valid-at`, and full-history queries,
including refs and retractions.

### Gate C — Failure and cutover

Fault-inject transaction, durable publish, reopen, import, and maintenance
failures. Prove atomicity, previous-valid-state recovery, no premature WAL
retirement, idempotent legacy import, and visible failure when no safe committed
state exists.

### Gate D — Real Vetch cadence

Replay a realistic mixture of capture, canvas edit, proposal, receipt,
epistemic transition, and agent-brief reads on a 1M+ fact base. Measure append,
checkpoint, current-space rebuild, selective current query, historical query,
reopen, memory, and file growth. Use receipt/slice batching plus idle
maintenance; do not hide recompact in the foreground path.

Status: the existing 1M cadence suites cover capture/edit/receipt append,
checkpoint, selective current/`:as-of` reads, and file growth. The one-run
Vetch trace still needs proposal, epistemic transition, current-space rebuild,
agent-brief, reopen/RSS, and a real-threshold maintenance cycle with
Vetch-owned product budgets. Gate D is not yet claimed complete.

### Gate E — Browser/native parity

Run the same portable graph and query corpus through native and BrowserDb.
Compare current/history results, export/import, corruption behavior, startup
memory, and long-running IndexedDB growth. This gate must pass before browser
Vicia replaces the legacy load path.

Status: tagged semantic parity, native↔browser portability, atomic rejection,
manifest fallback, and the shared corruption corpus pass in the 2×2 Gate E
matrix. File format v11 now adds generation-bound page-local base integrity,
and BrowserDb durably commits v10 migration before returning. A5-6c adds a
generation-aware `openPaged()` path, asynchronous verified full export, exact
page-0 authority checks across independent handles, sparse write rollback, and
post-import/maintenance return to sparse residency. The additive
`importGraphForPagedAccess()` cutover gate now accepts complete v10 migration
but rejects non-exportable truncated recovery before durable replacement,
while `importGraph()` remains recovery-compatible. All 57 structural browser
tests pass in the final headless-Chrome run. A5-6d now completes bounded 1M
open/query/growth and maintenance peak-memory evidence on the recorded host.
Foreground v11 open/query/write is bounded; legacy migration, import, full
export, and recompact are accepted only in a disposable DedicatedWorker. The
measured latter three sampled PSS deltas are 2.55 / 1.04 / 2.09 GiB. Vetch main
`6c5b1f7` consumes clean Vicia `9c8ae60`, adopts `openPaged()`, preflights v10,
uses strict paged-ready import, observes write advice, runs all O(total) work
under its shared Web Lock, terminates workers after success or failure, and
reopens through the bounded path. Its aggregate Chrome authority suite covers
the live Canvas, relation, Decision, GO, Braindump, and Condense callers plus a
visible fail-closed startup surface. **Gate E passes.** This verdict does not
claim Gate A's later removal of the legacy canvas co-authority or packaged
Windows WebView2 host verification.

## Recommended Work Order

1. Define and test a complete replayable canvas transaction model in Vetch.
2. Add Vicia-backed current-state reconstruction and legacy parity checking.
3. Close browser durability, recovery, and maintenance parity gaps exposed by
   that real caller.
4. Cut reads over to Vicia and demote `localStorage` to import/cache only.
5. Add the epistemic-state and decision/receipt ledger fixture.
6. Benchmark real current, historical, and incremental projection queries.
7. Add binding or public APIs only for measured gaps that indexed Datalog cannot
   solve cleanly.

## Decision Summary

The best Vicia for Vetch is a small, reliable temporal fact ledger with strong
transactions, recovery, selective Datalog reads, and native/browser parity. Its
job is to preserve what happened, what was valid, what is current, and how
those states are connected. Vetch's job is to decide what those facts mean and
what actions they authorize.
