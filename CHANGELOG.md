# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Add capability-scoped ledger handles for ordinary and lifecycle work. Native `InteractiveLedger` exposes only transact/retract writes, explicit write transactions, bounded pinned read views, and the transaction cursor; its drop leaves WAL-backed work for an explicit `MaintenanceLedger` lifetime, which owns idle maintenance, backup, and full fact-log export. Browser `BrowserInteractiveLedger` and `BrowserMaintenanceLedger` both use the paged IndexedDB constructor and separate atomic writes/bounded reads from maintenance/verified export/strict import. Existing `Minigraf` and `BrowserDb` APIs remain compatible. Rust compile-fail guards, generated TypeScript surface validation, and 74 real-Chrome tests pin the boundary without changing file bytes or Datalog semantics.
- Transaction-pinned read views now expose bounded typed reverse-reference reads through native `ReadView::refs_to` and browser `BrowserReadView.refsTo()`. The exact `(target, attribute)` VAET path borrows existing raw/prefix postcard bytes, merges resident delta history in index order, preserves scoped/unscoped retraction plus `asOf`/`validAt` semantics, and rejects incomplete row or 65,536-entry history work instead of truncating. The clean 1M receipt visits one leaf, performs zero owned VAET key decodes and zero full-leaf materialization, and records `0.028 ms` p95. File-format bytes and Datalog remain unchanged.
- Transaction-pinned read views now expose bounded typed current-entity reads over exact EAVT `(entity, attribute)` ranges. Native `ReadView::current_entities` returns `CurrentFact` values; browser `BrowserReadView.currentEntities()` returns structured JavaScript rows. Both preserve request order, merge committed and resident history without owned on-disk EAVT key decoding, retain bi-temporal assert/retract semantics, and reject incomplete row or 65,536-entry history work instead of truncating. The browser path keeps paged generation checks and an 8 MiB structured-result bound. File-format bytes and Datalog remain unchanged.
- Native `ReadView` and browser `BrowserReadView` pin one transaction cursor and valid-time selection across multiple selective Datalog queries. The foreground boundary requires explicit result budgets, rejects unindexed plans and query-local temporal overrides before I/O, and rejects oversized results instead of truncating them. Browser views retain the paged generation-checked demand path and add explicit timestamp and any-valid-time constructors; raw expressive Datalog remains unchanged.
- File format v12 adds adaptive page-local prefix-compressed B+tree leaves with restart interval 16. Each leaf keeps the raw v11 representation unless prefix encoding is smaller; readers accept both codecs and fail closed on malformed prefix or restart records. Existing v11 graphs open byte-exact and retain cheap v11 delta checkpoints, while caller-scheduled idle maintenance performs the COW v12 publication. The final uncontended 1M fill-90 receipt shrinks the graph from 301.363 to 269.586 MiB and, after bounded checkpoint construction improvements, records a 3,775.192/4,256.684 ms checkpoint p50/p95 with a passing 112.75% tail. Vetch rollout remains gated on the repeated point-read gate and real-browser WASM execution.

### Fixed

- Bounded Datalog reads now enforce `max_rows`/`:max-results` during fact visitation, binding and branch generation, and native/browser aggregate input reduction instead of materializing the complete candidate range before a final row-count check. The first excess entry fails closed with no partial result. Native capability handles also use an explicit checkpoint policy: interactive writes and transactions never trigger threshold or drop checkpoints, maintenance open/drop never publishes implicitly, and raw `Minigraf` retains its compatibility auto-checkpoint behavior.
- Initial v12 base construction serializes borrowed index-key views, reuses EAVT order for exact single-attribute AEVT ordering, and emits standalone separator bytes only for each leaf's first key. These changes preserve byte-identical pages while reducing clean fill-90 checkpoint p50/p95 by 28.5%/35.3% from the prior uncontended v12 receipt.
- Initial/full checkpoints now cache canonical value bytes once and sort one reusable fact-position buffer per EAVT/AEVT/AVET/VAET index instead of retaining four complete typed-key vectors. The clean 1M storage-layout receipt reduces fill-75 median checkpoint peak RSS from 744.750 to 281.250 MiB and selects a 90% production bulk-build fill, shrinking the fixture from 352.742 to 301.363 MiB while passing checkpoint, point-read, aggregate, and RSS gates. Public API and v11 page format are unchanged.
- Reference DB v4 receipts measure point reads with one excluded warmup plus 5/20 raw samples and report p50/p95/max instead of treating one post-checkpoint observation as a regression gate. Summary JSON no longer exposes a flat mixed-contract row set, and Markdown renders engine aggregates separately from redb/Fjall owned-result storage scans, including separate memory tables.
- Selected-attribute aggregate cursors now capture one committed-reader publication for their full resumable lifetime and fail closed if checkpoint authority changes between steps. Repository-only diagnostics count the selected pending snapshot, committed/pending visits, exact fact resolutions, emitted rows, peak per-entity reducer state, and yield/resume calls without changing the default API or v11 format. The `usize::MAX` benchmark checkpoint sentinel now also suppresses `PersistentFactStorage`'s nested drop-save, so fresh children can replay and measure the intended WAL-pending state instead of silently publishing it on close.
- Cross-database stress receipts now separate two comparable scan contracts instead of ranking incompatible work in one `fullScan` column. `engineAggregate` makes Vicia, Cozo, and SQLite compute count/sum internally and return one scalar row; redb is explicitly N/A because it has no query engine. `materializedScan` makes all four adapters produce one owned `Vec<i64>` and use the same Rust count/checksum fold. Each scan runs in a fresh child process so its peak RSS excludes build/checkpoint/append history and the other scan. Receipt and summary schemas advance to v2 and validate exact engine-specific execution boundaries plus independent arithmetic checksums.
- Broad committed attribute reads now resolve AEVT `FactRef`s in physical packed-page order instead of logical index order, preventing the 1 MiB page cache from repeatedly evicting and rereading pages during scans. Single-pattern aggregates feed a shared incremental aggregation sink directly instead of retaining one `HashMap` binding per matched fact; grouped, distinct, window, and UDF semantics remain intact. On the same clean HAL7800 1.01M-fact graph used by the preceding scan diagnosis, `(count ?v)` improved from 5.39 s / 563 MiB peak RSS to 2.01 s / 379 MiB, while the full-result query improved from 5.41 s to 2.56 s at the same approximately 506 MiB result-materialization RSS. File format, Datalog syntax, public API, and result-order contract are unchanged.
- Browser `openPaged()` selective reads now amortize IndexedDB/WASM callbacks with access-shaped speculative windows: exact-entity plans fetch up to 8 neighboring pages and attribute ranges up to 64 in one transaction, while only the demanded page is required to exist and pass integrity verification. Ordinary Datalog matches no longer allocate four UUID-namespaced temporal metadata bindings per result row, single-lookup access plans skip redundant fact-identity deduplication, and the net-asserted fold borrows canonical EAV/window keys instead of cloning attributes, encoded values, and facts into its maps. On the clean HAL7800 1.01M-fact scan-only comparison, wall time fell from 9.96 s to 3.58 s and peak RSS from 975 MiB to 513 MiB; the full stress scan fell from 4.763 s to 3.115 s. File format and query semantics are unchanged.
- Repeated delta checkpoints now extend one resident committed fact/index view with only the newly published segment. The old path reread every selected segment and rebuilt/sorted the complete delta reader after each checkpoint, which pushed the Vetch 1M/1,024-slice checkpoint p95 to 132.806 ms. Resident segment reuse reduced it to 58.566 ms; incremental shared readers reduce the final exact-commit result to 3.098 ms. The shared view update and pending-fact retirement occur behind one `FactStorage` write barrier, so concurrent readers cannot observe the new facts in both pending and committed indexes. File format, manifest publication, and recovery selection are unchanged.
- Exact sets of up to 128 concrete entities now retain the selective committed-index path instead of being reclassified as a full scan after the fourth identity. Mixed entity/attribute plans keep the existing four-lookup cap, so the larger budget cannot admit broad attribute reads. Native no-full-scan coverage exercises the 128-entity boundary; a 4,000-fact real-Chrome `openPaged()` regression proves the same query uses only single-page IndexedDB demand reads, never range-prefetches the fact base, and releases sparse staging afterward.
- Datalog planning now keeps a real fact pattern and its following per-fact temporal pseudo-attributes (`:db/valid-from`, `:db/valid-to`, `:db/tx-count`, `:db/tx-id`) in one selectivity-sorted join unit. Previously, another real pattern for the same entity could be moved between them and overwrite the matcher's hidden fact metadata, causing both pseudo reads to report the later fact's transaction. Dependency-safe expressions may still run inside the unit, and top-level rule invocations are converted to derived-fact patterns in source position. Native and real-Chrome regressions cover exact transaction correlation plus expression/rule ordering.
- Independent IndexedDB handles now pin the exact page-0 bytes they opened and compare that authority in every sparse read and write transaction. A handle that observes a replacement or newer publish fails with a reopen error instead of mixing generations. Cheap clones share their own successful page-0 updates. This uses the existing numeric page-0 record as the compare-and-swap authority and introduces no browser-only schema/metadata key that an older package could misread.
- Browser `open()` now commits an automatic v10→v11 migration to IndexedDB before returning a handle. The generated base-page catalog and page 0 are written in one transaction; an aborted transaction exposes no handle and preserves the exact v10 image for a later retry. Browser export also verifies every immutable base page while reading it instead of bypassing the v11 integrity boundary.
- Legacy v1–v9 migration now derives the exact fact range from `node_count`, packed-page metadata, and v4/v5 index roots; validates the historical v4 fact CRC and v5–v9 page checksum rules; and fails closed on missing, undecodable, contradictory, or physically incomplete published data. Migration streams original ledger rows directly into an append-only v11 candidate, preserving exact duplicates and v9 scoped-retraction windows, and changes authority only with the final page-0 publish.
- Selective Datalog reads now fail closed when a committed index scan or fact-page resolve fails. The executor previously converted either storage error into a full-scan plan, which could hide corruption and return apparently valid results from a different read path. Query access planning is now a separate deterministic internal boundary shared by current and temporal reads, and declared packed-fact ranges reject wrong-type pages instead of silently dropping their rows.
- Native open no longer reinitializes a non-empty file shorter than one 4KB page. A 1–4095-byte database now fails visibly without modifying the corrupt prefix; zero-byte paths remain the supported new-database creation surface.
- Browser import/export now honors page 0's published prefix. Full unpublished tail pages are removed before the atomic IndexedDB replacement and never leak through `exportGraph`; a trailing partial page enters the same previous-manifest recovery policy as native open. Physically incomplete fallback images remain queryable but visibly non-exportable until repair.
- Browser write-through failures no longer leave a silently usable handle ahead of IndexedDB. An aborted commit reloads the previous durable graph and rejects the operation; if durable reload itself is impossible, the whole handle is poisoned and rejects query/write/export/import/checkpoint/maintenance until reopen. A same-handle mutation guard also prevents async write/import/checkpoint/maintenance overlap.
- Reopen now treats a WAL shorter than its 32-byte header as the safe lazy-create crash window: SIGKILL can land after `create_new()` but before the header is written, when no acknowledged entry can yet exist. Replay returns an empty tail and the next writer installs a complete fsynced header instead of leaving the database unopenable. Found by the A8-extended 2,400-cycle kill -9 gate; 0-, 7-, and 31-byte regressions live in `src/wal.rs`.
- WAL replay no longer resets the tx counter below the committed watermark when a crash leaves a WAL with zero replayable entries (header-only sidecar from a torn first append, or a checkpoint/delete race). Before the fix, the next write reused an already-committed `tx_count` and its WAL entry was skipped by the replay dedup rule — an acknowledged write silently lost, plus `:as-of N` ambiguity. Found by the A7 kill -9 harness on its first cycle; deterministic regression in `tests/wal_test.rs`.
- `.graph.lock` acquisition is now atomic (PID staged in a temp file and hard-linked into place). Before the fix, SIGKILL between lock-file creation and the PID write left a contentless lock that no liveness check could classify, blocking open until manual deletion. The contention path also heals empty/unparseable artifacts left by pre-fix binaries. Found by the A7 kill -9 gate at cycle 191; unit coverage in `src/storage/backend/file.rs`.
- Browser `BrowserDb.importGraph()` is now atomic (A5-1): the durable replacement commits in a single IndexedDB `clear`+`put` transaction *before* the live handle switches to the new data, and empty blobs are rejected as invalid input. Before the fix, the in-memory swap happened first — a failed flush left memory on the new database while IndexedDB kept the old pages, and later write-through flushes tore the durable state; imports smaller than the previous database also leaked stale trailing pages in IndexedDB forever (unbounded growth across imports, bloated `exportGraph` blobs after reopen). The shared IndexedDB transaction promise now also hooks `onabort`, so a non-request abort (e.g. quota exhaustion at commit) rejects instead of hanging forever. Regression coverage: six wasm tests in `src/browser/` (flush-failure ordering and shrinking-import stale-page tests are red on the old code).

