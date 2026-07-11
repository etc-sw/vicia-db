# Minigraf Test Coverage Report

**Last Updated**: bounded exact-entity query planning (July 2026), 1286 native tests + 59 browser WASM tests in source

## Test Summary

**Total Tests**: 1286 ✅ (1274 passing, 12 ignored)
- ✅ 860 unit tests (lib — includes Wave 1 hash-join and selective-lookup test modules, bounded 128-entity selective planning/execution regressions, fact-metadata bundle planning and expression-boundary regressions, Wave 3 fault-injection unit tests, per-query limits #288, magic sets #289, ledger identity index regressions #287, scoped retract parser/storage regressions, v1–v9 duplicate/window-preserving COW migration gates, v10 delta manifest/segment/header migration gates, v11 descriptor/catalog codec and lazy fact/index corruption gates, bounded-open page-id accounting, fail-closed legacy migration, selected-delta base verification, sparse browser bootstrap planning/fallback/range-isolation gates, full-save/backup anti-blessing guards, T9C-B recompact base-start publish guards, T9C-C idle maintenance policy guards, Q2-B recompact input streaming guards, Q3-A public idle maintenance API guards, A5-4 compact-copy identity/watermark guard, A5-5 tagged/native-browser/corruption corpus consumers, A9 backup identity/linearization/conflict/symlink/watermark guards, A7 FileLock crash-robustness guards, A2 since-tail page-probe/no-full-scan guards, and A8 forget parser plus short-WAL recovery guards)
- ✅ 12 bi-temporal tests (integration)
- ✅ 11 complex query tests (integration)
- ✅ 9 recursive rules tests (integration)
- ✅ 12 concurrency tests (integration, 1 ignored: nightly stress)
- ✅ 22 WAL / crash recovery tests (integration — includes the A7-found header-only-WAL tx-counter regression)
- ✅ 24 session protocol tests (integration, A6 — framed pipe NDJSON, tagged values, child-process gate runs; A2 export_since, A8 forgotten frames, and A9 live-writer backup receipts/errors)
- ✅ 2 kill -9 durability harness tests (integration, A7 — default smoke + `#[ignore]`d 2,400-cycle nightly gate)
- ✅ 2 cross-platform compat tests (integration, Phase 8.1)
- ✅ 6 index tests (integration, Phase 6.1)
- ✅ 7 performance tests (integration, Phase 6.2/6.4b)
- ✅ 8 retraction tests (integration, Phase 6.4a)
- ✅ 4 edge case tests (integration, Phase 6.4a)
- ✅ 8 B+tree v6 tests (integration, Phase 6.5)
- ✅ 10 negation (`not`) tests (integration, Phase 7.1a)
- ✅ 14 not-join tests (integration, Phase 7.1b)
- ✅ 24 aggregation tests (integration, Phase 7.2a)
- ✅ 28 predicate expression tests (integration, Phase 7.2b)
- ✅ 18 disjunction tests (integration, Phase 7.3)
- ✅ 8 production pattern tests (integration, Phase 7.5 — cross-feature scenarios)
- ✅ 8 error handling tests (integration, Phase 7.5 — error-path coverage)
- ✅ 25 temporal metadata tests (integration, Phase 7.6 — `:db/valid-from`, `:db/valid-to`, `:db/tx-count`, `:db/tx-id`, `:db/valid-at`, including same-entity multi-transaction fact correlation plus expression/rule ordering boundaries)
- ✅ 14 window function tests (integration, Phase 7.7a — cumulative sum/count/min/avg, rank with ties, row-number, partition-by, desc ordering, mixed aggregate+window, edge cases, lag/lead parse rejection)
- ✅ 10 UDF tests (integration, Phase 7.7b — custom aggregates, custom predicates, UDF as window function, name collision guards, runtime errors, thread safety)
- ✅ 17 prepared statement tests (integration, Phase 7.8 — entity/value/as-of/valid-at slots, combined temporal+entity, AnyValidTime, error paths, plan reuse)
- ✅ 3 grammar conformance tests (integration, Phase 7.9 — pest shadow grammar + EDN corpus)
- ✅ 6 migration matrix tests (integration, Wave 3 #215 + legacy→v11 format migration — current round-trip, v7 fixture migrate, v3 empty migrate, corrupt magic, unsupported version, WAL replay idempotent)
- ✅ 7 multi-value index tests (integration, #287 — same entity+attribute batch values survive indexed public query paths, ref edges, `:as-of`, `:valid-at`, retraction, checkpoint/reopen)
- ✅ 5 retract valid-time tests (integration, Vetch ledger parity — scoped retract removes only the matching valid-time window, legacy retract still wipes all windows, Ref edge value, WriteTransaction parity, checkpoint/reopen)
- ✅ 7 fact-log export tests (integration, Vetch ledger receipts — public append-only export includes `asserted`, `tx_id`, `tx_count`, valid-time scope, legacy retractions, scoped Ref-edge retractions, checkpoint/reopen; A2 since-tail subsequence equivalence across base/delta/pending layers and stored-cursor reopen polling)
- ✅ 18 bulk forget tests (integration, A8 — query/fact-list closure, one-transaction 10k gate, history/as-of semantics, idempotence, re-assertion, finite/disjoint windows, errors, checkpoint/reopen)
- ✅ 1 incremental fact-log gate fixture (`tests/fact_log_since_benchmark.rs`, `#[ignore]`d — 1M-base since-tail latency, see `docs/BENCHMARKS.md` "A2: Incremental Fact Log")
- ✅ 17 delta checkpoint integration tests (integration, Vetch delta storage — v10 manifest publish, multi-segment append, base/delta and segment/segment Ref edges, later-segment retraction, deterministic export, corrupt-slot fallback)
- ✅ 5 delta checkpoint crash recovery tests (integration, Vetch delta storage — unpublished delta ignored, WAL replay, selected corrupt/truncated delta errors, stale WAL skip after header publish)
- ✅ 1 checkpoint rebuild benchmark test (integration, Vetch delta storage — small pending write benchmark gate)
- ✅ 1 delta index reader test (integration, Vetch delta storage — non-persistent layered reader gate)
- ✅ 1 delta index segment test (integration, Vetch delta storage — segment codec gate)
- ✅ 1 delta manifest recovery test (integration, Vetch delta storage — manifest recovery gate)
- ✅ 1 header extension gate test (integration, Vetch delta storage — v10 extension module gate)
- ✅ Agent-brief read-path benchmark harness (bench, Vetch Q1-A — current/as-of/prepared/export receipt-read surfaces; not counted as a test)
- ✅ Self-checking 1M paged-browser matrix harness (bench, Vetch A5-6d — import, fresh `openPaged` cold/warm probes, verified async export, 1,024-write threshold, maintenance reclaim, and post-maintenance reopen; not counted as a test)
- ✅ 4 index corruption tests (integration, v11 public API — header/catalog corruption rejects open; exact fact/EAVT pages fail closed on first selective query)
- ✅ 3 property-based tests (integration, Wave 3 #212/#213/#219 — proptest Datalog correctness vs naive reference evaluator)
- ✅ 1 long-haul smoke test (integration, Wave 3 #220 — 500 entities × 10 attrs × 10 cycles; ignored: nightly)
- ✅ 10 XTDB compat tests (integration, Wave 3 #221 — Apache 2.0 semantic ports of XTDB concepts)
- ✅ 9 Datomic compat tests (integration, Wave 3 #221 — independently written semantic ports of Datomic concepts)
- ✅ 5 magic sets tests (integration, #289 — demand-driven recursive evaluation correctness: bound transitive closure, all-free closure, subset invariant, multi-hop, mutual recursion)
- ✅ 2 Vicia API alias tests (integration, Vicia DB V2 — `ViciaDb` in-memory usage, legacy `Minigraf` interoperability, file-backed checkpoint/reopen)
- ✅ 15 doc tests (9 passing, 6 ignored: doc examples referencing internal types that cannot compile as standalone rustdoc tests)
- ➕ 59 browser-WASM tests (`wasm-bindgen-test`, headless Chrome — **not counted in the native total**; run via `CHROMEDRIVER=/path/to/chromedriver ./scripts/test-browser-wasm.sh`. A5-4 covers maintenance/failure ordering; A5-5 adds both-producer tagged portability and shared corruption/recovery; A5-6b adds durable v10→v11 migration and verified export; A5-6c adds bounded metadata-only open, cold/warm demand reads, exact page-0 stale-handle rejection, corrupt-page failure, sparse rollback/import/write/forget/maintenance convergence, asynchronous verified export, full-scan staging release, callback cleanup, and legacy recovery compatibility. The strict paged-import tests add complete v10 migration/reopen plus exact-state preservation for every non-exportable truncated recovery mutation; planner regressions prove same-entity per-fact transaction correlation and 128-entity paged reads without range prefetch. All 59 passed in the final Chrome run. The same script is enforced by the Browser WASM CI job.)

**Status**: ✅ **All 1274 non-ignored native tests passing** (12 ignored: 6 internal-type doc examples, 1 nightly concurrency stress, 1 nightly smoke, 1 Q2-B manual 1M recompact measurement, 1 delta-cadence measurement, 1 A7/A8 full kill -9 gate, 1 A2 1M-base since-tail gate fixture)

## Wave 3 Reliability Completion Status: ✅ COMPLETE

**Wave 3 issues**: #209, #210, #214 (WAL fault injection), #215 (migration matrix), #216 (index corruption), #217 (concurrency stress), #212, #213, #219 (property-based / coverage), #220 (long-haul smoke), #221 (XTDB/Datomic compat)

**New tests added by Wave 3** (+87 total):
- ✅ `wal_test.rs` — 9 new fault-injection tests (FaultInjectingBackend: write fail, flush fail, read fault, WAL CRC corruption, checkpoint atomicity, partial checkpoint recovery, multi-writer serialisation, concurrent write+checkpoint, backend error propagation)
- ✅ `tests/migration_matrix_test.rs` — 6 migration tests (current round-trip, v7 fixture migrate, v3 empty migrate, corrupt magic, unsupported version, WAL replay idempotent)
- ✅ `tests/index_corruption_test.rs` — original Wave 3 no-panic corpus, now replaced by 4 strict v11 public-API gates (header/catalog open rejection plus exact fact/EAVT first-read failure)
- ✅ `tests/concurrency_test.rs` — 5 new stress tests (stress readers during writer, failed write then success, rollback after partial work, open/write/checkpoint/query loop per thread, nightly stress loop)
- ✅ `tests/property_test.rs` — 3 proptest property tests (EAV fact model, bi-temporal monotonicity, retract visibility)
- ✅ `tests/smoke_test.rs` — 1 long-haul smoke test (500 entities × 10 attrs × 10 cycles, 7 invariants; `#[ignore]` nightly)
- ✅ `tests/xtdb_compat_test.rs` — 10 XTDB semantic compatibility tests
- ✅ `tests/datomic_compat_test.rs` — 9 Datomic semantic compatibility tests
- ✅ 40 new lib unit tests (FaultInjectingBackend unit tests, WAL corruption helpers, property test infrastructure)

**New CI workflows added**:
- ✅ `.github/workflows/fuzz.yml` — nightly fuzzing, 6 libFuzzer targets × 60s each
- ✅ `.github/workflows/coverage-gates.yml` — per-module coverage thresholds, fails PR if coverage drops
- ✅ `.github/workflows/smoke.yml` — nightly 5am UTC, 15-min timeout, `--include-ignored`

---

## Wave 2 Optimizer & Benchmarks Completion Status: ✅ COMPLETE

**Wave 2 issues**: #207 + #206 (predicate push-down + mixed rule optimization), #205 (cost-based not/or ordering), #229 (SIMD benchmarking + crossover analysis)

No new integration tests added — Wave 2 is entirely optimizer and benchmark work. Existing 850 tests cover all affected code paths. New benchmark groups: `simd_temporal`, `simd_as_of`, `simd_aggregate`.

---

## Wave 1 Performance Completion Status: ✅ COMPLETE

**Wave 1 issues**: #208 (selective B+Tree lookup), #202 (not/not-join hash-join), #203 (or/or-join hash-join), #204 (join_with_pattern hash-join)

**New unit test modules added**:
- ✅ `selective_lookup_tests` in `executor.rs` — entity-bound and attribute-bound point queries, threshold fallback, `as_of` full-scan path
- ✅ `not_hash_join_tests` in `executor.rs` — `not`/`not-join` pre-computed exclusion set at 1k/10k scale
- ✅ `or_hash_join_tests` in `executor.rs` — `or`/`or-join` empty-seed branch evaluation and hash-join back-join at scale
- ✅ `hash_join_tests` in `matcher.rs` — shared-`?e` join, value-position join, no-join-var fallback

---

## Phase 8 Completion Status: ✅ COMPLETE — v1.0.0

All Phase 8 sub-phases complete. See per-phase sections below.

---

## Phase 8.3d Completion Status: ✅ COMPLETE

**Phase 8.3d Features** (Node.js, complete — v0.25.0):
- ✅ `minigraf-node/src/lib.rs` — napi-rs bindings: `MiniGrafDb` class (open, inMemory, execute, checkpoint)
- ✅ `minigraf-node/package.json` — `minigraf` npm package; prebuilt `.node` binaries for Linux x86_64/aarch64, macOS universal2, Windows x86_64
- ✅ `node-ci.yml` — PR test matrix on 4 platforms
- ✅ `node-release.yml` — cross-compile, assemble platform packages, publish to npm on tag

---

## Phase 8.3c Completion Status: ✅ COMPLETE

**Phase 8.3c Features** (C FFI, complete — v0.24.0):
- ✅ `minigraf-c/src/lib.rs` — `cdylib` + `staticlib`; 7 exported functions: `minigraf_open`, `minigraf_open_in_memory`, `minigraf_execute`, `minigraf_string_free`, `minigraf_checkpoint`, `minigraf_close`, `minigraf_last_error`
- ✅ `minigraf-c/include/minigraf.h` — committed stable header (cbindgen-generated); header drift check in CI
- ✅ `c-ci.yml` — PR test matrix on 4 platforms + header drift check
- ✅ `c-release.yml` — builds platform tarballs (`.tar.gz` / `.zip`), uploads to GitHub Releases

---

## Phase 8.3b Completion Status: ✅ COMPLETE

**Phase 8.3b Features** (Java/JVM, complete — v0.23.0):
- ✅ `minigraf-ffi/java/` — Gradle 8.11 project: `build.gradle.kts`, `settings.gradle.kts`, `NativeLoader.kt` (runtime native extraction from JAR resources)
- ✅ `minigraf-ffi/java/src/test/kotlin/.../BasicTest.kt` — JUnit 5 suite: in-memory, transact/query, error handling, file-backed persistence
- ✅ `java-ci.yml` — PR test matrix on 4 platforms (Linux x86_64, Linux aarch64, macOS universal2, Windows x86_64)
- ✅ `java-release.yml` — cross-compiles natives, assembles fat JAR, publishes to Maven Central via NMCP

---

## Phase 8.3a Completion Status: ✅ COMPLETE

**Phase 8.3a Features** (Python, complete — v0.22.0):
- ✅ `minigraf-ffi/python/` — maturin project: `pyproject.toml`, Python extension module via UniFFI
- ✅ Pre-built wheels for Linux x86_64/aarch64, macOS universal2, Windows x86_64; no Rust toolchain required by end users
- ✅ `python-ci.yml` — PR test matrix on 4 platforms
- ✅ `python-release.yml` — builds wheels, publishes to PyPI on tag

---

## Phase 8.2 Completion Status: ✅ COMPLETE

**Phase 8.2 Features** (Mobile, complete — v0.21.0):
- ✅ `minigraf-ffi/src/lib.rs` — UniFFI 0.31 bindings: `MiniGrafDb` (open, openInMemory, execute, checkpoint), `MiniGrafError` (Parse, Query, Storage, Other)
- ✅ Android `.aar` release artifact — published to GitHub Packages (`io.github.adityamukho:minigraf-android`)
- ✅ iOS `.xcframework` release artifact — distributed via Swift Package Manager (`Package.swift` at repo root)
- ✅ `mobile.yml` CI — cross-compiles Android targets with `cargo-ndk`, generates Kotlin/Swift UniFFI bindings, assembles AAR and xcframework, publishes both on every tag
- ✅ `docs-check` CI job added to `rust.yml` and `release.yml` — gates releases on `cargo doc --all-features` passing cleanly

---

## Phase 8.1 Completion Status: ✅ COMPLETE

**Phase 8.1a Features** (browser WASM, complete):
- ✅ `BrowserDb` public API: `open_in_memory`, eager-compatible `open`, bounded `open_paged`, `execute`, `checkpoint`, `run_idle_maintenance`, synchronous `export_graph`, verified `export_graph_async`, recovery-compatible `import_graph`, strict `import_graph_for_paged_access`
- ✅ `BrowserBufferBackend` — `StorageBackend` over either a complete resident image or a sparse logical image with pinned authority metadata, bounded clean-page residency, and typed page demands; both preserve native `.graph` page bytes
- ✅ `IndexedDbBackend` — page-granular async IndexedDB source with batched exact-range reads and exact page-0 compare-and-swap authority; no browser-only schema key
- ✅ `wasm-pack` build generating `minigraf-wasm/` with JS glue and TypeScript `.d.ts`
- ✅ `wasm-bindgen-test` suite: 59 browser integration/unit tests passing in headless Chrome locally (wired into the Browser WASM CI job, with no cross-browser claim)

**Phase 8.1b Features** (WASI, complete):
- ✅ `FileBackend` verified under WASI capability-based filesystem (no changes needed)
- ✅ `wasm32-wasip1` CI workflow: build, unit tests (Wasmtime runner), smoke tests (Wasmtime + Wasmer)
- ✅ Thread-dependent tests gated with `#[cfg(not(target_os = "wasi"))]`

**Cross-platform compatibility** (issue #150, complete):
- ✅ `tests/cross_platform_compat_test.rs`: 2 native tests (raw page byte round-trip + fixture readability)
- ✅ `tests/fixtures/compat.graph`: committed v7 binary fixture with known facts
- ✅ `native_fixture_readable_by_browser_db` wasm-bindgen-test: imports native fixture, verifies both facts
- ✅ 795 tests passing (unit + integration + doc + wasm); version bumped to v0.20.0

## Phase 7.9 Completion Status: ✅ COMPLETE

**Phase 7.9 Features** (current, complete):
- ✅ `Minigraf::repl(&self) -> Repl<'_>` factory method — `Repl` now borrows `&Minigraf` for lifetime safety
- ✅ All internal types narrowed to `pub(crate)`: `FactStorage`, `PersistentFactStorage`, `FileHeader`, `StorageBackend`, `DatalogExecutor`, `PatternMatcher`, `Fact`, `TxId`, `VALID_TIME_FOREVER`, `Wal`, etc.
- ✅ Full rustdoc on all public API items with `# Examples` doctests; 8 new doctests added
- ✅ `[package.metadata.docs.rs]` in `Cargo.toml` — docs.rs builds with `all-features = true`
- ✅ `#![warn(missing_docs)]` — enforces documentation coverage going forward
- ✅ Bare `.unwrap()` in library code replaced with `.expect("lock poisoned")` / `.expect("WAL not initialized")`
- ✅ `cargo clippy -- -D warnings` clean
- ✅ macOS and Windows added to CI test matrix (`rust.yml`)
- ✅ crates.io and docs.rs badges + Installation section in `README.md`
- ✅ 788 tests passing (unit + integration + doc); version bumped to v0.19.0

## Phase 7.8 Completion Status: ✅ COMPLETE

**Phase 7.8 Features** (complete):
- ✅ `EdnValue::BindSlot(String)`, `AsOf::Slot(String)`, `ValidAt::Slot(String)`, `Expr::Slot(String)` AST variants in `types.rs`
- ✅ `BindValue` enum in `src/query/datalog/prepared.rs`: `Entity(Uuid)`, `Val(Value)`, `TxCount(u64)`, `Timestamp(i64)`, `AnyValidTime`
- ✅ `PreparedQuery` struct — stores parsed AST + optimised plan + `Arc` handles to fact store and registries; re-executes against live fact store state
- ✅ `prepare_query()` (pub(crate)) — parse, validate, compute query plan once
- ✅ `PreparedQuery::execute(bindings)` — deep-clone + AST walk substitution; type-checked per bind position; executor, optimizer, matcher unchanged
- ✅ Panic guards (no slot-name interpolation) in `executor.rs` (4 `ValidAt::Slot` sites, 1 `Expr::Slot` site) and `storage.rs` (`AsOf::Slot`)
- ✅ `Minigraf::prepare(query_str) -> Result<PreparedQuery>` on public API (`db.rs`)
- ✅ `BindValue` and `PreparedQuery` re-exported from `lib.rs`
- ✅ `tests/prepared_statements_test.rs` — 17 integration tests
- ✅ 780 tests passing (unit + integration + doc)

## Phase 7.7b Completion Status: ✅ COMPLETE

**Phase 7.7b Features** (current, complete):
- ✅ `UdfOps` and `PredicateDesc` types in `src/query/datalog/functions.rs` — register custom aggregates (init/step/finalise closures) and custom predicates (filter closure)
- ✅ `FunctionRegistry::register_aggregate` and `register_predicate` methods; collision guards reject re-registration of built-in names or duplicate UDFs
- ✅ `FindSpec::Udf` and `WhereClause::UdfPredicate` variants in `types.rs`; UDF aggregates usable in `:find` and `:over` window specs; UDF predicates usable in `:where`
- ✅ Parser extended: UDF aggregate invocations in `:find` / `:over`; UDF predicate invocations in `:where`; unknown function names deferred to runtime, not rejected at parse time
- ✅ Executor routes UDF aggregates through `FunctionRegistry` at query time; UDF predicates evaluated per binding row
- ✅ `Minigraf::register_aggregate` and `register_predicate` on the public API (`db.rs`)
- ✅ `tests/udf_test.rs` — 14 integration tests
- ✅ 753 tests passing (unit + integration + doc)

## Phase 7.7a Completion Status: ✅ COMPLETE

**Phase 7.7a Features** (current, complete):
- ✅ `FunctionRegistry` in `src/query/datalog/functions.rs` — string-keyed registry; built-in aggregates (`sum`, `count`, `min`, `max`, `avg`, `count-distinct`, `sum-distinct`) migrated into it; `window_ops` (init/step/finalise) on window-compatible entries; `is_builtin` flag
- ✅ `WindowFunc`, `Order`, `WindowSpec`, `FindSpec::Window` types in `types.rs`; `AggFunc` enum removed; `FindSpec::Aggregate.func` changed to `String`
- ✅ `parse_window_expr` in `parser.rs` — `(func ?v :over (:partition-by ?p :order-by ?o :desc))` syntax; `lag`/`lead` rejected; unknown function → parse error; non-window-compatible in `:over` → parse error
- ✅ `apply_post_processing`, `compute_aggregation`, `apply_window_functions`, `project_find_specs` in `executor.rs` — replaces `apply_aggregation`/`apply_agg_func`
- ✅ `FunctionRegistry` wired through `db.rs` (`Minigraf::Inner` gains `Arc<RwLock<FunctionRegistry>>`)
- ✅ `tests/window_functions_test.rs` — 12 integration tests (cumulative sum, running count/min/avg, rank with ties, row-number, partition-by, desc ordering, mixed aggregate+window, single-row and empty-result edge cases, lag/lead parse rejection)
- ✅ 746 tests passing (unit + integration + doc)

## Phase 7.6 Completion Status: ✅ COMPLETE

**Phase 7.6 Features** (current, complete):
- ✅ `PseudoAttr` enum and `AttributeSpec` wrapper type in `types.rs`
- ✅ `parse_query_pattern` in `parser.rs` — detects `:db/*` keywords in attribute position; rejects in entity/value positions
- ✅ `PatternMatcher::from_slice_with_valid_at` constructor — passes query-level `valid_at` into the matcher
- ✅ Hard-error guard in executor: per-fact pseudo-attrs require `:any-valid-time`
- ✅ `:db/valid-at` binds the effective query timestamp; `:any-valid-time` accepted as standalone keyword
- ✅ `tests/temporal_metadata_test.rs` — 16 integration tests (time-interval range queries, time-point lookups, tx-time correlation, `:db/valid-at` semantics, parse/runtime error guards)
- ✅ 647 tests passing (438 unit + 209 integration)

## Phase 7.5 Completion Status: ✅ COMPLETE

**Phase 7.5 Features** (complete):
- ✅ `cargo-llvm-cov` branch coverage tooling documented in `CONTRIBUTING.md`
- ✅ Baseline branch coverage recorded; executor.rs ~86.61%, evaluator.rs ~89.29% (up from ~75% / ~73%)
- ✅ `tests/production_patterns_test.rs` — 8 cross-feature integration tests
- ✅ `tests/error_handling_test.rs` — 8 error-path integration tests (1 ignored: confirmed or+neg-cycle stratification bug)
- ✅ Stream 3 unit tests: ~53 new tests for previously uncovered branches in executor.rs and evaluator.rs
- ✅ 617 tests passing (424 unit + 187 integration + 6 doc)

## Phase 7.4 Completion Status: ✅ COMPLETE

**Phase 7.4 Features** (current, complete):
- ✅ `filter_facts_for_query` returns `Arc<[Fact]>` — eliminates O(N) four-BTreeMap index rebuild on every non-rules query call
- ✅ `execute_query` path constructs zero `FactStorage` objects; `execute_query_with_rules` still converts for `StratifiedEvaluator`
- ✅ `PatternMatcher::from_slice(Arc<[Fact]>)` constructor added
- ✅ `apply_or_clauses` and `evaluate_not_join` signatures updated to accept `Arc<[Fact]>`
- ✅ Evaluator loop: `accumulated_facts` computed once per iteration (was 4 separate `get_asserted_facts()` calls)
- ✅ ~62–65% speedup on non-rules queries at 10K facts (`query/point_entity/10k`: 22 ms → 8.6 ms; `aggregation/count_scale/10k`: 28 ms → 9.7 ms)
- ✅ 4 new unit tests in `matcher.rs`, 2 new unit tests in `executor.rs` (6 total)
- ✅ Version bumped to v0.13.1

## Phase 7.3 Completion Status: ✅ COMPLETE

**Phase 7.3 Features** (current, complete):
- ✅ `WhereClause::Or(Vec<Vec<WhereClause>>)` and `WhereClause::OrJoin { join_vars, branches }` variants in `types.rs`
- ✅ `(or branch1 branch2 ...)` and `(or-join [?v...] branch1 branch2 ...)` in `:where` clauses and rule bodies
- ✅ `(and ...)` grouping clause to collect multiple clauses into a single branch
- ✅ `match_patterns_seeded` on `PatternMatcher`; `evaluate_branch` and `apply_or_clauses` helpers in `executor.rs`
- ✅ `DependencyGraph::from_rules` refactored with recursive `collect_clause_deps`; `Or`/`OrJoin` branches contribute positive dependency edges
- ✅ Rules with `or`/`or-join` in bodies routed to `mixed_rules` path in `StratifiedEvaluator`
- ✅ `tests/disjunction_test.rs`: 16 integration tests (Phase 7.3)
- ✅ Version bumped to v0.13.0

**Core Features Implemented** (Phase 6.2):
- ✅ Packed fact pages (`page_type = 0x02`): ~25 facts per 4KB page (~25× space reduction)
- ✅ LRU page cache (`cache.rs`): approximate-LRU, read-lock on hits, `Arc<Vec<u8>>` entries
- ✅ `CommittedFactReader` trait + `CommittedFactLoaderImpl`: on-demand fact resolution
- ✅ Pending `FactRef` (`page_id = 0`): resolves to in-memory pending facts vec
- ✅ `FileHeader` v5: `fact_page_format` byte (0x02 = packed); auto v4→v5 migration on open
- ✅ `OpenOptions::page_cache_size(usize)` builder method (default 256)
- ✅ EAVT/AEVT range scans in `get_facts_by_entity` / `get_facts_by_attribute`

**Phase 6.1 Features** (also complete):
- ✅ EAVT, AEVT, AVET, VAET covering indexes with bi-temporal keys
- ✅ `FactRef { page_id, slot_index }`: forward-compatible disk location pointer
- ✅ Canonical value encoding (`encode_value`) for sort-order-preserving comparisons
- ✅ B+tree page serialisation for index persistence (`btree.rs`)
- ✅ `FileHeader` v4: `eavt/aevt/avet/vaet_root_page` + `index_checksum` (CRC32)
- ✅ Auto-rebuild on checksum mismatch
- ✅ Query optimizer: `IndexHint`, `select_index()`, selectivity-based `plan()`

**Phase 5 Features** (also complete):
- ✅ Fact-level sidecar WAL (`<db>.wal`) with CRC32-protected binary entries
- ✅ WAL-before-apply ordering: WAL fsynced before facts touch in-memory state
- ✅ `FileHeader` v3 with `last_checkpointed_tx_count` (replay deduplication)
- ✅ `WriteTransaction` API (`begin_write`, `commit`, `rollback`)
- ✅ Crash recovery: WAL replay on open, corrupt entries discarded at first bad CRC32
- ✅ Checkpoint: WAL flushed to `.graph` file, then WAL cleared
- ✅ Thread-safe: concurrent readers + exclusive writer (Mutex + RwLock)
- ✅ File format v2→v3 migration on first checkpoint

**Phase 4 Features** (also complete):
- ✅ EAV data model with `tx_count`, `valid_from`, `valid_to` fields
- ✅ `VALID_TIME_FOREVER = i64::MAX` sentinel
- ✅ `FactStorage` temporal query methods (`get_facts_as_of`, `get_facts_valid_at`)
- ✅ Parser: EDN maps, `:as-of`, `:valid-at`, per-fact valid time overrides
- ✅ Executor: 3-step temporal filter (tx-time → asserted → valid-time)
- ✅ File format v1→v2 migration
- ✅ UTC-only timestamp parsing (chrono, avoids GHSA-wcg3-cvx6-7396)

**Phase 7.2b Features** (also complete):
- ✅ `BinOp` (14 variants), `UnaryOp` (5 variants), `Expr` AST, `WhereClause::Expr { expr, binding }` in `types.rs`
- ✅ Filter predicates: `[(< ?age 30)]`, `[(string? ?v)]`, `[(starts-with? ?tag "work")]`, `[(matches? ?email "...")]`
- ✅ Arithmetic bindings: `[(+ ?price ?tax) ?total]`, nested `[(+ (* ?a 2) ?b) ?result]`, type-predicate binding `[(integer? ?v) ?is-int]`
- ✅ `parse_expr` / `parse_expr_clause` with parse-time regex validation; `check_expr_safety` recurses into `not`/`not-join` bodies
- ✅ Dispatch at all 4 clause sites; `outer_vars_from_clause` updated for binding variable scope
- ✅ `eval_expr` / `eval_binop` / `is_truthy` / `apply_expr_clauses` in `executor.rs`; `apply_expr_clauses_in_evaluator` in `evaluator.rs`
- ✅ `tests/predicate_expr_test.rs`: 28 integration tests (Phase 7.2b)
- ✅ Version bumped to v0.12.0

**Phase 7.2a Features** (also complete):
- ✅ `count`, `count-distinct`, `sum`, `sum-distinct`, `min`, `max` aggregate functions in `:find` clause
- ✅ `:with` grouping clause — variables that participate in grouping but are excluded from output rows
- ✅ `AggFunc` enum, `FindSpec` enum; `DatalogQuery.find` migrated from `Vec<String>` to `Vec<FindSpec>`
- ✅ `apply_aggregation` post-processing in `executor.rs`; parse-time validation
- ✅ `tests/aggregation_test.rs`: 24 integration tests (Phase 7.2a)
- ✅ Version bumped to v0.11.0

**Phase 7.1 Features** (also complete):
- ✅ `src/query/datalog/stratification.rs`: `DependencyGraph`, `stratify()` — negative dependency edges + Bellman-Ford cycle detection; negative cycles rejected at rule registration time
- ✅ `WhereClause::Not(Vec<WhereClause>)` and `WhereClause::NotJoin { join_vars, clauses }` variants; all match arms updated
- ✅ `(not clause…)` — stratified negation; all body variables must be pre-bound by outer clauses
- ✅ `(not-join [?v…] clause…)` — existentially-quantified negation; only `join_vars` are shared from outer scope; remaining body variables are fresh
- ✅ Safety validation at parse time (unbound join vars → parse error; nesting constraint enforced)
- ✅ `StratifiedEvaluator`: stratifies rules, evaluates strata in order; `not`/`not-join` filters applied per binding in mixed-rule strata
- ✅ `evaluate_not_join` free function: handles both `Pattern` and `RuleInvocation` body clauses
- ✅ `tests/negation_test.rs`: 10 integration tests (Phase 7.1a)
- ✅ `tests/not_join_test.rs`: 14 integration tests (Phase 7.1b)
- ✅ Version bumped to v0.10.0

**Phase 6.5 Features** (also complete):
- ✅ `src/storage/btree_v6.rs`: proper on-disk B+tree with `build_btree` bulk-load and `range_scan` leaf-chain traversal
- ✅ `OnDiskIndexReader` + `CommittedIndexReader` trait: page-cache-backed index lookup; no full in-memory BTreeMap
- ✅ `MutexStorageBackend<B>`: backend mutex held per page read; cache-warm pages require no lock
- ✅ FileHeader v6 (80 bytes): adds `fact_page_count u64` field at bytes 72–80; auto v5→v6 migration
- ✅ `tests/btree_v6_test.rs`: 8 integration tests (B+tree insert/scan, multi-page, concurrent correctness, v5→v6 migration)
- ✅ `test_concurrent_range_scans_correctness` unit test: 8 barrier-synchronised threads, all return identical results
- ✅ Version bumped to v0.9.0; BENCHMARKS.md updated with v6 results

**Phase 6.4b Features** (also complete):
- ✅ Criterion benchmark suite at 1K–1M facts; results documented in `BENCHMARKS.md`
- ✅ heaptrack memory profiles: 10K=14.4MB / 100K=136MB / 1M=1.33GB peak heap (v5 baseline)
- ✅ Byte-layout unit tests pin all FileHeader v5 field offsets (`src/storage/mod.rs`)
- ✅ Byte-layout unit tests pin packed page header + record directory offsets (`src/storage/packed_pages.rs`)
- ✅ Dead `clap` dependency removed; `Cargo.toml` metadata complete; version bumped to v0.8.0
- ✅ README trimmed (794 → 166 lines); detail offloaded to GitHub wiki

**Phase 6.4a Features** (also complete):
- ✅ Retraction semantics fix: `net_asserted_facts()` computes net view per EAV triple in `filter_facts_for_query`
- ✅ `check_fact_sizes()` early validation in `db.rs`: rejects oversized facts before WAL write
- ✅ `MAX_FACT_BYTES` constant (`packed_pages.rs`): 4 080 bytes — maximum serialised size per fact
- ✅ 7 new retraction integration tests (`tests/retraction_test.rs`)
- ✅ 4 new edge case integration tests (`tests/edge_cases_test.rs`)

**Phase 3 Features** (also complete):
- ✅ Datalog parser (EDN syntax)
- ✅ Pattern matching with variable unification
- ✅ Query executor (transact, retract, query)
- ✅ Recursive rules with semi-naive evaluation
- ✅ Transitive closure queries
- ✅ Persistent storage (postcard serialization)
- ✅ REPL with multi-line and comment support

---

## Test Coverage by Module

### 1. Graph Types (`src/graph/types.rs`) - ✅ Excellent (8 tests)

- ✅ Fact creation, equality, retraction, entity references
- ✅ Transaction ID generation and ordering
- ✅ `VALID_TIME_FOREVER` sentinel, `with_valid_time()`, `TransactOptions`
- ✅ All `Value` types (String, Integer, Float, Boolean, Ref, Keyword, Null)

**Coverage**: ~95%

### 2. Fact Storage (`src/graph/storage.rs`) - ✅ Excellent (18+ tests)

**Core Operations**:
- ✅ Transact, retract, batch transact
- ✅ Get facts by entity/attribute, history tracking

**Phase 4 (Bi-temporal)**:
- ✅ `tx_count` increments, `get_facts_as_of()`, `get_facts_valid_at()`
- ✅ `load_fact()` preserves original `tx_id`/`tx_count`

**Phase 5 (WAL helpers)**:
- ✅ `get_all_facts()`, `restore_tx_counter()`, `allocate_tx_count()`

**Phase 6.1-6.2 (Index + CommittedFactReader)**:
- ✅ `set_committed_reader()` wires CommittedFactReader
- ✅ `get_facts_by_entity()` uses EAVT range scan
- ✅ `get_facts_by_attribute()` uses AEVT range scan
- ✅ `FactRef { page_id: 0 }` resolved to pending facts; `page_id >= 1` via CommittedFactReader
- ✅ MockLoader in tests verifies committed path

**Coverage**: ~93%

### 3. WAL (`src/wal.rs`) - ✅ Excellent (8 unit tests)

- ✅ Empty WAL round-trip
- ✅ Single-fact and multi-fact entry round-trips
- ✅ Reopen-and-append
- ✅ Bad magic header rejected
- ✅ Truncated entry stops replay (partial write discard)
- ✅ `delete_file()` removes WAL

**Coverage**: ~97%

### 4. Database API (`src/db.rs`) - ✅ Excellent (17+ unit tests)

- ✅ In-memory transact and query round-trip
- ✅ Explicit `WriteTransaction` commit and rollback
- ✅ `build_query_view()` read-your-own-writes within transaction
- ✅ Reentrant `begin_write()` returns error
- ✅ File-backed open, transact, reopen (persistence)
- ✅ WAL written before in-memory apply
- ✅ Auto-checkpoint threshold fires, `checkpoint()` manual trigger
- ✅ Public `run_idle_maintenance()` no-op/checkpoint/recompact/convergence/error guards
- ✅ Concurrent `execute()` during active `WriteTransaction`

**Coverage**: ~93%

### 5. Covering Indexes (`src/storage/index.rs`) - ✅ Excellent (12 tests)

- ✅ `FactRef` field access
- ✅ `encode_value` sort order: integers, cross-type, floats, NaN canonicalization
- ✅ EAVT key ordering by entity
- ✅ AVET key ordering by value bytes
- ✅ VAET only populated for `Value::Ref`
- ✅ `Indexes::insert` populates all four indexes
- ✅ Asserted/retracted ref facts with the same E/A/V/window/tx identity stay distinct in all four indexes

**Coverage**: ~98%

### 6. B+tree Persistence (`src/storage/btree.rs`) - ✅ Good (4 tests)

- ✅ Empty EAVT roundtrip (exactly 1 page)
- ✅ Small EAVT roundtrip (10 entries)
- ✅ Large EAVT roundtrip (150 entries, multi-page)
- ✅ Sort order preserved after serialise/deserialise

**Coverage**: ~90%

### 7. LRU Page Cache (`src/storage/cache.rs`) - ✅ Good (6 tests)

- ✅ Cache miss loads from backend
- ✅ Cache hit returns same `Arc` without backend read
- ✅ LRU eviction evicts correct (oldest) page
- ✅ `put_dirty` / `flush` writes back to backend
- ✅ `invalidate` removes entry
- ✅ `cached_page_count` reports correctly

**Coverage**: ~92%

### 8. Packed Pages (`src/storage/packed_pages.rs`) - ✅ Good (10 tests)

- ✅ Single fact pack/unpack roundtrip
- ✅ Multiple facts pack/unpack roundtrip
- ✅ Correct `FactRef` slot assignments
- ✅ Oversized fact returns `Err` (not panic)
- ✅ `read_all_from_pages` with known page IDs
- ✅ Wrong page type returns `Err`
- ✅ **Byte-layout pin**: page header (bytes 0–11) field positions verified (Phase 6.4b)
- ✅ **Byte-layout pin**: record directory entries at byte 12+ verified (Phase 6.4b)

**Coverage**: ~93%

### 9. FileHeader (`src/storage/mod.rs`) - ✅ Excellent (21 tests)

- ✅ Current v11 legacy-header serialisation: 84 bytes with exact field offsets
- ✅ Current v11 legacy-header roundtrip preserves the header checksum
- ✅ v3, v4, and v6 legacy headers decode with version-appropriate zero-filled fields; v5 is accepted by validation
- ✅ Truncated v4 and v7 headers are rejected
- ✅ Header validation covers magic, versions 1–11, positive page count, and root/fact-page bounds
- ✅ v9, v10, and v11 are accepted explicitly
- ✅ `FORMAT_VERSION == 11`
- ✅ **Byte-layout pin**: all legacy-header fields use their exact little-endian offsets in v11

**Coverage**: ~98%

### 10. Datalog Parser (`src/query/datalog/parser.rs`) - ✅ Excellent (27 tests)

- ✅ All tokens, numbers, strings, booleans, UUIDs, nil
- ✅ Transact/Retract/Query/Rule commands
- ✅ `:as-of` (counter + ISO 8601 timestamp)
- ✅ `:valid-at` (timestamp + `:any-valid-time`)
- ✅ EDN map `{:key val}` with transaction-level valid time
- ✅ Per-fact valid time override (4-element fact vector)
- ✅ Retract transaction-level and per-fact valid-time maps
- ✅ Reject negative `:as-of` counter and invalid timestamps

**Coverage**: ~98%

### 11. Datalog Types, Matcher, Executor, Rules, Evaluator - ✅ Good-Excellent

- Types: ~95% (7 tests)
- Matcher: ~85% (6 tests)
- Executor: ~94% (18 tests) — including temporal filter and optimizer integration
- Rule Registry: ~95% (6 tests)
- Recursive Evaluator: ~95% (10 tests)

### 12. Storage Backends (`src/storage/backend/`) - ✅ Good (8 tests)

- ✅ FileBackend create/write/read, persistence across close/reopen
- ✅ MemoryBackend write/read, error handling

**Coverage**: ~85%

### 13. Temporal (`src/temporal.rs`) - ✅ Good

- ✅ UTC timestamp parsing and formatting
- ✅ Chrono CVE GHSA-wcg3-cvx6-7396 avoidance verified

**Coverage**: ~90%

---

## Integration Tests

### Complex Queries (`tests/complex_queries_test.rs`) - ✅ 10 tests

- ✅ 3-pattern and 4-pattern joins, self-joins, entity reference joins
- ✅ No results, partial matches, variable reuse, multiple values, empty database

### Recursive Rules (`tests/recursive_rules_test.rs`) - ✅ 9 tests

- ✅ Transitive closure, cycles, long chains, diamond patterns
- ✅ Ancestor/descendant, family trees, multiple recursive predicates

### Concurrency (`tests/concurrency_test.rs`) - ✅ 12 tests (1 ignored: nightly)

- ✅ Concurrent rule registration (5 threads), concurrent queries with rules (10 threads)
- ✅ Read-heavy workload (50 threads), recursive evaluation concurrency
- ✅ No deadlocks (20 threads mixed), RwLock consistency (10 writers + 10 readers)
- ✅ Stress readers during writer: concurrent readers see consistent state while writer holds lock (Wave 3 #217)
- ✅ Failed write followed by successful write: DB remains usable after write error (Wave 3 #217)
- ✅ Rollback after partial work: partial transaction leaves no trace (Wave 3 #217)
- ✅ Open/write/checkpoint/query loop per thread: 10-thread concurrent lifecycle (Wave 3 #217)
- ✅ Stress open/write loop (nightly, `#[ignore]`): high-contention loop stress test (Wave 3 #217)

### Bi-temporal (`tests/bitemporal_test.rs`) - ✅ 10 tests

- ✅ As-of counter and timestamp snapshots
- ✅ Valid-at inside/outside/boundary, default filter, any-valid-time
- ✅ Combined bi-temporal (both dimensions), multi-entity valid ranges

### WAL / Crash Recovery (`tests/wal_test.rs`) - ✅ 22 tests

- ✅ Basic persistence (file-backed transact and query)
- ✅ WAL replay after `mem::forget` crash simulation
- ✅ Stale WAL dedup via `last_checkpointed_tx_count`
- ✅ Corrupt/partial entry discard on recovery
- ✅ Manual checkpoint clears WAL and updates header
- ✅ Auto-checkpoint fires at threshold
- ✅ Explicit transaction crash safety and rollback
- ✅ Multi-transact rollback leaves no trace
- ✅ Concurrent reads while writer holds exclusive lock
- ✅ Implicit `execute()` WAL ordering verified
- ✅ v2→v3 format migration on checkpoint
- ✅ FaultInjectingBackend write-fail: WAL write error propagates correctly (Wave 3 #209)
- ✅ FaultInjectingBackend flush-fail: flush error propagates without data corruption (Wave 3 #209)
- ✅ FaultInjectingBackend read-fault: read error on replay returns Err (Wave 3 #210)
- ✅ WAL CRC corruption: corrupt entry discarded, replay continues (Wave 3 #210)
- ✅ Checkpoint atomicity: backend write-fail during checkpoint leaves WAL intact (Wave 3 #214)
- ✅ Partial checkpoint recovery: incomplete checkpoint detected and WAL replayed (Wave 3 #214)
- ✅ Multi-writer serialisation: concurrent writers serialise correctly under fault conditions (Wave 3 #217)
- ✅ Concurrent write+checkpoint: no deadlock or data loss under concurrent fault injection (Wave 3 #214)
- ✅ Backend error propagation: storage errors surface as Err, not panic (Wave 3 #209)
- ✅ Header-only WAL preserves tx counter and acked writes (A7-found regression: counter must hold the committed watermark, next write must extend not reuse tx_counts, and its WAL entry must replay)
- ✅ Short WAL header recovery (A8 gate finding): 0/7/31-byte lazy-create artifacts replay as empty and are reinitialized before append; full-size bad-magic headers still reject
- ✅ `last_tx_count` page probe: last slot carries the page max; empty / non-packed / short pages read as `None` (A2)

### kill -9 Durability Harness (`tests/kill9_durability_test.rs`) - ✅ 2 tests (A7)

- ✅ Smoke: 24 kill cycles against real `minigraf --session --file` children (default suite, ~1 s)
- ✅ `#[ignore]` Nightly gate: A8-extended run passed 2,400 kill cycles / 169,275 acked transactions / 333 acked forgets, zero lost, zero unopenable (see `docs/BENCHMARKS.md` "A8: Bulk Valid-Time Closure")
- Per-cycle audit: acked exactly-once, in-flight all-or-nothing promotion, transaction atomicity, phantom/duplicate detection, tx-count monotonicity, functional-after-recovery probe
- FileLock crash-robustness unit tests (6) live in `src/storage/backend/file.rs`

### Bulk Valid-Time Closure (`tests/forget_test.rs`) - ✅ 18 tests (A8)

- ✅ Query-result and explicit-fact-list forms close exact EAV triples under one `tx_count`
- ✅ Current, explicit `:valid-at`, and transaction-time `:as-of` views preserve reversible history semantics
- ✅ Idempotent no-op, earlier re-truncation, finite/disjoint windows, same-start overlap deduplication, and re-assert-after-forget
- ✅ Parser/runtime errors reject ambiguous temporal selectors, malformed EAV result order, prepare, and explicit `WriteTransaction` staging
- ✅ Checkpoint/reopen retains closure history
- ✅ Browser WASM in-memory parity: closure result JSON, current invisibility, and historical visibility
- ✅ Gate: 10,000 query-selected facts close atomically as 20,000 scoped retract + truncated re-assert records in 86.1 ms release

### Covering Indexes (`tests/index_test.rs`) - ✅ 6 tests (Phase 6.1)

- ✅ EAVT/AEVT/AVET/VAET save and reload roundtrip
- ✅ Bi-temporal queries still correct after index save/reload
- ✅ Recursive rules regression (indexes don't break rule evaluation)
- ✅ Index checksum mismatch triggers rebuild
- ✅ v3→v4 format migration on first save

### Packed Pages / Performance (`tests/performance_test.rs`) - ✅ 7 tests (Phase 6.2)

- ✅ 1K facts correct after packed save/reload
- ✅ Packed file size < one-per-page estimate (compactness check)
- ✅ Bitemporal query correct after packed reload
- ✅ As-of query correct after packed reload
- ✅ Recursive rules unchanged after Phase 6.2
- ✅ Explicit transaction survives packed reload
- ✅ `page_cache_size` option accepted without panic

### Retraction Semantics (`tests/retraction_test.rs`) - ✅ 7 tests (Phase 6.4a)

- ✅ Assert then retract; current-time query returns no results
- ✅ Assert at tx=1, retract at tx=3; `:as-of 2` shows fact, `:as-of 4` hides it
- ✅ Assert, retract, re-assert; current-time query returns fact
- ✅ Retraction + `:any-valid-time` combo
- ✅ Recursive rule: retracted edge not traversed (`:as-of` after retraction)
- ✅ Recursive rule: retracted edge is visible in historical snapshot before retraction
- ✅ Multiple retractions for different entities in same transaction

### Edge Cases (`tests/edge_cases_test.rs`) - ✅ 4 tests (Phase 6.4a)

- ✅ Oversized fact in file-backed database returns `Err`, not panic
- ✅ In-memory database accepts facts of any size (no size limit)
- ✅ Fact at exactly `MAX_FACT_BYTES` is accepted
- ✅ Fact at `MAX_FACT_BYTES + 1` is rejected with clear error message

### B+Tree v6 (`tests/btree_v6_test.rs`) - ✅ 8 tests (Phase 6.5)

- ✅ Single-page B+tree insert and range scan correctness
- ✅ Multi-page B+tree (leaf chain traversal across multiple pages)
- ✅ Range scan with exclusive upper bound
- ✅ Empty range scan returns empty result
- ✅ Concurrent range scans — 8 barrier-synchronised threads all return identical results
- ✅ v5 database opens and migrates to v6 on first checkpoint
- ✅ v6 database survives close/reopen with correct fact count
- ✅ Index lookup via `OnDiskIndexReader` returns correct `FactRef`s

### Negation — `not` (`tests/negation_test.rs`) - ✅ 10 tests (Phase 7.1a)

- ✅ Basic `not` — exclude entities where a pattern matches
- ✅ `not` with multi-clause body
- ✅ `not` in a rule body (stratification + derived negation)
- ✅ `not` with `:as-of` time travel
- ✅ `not` with `:valid-at`
- ✅ Negative cycle via `not` at rule registration → `Err`, rule not registered
- ✅ `not` where no entities match the body — all outer bindings survive
- ✅ Safety check: unbound variable in `not` body → parse error
- ✅ Nested `not` rejected at parse time
- ✅ `not` with `RuleInvocation` in body — derived rule facts correctly negated end-to-end

### Not-Join (`tests/not_join_test.rs`) - ✅ 14 tests (Phase 7.1b)

- ✅ Basic `not-join` — exclude entities where existentially-quantified dependency exists
- ✅ Multiple join variables in `not-join`
- ✅ Multi-clause body with a local variable linking inner patterns
- ✅ `not-join` in a rule body
- ✅ Multi-stage filtering chain (two independent `not-join` rules applied progressively)
- ✅ `not-join` vs `not` semantic difference (inner-only variable)
- ✅ `not-join` with `:as-of` time travel
- ✅ Unbound join variable → parse error naming the variable
- ✅ Nested `not-join` rejected at parse time
- ✅ `RuleInvocation` in `not-join` body — derived facts correctly negated end-to-end
- ✅ No-match survival — when no entity satisfies the body, all outer bindings survive
- ✅ `not-join` with `:valid-at`
- ✅ Negative cycle via `not-join` at rule registration → `Err`, rule not registered
- ✅ `not` and `not-join` coexist in the same query

### Window Functions (`tests/window_functions_test.rs`) - ✅ 12 tests (Phase 7.7a)

- ✅ Cumulative sum over ordered partition
- ✅ Running count and running min
- ✅ Running average
- ✅ Rank with ties (equal values share rank)
- ✅ Row-number (unique sequential position regardless of ties)
- ✅ Partition-by — window resets per group
- ✅ Descending order in window spec
- ✅ Mixed aggregate + window in same `:find`
- ✅ Single-row result (window function on one row)
- ✅ Empty-result edge case (no matching facts)
- ✅ `lag` / `lead` rejected at parse time

### User-Defined Functions (`tests/udf_test.rs`) - ✅ 9 tests (Phase 7.7b)

- ✅ `custom_aggregate_geometric_mean` — UDF aggregate registered and used in `:find`
- ✅ `custom_aggregate_empty_result` — UDF aggregate on empty result set returns correct identity
- ✅ `custom_predicate_filter` — UDF predicate in `:where` filters binding rows
- ✅ `udf_as_window_function` — UDF aggregate used inside `:over` window spec
- ✅ `name_collision_builtin_aggregate` — registering a UDF with a built-in name returns `Err`
- ✅ `name_collision_udf_on_udf` — registering a second UDF with the same name returns `Err`
- ✅ `unknown_function_runtime_error` — invoking an unregistered aggregate name at query time returns `Err`
- ✅ `unknown_predicate_runtime_error` — invoking an unregistered predicate name at query time returns `Err`
- ✅ `thread_safety` — concurrent UDF registration and query execution from multiple threads

### Prepared Statements (`tests/prepared_statements_test.rs`) - ✅ 17 tests (Phase 7.8)

- ✅ `prepare_and_execute_entity_slot` — entity `$slot` substituted at execute time; correct results returned
- ✅ `prepare_and_execute_value_slot` — value `$slot` substituted at execute time; correct filtering
- ✅ `prepare_and_execute_as_of_tx_count` — `:as-of $tx` with `TxCount` variant; time-travel query returns correct snapshot
- ✅ `prepare_and_execute_as_of_timestamp` — `:as-of $tx` with `Timestamp` variant (millis)
- ✅ `prepare_and_execute_valid_at_timestamp` — `:valid-at $date` with `Timestamp` variant
- ✅ `prepare_and_execute_valid_at_any` — `:valid-at $va` with `AnyValidTime` variant; all time-windows returned
- ✅ `prepare_and_execute_combined_temporal_and_entity` — `:as-of $tx` + entity `$slot` simultaneously (primary agentic loop pattern)
- ✅ `plan_is_reused_across_executions` — same `PreparedQuery` executed twice with different bindings; both correct
- ✅ `error_missing_bind_value` — missing `$slot` at execute time returns `Err`
- ✅ `error_wrong_type_for_as_of` — `Val` supplied for `:as-of` slot returns type-mismatch `Err`
- ✅ `error_wrong_type_for_valid_at` — `TxCount` supplied for `:valid-at` slot returns type-mismatch `Err`
- ✅ `error_wrong_type_for_entity` — `Val` supplied for entity slot returns type-mismatch `Err`
- ✅ `error_attribute_slot_rejected` — `$slot` in attribute position rejected at prepare time
- ✅ `prepare_with_no_slots` — static query prepared and executed correctly (no bindings needed)
- ✅ `prepare_transact_rejected` — preparing a `(transact ...)` command returns `Err`
- ✅ `execute_with_extra_bindings` — extra `BindValue`s beyond declared slots are silently ignored
- ✅ `multiple_slots_same_execute` — multiple distinct `$slot` names resolved in a single `execute()` call

### Migration Matrix (`tests/migration_matrix_test.rs`) - ✅ 6 tests (Wave 3 #215 + v11 migration)

- ✅ current format round-trip — facts written and read back correctly after save/load
- ✅ v7 fixture migrate — committed v7 fixture opens and upgrades to current v11 format
- ✅ v3 empty migrate — empty v3 database opens cleanly
- ✅ corrupt magic — file with bad magic header returns `Err`, not panic
- ✅ unsupported version — file with unrecognised format version returns `Err`
- ✅ WAL replay idempotent — replaying a WAL twice produces the same result as replaying once

### Multi-Value Index Regression (`tests/multivalue_index_test.rs`) - ✅ 7 tests (#287)

- ✅ entity-bound indexed query keeps N=3 same entity+attribute values from one batch
- ✅ N=10 same entity+attribute values survive one transaction
- ✅ mixed string/integer/boolean/keyword values survive one same-attribute batch
- ✅ ref edge values survive same entity+attribute batch and attribute+ref-value lookups
- ✅ `:as-of` replay sees every pre-retraction value; current view hides all retracted values
- ✅ per-fact valid windows remain queryable with `:valid-at`
- ✅ checkpoint/reopen committed indexes preserve every same entity+attribute value

### Retract Valid-Time Regression (`tests/retract_valid_time_test.rs`) - ✅ 5 tests

- ✅ per-fact scoped retract removes only the matching valid-time window
- ✅ transaction-level scoped retract options apply to Ref edge values
- ✅ legacy retract still removes every valid-time window for the same EAV triple
- ✅ explicit `WriteTransaction` scoped retract matches implicit `Minigraf::execute`
- ✅ checkpoint/reopen preserves scoped retraction semantics

### Fact-Log Export (`tests/fact_log_export_test.rs`) - ✅ 7 tests

- ✅ append-only export includes assertion and legacy retraction records with `tx_id`, `tx_count`, valid-time scope, and `asserted`
- ✅ scoped Ref-edge retractions export as exact valid-time windows, not all-valid-time legacy wipes
- ✅ same Ref E/A/V retract and assert in one `WriteTransaction` exports as two same-tx rows distinguished by `asserted`
- ✅ checkpoint/reopen preserves Ref values and per-window fact-log records
- ✅ A2 `export_fact_log_since`: exact ordered `tx_count > since` subsequence of the full export (asserted + retracted, valid-time scope preserved); since=0 equals full export, since≥head is empty
- ✅ A2 since-tail spans base / delta-segment / pending layers with subsequence equivalence at every layer boundary
- ✅ A2 stored-cursor poll across reopen sees exactly the new assertion + retraction past the cursor

### Delta Checkpoint Integration (`tests/delta_checkpoint_integration_test.rs`) - ✅ 17 tests

- ✅ delta checkpoint writes a v10 manifest payload and retires WAL only after durable publish
- ✅ base checksum remains scoped to the base view during delta publish
- ✅ manifest base-root mismatch rejects reopen
- ✅ second checkpoint over visible delta rotates to the inactive manifest slot
- ✅ second checkpoint appends only pending segment pages instead of rewriting accumulated delta facts
- ✅ base-to-delta and segment-to-segment `Value::Ref` edges survive checkpoint/reopen
- ✅ later delta segment retraction hides an earlier delta assertion in current view while history remains queryable
- ✅ multi-segment export preserves deterministic tx order across base and deltas
- ✅ corrupt newer header slot, manifest payload, or delta segment falls back to the previous valid manifest
- ✅ corrupt older segment referenced by the selected multi-segment manifest errors instead of opening base-only
- ✅ both invalid manifest slots error instead of silently dropping committed delta writes
- ✅ full rebuild fallback after a visible delta preserves results and fact-log rows
- ✅ recompact writes a copy-on-write base, records checksum-protected base fact start, and keeps unpublished candidate pages invisible before page 0 publish
- ✅ recompact streams visible facts into candidate pages without materializing an intermediate committed `Vec<Fact>`
- ✅ recompact preserves fact-log storage order before/after/reopen, including `Value::Ref` assertions and scoped retractions
- ✅ private idle delta maintenance noops on healthy deltas, recompact scheduled/backpressure deltas, rejects pending facts, and preserves the previous visible delta on fault-injected phase-2 failure
- ✅ public `run_idle_maintenance()` checkpoints pending file writes, recompacts threshold deltas, converges on a second idle call, rejects same-thread active write transactions, and leaves foreground `checkpoint()` free of hidden recompact
- ✅ FileBackend non-header page writes do not publish disk page 0; durable page-count changes require an explicit header write

### Delta Checkpoint Crash Recovery (`tests/delta_checkpoint_crash_recovery_test.rs`) - ✅ 5 tests

- ✅ pre-header crash replays WAL and ignores unpublished delta bytes
- ✅ unpublished corrupt delta pages do not block WAL recovery
- ✅ selected corrupt or truncated delta errors even if a WAL exists
- ✅ post-header/pre-WAL-delete crash skips stale WAL without duplicating facts

### Index Corruption (`tests/index_corruption_test.rs`) - ✅ 5 tests (Wave 3 #216)

- ✅ checksum corruption — database with corrupted index checksum rebuilds index and serves correct query
- ✅ btree leaf no-panic — corrupt btree leaf page returns `Err` without panic
- ✅ btree internal no-panic — corrupt btree internal page returns `Err` without panic
- ✅ root pointer mismatch no-panic — mismatched root pointer in header handled without panic
- ✅ non-critical corruption query check — database with non-critical corruption still serves queries on good data

### Property-Based Tests (`tests/property_test.rs`) - ✅ 3 tests (Wave 3 #212/#213/#219)

- ✅ `prop_eav_model` — arbitrary EAV fact sets stored and retrieved correctly (proptest)
- ✅ `prop_bitemporal_monotonicity` — tx-time advances monotonically across arbitrary transactions (proptest)
- ✅ `prop_retract_visibility` — retracted facts are invisible in current view and visible in pre-retraction `:as-of` snapshot (proptest)

### Long-Haul Smoke (`tests/smoke_test.rs`) - ✅ 1 test (Wave 3 #220, `#[ignore]` nightly)

- ✅ `smoke_large_graph_10_cycles` — 500 entities × 10 attributes × 10 update cycles; 7 invariants verified: active count (333), retracted count, fact count bounds, temporal snapshot integrity, prepared query consistency, rule transitive closure, WAL checkpoint round-trip

### XTDB Compatibility (`tests/xtdb_compat_test.rs`) - ✅ 10 tests (Wave 3 #221)

- ✅ `xtdb_eav_triple_model` — entity attributes are independent queryable facts
- ✅ `xtdb_transaction_time_as_of` — `:as-of` by tx_count matches XTDB transaction-time semantics
- ✅ `xtdb_valid_time_travel` — `:valid-at` point-in-time filter matches XTDB valid-time semantics
- ✅ `xtdb_retraction_current_view` — retracted facts absent from current-time view
- ✅ `xtdb_retraction_historical_view` — retracted facts visible in pre-retraction snapshot
- ✅ `xtdb_datalog_join` — multi-pattern join matches XTDB Datalog query semantics
- ✅ `xtdb_datalog_negation` — `not` clause matches XTDB negation semantics
- ✅ `xtdb_recursive_rules` — recursive rule transitive closure matches XTDB rule semantics
- ✅ `xtdb_parameterized_query` — prepared-statement `$slot` bindings match XTDB `:in` semantics
- ✅ `xtdb_bitemporal_combined` — combined `:as-of` + `:valid-at` query matches XTDB bi-temporal semantics

### Datomic Compatibility (`tests/datomic_compat_test.rs`) - ✅ 9 tests (Wave 3 #221)

- ✅ `datomic_entity_attributes_are_independent_facts` — EAV datom model: each attribute independently queryable
- ✅ `datomic_multiple_entities_same_attribute` — multiple entities share the same attribute; all (entity, value) pairs returned
- ✅ `datomic_transaction_time_as_of` — `:as-of tx_count` matches Datomic transaction-time semantics
- ✅ `datomic_retract_all_entity_facts` — fully-retracted entity absent from all queries
- ✅ `datomic_multi_variable_find` — multi-variable `:find` returns correct tuple count
- ✅ `datomic_ground_value_binding` — constant binding in `:where` clause filters correctly
- ✅ `datomic_parameterized_query_prepared` — prepared `$slot` bindings match Datomic `:in` clause semantics
- ✅ `datomic_named_rule_reuse` — named reusable rules match Datomic rule semantics
- ✅ `datomic_predicate_expression_filter` — predicate expression `[(>= ?a 18)]` matches Datomic expression clause semantics

---

## Coverage Metrics

**Overall Code Coverage**: ~94% (estimate)

**By Category**:
- ✅ Happy path: ~98%
- ✅ Core Datalog operations: ~95%
- ✅ Recursive rules: ~95%
- ✅ Bi-temporal queries: ~95%
- ✅ WAL and crash recovery: ~94%
- ✅ Transaction API: ~93%
- ✅ Covering indexes: ~94%
- ✅ Packed pages + LRU cache: ~91%
- ✅ Error handling: ~84% (raised from ~82% via edge case tests)
- ✅ Edge cases: ~90% (raised from ~87% via edge case + retraction tests)
- ✅ Concurrency: ~92%
- ✅ Performance benchmarks: Criterion suite run at 1K–1M facts; documented in `BENCHMARKS.md` (Phase 6.4b)

---

## What's Thoroughly Tested ✅

### Phase 3 Core Features
1. Datalog Core — Transact, retract, query
2. Pattern Matching — Variable unification, multi-pattern joins
3. Fact Storage — EAV model, history, retractions
4. EDN Parsing — All Datalog syntax variations
5. Storage Backends — File and memory persistence
6. Recursive Rules — Semi-naive evaluation, fixed-point iteration
7. Transitive Closure — Multi-hop reachability
8. Cycle Handling — Graphs with cycles converge correctly
9. Complex Queries — 3+ patterns, self-joins, entity references
10. Concurrency — Thread-safe rule registration and querying

### Phase 4 Bi-temporal Features
11. Transaction Time — `tx_count` increments, `get_facts_as_of()` snapshots
12. Valid Time — `valid_from`/`valid_to` filtering, boundary semantics
13. Time Travel Queries — `:as-of` counter and timestamp
14. Valid-at Queries — Point-in-time filter, `:any-valid-time`
15. Combined Bi-temporal — Both dimensions in one query
16. Transact with Valid Time — Batch-level and per-fact overrides
17. File Format Migration — v1→v2 with correct temporal defaults

### Phase 5 ACID + WAL Features
18. WAL Format — CRC32-protected entries, partial-write discard
19. Crash Recovery — WAL replay on open, dedup via `last_checkpointed_tx_count`
20. Explicit Transactions — `begin_write` / `commit` / `rollback`
21. WAL Ordering — WAL fsynced before in-memory apply (both implicit and explicit paths)
22. Checkpoint — WAL flushed to `.graph`, WAL deleted, header updated
23. Auto-checkpoint — Fires at configurable WAL entry threshold
24. Thread Safety — Concurrent readers + exclusive writer verified with Barrier

### Phase 6.1 Index Features
25. EAVT/AEVT/AVET/VAET — Four covering indexes with bi-temporal keys
26. FactRef — Disk location pointer, slot_index always 0 in 6.1
27. Value Encoding — Sort-order-preserving canonical encoding
28. B+tree Persistence — Multi-page blob strategy, sort order preserved
29. FileHeader v4 — Index root pages, CRC32 checksum
30. Index Rebuild — Triggered by checksum mismatch on open
31. Query Optimizer — Index hint selection, join reordering by selectivity

### Phase 6.2 Packed Page + Cache Features
32. Packed Pages — ~25 facts/page, header + directory + records layout
33. FactRef Semantics — `page_id=0` = pending, `page_id>=1` = committed via cache
34. CommittedFactReader — Trait + impl wired in PersistentFactStorage::load()
35. LRU Page Cache — Read-lock on hits, Arc cloning, eviction correctness
36. v4→v5 Migration — Reads one-per-page, repacks, saves with new format
37. EAVT/AEVT Range Scans — O(log n) entity and attribute lookups

### Phase 6.4a Retraction Semantics + Edge Cases
38. Retraction Net View — `net_asserted_facts()` groups by EAV triple, keeps highest `tx_count`
39. Current-Time Retraction — Retracted fact absent from query results with no `:as-of`
40. As-Of Retraction — Retraction visible/invisible at correct tx boundary
41. Re-Assert After Retract — Fact reappears when re-asserted
42. Retraction in Recursive Rules — Retracted edges not traversed in rule derivation
43. Oversized-Fact Early Validation — `check_fact_sizes()` rejects before WAL write
44. `MAX_FACT_BYTES` Boundary — Exact-size accepted, +1 rejected with clear error

### Phase 6.4b Byte-Layout Pins
45. FileHeader v5 Field Offsets — All 10 fields pinned at exact byte positions (big-endian detection coverage)
46. Packed Page Header Layout — page_type, reserved, record_count u16 LE, next_page u64 LE at bytes 0–11
47. Packed Page Record Directory — (offset u16 LE, length u16 LE) per slot, starting at byte 12

### Phase 6.5 On-Disk B+Tree Indexes
48. B+Tree Build + Range Scan — `build_btree` inserts and `range_scan` retrieves with correct ordering
49. Multi-Page Leaf Chain — range scan correctly follows `next_leaf` pointers across page boundaries
50. Concurrent Range Scans — 8 barrier-synchronised threads, all return identical non-empty results
51. v5→v6 Migration — database opened from v5 format migrates to v6 on first checkpoint
52. `OnDiskIndexReader` FactRef Lookup — committed facts resolved correctly via page cache
53. `MutexStorageBackend` — cache-warm pages acquire no backend lock; cache-cold pages lock briefly

### Phase 7.1 Stratified Negation
54. `not` — basic absence query excludes entities where pattern matches
55. `not` in rule body — stratified mixed-rule evaluation applies negation per binding
56. `not-join` — existentially-quantified exclusion with explicit join variables
57. `not-join` multi-clause body — inner variables link patterns without escaping to outer scope
58. `not-join` in rule body — negation inside derived rules
59. Negative cycle rejection — `not` / `not-join` creating a dependency cycle → `Err` at registration, rule not added
60. Safety validation — unbound variables in `not` body or `join_vars` → parse error with variable name
61. Nesting constraint — `not-join` inside `not` or `not-join` → parse error
62. `RuleInvocation` in `not-join` body — derived facts in accumulated store correctly negated
63. Time travel with negation — `not-join` respects `:as-of` and `:valid-at` temporal filters
64. `not` and `not-join` coexistence in the same query

---

## What's Not Tested Yet ⏳

### Phase 7.3+ (Remaining Datalog Completeness)
- ⏳ Disjunction (`or` / `or-join`) — Phase 7.3
- ⏳ Query optimizer improvements for new clause types (aggregation, expr, disjunction) — Phase 7.4
- ⏳ Prepared statements with temporal bind slots — Phase 7.6
- ⏳ Temporal metadata pseudo-attributes (`:db/valid-from`, `:db/valid-to`, `:db/tx-count`) — Phase 7.7

### Known Limitations (Acceptable for Phase 3-7.2b)
- ⏳ Crash during checkpoint write (safe by construction — WAL not deleted until save succeeds; explicit test deferred to Phase 7.5)
- ⏳ Disjunction — Phase 7.3
- ⏳ Known `not-join` limitation: when a rule B positively invokes rule A and both are stratum 0, single-pass mixed-rule evaluation means B may not see A's derived facts unless rules are declared in dependency order
- ⏳ `matches?` pattern compiled per-row (no caching); will be optimised in Phase 7.9b (`FunctionRegistry`)

---

## Test Execution

```bash
# Run all tests
cargo test

# Run tests quietly with summary
cargo test --quiet

# Run specific test suites
cargo test --lib                       # Unit tests (759)
cargo test --test bitemporal           # Bi-temporal (10)
cargo test --test complex_queries      # Complex queries (10)
cargo test --test recursive_rules      # Recursive rules (9)
cargo test --test concurrency          # Concurrency (12, 1 ignored)
cargo test --test wal_test             # WAL / crash recovery (22)
cargo test --test kill9_durability_test  # A7 kill -9 harness (smoke; full gate: -- --ignored)
cargo test --test index_test           # Covering indexes (6)
cargo test --test performance_test     # Packed pages (7)
cargo test --test retraction_test      # Retraction semantics (7)
cargo test --test edge_cases_test      # Edge cases (4)
cargo test --test btree_v6_test        # B+tree v6 (8)
cargo test --test negation_test        # stratified not (10)
cargo test --test not_join_test        # not-join (14)
cargo test --test aggregation_test     # aggregation (24)
cargo test --test predicate_expr_test  # arithmetic & predicate expr (28)
cargo test --test window_functions_test # window functions (12)
cargo test --test udf_test             # user-defined functions (9)
cargo test --test prepared_statements_test # prepared statements (17)
cargo test --test migration_matrix_test    # migration matrix (6)
cargo test --test multivalue_index_test    # same entity+attribute multi-value regression (7)
cargo test --test retract_valid_time_test  # scoped retract valid-time parity (5)
cargo test --test fact_log_export_test     # append-only fact-log export + A2 since-tail (7)
cargo test --release --test fact_log_since_benchmark -- --ignored # A2 1M-base gate fixture
cargo test --test delta_checkpoint_integration_test # v10 delta checkpoint integration (17)
cargo test --test delta_checkpoint_crash_recovery_test # v10 delta checkpoint crash recovery (5)
cargo test --test index_corruption_test    # index corruption (5)
cargo test --test property_test            # property-based (3)
cargo test --test xtdb_compat_test         # XTDB compat (10)
cargo test --test datomic_compat_test      # Datomic compat (9)
cargo test --test vicia_api_alias_test     # Vicia DB API alias compatibility (2)
cargo test --test smoke_test -- --include-ignored  # long-haul smoke (1, nightly)

# Run with output
cargo test -- --nocapture
```

---

## Conclusion

**Wave 3 Status**: ✅ **COMPLETE**

**Test Quality**: ✅ **Excellent** — High confidence in all Phase 3-8.1 + Wave 3 reliability features

**Strengths**:
- WAL crash safety verified with real `mem::forget` simulation
- Both implicit and explicit transaction write paths verified
- Thread safety proven with Barrier-synchronized concurrent tests
- Index persistence and CRC32 sync check verified
- Packed page compactness verified against one-per-page estimate
- CommittedFactReader wiring verified with MockLoader in unit tests
- Retraction semantics verified across current-time, as-of, and recursive-rule queries
- Oversized-fact early rejection verified for file-backed databases
- Criterion benchmarks validated performance at 1K–1M facts
- Byte-layout tests pin FileHeader v5/v6 and packed page header field offsets
- On-disk B+tree correctness and concurrent scan safety verified (Phase 6.5)
- Stratified negation (`not` / `not-join`) verified: safety validation, stratification, negative cycle rejection, time-travel integration (Phase 7.1)
- Aggregation verified: all 6 aggregate functions, `:with` grouping, bi-temporal + aggregate, rule + aggregate (Phase 7.2a)
- Arithmetic & predicate expressions verified: all operators, silent-drop semantics, int/float promotion, regex validation, expr in not/rule body, bi-temporal + expr (Phase 7.2b)
- Disjunction (`or` / `or-join`) verified: flat queries, rule bodies, nested or/not/expr, or-join with private variables, dependency graph (Phase 7.3)
- Window functions verified: cumulative aggregates, rank/row-number, partition-by, desc ordering, mixed aggregate+window (Phase 7.7a)
- User-defined functions verified: custom aggregates, custom predicates, UDF as window function, name collision guards, runtime error handling, thread safety (Phase 7.7b)
- Prepared statements verified: entity/value/as-of/valid-at slot positions, AnyValidTime, combined temporal+entity (agentic loop pattern), plan reuse, all error paths (Phase 7.8)
- Public API surface verified via rustdoc doctests and integration tests: `Minigraf::open`, `execute`, `prepare`, `export_fact_log`, `run_idle_maintenance`, `repl`, `WriteTransaction`, `OpenOptions` (Phase 7.9 + Vetch ledger export + Q3-A maintenance)
- WAL fault injection verified: write-fail, flush-fail, read-fault, CRC corruption, checkpoint atomicity, concurrent write+checkpoint (Wave 3)
- Migration matrix verified: current round-trip, v7 fixture migrate, v3 empty migrate, corrupt magic, unsupported version, WAL replay idempotent (Wave 3 + v11 migration)
- Multi-value index regression verified: same entity+attribute batch values survive indexed public query paths, ref edge lookups, temporal replay, retraction, and checkpoint/reopen (#287)
- Retract valid-time parity verified: scoped retractions remove only the matching valid-time window while legacy retractions still wipe every valid-time window for the same EAV triple
- Delta checkpoint integration verified: v10 multi-segment manifest append, base/delta and segment/segment `Value::Ref` edges, later-segment retractions, deterministic multi-delta export, and corrupt-slot fallback
- Delta checkpoint crash recovery verified: unpublished delta bytes ignored, WAL replay preserved, selected corrupt/truncated deltas rejected, stale WAL skipped after header publish
- Index corruption resilience verified: v11 header/catalog corruption rejects open; exact fact/index corruption propagates from the first public query
- Property-based testing verified: EAV model, bi-temporal monotonicity, retract visibility (Wave 3)
- Long-haul smoke verified: 500 entities × 10 attrs × 10 cycles, 7 invariants, nightly CI (Wave 3)
- XTDB compatibility verified: 10 semantic ports covering EAV, time travel, negation, rules, prepared queries (Wave 3)
- Datomic compatibility verified: 9 independently written semantic ports covering datom model, tx-time, retraction, Datalog patterns (Wave 3)
- 1286 native tests covering all Phase 3-8.1 features + Wave 3 reliability/compat + Vetch ledger identity/export regressions + Vetch delta multi-segment checkpoint and native/browser maintenance/parity regressions + exact per-fact metadata planning + bounded exact-entity planning/execution + A6 session protocol + A7/A8 kill -9 durability + A2 incremental fact log + A8 bulk valid-time closure + A9 linearized backup + A5-6 fail-closed query/page integrity/sparse bootstrap planning (plus 59 browser-WASM tests in source, WASI, cross-platform compat, and fuzzing CI)

**Confidence Level**: ✅ **Production-ready for Wave 3 scope**

**Readiness for Phase 9**: ✅ **Ready to proceed**

The fault-injection-tested, property-based-tested, XTDB/Datomic-compatible, fuzz-hardened, WebAssembly-capable (browser + WASI), publish-ready, prepared-statement-capable, UDF-capable, window-function-capable, disjunction + aggregation + arithmetic/predicate expression capable, stratified-negation-capable, on-disk B+tree indexed, packed, cached bi-temporal Datalog engine is **solid, well-tested, documented, and benchmarked**.

---

**Next Steps**: Phase 9 (Ecosystem & Tooling — examples, wiki guides, performance baseline updates)