### Added

- `vicia.pending-isolation.v3` replaces the five-owner pending representation with one canonical fact arena, interned attributes, `u32` `PendingFactId`, compact duplicate buckets, and logarithmically bounded sorted ID runs. Selected-attribute cursors snapshot IDs only, and WAL recovery applies one decoded transaction at a time. The clean 1M unrelated full receipt measures 221.445 MiB live database RSS (down from 1,152.316 MiB), 171.842 MiB accounted payload, 0.285 MiB replay-retained RSS, and unchanged exact count/checksum; the default public API and v11 format are unchanged.
- `vicia.pending-isolation.v2` clean-source benchmark receipts and `just pending-isolation-smoke` / `just pending-isolation-full`: v1 selected-cursor isolation plus non-cloning live ownership accounting for pending facts, duplicate keys, and EAVT/AEVT/AVET/VAET; exact owned-buffer counts; decoded-WAL overlap; and a separate fresh-child allocator-trim/post-drop RSS audit. HAL7800 evidence at source `84495e6` attributes the 1M pending shape to 747.949 MiB of direct payload across five structural owners and 9M small buffers, 404.367 MiB of container/allocator residual, and 139.617 MiB of replay-retained RSS. The default public API and v11 format are unchanged.
- Vetch-owned Gate D exact trace evidence: release `minigraf --session` on a 1M-fact base replays 1,024 capture/edit/proposal/receipt/epistemic slices, discovers the 1,024-card current space from persisted space membership, splits all exact reads at 128 entities, crosses the real delta threshold, runs maintenance outside foreground work, explicitly checkpoints, reopens, and verifies current/history/activation fingerprints. On the recorded WSL2 host at source `e60a7c2`, append/checkpoint/current/history/agent-brief/current-space p95 measured 2.378/3.098/0.259/0.214/0.590/173.988 ms; reopen was 4.357 ms, foreground RSS peaked at 57.480 MiB, maintenance took 7.510 s at 947.512 MiB peak RSS, foreground growth was 28.078 MiB, and every product budget passed.
- Vetch main `1b57689` vendors clean Vicia `e60a7c2`, closes the exact caller contract with a v3 commit/event/chunk canvas ledger and bounded 128-entity reads, rejects damaged activation/import anchors before later mutation, and preserves the full Gate D receipt at `qa/done/vicia-gate-d-full-e60a7c2.json`.
- `BrowserDb.executeAtomic(commands)` accepts a bounded write-only list of transact/retract commands and publishes the mixed facts under one transaction identity in one IndexedDB readwrite transaction containing the exact dirty-page set. Invalid commands, duplicate same-EAV ordering, resource-limit overflow, and IndexedDB aborts reject without exposing a partial live or durable state. Vetch uses this for event-plus-projection and Condense head replacement writes that cannot be expressed as one Datalog command.
- Gate E caller closeout evidence: Vetch main `6c5b1f7` vendors the clean `@vicia-db/browser` build from `9c8ae60`, moves every foreground authority handle to `openPaged()`, preflights legacy migration before mount, and runs strict import, verified export, and maintenance in disposable workers under the shared Web Lock. Its Chrome acceptance proves v10→v11 reopen, truncated-import preservation, advice scheduling, truthful termination receipts, all live ledger callers, and a visible fail-closed startup surface. This closes browser/native parity Gate E without claiming the separate Gate A authority cutover or packaged Windows WebView2 host smoke.
- `BrowserDb.importGraphForPagedAccess()` adds a strict, additive authority-cutover boundary while leaving `importGraph()` recovery-compatible. It constructs, validates, and migrates the candidate through the shared import pipeline, but rejects before IndexedDB or live-state replacement unless the resulting image is complete current-format v11 authority accepted by the bounded sparse planner. Complete v10 imports migrate and reopen through `openPaged()`; every shared non-exportable truncated-recovery mutation preserves the exact prior IndexedDB pages and live sentinel.
- A5-6d reproducible 1M paged-browser acceptance runner and evidence: Chrome 150 launches fresh renderers against one v11 IndexedDB profile, samples Linux process-tree RSS/PSS/private memory, measures first/middle/last cold and warm selective reads, 1,024 one-fact writes, verified async export, soft-threshold reopen, recompact, and post-maintenance reopen. `openPaged()` measured a 17.8 ms five-run maximum and the open-plus-six-probe phase added at most 51.1 MiB sampled PSS; writes measured 8.3 ms p95. Legacy v10 migration, import, export, and recompact remain explicit O(total) operations; the measured latter three added 2.55/1.04/2.09 GiB sampled PSS. This fixes the Vetch contract at a disposable DedicatedWorker rather than the UI renderer; Vetch `6c5b1f7` supplies the matching caller adoption.
- A5-6c generation-aware sparse IndexedDB paging: `BrowserDb.openPaged()` uses a two-phase v11 bootstrap (page 0 plus bounded catalog/manifest metadata, then exact candidate segment ranges until one valid recovery candidate is selected) and fetches immutable base fact/index pages on deterministic query demand. Selective failures remain errors; explicit full scans bulk-fetch only the declared base fact range and release staging afterward. A bounded resident page cache, sparse write rollback, v10 migration, import, and idle-maintenance convergence all retain the same authority boundary. `exportGraphAsync()` verifies and serialises the complete published image without requiring all pages to remain resident. `open()` and synchronous `exportGraph()` remain eager compatibility APIs. The browser source now contains 62 structural tests, including the later atomic mixed-write gates; A5-6d records the 1M matrix and Vetch `6c5b1f7` adopts the bounded path.
- A5-6b generation-bound base-page integrity (file format v11): each published immutable base has an in-file `MGPGC001` catalog containing one CRC32 per exact 4KB fact/index page, bound to the base generation and absolute page id. Page 0 carries a checksummed catalog descriptor; open reads only page 0 plus catalog/manifest metadata, while committed fact/index reads verify pages on first cache miss. Fresh/full/COW publishers sync and read back the catalog before page 0; v1–v9 migrations append a verified COW base without overwriting the legacy published image; v10 images append only the catalog without rewriting base/delta/manifest bytes. Selected-delta base corruption remains an error at first access, full-save and native backup refuse to bless/copy a corrupt base, and catalog/descriptor/truncation corruption rejects open without rewriting the source. CRC32 detects accidental corruption; it is not an authentication boundary.
- A5-5 shared Gate E tagged portability/corruption corpus: the same setup produces a native v10 fixture and a real-Chrome BrowserDb fixture; both consumers run both graphs and compare exact tagged current/history/valid-time/Ref/Keyword/retraction/VAET results. Shared mutations prove previous-manifest fallback and hard-error boundaries for slots, manifests, segments, truncation, headers, and unpublished tails. Browser query results now use the same lossless `src/json_value.rs` encoder as session frames. The then-23-test BrowserDb suite runs through a repeatable headless-Chrome script, now enforced by CI. The corpus exposed page-local base integrity as the remaining bounded-read storage blocker rather than claiming Gate E complete.
- A9 linearized live-writer backup: native `Minigraf::backup_to()` and the session `backup` op hold one write lock from source checkpoint through exact published-prefix copy, candidate fsync, and atomic no-clobber publish. `BackupOutcome`/session receipts report the exact included `tx_count` and bytes. Existing graph/WAL/lock targets, source aliases, in-memory handles, and missing parents reject; deterministic clone-writer, full-history, conflict-cleanup, and real child-session gates prove the backup is openable and excludes later writes. Public `checkpoint(); fs::copy()` is not the concurrency contract.
- A5-4 browser atomic compact maintenance: `BrowserDb.runIdleMaintenance()` applies existing delta thresholds, streams the full-history ledger into a fresh contiguous graph, atomically replaces IndexedDB, and swaps live state only after commit. Browser write results now expose ordered `tx_count`, `durability`, `maintenance_pending`, and `advice`. Four 100K-base soft-threshold cycles reclaimed stale page records and reset write latency; IndexedDB discovery now uses worker-compatible `globalThis` instead of `window`, and a real module DedicatedWorker smoke passes open/write/query/maintenance.
- A8 bulk valid-time closure (`(forget ...)`, harrekki P1 #6): atomically closes every valid-time window selected by a three-column EAV query or explicit fact list, optionally at `{:valid-to ...}`, by committing exact scoped retract + truncated re-assert records under one `tx_count` and one WAL entry. History and earlier `:as-of` snapshots remain intact; no-match calls are idempotent and consume no transaction. Exposed through native `Minigraf::execute`, session frames, REPL, and browser WASM; crash atomicity is audited by the extended A7 kill -9 harness and the 10k-result-set gate lives in `tests/forget_test.rs`.
- Durability and caller-rules documentation (A5-3, docs only — no behavior change): `docs/DURABILITY_AND_CALLER_RULES.md` classifies what `execute`/`checkpoint`/`importGraph` guarantee at return per backend (native WAL-fsync tier vs browser single-IndexedDB-transaction commit under the Chrome 121+ `relaxed` default), the browser flush-failure handle-poisoning rule, failure/corruption states for open/execute/checkpoint/import, the tagged value contract, and browser caller rules (Web Locks single-writer, batch+debounce, worker maintenance). Closes the A5-3 evidence gate in `docs/APP_ADOPTION_GAP_PLAN.md`.
- Browser IndexedDB growth benchmark mode (A5-2, measurement tooling only — no library behavior change): `examples/browser/bench-driver.cjs growth <cycles> <factsPerCycle> <sampleEvery> <fixture|empty>` drives repeated write executes against `BrowserDb`, sampling IndexedDB page count, header page/fact counts (parsed from page 0), storage estimate, heap, and per-commit latency percentiles, then measures the export→import round-trip and reopen-after-growth. Evidence recorded in `docs/BENCHMARKS.md` "A5: Browser IndexedDB Growth": browser write growth is unbounded (quadratic in commits, per-commit cost linear in delta segments, no reachable recompact, round-trip is a size identity).
- A2 incremental fact log (`Minigraf::export_fact_log_since(since_tx_count)`, harrekki P0 #2): returns exactly the `tx_count > since` subsequence of `export_fact_log()` — same `FactRecord` shape and deterministic storage order, assertions and retractions with their valid-time scope — at cost proportional to the tail. Committed packed pages are tx-nondecreasing (append-order checkpoints, order-preserving recompact), so a page-probe binary search locates the tail in O(log pages) reads even after a recompact folds it into the base; delta and pending layers filter in memory. Gate PASSED at a 1M-fact base: 100-record base tail in 91 µs cold vs 256 ms full export (`docs/BENCHMARKS.md` "A2: Incremental Fact Log"). Session op `export_since` implemented behind a proposed frame shape pending caller-lane ACK (`docs/SESSION_PROTOCOL.md`).
- A7 kill -9 durability harness (`tests/kill9_durability_test.rs`, harrekki P0 #3): SIGKILLs real `minigraf --session --file` child processes at randomized instants — including checkpoint-biased windows — over growing `.graph` lineages, auditing every acknowledged transaction after reopen (acked exactly-once, in-flight all-or-nothing, atomicity, phantoms, tx-count monotonicity, functional-after-recovery probe). Default-suite smoke plus `#[ignore]`d nightly gate. Gate PASSED: 2,400 kill cycles, 155,699 acked transactions, zero lost, zero unopenable files (`docs/BENCHMARKS.md` "A7: kill -9 Durability Gate").
- Add `Minigraf::run_idle_maintenance()` as an explicit embedder maintenance hook for file-backed databases
  - Checkpoints pending WAL-backed writes first, then runs private delta maintenance under the same write lock
  - Returns public `MaintenanceOutcome` with non-exhaustive checkpoint, delta, and advice enums instead of exposing internal `CheckpointOutcome`
  - Keeps raw recompact private and keeps foreground `checkpoint()` free of hidden threshold-triggered recompact
  - Added unit coverage for in-memory no-op, pending file-write checkpoint, threshold recompact, convergence, same-thread write transaction rejection, foreground checkpoint policy, and phase-2 failure visibility preservation

### Infrastructure

- Split Python, Node.js, and browser WASM/WASI bindings into independent repos under `project-minigraf` org (#231)
  - `minigraf-python`: https://github.com/project-minigraf/minigraf-python
  - `minigraf-node`: https://github.com/project-minigraf/minigraf-node
  - `minigraf-wasm`: https://github.com/project-minigraf/minigraf-wasm
- Add `cascade.yml`: publishes `minigraf-ffi` to crates.io and dispatches releases to binding repos on every version tag
- Publish `minigraf-ffi` to crates.io (previously internal only)

### Documentation

- Add `docs/ERROR_REFERENCE.md`: full inventory of user-facing errors (PRS/QRY/STG/WAL/API categories, 113 entries) with cause, resolution steps, and bad-input examples; docs-only reference codes PRS-001…API-009 (#192)
- Add `docs/MAINTENANCE_API_CONTRACT.md`: Q3-B caller guidance for `run_idle_maintenance()` safe windows, outcome semantics, retry/error policy, and Vetch scheduling boundaries

### Bug fixes

- Add deterministic append-only fact-log export for Vetch ledger receipts
  - `Minigraf::export_fact_log()` returns public `FactRecord` values with entity, attribute, value, `tx_id`, `tx_count`, valid-time scope, and `asserted`
  - `FactValidTime::AllValidTime` distinguishes legacy unscoped retractions from exact scoped valid-time windows
  - Public docs clarify that Datalog `Value::Ref` values require `#uuid "..."`; keyword value literals remain `Value::Keyword`
  - Added `tests/fact_log_export_test.rs` coverage for legacy retractions, scoped Ref-edge retractions, and checkpoint/reopen Ref preservation
- Fix indexed public Datalog queries silently collapsing same entity+attribute multi-value facts inserted in one transaction (#287)
  - `EAVT`, `AEVT`, `AVET`, and `VAET` index keys now include canonical value bytes plus `tx_id`/`asserted` identity so BTree-backed lookups preserve ledger-distinct facts sharing one `tx_count`
  - `selective_fact_fetch` deduplicates by full fact identity, including value bytes, valid-time window, `tx_id`, and `asserted`
  - Current file format is v9; v7/v8 packed files rebuild persisted indexes from fact pages on open
  - Added `tests/multivalue_index_test.rs` coverage for N=3/N=10 batches, mixed value types, ref edges, `:as-of`, `:valid-at`, retraction, and checkpoint/reopen
- Add valid-time scoped retract parity for public Datalog and explicit transactions
  - `(retract {:valid-from ... :valid-to ...} [...])` and per-fact `[e a v {:valid-from ...}]` maps now parse and execute like transact valid-time maps
  - Legacy `(retract [[e a v]])` remains an unscoped ledger retraction that removes every valid-time window for the same EAV triple
  - Scoped retractions remove only the exact valid-time window they name, including `Value::Ref` edge values
  - File format bumped to v9; v1–v8 legacy retractions are normalized to an explicit unscoped sentinel during migration/rebuild
  - Added `tests/retract_valid_time_test.rs` coverage for implicit execute, `WriteTransaction`, checkpoint/reopen, transaction-level options, and Ref edge values

## v1.1.1 — 2026-05-17

Patch release. Fixes cargo-dist Windows build failure that prevented REPL binaries and crates.io publish from completing for v1.1.0. No code changes to the library itself.

### Build

- Exclude `fuzz` crate from workspace so cargo-dist can build on Windows (MSVC linker incompatible with `#![no_main]` libFuzzer targets) — fixes #263

## v1.1.0 — 2026-05-17

Drop-in replacement for v1.0.0. No file-format changes, no public API changes, no query surface changes. Upgrading requires no code changes.

### Performance

- Hash-join replaces nested-loop join for multi-clause queries — O(N) instead of O(N²) for large fact sets (#202, #203, #204)
- Selective B+Tree fact fetch: queries with bound entity/attribute skip full-scan and read only relevant index pages (#208)
- Predicate push-down into join ordering (#207)
- Cost-based clause ordering for `not`/`or` rules (#206, #205)
- SIMD crossover analysis and benchmarking infrastructure (#229)

### Bug fixes

- Fixed critical correctness and durability bugs found during deep audit (#225): fact visibility edge cases, WAL entry ordering, checkpoint atomicity
- Fixed read-only handle `Drop` triggering unnecessary checkpoint, modifying the file on close (#226)
- `QueryResult::Transacted` and `Retracted` reverted to tuple variants — struct-variant form introduced post-1.0 was a breaking change (#261)
- Added `Minigraf::current_tx_count() -> u64` as additive API to expose the `:as-of` monotonic counter without breaking existing pattern matches

### Reliability & testing

- WAL fault injection harness (`FaultInjectingBackend`) with 9 crash-recovery tests (#209, #210, #214)
- Storage/migration resilience: migration matrix, index corruption recovery, concurrency stress tests (#215, #216, #217)
- Property-based query correctness tests against a reference evaluator (proptest, #212)
- Datalog parser/evaluator fuzz targets with seed corpus (#213)
- Per-module branch coverage gates in CI (#219)
- Long-haul smoke suite: 500 entities × 10 write/read/checkpoint cycles (#220)
- XTDB and Datomic semantic compatibility tests (#221)

### Internal

- Workspace-wide clippy lint enforcement — 336 violations fixed (#232)
- Grammar conformance test harness: pest shadow grammar + EDN corpus (#233)
- CI: codecov-action v3→v5, actions-rs→dtolnay migration

## Reliability Hardening — 2026-05-17

### Summary

Six PRs hardening the v1.0.0 codebase: WAL fault injection, storage/migration resilience, query correctness (property-based + coverage gates), long-haul smoke testing, and XTDB/Datomic semantic compatibility. No API changes, no file-format changes, no new runtime dependencies.

**PR #254 — #209 + #210 + #214: WAL Fault Injection**

- `FaultInjectingBackend` (`#[cfg(test)]`-only wrapper around `StorageBackend`) — configurable write-fail, flush-fail, read-fault injection
- 9 new WAL fault tests: write-fail propagation, flush-fail without data corruption, read-fault on WAL replay, CRC corruption discard, checkpoint atomicity under backend failure, partial checkpoint recovery via WAL replay, multi-writer serialisation, concurrent write+checkpoint, backend error propagation as `Err` not panic

**PR #257 — #215 + #216 + #217: Storage & Migration Resilience**

- `tests/migration_matrix_test.rs` — 5 migration tests: v7 round-trip, v3 empty migrate, corrupt magic returns `Err`, unsupported version returns `Err`, WAL replay idempotent
- `tests/index_corruption_test.rs` — 5 corruption-resilience tests: checksum mismatch triggers index rebuild, btree leaf/internal corrupt pages return `Err` without panic, root pointer mismatch handled, non-critical corruption still serves queries on good data
- 5 new concurrency stress tests in `tests/concurrency_test.rs`: stress readers during writer, failed write then success, rollback after partial work, open/write/checkpoint/query loop per thread, nightly stress loop (`#[ignore]`)

**PR #256 — #212 + #213 + #219: Property-Based Testing & Coverage Gates**

- `tests/property_test.rs` (proptest, `cfg(not(wasm32))`): 3 property tests — EAV model correctness vs naive reference evaluator, bi-temporal monotonicity, retract visibility invariant
- `.github/workflows/coverage-gates.yml` — per-module branch coverage thresholds; CI fails if coverage drops below gate

**PR #258 — #220: Long-Haul Smoke Suite**

- `tests/smoke_test.rs` (`#[ignore]` nightly): `smoke_large_graph_10_cycles` — 500 entities × 10 attributes × 10 update cycles; 7 invariants: active count (333), retracted count, fact count bounds, temporal snapshot integrity, prepared query consistency, recursive rule transitive closure, WAL checkpoint round-trip
- `.github/workflows/smoke.yml` — nightly 5am UTC, 15-min timeout, runs `--include-ignored`

**PR #259 — #221: XTDB & Datomic Compatibility Corpus**

- `tests/xtdb_compat_test.rs` — 10 semantic ports (Apache 2.0): EAV, tx-time `:as-of`, valid-time `:valid-at`, retraction (current + historical), Datalog join, negation, recursive rules, parameterised queries, combined bi-temporal
- `tests/datomic_compat_test.rs` — 9 independently written semantic ports: datom model, multi-entity attribute, tx-time `:as-of`, retract-entity, multi-variable `:find`, ground-value binding, parameterised query (prepared), named reusable rules, predicate expression filter

### Tests

935 tests passing (943 total, 8 ignored: 6 or+neg-cycle stratification doc tests, 1 nightly concurrency stress, 1 nightly smoke).

---

## Optimizer & Benchmarks — 2026-05-16

### Summary

Three optimisation PRs extending the v1.0.0 performance work. No API changes, no file-format changes, no new runtime dependencies. Test count unchanged at 850.

**PR #249 — #207 + #206: Predicate Push-Down & Mixed Rule Optimization**

- `optimizer::plan()` extended to accept `Expr` (predicate/arithmetic) clauses; they are interleaved at the earliest position where all their variables are bound, minimising intermediate binding sets
- `StratifiedEvaluator` gains mixed-rule path: when a rule stratum contains both positive-only rules and rules with `not`/`not-join`, the evaluator now evaluates all positive rules first and applies negation filters in a second pass within the same stratum — eliminates a class of ordering-dependent bugs in cross-stratum negation

**PR #251 — #205: Cost-Based `not`/`or` Ordering**

- `optimizer::plan()` assigns selectivity estimates to `not`/`not-join` and `or`/`or-join` clauses; they are sorted after their ground-variable producers but before unconstrained pattern scans
- `not`/`not-join` blocks placed after the patterns that produce their join variables (was: end of plan regardless); `or`/`or-join` blocks placed by estimated output cardinality

**PR #253 — #229: SIMD Benchmarking & Crossover Analysis**

- `benches/simd_helpers.rs`: `valid_time_filter_simd`, `as_of_filter_simd`, `sum_simd_i64` — portable SIMD kernels using `wide::i64x4` / `u64x4`
- Criterion benchmark groups: `simd_temporal`, `simd_as_of`, `simd_aggregate` — scalar vs. SIMD crossover analysis at 1K–1M facts
- Analysis result: SIMD crossover occurs at ~8K–16K facts per query; scalar path preferred below that threshold; SIMD integration deferred to post-1.0 backlog pending real-workload profiling

### Tests

850 tests passing (844 passing + 6 ignored: confirmed `or`+neg-cycle stratification bug, deferred to post-1.0 backlog). All optimizer PRs are optimisations; no new integration tests added.

---

## Performance — 2026-05-15

### Summary

Four O(N²) query-engine bottlenecks eliminated. No API changes, no file-format changes, no new dependencies.

**PR #246 — #208: B+Tree Selective Lookup**

- `get_facts_by_entity`, `get_facts_by_attribute`, `get_facts_by_entity_attribute` promoted from `#[cfg(test)]` to production in `src/graph/storage.rs`
- New `selective_fact_fetch` helper in `executor.rs`: inspects query patterns for bound entity literals and bound attribute strings; calls index-driven fetches instead of `get_all_facts()` when ≤4 distinct lookups detected
- `as_of` queries continue to use full scan (required for correctness)
- New benchmark groups: `btree_lookup/entity_point`, `btree_lookup/attribute_scan`

**PR #247 — #202 + #203 + #204: Hash-Join Cluster**

- **#202** (`executor.rs`): `not`/`not-join` bodies pre-computed once into `HashSet<Vec<(String, Value)>>` keyed on join variables; O(1) probe per outer binding replaces O(N) re-scan. `normalize_value` handles `keyword→Ref` representation asymmetry in value position.
- **#203** (`executor.rs`): `or`/`or-join` branches now evaluated from a single empty seed; branch results hash-joined back onto incoming bindings on shared user-visible variables (`__`-prefixed metadata keys excluded). `or-join` projects to `join_vars` before joining.
- **#204** (`matcher.rs`): `join_with_pattern` detects join variable (entity position first, value position second), builds `HashMap<Value, Vec<Bindings>>` once, probes per existing binding O(1). `normalize_join_value` handles `keyword→Ref`. Falls back to nested-loop when no join variable found.

### Tests

850 tests passing (844 passing + 6 ignored: confirmed `or`+neg-cycle stratification bug, deferred to post-1.0 backlog).

---

## v1.0.0 — Phase 8 Complete (2026-05-01)

### Milestone

This is the **v1.0.0 release**. The public Rust API and the `.graph` file format are now stable
and committed to semantic versioning. File format stability is guaranteed from this release.

### Phase 8 summary

All Phase 8 cross-platform targets have shipped:

- **8.1a** — Browser WASM (`BrowserDb`, `IndexedDbBackend`, `@minigraf/browser` on npm) — v0.20.0
- **8.1b** — Server-side WASM (`wasm32-wasip1` / WASI, Wasmtime/Wasmer CI) — v0.20.0
- **8.2** — Mobile bindings (Android `.aar` on GitHub Packages, iOS `.xcframework` via SPM, UniFFI) — v0.21.0
- **8.3a** — Python (`minigraf` on PyPI, pre-built wheels) — v0.22.0
- **8.3b** — Java/JVM (`io.github.adityamukho:minigraf-jvm` on Maven Central, fat JAR) — v0.23.0
- **8.3c** — C FFI (`minigraf.h` + platform tarballs on GitHub Releases) — v0.24.0
- **8.3d** — Node.js (`minigraf` on npm, pre-built `.node` binaries) — v0.25.0

### Also in this release

- `pkg/` renamed to `minigraf-wasm/`, `swift/` renamed to `minigraf-swift/` — consistent
  top-level naming across all workspace packages (issue #179)
- `@minigraf/browser` now published to npm on every tagged release (issue #179)
- `@minigraf/wasi` published to npm on every tagged release (issue #178) — WASI binary packaged for Node.js WASI consumers
- Per-platform READMEs added: `minigraf-wasm/`, `minigraf-node/`, `minigraf-ffi/python/`,
  `minigraf-c/`, `minigraf-ffi/java/`

### Tests

795 tests passing (788 passing + 7 ignored: confirmed `or`+neg-cycle stratification bug,
deferred to post-1.0 backlog).

## v0.25.0 — 2026-04-26

### Added
- **Phase 8.3d**: Node.js bindings published to npm as `minigraf`.
  Install with `npm install minigraf`. No build step required — prebuilt
  `.node` binaries for Linux x86_64/aarch64, macOS universal2, Windows x86_64.
  API: `new MiniGrafDb(path)`, `MiniGrafDb.inMemory()`, `.execute(datalog)`,
  `.checkpoint()`. Full TypeScript definitions included.

## v0.24.0 — Phase 8.3c: C Bindings (2026-04-26)

### Added
- **Phase 8.3c**: C bindings distributed as GitHub Releases tarballs.
  Download `minigraf-c-v0.24.0-<platform>.tar.gz` (Linux/macOS) or `.zip` (Windows)
  from the release page. Each archive contains the prebuilt shared library plus
  `minigraf.h`. API: `minigraf_open`, `minigraf_open_in_memory`, `minigraf_execute`,
  `minigraf_string_free`, `minigraf_checkpoint`, `minigraf_close`, `minigraf_last_error`.
  Memory contract mirrors SQLite: `minigraf_execute` returns a heap-allocated JSON string;
  call `minigraf_string_free` to release it.
- `minigraf-c/`: new workspace crate (`cdylib` + `staticlib`) — `Cargo.toml`, `src/lib.rs`
- `minigraf-c/cbindgen.toml`: cbindgen 0.29.2 configuration
- `minigraf-c/include/minigraf.h`: committed stable header (cbindgen-generated)
- `.github/workflows/c-ci.yml`: PR test matrix on 4 platforms + header drift check
- `.github/workflows/c-release.yml`: release workflow — builds + packages platform tarballs,
  uploads to GitHub Releases

795 tests.

## v0.23.0 — Phase 8.3b: Java Desktop JVM Bindings (2026-04-25)

### Added
- **Phase 8.3b**: Java desktop JVM bindings published to Maven Central as
  `io.github.adityamukho:minigraf-jvm:0.23.0`. Add to Gradle:
  `implementation("io.github.adityamukho:minigraf-jvm:0.23.0")`.
  Fat JAR with embedded natives for Linux x86_64/aarch64, macOS universal2,
  Windows x86_64. API: `MiniGrafDb.open(path)`, `MiniGrafDb.openInMemory()`,
  `.execute(datalog)`, `.checkpoint()`.
- `minigraf-ffi/java/`: Gradle 8.11 project — `build.gradle.kts`, `settings.gradle.kts`,
  `NativeLoader.kt` (runtime native extraction from JAR resources), and Gradle wrapper
- `minigraf-ffi/java/src/test/kotlin/.../BasicTest.kt`: JUnit 5 suite (in-memory, transact/query,
  error handling, file-backed persistence)
- `.github/workflows/java-ci.yml`: PR test matrix on 4 platforms (Linux x86_64, Linux aarch64,
  macOS universal2, Windows x86_64)
- `.github/workflows/java-release.yml`: release workflow — cross-compiles natives on 4 platforms,
  assembles fat JAR, publishes to Maven Central via Sonatype OSSRH

795 tests.

## v0.22.0 — Phase 8.3a: Python Bindings (2026-04-25)

### Added
- **Phase 8.3a**: Python bindings published to PyPI as `minigraf`.
  Install with `pip install minigraf`. API: `MiniGrafDb.open(path)`,
  `MiniGrafDb.open_in_memory()`, `.execute(datalog)`, `.checkpoint()`.
  Pre-built wheels for Linux x86_64/aarch64, macOS universal2, Windows x86_64.

## v0.21.1 — Patch: mobile/WASM docs (2026-04-19)

### Changed
- `src/lib.rs`: added **Feature Flags** section and **WebAssembly targets** subsection to crate-level docs — browser feature, `wasm32-unknown-unknown` target switcher note, and WASI build command
- `README.md`: updated "For Mobile Apps" section — replaced Phase 8 placeholder with current state, added Kotlin/Swift quick-start snippets and link to wiki integration guide
- Wiki `Use-Cases.md`: replaced Integration placeholder with full Android (Gradle setup, Kotlin API, error handling, threading) and iOS (SPM setup, Swift API, error handling, async) integration guides

795 tests.

## v0.21.0 — Phase 8.2: Android/iOS Mobile Bindings (2026-04-19)

### Added
- `minigraf-ffi` crate: UniFFI 0.31 bindings exposing `MiniGrafDb` (open, openInMemory, execute, checkpoint) and `MiniGrafError` (Parse, Query, Storage, Other) to Kotlin and Swift
- Android `.aar` release artifact, published to GitHub Packages (`io.github.adityamukho:minigraf-android`)
- iOS `.xcframework` release artifact, distributed via Swift Package Manager (`Package.swift` at repo root)
- `mobile.yml` CI workflow: cross-compiles Android targets with `cargo-ndk`, generates Kotlin/Swift UniFFI bindings, assembles AAR with Gradle, assembles xcframework with `xcodebuild`, and publishes both on every tag
- `docs-check` CI job in `rust.yml` and `release.yml` — gates releases on `cargo doc --all-features` passing cleanly

### Fixed
- `release.yml`: added `docs-check` to `host` job's `needs` and `if` condition
- `wasm-release.yml` / `mobile.yml`: retry loops extended from 20 to 40 attempts; `inputs.tag || github.ref_name` ordering corrected
- `minigraf-ffi/android/gradlew`: removed inner double-quotes from `DEFAULT_JVM_OPTS` and replaced xargs/sed eval block with direct `exec` — fixes "Could not find main class" and garbled usage output
- `minigraf-ffi/android/build.gradle.kts`: added `android { publishing { singleVariant("release") } }` — fixes AGP 8.x "SoftwareComponent 'release' not found"
- `mobile.yml` Package.swift commit: pushes to unprotected `swift-releases` branch and moves tag via `gh api -F force=true` — avoids branch-protection blocks and string/boolean type mismatch

795 tests.

## v0.20.1 — Patch: docs.rs browser module visibility (2026-04-19)

### Fixed
- `browser` module now appears on docs.rs: added `docsrs` to the `cfg` gate and `doc(cfg(...))` badge annotation (`src/lib.rs`)

## v0.20.0 — Phase 8.1: WebAssembly Support (2026-04-18)

### Added
- **Phase 8.1a** — Browser WASM (`wasm32-unknown-unknown` + `wasm-bindgen`):
  - `BrowserDb` public API: `open_in_memory()`, `execute()`, `checkpoint()`, `export_graph()`, `import_graph()`
  - `BrowserBufferBackend` — in-memory `StorageBackend` over a flat page buffer, identical byte layout to the native `.graph` format
  - `IndexedDbBackend` — page-granular IndexedDB storage (one 4 KB entry per page); only dirty pages written on checkpoint
  - `wasm-pack` build workflow (`wasm32-unknown-unknown --features browser`) generating `minigraf-wasm/` with JS glue and TypeScript definitions
  - `wasm-bindgen-test` browser integration tests (Chrome + Firefox via `wasm-pack test`)
- **Phase 8.1b** — Server-side WASM (`wasm32-wasip1` / WASI):
  - `FileBackend` verified under WASI's capability-based filesystem (no backend changes needed)
  - CI workflow (`wasm-wasi.yml`) builds, unit-tests, and smoke-tests under Wasmtime and Wasmer on every push/PR
  - Thread-dependent tests gated with `#[cfg(not(target_os = "wasi"))]`
- **Cross-platform compatibility tests** (issue #150):
  - `tests/cross_platform_compat_test.rs` — native round-trip (raw page byte copy) and fixture-readability tests
  - `tests/fixtures/compat.graph` — committed v7 binary fixture containing `:alice :name "Alice"` and `:alice :age 30`
  - `examples/generate_compat_fixture.rs` — reproducible fixture generator (native only; no-op on wasm32)
  - `native_fixture_readable_by_browser_db` wasm-bindgen-test — loads native fixture via `BrowserDb::import_graph`, verifies both facts
- Release workflow: WASM artifacts (WASI binary + browser tarball) built and attached on every tag; `cargo publish` to crates.io on release

795 tests.

## v0.19.0 — Phase 7.9: Publish Prep (2026-04-08)

### Changed (breaking — internal visibility only)
- `Minigraf::repl()` factory method replaces direct `Repl::new(FactStorage)` constructor — users call `db.repl().run()` instead
- All internal types narrowed to `pub(crate)`: `FactStorage`, `PersistentFactStorage`, `FileHeader`, `StorageBackend`, `DatalogExecutor`, `PatternMatcher`, `Fact`, `TxId`, `VALID_TIME_FOREVER`, `Wal`, and all related internals
- `Minigraf::inner_fact_storage()` removed (was unused)

### Added
- `Minigraf::repl(&self) -> Repl<'_>` — constructs an interactive REPL session; `Repl` now borrows `&Minigraf` for lifetime safety
- Full rustdoc on all public API items with `# Examples` doctests
- `[package.metadata.docs.rs]` in `Cargo.toml` — docs.rs builds with `all-features = true`
- `#![warn(missing_docs)]` — enforces documentation coverage going forward
- crates.io and docs.rs badges in `README.md`
- Installation section in `README.md` (`cargo add minigraf` / `[dependencies]` block)
- macOS and Windows added to CI test matrix (`rust.yml`)
- Strict `cargo clippy -- -D warnings` step in `rust-clippy.yml`

### Fixed
- Bare `.unwrap()` in library code replaced with `.expect("lock poisoned")` (RwLock operations in `cache.rs`, `evaluator.rs`) and `.expect("WAL not initialized")` (`db.rs`)
- `FileHeader::to_bytes` now takes `self` by value (clippy `wrong_self_convention`)
- Broken intra-doc link `[Repl::run]` in `db.rs` fixed to `[crate::repl::Repl::run]`

788 tests.

## v0.18.0 — Phase 7.8: Prepared Statements (2026-04-04)

### Added
- `Minigraf::prepare(query_str) -> Result<PreparedQuery>` — parse and plan a query once,
  returning a `PreparedQuery` that can be executed many times with different bind values
- `PreparedQuery::execute(bindings: &[(&str, BindValue)]) -> Result<QueryResult>` — substitute
  named `$slot` tokens and run against the current fact store state; plan is reused on each call
- `BindValue` enum — `Entity(Uuid)`, `Val(Value)`, `TxCount(u64)`, `Timestamp(i64)`,
  `AnyValidTime`; each variant is permitted only in the appropriate bind-slot position
- `$identifier` bind slot tokens in parser — accepted in entity position, value position,
  `:as-of`, and `:valid-at`; attribute position is intentionally rejected at prepare time
- `EdnValue::BindSlot(String)`, `AsOf::Slot(String)`, `ValidAt::Slot(String)`,
  `Expr::Slot(String)` AST variants (parse-only; panic at runtime if unsubstituted)
- `BindValue` and `PreparedQuery` re-exported from `lib.rs` (public API surface)
- `tests/prepared_statements_test.rs` — 17 integration tests covering all slot positions,
  combined temporal + entity parameterisation, plan reuse, and all error paths

### Internal
- `src/query/datalog/prepared.rs` — new module: `prepare_query()`, substitution logic,
  19 unit tests; manual `Debug` impl for `PreparedQuery` (avoids `FactStorage: Debug` bound)
- Panic guards (no slot-name interpolation) in `executor.rs` (4 sites) and `storage.rs` (1 site)
  for unsubstituted slot variants; CodeQL-safe (no user-controlled string in panic message)

### Unchanged
- `db.execute(str)` string API — no breaking change
- Executor, optimizer, matcher — no changes required

## v0.17.0 — Phase 7.7b: User-Defined Functions (2026-04-02)

### Added
- `Minigraf::register_aggregate(name, init, step, finalise)` — register a custom aggregate
  function usable in both `:find` grouping and `:over` (window) clauses
- `Minigraf::register_predicate(name, f)` — register a single-argument filter predicate
  usable in `[(name? ?var)]` `:where` clauses
- `FunctionRegistry::register_aggregate_desc` / `register_predicate_desc` (internal API)
- `WindowFunc::Udf(String)` and `UnaryOp::Udf(String)` AST variants for runtime-resolved functions
- `UdfOps`, `AggImpl`, `PredicateDesc` types in `functions.rs`

### Changed
- `AggregateDesc` now uses `AggImpl` discriminator instead of `window_compatible`+`window_ops`
- `apply_expr_clauses` now returns `Result<Vec<Binding>>` and accepts `&FunctionRegistry`
- `eval_expr` accepts `Option<&FunctionRegistry>` for UDF predicate resolution
- `WindowSpec::func_name()` now returns `String` instead of `&'static str`
- Parser emits `Udf` variants for unknown names instead of erroring (runtime validation)

### Test count: 727 tests

## v0.16.0 — Phase 7.7a: Window Functions (2026-04-02)

### Added
- **Window functions** in Datalog `:find` clause: `(sum ?v :over (...))`, `(count ?v :over (...))`, `(min ?v :over (...))`, `(max ?v :over (...))`, `(avg ?v :over (...))`, `(rank :over (...))`, `(row-number :over (...))` with unbounded-preceding (cumulative from partition start to current row) frame
- **`:partition-by ?var`** optional clause: absent means whole result set is one partition
- **`:order-by ?var`** required in every `:over` clause; `:desc` optional (default ascending)
- **`FunctionRegistry`** (`src/query/datalog/functions.rs`): string-keyed registry of aggregate descriptors; all built-in aggregates migrated into it; `window_ops` (init/step/finalise) on window-compatible entries; `is_builtin` flag separates built-ins from future UDFs
- **Mixed queries**: regular aggregates and window functions may coexist in the same `:find` clause; aggregates collapse rows first, windows annotate over collapsed rows
- **`AggregateDesc`**, **`AggState`**, **`WindowOps`** types in `functions.rs`
- **`WindowFunc`**, **`Order`**, **`WindowSpec`**, **`FindSpec::Window`** types in `types.rs`
- `tests/window_functions_test.rs`: 12 integration tests (cumulative sum, running count/min/avg, rank with ties, row-number, partition-by, desc ordering, mixed aggregate+window, single-row and empty-result edge cases, lag/lead parse rejection)

### Changed
- `FindSpec::Aggregate { func }`: type of `func` changed from `AggFunc` enum to `String`; dispatch goes through `FunctionRegistry` — internal change, no public API impact
- `AggFunc` enum removed from `types.rs`; all aggregate dispatch centralised in `functions.rs`
- `apply_aggregation` and `apply_agg_func` removed from `executor.rs`; replaced by `apply_post_processing` + helpers

### Total
707 tests (unit + integration + doc)

## v0.15.0 — Phase 7.6: Temporal Metadata Bindings (2026-04-01)

### Added
- **Temporal pseudo-attributes**: `:db/valid-from`, `:db/valid-to`, `:db/tx-count`, `:db/tx-id`, and `:db/valid-at` are now first-class bindable values in Datalog `:where` patterns
- `PseudoAttr` enum and `AttributeSpec` wrapper type in `types.rs` — clean type-safe representation for real vs. pseudo attributes in `Pattern`
- `parse_query_pattern` in `parser.rs` — detects `:db/*` keywords in the attribute position; rejects them in entity/value positions (parse error)
- `PatternMatcher::from_slice_with_valid_at` constructor — passes query-level `valid_at` into the matcher
- Hard-error guard in executor: per-fact pseudo-attrs (`:db/valid-from`, `:db/valid-to`, `:db/tx-count`, `:db/tx-id`) require `:any-valid-time`; error message tells user exactly what to add
- `:db/valid-at` binds the effective query timestamp: explicit `:valid-at <ts>` → `Value::Integer(ts)`, no `:valid-at` → `Value::Integer(now)`, `:any-valid-time` → `Value::Null`
- `:any-valid-time` now accepted as a standalone top-level query keyword (previously required `:valid-at :any-valid-time` form)
- `tests/temporal_metadata_test.rs`: 16 new integration tests covering time-interval range queries, time-point lookups, tx-time correlation, `:db/valid-at` semantics, and all parse/runtime error guards

### Total
647 tests (438 unit + 209 integration)

## v0.14.0 — Phase 7.5: Tests + Error Coverage (2026-03-31)

### Added
- `tests/production_patterns_test.rs`: 8 cross-feature integration tests combining not+as-of, not-join+count, count+not, count+valid-at, recursion+not, or+count, or+sum, count+as-of-sequence
- `tests/error_handling_test.rs`: 8 integration-level error-path tests covering runtime type errors (sum/string, sum/mixed, max/boolean), stratification errors (negative cycles), and parse safety errors (not-join unbound join var, or mismatched vars, aggregate unbound var)
- Stream 3: ~109 unit tests for parser-unreachable branches and aggregation/arithmetic edge cases in `executor.rs` and `evaluator.rs`
- `cargo-llvm-cov` branch coverage command documented in `CONTRIBUTING.md`
- CI coverage enforcement: `cargo-tarpaulin --fail-under 75` gates every PR; Codecov 75% threshold with 2% drop tolerance; `fail_ci_if_error: true`
- Nightly `cargo-llvm-cov --branch` workflow: uploads LCOV to Codecov (`branch-coverage` flag) and attaches HTML artifact (30-day retention); also triggerable via `workflow_dispatch`
- Codecov badge added to `README.md`

### Coverage
- Branch coverage: `executor.rs` ~85.71% (from ~75%), `evaluator.rs` ~89.29% (from ~73%)
- Remaining uncovered branches: NaN-check defensive code not reachable via public API
- Total: 617 tests (424 unit + 187 integration + 6 doc)

### Known Issues
- `or`-with-negative-cycle: stratification does not currently detect negative cycles inside `or` branches. Tracked via `#[ignore]` in `tests/error_handling_test.rs::or_negative_cycle_rejected`.

## [0.13.1] — 2026-03-27

### Performance

- **`filter_facts_for_query` snapshot fix** — function now returns `Arc<[Fact]>` instead of a throwaway `FactStorage`, eliminating the O(N) four-BTreeMap index rebuild that occurred on every non-rules query call. `execute_query` path constructs zero `FactStorage` objects. `execute_query_with_rules` still converts `Arc<[Fact]>` back to `FactStorage` for `StratifiedEvaluator` (deferred).
- ~62–65% speedup on non-rules queries at 10K facts: `query/point_entity/10k` 22 ms → 8.6 ms; `aggregation/count_scale/10k` 28 ms → 9.7 ms.
- Evaluator loop: `accumulated_facts` computed once per iteration (was 4 separate `get_asserted_facts()` calls).

### Added

- `PatternMatcher::from_slice(Arc<[Fact]>)` constructor — creates a matcher from an immutable fact snapshot without index reconstruction.

### Technical

- `apply_or_clauses` and `evaluate_not_join` signatures updated to accept `Arc<[Fact]>` instead of `&FactStorage`.
- 6 new tests: 4 in `matcher.rs` (unit), 2 in `executor.rs` (unit).

### Tests

- Total: 568 tests passing (390 unit + 172 integration + 6 doc)

## [0.13.0] — 2026-03-26

### Added
- **Disjunction (`or` / `or-join`)**: queries and rule bodies can now use `(or branch1 branch2 ...)` and `(or-join [?v...] branch1 branch2 ...)` where-clauses. Branches support all other clause types including `not`, `not-join`, `Expr`, and nested `or`/`or-join`. `(and ...)` groups multiple clauses into a single branch.
- `match_patterns_seeded` on `PatternMatcher` for seeded branch evaluation.
- `evaluate_branch` and `apply_or_clauses` as `pub(crate)` helpers in `executor.rs`.

### Technical
- `WhereClause` enum gains `Or(Vec<Vec<WhereClause>>)` and `OrJoin { join_vars, branches }` variants.
- `DependencyGraph::from_rules` refactored with recursive `collect_clause_deps` helper; `Or`/`OrJoin` branches contribute positive dependency edges.
- Rules with `or`/`or-join` in their bodies route to the `mixed_rules` path in `StratifiedEvaluator`.

## [0.12.0] - 2026-03-25

### Added
- `BinOp` enum (14 variants: `Lt`, `Gt`, `Lte`, `Gte`, `Eq`, `Neq`, `Add`, `Sub`, `Mul`, `Div`, `StartsWith`, `EndsWith`, `Contains`, `Matches`) in `types.rs`
- `UnaryOp` enum (5 variants: `StringQ`, `IntegerQ`, `FloatQ`, `BooleanQ`, `NilQ`) in `types.rs`
- `Expr` enum (`Var`, `Lit`, `BinOp`, `UnaryOp`) — composable expression AST in `types.rs`
- `WhereClause::Expr { expr: Expr, binding: Option<String> }` variant — `None` = filter, `Some(var)` = arithmetic binding
- `parse_expr_arg` / `parse_expr` / `parse_expr_clause` in `parser.rs`; dispatch at all 4 clause sites (query `:where`, rule body, `not` body, `not-join` body)
- Parse-time regex validation for `matches?` patterns via `regex-lite`; invalid patterns are rejected with a clear error
- `check_expr_safety` + `check_expr_safety_with_bound` in `parser.rs` — forward-pass safety check; recurses into `not`/`not-join` bodies; unbound `Expr::Var` references are rejected at parse time
- `outer_vars_from_clause` updated for `WhereClause::Expr` — binding variable contributes to scope for subsequent clauses
- `eval_expr`, `eval_binop`, `is_truthy`, `apply_expr_clauses` in `executor.rs` — evaluate expression trees against a binding; type mismatches and div/0 silently drop the row
- `apply_expr_clauses_in_evaluator` in `evaluator.rs` — sibling helper for rule body and `not-join` evaluation paths
- `not_body_matches` in `executor.rs` updated to seed with outer binding for expr-only `not` bodies
- `tests/predicate_expr_test.rs` — 28 integration tests covering all operators, silent-drop semantics, integer division, NaN, int/float promotion, string predicates, regex, expr in `not` body, expr in rule body, bi-temporal + expr, arithmetic into aggregate

### Semantics
- Comparison operators (`<`, `>`, `<=`, `>=`) require both operands to be numeric (`Integer` or `Float`); type mismatch → row dropped
- `=` / `!=` use structural equality on `Value` — type mismatch returns `false`/`true`, not an error
- Integer `+` `Float` promotes to `Float`; integer division truncates; division by zero → row dropped; NaN result → row dropped
- `is_truthy`: `Boolean(true)` → true; non-zero `Integer` or `Float` → true; everything else (including `Keyword`, `Ref`, `Null`, zero, empty string, `Boolean(false)`, `-0.0`) → false
- `matches?` pattern compiled at eval time via `regex-lite`; pattern must be a string literal validated at parse time

### Tests
- Added `tests/predicate_expr_test.rs` (28 integration tests)
- Total: 527 tests passing (365 unit + 156 integration + 6 doc)

## [0.11.0] - 2026-03-25

### Added
- Aggregation in `:find` clause: `count`, `count-distinct`, `sum`, `sum-distinct`, `min`, `max`
- `:with` grouping clause — variables that participate in grouping but are excluded from output rows
- `AggFunc` enum and `FindSpec` enum in `src/query/datalog/types.rs`; `DatalogQuery.find` migrated from `Vec<String>` to `Vec<FindSpec>`; `DatalogQuery.with_vars: Vec<String>` field added
- `apply_aggregation` post-processing step in `executor.rs` — runs after binding collection when any aggregate is present
- `extract_variables` helper in `executor.rs` — non-aggregate extraction path (replaces inline loops)
- `apply_agg_func` and `value_type_name` helpers in `executor.rs`
- `parse_aggregate` helper in `parser.rs`; `:find` arm extended to accept `EdnValue::List` (aggregate expressions); `:with` keyword arm added
- Parse-time validation: aggregate variables must be bound in `:where`; `:with` without any aggregate is rejected
- `tests/aggregation_test.rs` — 24 integration tests covering all aggregates, `:with`, rules, negation, temporal filters

### Semantics
- `count`/`count-distinct` with no grouping vars on zero bindings → `[[0]]` (SQL behavior)
- All other aggregates on zero bindings → empty result set
- All aggregates skip `Value::Null` silently (SQL behavior)
- Type mismatches (e.g. `sum` on `String`) fail fast with a runtime error
- `min`/`max` on mixed `Integer`/`Float` is a runtime error
- `:with ?v` adds `?v` to the grouping key without adding it to output columns

### Tests
- Added `tests/aggregation_test.rs` (24 integration tests)
- Total: 461 tests passing (327 unit + 128 integration + 6 doc)

## [0.10.0] - 2026-03-24

### Added
- `src/query/datalog/stratification.rs` — `DependencyGraph` and `stratify()`: analyse rule dependency graphs at registration time; programs with negative cycles are rejected with a clear error
- `WhereClause::Not(Vec<WhereClause>)` and `WhereClause::NotJoin { join_vars, clauses }` variants in `types.rs`; all exhaustive matches updated
- `(not clause…)` in `:where` and rule bodies — stratified negation where all body variables must be pre-bound by outer clauses
- `(not-join [?v…] clause…)` — existentially-quantified negation with explicit join-variable declaration; body variables not in `join_vars` are fresh/unbound
- Safety check at parse time: every `not` body variable must be bound by an outer clause; every `join_vars` variable in `not-join` must be bound by an outer clause
- Nesting constraint: `not-join` cannot appear inside `not` or another `not-join` — rejected at parse time
- `StratifiedEvaluator` in `evaluator.rs`: stratifies rules, runs positive rules first, then applies `not`/`not-join` filters per binding for mixed rules
- `evaluate_not_join` free function in `evaluator.rs`: builds partial binding from `join_vars`, converts `Pattern` and `RuleInvocation` body clauses to patterns, runs `PatternMatcher`; returns `true` if body is satisfiable (reject outer binding)
- `rule_invocation_to_pattern` extracted as `pub(super)` free function from `RecursiveEvaluator`
- Two not-post-filter sites in `executor.rs` now handle both `Not` and `NotJoin` via `evaluate_not_join`
- `tests/negation_test.rs` — 10 integration tests for `not` (Phase 7.1a): basic absence, multi-clause, rule body, time-travel, negative cycle rejection
- `tests/not_join_test.rs` — 14 integration tests for `not-join` (Phase 7.1b): basic exclusion, multiple join vars, multi-clause body, rule body, `:as-of`, `:valid-at`, negative cycle at registration, `not`+`not-join` coexistence, `RuleInvocation` in body end-to-end

### Changed
- `Rule.body` changed from `Vec<EdnValue>` to `Vec<WhereClause>` to support negation clauses alongside patterns
- `executor.rs` `execute_query_with_rules` now delegates to `StratifiedEvaluator` instead of `RecursiveEvaluator` directly
- `rules.rs` `register_rule` runs `stratify()` after each registration; returns `Err` on negative cycle (rules are not registered on error)

## [0.9.0] - 2026-03-23

### Added
- `src/storage/btree_v6.rs` — proper on-disk B+tree for all four covering indexes (EAVT, AEVT, AVET, VAET); each B+tree node is one 4KB page (internal + leaf), with `build_btree` for bulk-load and `range_scan` for leaf-chain traversal
- `OnDiskIndexReader` struct + `CommittedIndexReader` trait — page-cache-backed index lookup replacing the full in-memory BTreeMap; index memory usage is now O(cache_pages), not O(facts)
- `MutexStorageBackend<B>` adapter — holds backend mutex only for the duration of a single `read_page` call on a cache miss; cache-warm pages require no lock, enabling concurrent range scans to proceed in parallel
- `tests/btree_v6_test.rs` — 8 integration tests covering B+tree insert/range-scan, multi-page leaf chains, concurrent scan correctness with Barrier-synchronised threads, and v5→v6 migration roundtrip
- `test_concurrent_range_scans_correctness` unit test in `btree_v6.rs` — verifies all 8 concurrent threads return identical non-empty scan results
- `bench_concurrent_btree_scan` Criterion benchmark — measures wall-clock latency at 2/4/8 concurrent EAVT range scans; results updated in `BENCHMARKS.md`
- `FileHeader` v6 (80 bytes): adds `fact_page_count u64` field at bytes 72–80; automatic v5→v6 migration on first checkpoint

### Changed
- `FORMAT_VERSION` bumped 5→6; v5 databases auto-migrated on first save
- `BENCHMARKS.md` updated with v6 open/memory improvements, concurrent B+tree scan results, heaptrack v6 numbers, and a "How to read these numbers" methodology section
- `README.md` and `BENCHMARKS.md`: performance table updated to reflect v6 open-time reduction (~2.4×) and peak-heap reduction (~21%)

### Fixed
- Concurrent B+tree range scans no longer serialise on cache-warm pages — `4→8 thread` scaling ratio improved from ~2.2× to ~1.9×

## [0.8.0] - 2026-03-22

### Added
- `BENCHMARKS.md` — full Criterion benchmark results at 1K/10K/100K/1M facts with machine spec, HTML report references, and heaptrack memory profiles
- `examples/memory_profile.rs` — heaptrack profiling binary; accepts fact count as positional arg
- `Cargo.toml` metadata: `repository`, `keywords`, `categories`, `readme`, `documentation` fields
- Memory profile table in `README.md` "Performance" section

### Changed
- `README.md` Performance section now links to `BENCHMARKS.md` for full benchmark details
- Phase badge and status text updated to reflect Phase 6.4b completion
- crates.io publish deferred to Phase 7.8 (API cleanup + publish prep; file format v6 now complete)

### Removed
- Dead `clap` dependency from `[dependencies]` — `clap` was listed but never imported in library or binary code

## [0.7.1] - 2026-03-22

### Fixed
- Retraction semantics in Datalog queries: `filter_facts_for_query` Step 2 now computes the *net view* per `(entity, attribute, value)` triple via `net_asserted_facts()`. Previously, retracted facts continued to appear in query results because the original assertion record remained in the append-only log. Now, for each EAV triple in the tx window, only the record with the highest `tx_count` is considered — if it is a retraction, the triple is excluded from results.
- Oversized facts are now rejected early in `db.rs` (`check_fact_sizes`) before any WAL write, using the `MAX_FACT_BYTES` constant (4 080 bytes) exported from `packed_pages.rs`. Previously, oversized facts could cause a panic deep in the page-packing path.

### Added
- `net_asserted_facts(facts: Vec<Fact>) -> Vec<Fact>` helper in `src/graph/storage.rs`: groups facts by EAV triple, keeps the record with the highest `tx_count`, and discards the triple if that record is a retraction. Used by both `executor.rs` and `storage.rs`.
- `check_fact_sizes(facts: &[Fact])` in `src/db.rs`: validates all facts against `MAX_FACT_BYTES` and returns a descriptive `Err` before writing to the WAL.
- `MAX_FACT_BYTES: usize` constant in `src/storage/packed_pages.rs`: `PAGE_SIZE - PACKED_HEADER_SIZE - 4` = 4 080 bytes.
- `tests/retraction_test.rs` — 7 integration tests covering: assert/retract with no `:as-of`, as-of snapshot before/after retraction boundary, re-assert after retract, `:any-valid-time` with retraction, recursive rule retraction visibility at and before the retraction boundary.
- `tests/edge_cases_test.rs` — 4 integration tests covering: oversized-fact file-backed error path, `MAX_FACT_BYTES` exact boundary (accepted), `MAX_FACT_BYTES + 1` (rejected), in-memory database has no size limit.

## [0.7.0] - 2026-03-22

### Added
- Packed fact pages (`page_type = 0x02`): ~25 facts per 4KB page, ~25× disk space reduction vs v4
- LRU page cache (`src/storage/cache.rs`): configurable capacity (default 256 pages = 1MB)
- `OpenOptions::page_cache_size(usize)` — tune page cache capacity
- `CommittedFactReader` trait: index-driven fact resolution via page cache (no startup load-all)
- File format v5: `fact_page_format` header field; auto-migration from v4 on first open
- Page-based CRC32 checksum (v5): streams raw committed pages instead of all facts

### Changed
- `PersistentFactStorage::new()` takes `page_cache_capacity: usize` as second argument
- Committed facts no longer loaded into `Vec<Fact>` at startup; only pending facts held in memory
- `FactStorage::get_facts_by_entity`, `get_facts_by_attribute` use EAVT/AEVT index range scans

### Fixed
- v4 databases auto-migrated to v5 packed format on first open (no data loss)

## [0.6.0] - 2026-03-21

### Added
- Four Datomic-style covering indexes (EAVT, AEVT, AVET, VAET) with bi-temporal keys (`valid_from`, `valid_to` in all key tuples)
- `FactRef { page_id: u64, slot_index: u16 }` — forward-compatible disk location pointer (slot_index=0 in 6.1)
- Canonical value encoding (`encode_value`) with sort-order-preserving byte representation
- B+tree page serialization for index persistence (`src/storage/btree.rs`)
- `FileHeader` v4 (72 bytes): adds `eavt_root_page`, `aevt_root_page`, `avet_root_page`, `vaet_root_page` (4×8=32 bytes), `index_checksum` (u32), replacing the `reserved` field
- CRC32 sync check on open: index mismatch triggers automatic rebuild
- `FactStorage::replace_indexes()` and `index_counts()` for index lifecycle management
- Query optimizer (`src/query/datalog/optimizer.rs`): `IndexHint` enum, `select_index()`, `plan()` with selectivity-based join reordering
- Join reordering skipped under `wasm` feature flag
- `Cargo.toml` `[features]` section with `default = []` and `wasm = []`
- 6 integration tests in `tests/index_test.rs` for save/reload, bi-temporal, recursive rules regression

### Changed
- `FactStorage` internal structure: `FactData { facts, indexes }` under single `Arc<RwLock<FactData>>` for consistent snapshots
- `PersistentFactStorage::save()` writes index B+tree pages and updates header checksum
- `PersistentFactStorage::load()` performs sync check and fast-path index load
- `executor::execute_query()` now calls `optimizer::plan()` before pattern matching
- File format version bumped 3→4; automatic v1/v2/v3→v4 migration on first save
- `FORMAT_VERSION` constant updated to 4

### Fixed
- NaN values in `Value::Float` now canonicalize to a single bit pattern in index encoding (deterministic sort order)

## [0.5.0] - 2026-03-21

### Added
- Write-ahead log (WAL): fact-level sidecar `<db>.wal` with CRC32-protected binary entries
- `WriteTransaction` API: `begin_write()` / `commit()` / `rollback()` for explicit ACID transactions
- Crash recovery: WAL entries replayed on open; corrupt/partial entries discarded at first bad CRC32
- Checkpoint: `checkpoint()` flushes WAL facts to `.graph` and deletes the WAL; auto-checkpoint on configurable threshold
- `FileHeader` v3: `last_checkpointed_tx_count` field (repurposes unused `edge_count` slot)
- `FactStorage` helpers: `get_all_facts()`, `restore_tx_counter()`, `allocate_tx_count()`
- `OpenOptions` builder: `OpenOptions::new().path("db.graph").open()` or `Minigraf::in_memory()`
- `--file <path>` CLI flag for the REPL binary
- 41 new tests covering WAL, crash recovery, transactions, and checkpoint

### Changed
- `src/minigraf.rs` replaced by `src/db.rs` — `Minigraf`, `OpenOptions`, `WriteTransaction` public API
- File format version bumped 2→3; automatic v1/v2→v3 migration on first checkpoint
- REPL version string now tracks `CARGO_PKG_VERSION` automatically

### Fixed
- WAL-before-apply ordering: facts are now applied to in-memory state only after the WAL entry is fsynced, ensuring crash safety for both implicit (`execute()`) and explicit (`WriteTransaction`) write paths

## [0.4.0] - 2026-03-21

### Added
- Bi-temporal support: every fact now carries transaction time (`tx_id`, `tx_count`)
  and valid time (`valid_from`, `valid_to`)
- `:as-of N` query modifier for transaction time travel (counter or ISO 8601 timestamp)
- `:valid-at "date"` query modifier for valid time point-in-time queries
- `:valid-at :any-valid-time` to disable valid time filtering
- `(transact {:valid-from ... :valid-to ...} [...])` syntax for specifying valid time
- Per-fact valid time override in transact (4-element fact vectors with metadata map)
- File format version 2 with automatic migration from version 1

### Changed
- **Breaking behaviour**: queries without `:valid-at` now return only currently valid
  facts (`valid_from <= now < valid_to`). Existing Phase 3 databases are unaffected
  because all migrated facts have `valid_to = MAX`.
- `FactStorage::transact()` now accepts an optional `TransactOptions` parameter

### Fixed
- `PersistentFactStorage::load()` previously discarded original `tx_id` when loading
  facts from disk, making time-travel queries on persisted databases incorrect

## [0.3.0] - 2026-03-10

### Added
- Datalog core implementation with recursive rules
- Entity-Attribute-Value (EAV) data model
- Pattern matching with variable unification
- Semi-naive evaluation for recursive rules
- Transitive closure support with cycle handling
- Rule registry for rule management
- Persistent storage with postcard serialization
- REPL with multi-line command support and comments
- 123 comprehensive tests (94 unit + 26 integration + 3 doc)

### Changed
- Replaced GQL-inspired syntax with Datalog EDN syntax
- Data model changed from property graph to EAV triples
- Query executor rewritten for Datalog pattern matching

## [0.2.0] - 2026-02-01

### Added
- Persistent storage backend with `.graph` file format
- StorageBackend trait for platform abstraction
- FileBackend implementation (4KB pages, cross-platform)
- MemoryBackend for testing
- PersistentGraphStorage layer for serialization
- Embedded API (`Minigraf::open()`, `Minigraf::execute()`)
- Auto-save on drop

### Changed
- Graph storage now supports persistence

## [0.1.0] - 2026-01-15

### Added
- Initial release
- In-memory property graph implementation
- Basic graph operations (nodes, edges, properties)
- Interactive REPL
- Thread-safe storage with `Arc<RwLock<>>`
