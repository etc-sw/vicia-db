//! Browser WASM support: `BrowserDb` async façade backed by IndexedDB.
//!
//! This module is only compiled for `wasm32-unknown-unknown` with the `browser`
//! feature enabled. It is **not** compatible with Node.js, Deno, Bun, or any
//! server-side runtime. For server-side Node.js, use `@minigraf/node` (Phase 8.3).

/// Synchronous in-memory page buffer with dirty-page tracking.
pub mod buffer;
/// Async IndexedDB backend for browser WASM persistence.
pub mod indexeddb;
mod maintenance;

use crate::browser::buffer::{BrowserBufferBackend, page_not_resident_id};
use crate::browser::indexeddb::IndexedDbBackend;
use crate::graph::FactStorage;
use crate::json_value::to_tagged_json;
use crate::query::datalog::access_plan::QueryAccessPlan;
use crate::query::datalog::executor::{DatalogExecutor, OwnedAggregateStep, QueryResult};
use crate::query::datalog::functions::FunctionRegistry;
use crate::query::datalog::parser::parse_datalog_command;
use crate::query::datalog::rules::RuleRegistry;
use crate::query::datalog::types::{DatalogCommand, ForgetSource, ValidAt};
use crate::storage::delta_growth::DeltaMaintenanceDecision;
use crate::storage::persistent_facts::{
    BrowserPageRange, BrowserV11BootstrapPlan, PersistentFactStorage,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

const BROWSER_AGGREGATE_ENTRY_BUDGET: usize = 4_096;
const BROWSER_AGGREGATE_RESIDENT_PAGE_LIMIT: usize = 192;
const BROWSER_READ_VIEW_MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

async fn yield_browser_task() -> Result<(), JsValue> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let Some(window) = web_sys::window() else {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("window unavailable"),
            );
            return;
        };
        if let Err(error) =
            window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0)
        {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
    });
    JsFuture::from(promise).await.map(|_| ())
}

/// Internal state shared by all `BrowserDb` clones.
struct BrowserDbInner {
    fact_storage: FactStorage,
    rules: Arc<RwLock<RuleRegistry>>,
    functions: Arc<RwLock<FunctionRegistry>>,
    pfs: PersistentFactStorage<BrowserBufferBackend>,
    /// `None` for in-memory databases (no IDB backing).
    idb: Option<IndexedDbBackend>,
    /// Serialises async durability mutations on one handle. Vetch also owns a
    /// cross-tab Web Lock; this guard closes same-handle overlap across await.
    mutation_in_flight: bool,
    /// Set when a write changed the live page image but its IndexedDB commit
    /// failed. Queries and later writes must not expose or promote that image;
    /// the only safe recovery is to discard the handle and reopen.
    durability_poisoned: bool,
    /// Set only when a failed IndexedDB page commit was followed by a
    /// successful reload of the previous durable page image. The rejected
    /// operation is then absent from both memory and IndexedDB, so the handle
    /// remains usable after the Promise rejection.
    mutation_failure_rolled_back: bool,
    /// Becomes true only after the live transaction counter/fact/PFS state has
    /// started advancing. A semantic error before this boundary is a normal
    /// rejection, not a durability poison event.
    mutation_advanced: bool,
    /// `true` for the bounded page-on-demand compatibility path. The legacy
    /// `open()` wrapper uses the same sparse core but prefetches the complete
    /// published image and keeps synchronous `exportGraph()` compatible.
    paged: bool,
    /// Caller-selected open contract. Unlike `paged`, this remains `Paged`
    /// while an imported physically truncated legacy recovery image is held
    /// eagerly/read-only, so a later clean import or maintenance repair can
    /// return to the requested bounded path.
    open_mode: BrowserOpenMode,
    /// A paged read may await IndexedDB between deterministic sync retries.
    /// Exclude mutation/import/maintenance during that gap so no operation can
    /// observe or publish a half-resolved read snapshot.
    paged_read_in_flight: bool,
    projection_tail_cache: crate::graph::current_projection::CurrentProjectionTailCache,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserOpenMode {
    EagerCompatibility,
    Paged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserImportPolicy {
    /// Preserve the long-standing native-compatible recovery behavior.
    RecoveryCompatible,
    /// Publish only a complete current-format image that a fresh
    /// `BrowserDb.openPaged()` can accept as bounded authority.
    RequirePagedReady,
}

struct PreparedBrowserForget {
    facts: Vec<crate::graph::types::Fact>,
    count: usize,
    tx_id: crate::graph::types::TxId,
}

impl BrowserOpenMode {
    fn is_paged(self) -> bool {
        matches!(self, Self::Paged)
    }
}

const BROWSER_IDB_BATCH_PAGES: u64 = 256;

/// Browser-only Minigraf database handle backed by IndexedDB.
///
/// All public methods return `Promise`s. Use `await` in JavaScript.
///
/// **Not compatible with Node.js.** Use `@minigraf/node` for server-side use.
#[wasm_bindgen]
pub struct BrowserDb {
    inner: Rc<RefCell<BrowserDbInner>>,
}

/// Browser selective-query handle pinned to one transaction and valid-time instant.
#[wasm_bindgen]
pub struct BrowserReadView {
    inner: Rc<RefCell<BrowserDbInner>>,
    tx_count: u64,
    valid_at: ValidAt,
}

/// Foreground browser capability for atomic writes and bounded read views.
///
/// This type always opens persistent authority through the paged path and does
/// not expose maintenance, import, or full export methods. `BrowserDb` remains
/// the raw compatibility surface.
#[wasm_bindgen]
pub struct BrowserInteractiveLedger {
    db: BrowserDb,
}

#[wasm_bindgen]
impl BrowserInteractiveLedger {
    /// Open a persistent interactive ledger through `BrowserDb.openPaged()`.
    #[wasm_bindgen(js_name = open)]
    pub async fn open(db_name: &str) -> Result<BrowserInteractiveLedger, JsValue> {
        Ok(Self {
            db: BrowserDb::open_paged(db_name).await?,
        })
    }

    /// Open an in-memory interactive ledger for tests and ephemeral work.
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<BrowserInteractiveLedger, JsValue> {
        Ok(Self {
            db: BrowserDb::open_in_memory()?,
        })
    }

    /// Execute a bounded transact/retract command list as one atomic write.
    #[wasm_bindgen(js_name = executeAtomic)]
    pub async fn execute_atomic(&self, commands: Vec<String>) -> Result<String, JsValue> {
        self.db.execute_atomic(commands).await
    }

    /// Capture a bounded selective read view at the current transaction.
    #[wasm_bindgen(js_name = readView)]
    pub fn read_view(&self) -> Result<BrowserReadView, JsValue> {
        self.db.read_view()
    }

    /// Capture a bounded selective read view at explicit transaction and valid time.
    #[wasm_bindgen(js_name = readViewAt)]
    pub fn read_view_at(
        &self,
        as_of: u64,
        valid_at_millis: i64,
    ) -> Result<BrowserReadView, JsValue> {
        self.db.read_view_at(as_of, valid_at_millis)
    }

    /// Capture a bounded selective read view across every valid-time window.
    #[wasm_bindgen(js_name = readViewAnyValidTime)]
    pub fn read_view_any_valid_time(&self, as_of: u64) -> Result<BrowserReadView, JsValue> {
        self.db.read_view_any_valid_time(as_of)
    }
}

/// Browser capability for disposable-worker maintenance and portability work.
///
/// This type has no foreground execute or read methods. Persistent opens use
/// the paged bootstrap before an explicit O(total) operation begins.
#[wasm_bindgen]
pub struct BrowserMaintenanceLedger {
    db: BrowserDb,
}

#[wasm_bindgen]
impl BrowserMaintenanceLedger {
    /// Open a persistent maintenance ledger through `BrowserDb.openPaged()`.
    #[wasm_bindgen(js_name = open)]
    pub async fn open(db_name: &str) -> Result<BrowserMaintenanceLedger, JsValue> {
        Ok(Self {
            db: BrowserDb::open_paged(db_name).await?,
        })
    }

    /// Open an in-memory maintenance ledger for tests and ephemeral work.
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<BrowserMaintenanceLedger, JsValue> {
        Ok(Self {
            db: BrowserDb::open_in_memory()?,
        })
    }

    /// Run caller-scheduled idle maintenance.
    #[wasm_bindgen(js_name = runIdleMaintenance)]
    pub async fn run_idle_maintenance(&self) -> Result<String, JsValue> {
        self.db.run_idle_maintenance().await
    }

    /// Rebuild exact current-attribute projections as one v13 publication.
    #[wasm_bindgen(js_name = rebuildCurrentProjections)]
    pub async fn rebuild_current_projections(
        &self,
        attributes: Vec<String>,
    ) -> Result<String, JsValue> {
        self.db.rebuild_current_projections(attributes).await
    }

    /// Export the complete verified `.graph` image.
    #[wasm_bindgen(js_name = exportGraph)]
    pub async fn export_graph(&self) -> Result<js_sys::Uint8Array, JsValue> {
        self.db.export_graph_async().await
    }

    /// Atomically import a graph only when it can reopen as bounded paged authority.
    #[wasm_bindgen(js_name = importGraph)]
    pub async fn import_graph(&self, data: js_sys::Uint8Array) -> Result<(), JsValue> {
        self.db.import_graph_for_paged_access(data).await
    }
}

#[wasm_bindgen]
impl BrowserDb {
    /// Open an in-memory database (no IndexedDB — for testing only).
    ///
    /// Data is lost when the page is closed. Use `BrowserDb.open()` for persistence.
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<BrowserDb, JsValue> {
        let page0 =
            crate::storage::header_extension::build_header_page(crate::storage::FileHeader::new())
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let buffer = BrowserBufferBackend::load_pages(HashMap::from([(0, page0)]));
        let pfs = PersistentFactStorage::new(buffer, 256)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let fact_storage = pfs.storage().clone();

        Ok(BrowserDb {
            inner: Rc::new(RefCell::new(BrowserDbInner {
                fact_storage,
                rules: Arc::new(RwLock::new(RuleRegistry::new())),
                functions: Arc::new(RwLock::new(FunctionRegistry::with_builtins())),
                pfs,
                idb: None,
                mutation_in_flight: false,
                durability_poisoned: false,
                mutation_failure_rolled_back: false,
                mutation_advanced: false,
                paged: false,
                open_mode: BrowserOpenMode::EagerCompatibility,
                paged_read_in_flight: false,
                projection_tail_cache:
                    crate::graph::current_projection::CurrentProjectionTailCache::default(),
            })),
        })
    }

    /// Capture a selective foreground read view at the current transaction.
    ///
    /// The returned handle retains this transaction cursor and creation-time
    /// valid-time point even if later writes advance the database.
    #[wasm_bindgen(js_name = readView)]
    pub fn read_view(&self) -> Result<BrowserReadView, JsValue> {
        self.create_read_view(
            None,
            ValidAt::Timestamp(crate::graph::types::tx_id_now().cast_signed()),
        )
    }

    /// Create a selective read view at explicit transaction and valid-time points.
    #[wasm_bindgen(js_name = readViewAt)]
    pub fn read_view_at(
        &self,
        as_of: u64,
        valid_at_millis: i64,
    ) -> Result<BrowserReadView, JsValue> {
        self.create_read_view(Some(as_of), ValidAt::Timestamp(valid_at_millis))
    }

    /// Create a selective transaction-pinned view over every valid-time window.
    #[wasm_bindgen(js_name = readViewAnyValidTime)]
    pub fn read_view_any_valid_time(&self, as_of: u64) -> Result<BrowserReadView, JsValue> {
        self.create_read_view(Some(as_of), ValidAt::AnyValidTime)
    }

    /// Open or create a database backed by IndexedDB.
    ///
    /// `db_name` is used as both the IndexedDB database name and object store name.
    /// Called as `await BrowserDb.open("mydb")` — NOT `new BrowserDb()`.
    #[wasm_bindgen(js_name = open)]
    pub async fn open(db_name: &str) -> Result<BrowserDb, JsValue> {
        let idb = IndexedDbBackend::open(db_name).await?;
        Self::open_from_idb_mode(idb, BrowserOpenMode::EagerCompatibility).await
    }

    /// Open a persistent database with generation-checked, page-on-demand
    /// IndexedDB reads.
    ///
    /// This is the bounded Vetch authority path. Immutable base fact/index
    /// pages are fetched only when a query touches them and retained by the
    /// core's fixed-size page cache. `exportGraphAsync()` is the matching
    /// portability API; synchronous `exportGraph()` remains for in-memory and
    /// eager-compatibility handles.
    #[wasm_bindgen(js_name = openPaged)]
    pub async fn open_paged(db_name: &str) -> Result<BrowserDb, JsValue> {
        let idb = IndexedDbBackend::open(db_name).await?;
        Self::open_from_idb_mode(idb, BrowserOpenMode::Paged).await
    }

    async fn open_from_idb(idb: IndexedDbBackend) -> Result<BrowserDb, JsValue> {
        Self::open_from_idb_mode(idb, BrowserOpenMode::EagerCompatibility).await
    }

    #[cfg(test)]
    async fn open_paged_from_idb(idb: IndexedDbBackend) -> Result<BrowserDb, JsValue> {
        Self::open_from_idb_mode(idb, BrowserOpenMode::Paged).await
    }

    async fn open_from_idb_mode(
        idb: IndexedDbBackend,
        mode: BrowserOpenMode,
    ) -> Result<BrowserDb, JsValue> {
        let (pfs, paged) = open_persistent_storage(&idb, mode).await?;
        let fact_storage = pfs.storage().clone();

        Ok(BrowserDb {
            inner: Rc::new(RefCell::new(BrowserDbInner {
                fact_storage,
                rules: Arc::new(RwLock::new(RuleRegistry::new())),
                functions: Arc::new(RwLock::new(FunctionRegistry::with_builtins())),
                pfs,
                idb: Some(idb),
                mutation_in_flight: false,
                durability_poisoned: false,
                mutation_failure_rolled_back: false,
                mutation_advanced: false,
                paged,
                open_mode: mode,
                paged_read_in_flight: false,
                projection_tail_cache:
                    crate::graph::current_projection::CurrentProjectionTailCache::default(),
            })),
        })
    }

    /// Execute a Datalog command string and return a JSON-encoded result.
    ///
    /// Returns a `Promise<string>` in JavaScript. The JSON shape is:
    /// - Query: `{"variables": [...], "results": [[...], ...]}`
    /// - Transact: `{"transacted": <tx_id>, "tx_id": <tx_id>, "tx_count": <n>, ...}`
    /// - Retract: `{"retracted": <tx_id>, "tx_id": <tx_id>, "tx_count": <n>, ...}`
    /// - Forget: `{"forgotten": <count>, "tx_id": <tx_id or null>, ...}`
    /// - Rule: `{"ok": true}`
    ///
    /// Successful mutations also report `durability` (`"published"` for
    /// IndexedDB and `"memory"` for in-memory handles),
    /// `maintenance_pending`, and `advice`. Advice is one of `"none"`,
    /// `"schedule_idle_maintenance"`, or `"reduce_checkpoint_cadence"`.
    /// A no-match forget reports `durability: "noop"`, `advice: "none"`,
    /// and null transaction fields.
    #[wasm_bindgen(js_name = execute)]
    pub async fn execute(&self, datalog: String) -> Result<String, JsValue> {
        self.ensure_usable()?;
        let cmd = parse_datalog_command(&datalog).map_err(|e| JsValue::from_str(&e.to_string()))?;

        // Peek at the discriminant before consuming `cmd`.
        let is_read = matches!(cmd, DatalogCommand::Query(_) | DatalogCommand::Rule(_));

        if is_read {
            let guarded = self.begin_paged_read()?;
            let result = self.execute_read_command(cmd).await;
            if guarded {
                self.finish_paged_read();
            }
            return result.map(query_result_to_json);
        }

        match cmd {
            DatalogCommand::Transact(tx) => {
                let facts = crate::db::Minigraf::materialize_transaction(&tx)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                self.begin_mutation()?;
                let result = self.apply_write(facts, false).await;
                self.finish_mutation(result.is_err());
                result
            }
            DatalogCommand::Retract(tx) => {
                let facts = crate::db::Minigraf::materialize_retraction(&tx)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                self.begin_mutation()?;
                let result = self.apply_write(facts, true).await;
                self.finish_mutation(result.is_err());
                result
            }
            DatalogCommand::Forget(spec) => {
                let prepared = self.prepare_forget(&spec).await?;
                self.begin_mutation()?;
                let result = self.execute_prepared_forget(prepared).await;
                self.finish_mutation(result.is_err());
                result
            }
            // Handled above; unreachable but required for exhaustiveness.
            DatalogCommand::Query(_) | DatalogCommand::Rule(_) => unreachable!(),
        }
    }

    /// Execute a bounded transact/retract command list as one atomic write.
    ///
    /// Every command is parsed and materialized before database state changes.
    /// All resulting facts then receive one shared `tx_id` and `tx_count`, and
    /// persistent handles publish one IndexedDB dirty-page set in one readwrite
    /// transaction. Query, rule, and forget commands are rejected.
    ///
    /// The batch must contain 1..=256 commands, at most 262,144 facts, and at
    /// most 64 MiB of Datalog source. An allocation-free fact/token preflight
    /// rejects syntactic bombs before the normal parser builds its AST. The
    /// JSON result reports command/fact counts plus the same durability,
    /// maintenance, and advice fields as [`BrowserDb::execute`].
    #[wasm_bindgen(js_name = executeAtomic)]
    pub async fn execute_atomic(&self, commands: Vec<String>) -> Result<String, JsValue> {
        self.ensure_usable()?;
        #[cfg(feature = "bench-internals")]
        let preparation_started = js_sys::Date::now();
        let prepared = crate::db::Minigraf::materialize_atomic_write_commands(&commands)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        #[cfg(feature = "bench-internals")]
        let prepared = {
            let mut prepared = prepared;
            prepared.benchmark_preparation_ms = js_sys::Date::now() - preparation_started;
            prepared
        };

        self.begin_mutation()?;
        let result = self.apply_atomic_write(prepared).await;
        self.finish_mutation(result.is_err());
        result
    }

    /// Resolve every page needed by a forget before the mutation boundary.
    /// Retrying after `tx_count` allocation could apply one semantic command
    /// twice, so paged I/O is complete before `begin_mutation()` returns.
    async fn prepare_forget(
        &self,
        spec: &crate::query::datalog::types::ForgetSpec,
    ) -> Result<PreparedBrowserForget, JsValue> {
        use crate::graph::types::tx_id_now;

        let guarded = self.begin_paged_read()?;
        let result = self.prepare_forget_with_paging(spec, tx_id_now()).await;
        if guarded {
            self.finish_paged_read();
        }
        result
    }

    async fn prepare_forget_with_paging(
        &self,
        spec: &crate::query::datalog::types::ForgetSpec,
        tx_id: crate::graph::types::TxId,
    ) -> Result<PreparedBrowserForget, JsValue> {
        if matches!(
            &spec.source,
            ForgetSource::Query(query) if QueryAccessPlan::for_query(query).is_full_scan()
        ) {
            self.prefetch_full_scan_pages().await?;
        }

        let closure_time = spec.valid_to.unwrap_or_else(|| tx_id.cast_signed());
        loop {
            let prepared: anyhow::Result<_> = {
                let inner = self.inner.borrow();
                let executor = DatalogExecutor::new_with_rules_and_functions(
                    inner.fact_storage.clone(),
                    inner.rules.clone(),
                    inner.functions.clone(),
                );
                crate::db::Minigraf::resolve_forget_triples(spec, &executor, closure_time).and_then(
                    |triples| {
                        crate::db::Minigraf::materialize_closure(
                            &inner.fact_storage,
                            &triples,
                            closure_time,
                        )
                    },
                )
            };
            match prepared {
                Ok((facts, count)) => {
                    return Ok(PreparedBrowserForget {
                        facts,
                        count,
                        tx_id,
                    });
                }
                Err(error) => match page_not_resident_id(&error) {
                    Some(page_id) => self.fetch_and_stage_page(page_id).await?,
                    None => return Err(JsValue::from_str(&error.to_string())),
                },
            }
        }
    }

    /// Apply a fully resolved bulk valid-time closure as one transaction, then
    /// publish its dirty page set atomically to IndexedDB.
    async fn execute_prepared_forget(
        &self,
        prepared: PreparedBrowserForget,
    ) -> Result<String, JsValue> {
        // ── Sync section: hold borrow, do ALL sync work, collect owned data ──
        let (dirty_pages, result_json) = {
            let mut inner = self.inner.borrow_mut();

            // Nothing matched: no tx_count consumed, nothing to flush.
            if prepared.facts.is_empty() {
                return Ok(serde_json::json!({
                    "forgotten": 0,
                    "tx_id": serde_json::Value::Null,
                    "tx_count": serde_json::Value::Null,
                    "durability": "noop",
                    "maintenance_pending": false,
                    "advice": "none",
                })
                .to_string());
            }

            let tx_count = inner.fact_storage.allocate_tx_count();
            inner.mutation_advanced = true;
            let tx_id = prepared.tx_id;

            for mut fact in prepared.facts {
                fact.tx_id = tx_id;
                fact.tx_count = tx_count;
                inner
                    .fact_storage
                    .load_fact(fact)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
            }

            inner.pfs.mark_dirty();
            inner
                .pfs
                .save()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;

            let dirty_pages = take_dirty_pages(&mut inner.pfs)?;
            if inner.paged {
                configure_sparse_authority(&mut inner.pfs)?;
            }

            let decision = inner.pfs.delta_maintenance_decision();
            let durability = if inner.idb.is_some() {
                "published"
            } else {
                "memory"
            };
            let json = serde_json::json!({
                "forgotten": prepared.count,
                "tx_id": tx_id,
                "tx_count": tx_count,
                "durability": durability,
                "maintenance_pending": !matches!(
                    decision,
                    DeltaMaintenanceDecision::ContinueDeltaAppend
                ),
                "advice": browser_write_advice(decision),
            })
            .to_string();

            (dirty_pages, json)
        };
        // ── Borrow dropped here ───────────────────────────────────────────────

        // ── Async section: flush to IDB (no RefCell borrow held) ─────────────
        if !dirty_pages.is_empty() {
            let idb = self
                .inner
                .borrow()
                .idb
                .as_ref()
                .map(IndexedDbBackend::clone_handle);
            if let Some(idb) = idb {
                self.flush_dirty_pages_or_restore(idb, dirty_pages).await?;
            }
        }

        self.evict_sparse_staging();

        Ok(result_json)
    }

    /// Flush all dirty pages to IndexedDB.
    ///
    /// Write-through means individual `execute()` calls already flush dirty
    /// pages, and `import_graph()` performs its own atomic flush, so
    /// `checkpoint()` is only needed after explicit bulk ops.
    /// No-op for in-memory databases.
    pub async fn checkpoint(&self) -> Result<(), JsValue> {
        self.begin_mutation()?;
        let result = self.checkpoint_inner().await;
        self.finish_mutation(result.is_err());
        result
    }

    async fn checkpoint_inner(&self) -> Result<(), JsValue> {
        let (dirty_pages, has_idb) = {
            let mut inner = self.inner.borrow_mut();
            inner.mutation_advanced = true;
            inner
                .pfs
                .save()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let pages = take_dirty_pages(&mut inner.pfs)?;
            if inner.paged {
                configure_sparse_authority(&mut inner.pfs)?;
            }
            (pages, inner.idb.is_some())
        };

        if has_idb && !dirty_pages.is_empty() {
            let idb = self
                .inner
                .borrow()
                .idb
                .as_ref()
                .ok_or_else(|| JsValue::from_str("checkpoint IndexedDB handle is missing"))?
                .clone_handle();
            self.flush_dirty_pages_or_restore(idb, dirty_pages).await?;
        }
        self.evict_sparse_staging();
        Ok(())
    }

    /// Reclaim superseded browser pages during a caller-scheduled idle window.
    ///
    /// When the existing delta-growth policy crosses its soft or hard
    /// threshold, this builds a fresh contiguous `.graph` image from the full
    /// append-only fact log, atomically replaces the IndexedDB page set, and
    /// only then swaps the live handle. It never runs from foreground
    /// `execute()` or `checkpoint()`.
    ///
    /// The returned JSON uses the native maintenance vocabulary:
    /// `checkpoint`, `delta`, and `advice`, plus before/after page counts.
    /// In-memory databases and healthy delta lineages return a no-op.
    #[wasm_bindgen(js_name = runIdleMaintenance)]
    pub async fn run_idle_maintenance(&self) -> Result<String, JsValue> {
        self.begin_mutation()?;
        let result = maintenance::run_idle_maintenance(self, false).await;
        // The replacement transaction is atomic and the live swap happens
        // after it commits. A rejected maintenance attempt leaves the old
        // memory and old IndexedDB image aligned, so it does not poison.
        self.finish_mutation(false);
        self.evict_sparse_staging();
        result
    }

    async fn rebuild_current_projections(
        &self,
        attributes: Vec<String>,
    ) -> Result<String, JsValue> {
        self.begin_mutation()?;
        let result = self.rebuild_current_projections_inner(&attributes).await;
        // The projection patch is detached from the live PFS and IndexedDB
        // commits it atomically. A rejected plan or transaction leaves the
        // previous authority usable.
        self.finish_mutation(false);
        self.evict_sparse_staging();
        result
    }

    async fn rebuild_current_projections_inner(
        &self,
        attributes: &[String],
    ) -> Result<String, JsValue> {
        let attributes = crate::db::normalize_current_projection_attributes(attributes)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let valid_time_floor = i64::try_from(crate::graph::types::tx_id_now())
            .map_err(|_| JsValue::from_str("current projection valid-time floor exceeds i64"))?;
        self.checkpoint_inner().await?;

        let (before_pages, idb) = {
            let inner = self.inner.borrow();
            let idb = inner
                .idb
                .as_ref()
                .ok_or_else(|| {
                    JsValue::from_str(
                        "current projection rebuild requires a persistent maintenance ledger",
                    )
                })?
                .clone_handle();
            let before_pages = inner
                .pfs
                .published_page_count()
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            (before_pages, idb)
        };
        let budget = crate::db::current_projection_budget_bytes(before_pages)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;

        // The builder streams the immutable AEVT base. Resolve that complete
        // integrity-covered range only inside this explicit O(total) worker
        // operation; foreground reads retain their bounded demand paging.
        self.prefetch_projection_source_pages().await?;

        let (identity, images, projection_bytes, row_count) = {
            let inner = self.inner.borrow();
            let identity = inner
                .pfs
                .projection_ledger_identity()
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            let mut images = Vec::with_capacity(attributes.len());
            let mut projection_bytes = 0_u64;
            let mut row_count = 0_u64;
            for attribute in &attributes {
                let candidate = inner
                    .fact_storage
                    .build_current_projection_candidate(attribute, valid_time_floor)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
                inner
                    .fact_storage
                    .validate_current_projection_candidate(&candidate)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
                let image = crate::storage::current_projection_image::encode(&candidate, identity)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
                projection_bytes = projection_bytes
                    .checked_add(image.padded_bytes())
                    .ok_or_else(|| JsValue::from_str("current projection byte count overflow"))?;
                if projection_bytes > budget {
                    return Err(JsValue::from_str(&format!(
                        "current projection images require {projection_bytes} bytes, exceeding the {budget}-byte maintenance budget"
                    )));
                }
                row_count = row_count
                    .checked_add(image.row_count())
                    .ok_or_else(|| JsValue::from_str("current projection row count overflow"))?;
                images.push(image);
            }
            (identity, images, projection_bytes, row_count)
        };

        let image_refs = attributes
            .iter()
            .zip(&images)
            .map(|(attribute, image)| (attribute.as_str(), valid_time_floor, image))
            .collect::<Vec<_>>();
        let plan = self
            .inner
            .borrow()
            .pfs
            .plan_current_projection_publication(&image_refs)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let receipt = plan.receipt();
        idb.write_pages(plan.into_pages()).await?;

        // Reopen only after the page-0 CAS transaction commits. This validates
        // the same v13 catalog selection used by native open before the live
        // handle can observe it.
        let mode = self.inner.borrow().open_mode;
        let (reopened, paged) = open_persistent_storage(&idb, mode).await?;
        let reopened_storage = reopened.storage().clone();
        let mut inner = self.inner.borrow_mut();
        inner.pfs = reopened;
        inner.fact_storage = reopened_storage;
        inner.paged = paged;
        inner.projection_tail_cache.clear();
        drop(inner);

        Ok(serde_json::json!({
            "checkpoint": "noop",
            "generation": receipt.generation,
            "base_generation": identity.base_generation(),
            "manifest_generation": identity.manifest_generation(),
            "tx_count": identity.tx_count(),
            "valid_time_floor": valid_time_floor,
            "attribute_count": attributes.len(),
            "row_count": row_count,
            "projection_bytes": projection_bytes,
            "before_pages": before_pages,
            "after_pages": receipt.published_page_count,
            "arena_reused": receipt.arena_reused,
        })
        .to_string())
    }

    /// Serialise the current database to a portable `.graph` blob.
    ///
    /// The blob is byte-for-bit compatible with native `.graph` files opened by
    /// `Minigraf::open()`. Pages are always in ascending `page_id` order.
    ///
    /// Call `checkpoint()` on native before importing a file here to ensure
    /// no WAL entries are missing from the main file.
    #[wasm_bindgen(js_name = exportGraph)]
    pub fn export_graph(&self) -> Result<js_sys::Uint8Array, JsValue> {
        self.ensure_usable()?;
        let inner = self.inner.borrow();
        let page_count_u64 = inner
            .pfs
            .with_backend(BrowserBufferBackend::exportable_page_count)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let page_count = usize::try_from(page_count_u64)
            .map_err(|_| JsValue::from_str("published page count exceeds addressable memory"))?;
        let capacity = page_count
            .checked_mul(crate::storage::PAGE_SIZE)
            .ok_or_else(|| JsValue::from_str("published graph size exceeds addressable memory"))?;

        let mut blob = Vec::with_capacity(capacity);
        for id in 0..page_count_u64 {
            let page = inner
                .pfs
                .read_published_page(id)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            blob.extend_from_slice(&page);
        }
        Ok(js_sys::Uint8Array::from(blob.as_slice()))
    }

    /// Asynchronously serialise the current published image without requiring
    /// every IndexedDB page to remain resident in WebAssembly memory.
    ///
    /// Each immutable base page passes through the loaded v11 generation/page
    /// checksum catalog. If another handle publishes a newer image while this
    /// paged handle is reading, the pinned page-0 authority rejects the export
    /// instead of mixing generations.
    #[wasm_bindgen(js_name = exportGraphAsync)]
    pub async fn export_graph_async(&self) -> Result<js_sys::Uint8Array, JsValue> {
        self.ensure_usable()?;
        let paged = self.inner.borrow().paged;
        if !paged {
            return self.export_graph();
        }

        let guarded = self.begin_paged_read()?;
        let result = self.export_graph_from_idb().await;
        if guarded {
            self.finish_paged_read();
        }
        result
    }

    async fn export_graph_from_idb(&self) -> Result<js_sys::Uint8Array, JsValue> {
        let (idb, mut verifier) = {
            let inner = self.inner.borrow();
            let verifier = inner
                .pfs
                .begin_browser_export_verification()
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            let idb = inner
                .idb
                .as_ref()
                .ok_or_else(|| JsValue::from_str("paged export is missing its IndexedDB source"))?
                .clone_handle();
            (idb, verifier)
        };
        let page_count = verifier.published_page_count();

        let capacity = usize::try_from(page_count)
            .ok()
            .and_then(|count| count.checked_mul(crate::storage::PAGE_SIZE))
            .ok_or_else(|| JsValue::from_str("published graph size exceeds addressable memory"))?;
        let mut blob = Vec::with_capacity(capacity);
        let mut start = 0u64;
        while start < page_count {
            let count = (page_count - start).min(BROWSER_IDB_BATCH_PAGES);
            let pages = idb.load_page_range(start, count).await?;
            {
                let inner = self.inner.borrow();
                inner
                    .pfs
                    .verify_browser_export_batch(&mut verifier, &pages)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
            }
            for (_, page) in pages {
                blob.extend_from_slice(&page);
            }
            start = start
                .checked_add(count)
                .ok_or_else(|| JsValue::from_str("published graph page range overflow"))?;
        }
        verifier
            .finish()
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(js_sys::Uint8Array::from(blob.as_slice()))
    }

    /// Replace the current database with a `.graph` blob.
    ///
    /// The blob must be a checkpointed native `.graph` file (no pending WAL sidecar).
    /// The import is **atomic**: the durable replacement commits in a single
    /// IndexedDB transaction — clearing any stale pages left by a previously
    /// larger database — *before* the live handle switches to the new data.
    /// On any error (invalid blob, IndexedDB failure) neither the queryable
    /// state nor the durable state is modified.
    ///
    /// Every operation through this handle is rejected while import is in
    /// flight, so no query or export can observe an unacknowledged state.
    /// Cross-handle and cross-tab exclusion remains caller policy; Vetch
    /// should hold its per-database Web Lock for the writing handle's full
    /// lifetime so a stale second handle cannot publish after replacement.
    #[wasm_bindgen(js_name = importGraph)]
    pub async fn import_graph(&self, data: js_sys::Uint8Array) -> Result<(), JsValue> {
        self.import_graph_with_policy(data, BrowserImportPolicy::RecoveryCompatible)
            .await
    }

    /// Atomically import a `.graph` blob only when the post-migration candidate
    /// is ready for a fresh bounded `BrowserDb.openPaged()`.
    ///
    /// Complete legacy graphs are migrated to the current format before this
    /// check. A physically truncated legacy image that normal `importGraph()`
    /// may retain through previous-manifest recovery is rejected before any
    /// IndexedDB or live-handle replacement. Use this boundary for authority
    /// cutovers that will immediately reopen through `openPaged()`.
    #[wasm_bindgen(js_name = importGraphForPagedAccess)]
    pub async fn import_graph_for_paged_access(
        &self,
        data: js_sys::Uint8Array,
    ) -> Result<(), JsValue> {
        self.import_graph_with_policy(data, BrowserImportPolicy::RequirePagedReady)
            .await
    }

    async fn import_graph_with_policy(
        &self,
        data: js_sys::Uint8Array,
        policy: BrowserImportPolicy,
    ) -> Result<(), JsValue> {
        self.begin_mutation()?;
        let result = self.import_graph_inner(data, policy).await;
        // Import builds and durably replaces before swapping live state, so a
        // rejected import leaves both sides aligned and does not poison the
        // handle.
        self.finish_mutation(false);
        result
    }

    async fn import_graph_inner(
        &self,
        data: js_sys::Uint8Array,
        policy: BrowserImportPolicy,
    ) -> Result<(), JsValue> {
        let bytes = data.to_vec();
        if bytes.len() < crate::storage::PAGE_SIZE {
            return Err(JsValue::from_str(
                "import data is shorter than one complete .graph header page",
            ));
        }

        // Native open treats a partial physical tail as an interrupted
        // unpublished candidate and lets manifest validation fall back to the
        // previous committed state. Feed only complete pages into the same PFS
        // recovery logic so browser import has the identical policy. A partial
        // selected state with no valid predecessor still fails below.
        let complete_len = bytes.len() - (bytes.len() % crate::storage::PAGE_SIZE);

        let mut pages = HashMap::new();
        let complete_bytes = bytes
            .get(..complete_len)
            .ok_or_else(|| JsValue::from_str("import complete-page range is invalid"))?;
        for (i, chunk) in complete_bytes.chunks(crate::storage::PAGE_SIZE).enumerate() {
            let page_id =
                u64::try_from(i).map_err(|_| JsValue::from_str("import page id exceeds u64"))?;
            pages.insert(page_id, chunk.to_vec());
        }

        // Build the replacement storage locally — no live state is touched yet,
        // so any parse/validation failure leaves the database unchanged.
        let buffer = BrowserBufferBackend::load_pages_all_dirty(pages);
        let mut new_pfs = PersistentFactStorage::new(buffer, 256)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        new_pfs
            .with_backend_mut(BrowserBufferBackend::retain_declared_prefix)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let new_fact_storage = new_pfs.storage().clone();

        // Flush the complete post-construction page set (construction may have
        // written extra pages via format migration) and drain the dirty set so
        // the next checkpoint() does not re-flush the whole database.
        let full_pages = new_pfs.with_backend(|b| b.all_pages());
        new_pfs.with_backend_mut(|b| {
            b.take_dirty();
        });

        let open_mode = self.inner.borrow().open_mode;
        let imported_version = new_pfs
            .with_backend(|backend| backend.read_page_raw(0))
            .and_then(|page0| crate::storage::FileHeader::from_bytes(&page0))
            .map_err(|error| JsValue::from_str(&error.to_string()))?
            .version;

        if policy == BrowserImportPolicy::RequirePagedReady {
            if imported_version < crate::storage::INTEGRITY_FORMAT_VERSION
                || imported_version > crate::storage::MAX_READABLE_FORMAT_VERSION
            {
                return Err(JsValue::from_str(&format!(
                    "strict paged import requires a paged-ready v{}..=v{} format, but recovery selected v{imported_version}",
                    crate::storage::INTEGRITY_FORMAT_VERSION,
                    crate::storage::MAX_READABLE_FORMAT_VERSION,
                )));
            }
            validate_sparse_authority(&new_pfs).map_err(|error| {
                JsValue::from_str(&format!(
                    "strict paged import candidate is not openPaged-ready: {}",
                    js_value_message(&error),
                ))
            })?;
        }

        let new_paged = if open_mode.is_paged()
            && imported_version >= crate::storage::INTEGRITY_FORMAT_VERSION
            && imported_version <= crate::storage::MAX_READABLE_FORMAT_VERSION
        {
            configure_sparse_authority(&mut new_pfs)?;
            new_pfs.with_backend_mut(BrowserBufferBackend::evict_all_clean_unpinned);
            true
        } else if open_mode.is_paged()
            && imported_version
                == crate::storage::header_extension::LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION
        {
            // A physically truncated v10 image may legitimately recover the
            // previous manifest but cannot gain v11 per-page integrity without
            // filling published holes. Preserve that exact read-only recovery
            // state eagerly; a later clean import/maintenance can return this
            // handle to sparse mode because `open_mode` remains Paged.
            false
        } else if open_mode.is_paged() {
            return Err(JsValue::from_str(&format!(
                "openPaged import cannot represent recovered format v{imported_version} as bounded authority",
            )));
        } else {
            false
        };

        // Durable replace commits BEFORE the live handle switches. If it fails,
        // the single IDB transaction rolls back and the live state was never
        // touched, so memory and IndexedDB stay consistent on the old database.
        let idb = self
            .inner
            .borrow()
            .idb
            .as_ref()
            .map(IndexedDbBackend::clone_handle);
        if let Some(idb) = idb {
            idb.replace_all_pages(full_pages).await?;
        }

        let mut inner = self.inner.borrow_mut();
        inner.pfs = new_pfs;
        inner.fact_storage = new_fact_storage;
        inner.paged = new_paged;
        inner.projection_tail_cache.clear();
        Ok(())
    }
}

#[wasm_bindgen]
impl BrowserReadView {
    /// Transaction cursor pinned by this view.
    #[wasm_bindgen(getter, js_name = txCursor)]
    pub fn tx_cursor(&self) -> u64 {
        self.tx_count
    }

    /// Execute one selective query and return a complete JSON result.
    ///
    /// `max_rows` bounds both the complete result and conservative execution
    /// work across source visits, bindings, branches, and aggregate inputs, so
    /// a query may reject even when its final projection would fit. Both limits
    /// are mandatory. Oversized results are rejected, never truncated, and
    /// queries without an indexed seed are rejected before I/O.
    pub async fn query(
        &self,
        datalog: String,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<String, JsValue> {
        #[cfg(any(test, feature = "bench-internals"))]
        crate::storage::current_projection_image::reset_projection_read_diagnostics();
        if max_bytes == 0 || max_bytes > BROWSER_READ_VIEW_MAX_RESULT_BYTES {
            return Err(JsValue::from_str(&format!(
                "read-view max_bytes must be in 1..={BROWSER_READ_VIEW_MAX_RESULT_BYTES}; got {max_bytes}"
            )));
        }
        let command = crate::db::prepare_read_view_query(
            &datalog,
            self.tx_count,
            self.valid_at.clone(),
            max_rows,
        )
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let db = BrowserDb {
            inner: Rc::clone(&self.inner),
        };
        db.ensure_usable()?;
        let guarded = db.begin_paged_read()?;
        let result = db.execute_read_view_command(command).await;
        if guarded {
            db.finish_paged_read();
        }
        let result = result?;
        if let QueryResult::QueryResults { results, .. } = &result
            && results.len() > max_rows
        {
            return Err(JsValue::from_str(&format!(
                "read-view result has {} rows, exceeding max_rows {max_rows}; result was rejected without truncation",
                results.len()
            )));
        }
        let json = query_result_to_json(result);
        if json.len() > max_bytes {
            return Err(JsValue::from_str(&format!(
                "read-view result has {} bytes, exceeding max_bytes {max_bytes}; result was rejected without truncation",
                json.len()
            )));
        }
        Ok(json)
    }

    /// Read exact current entity/attribute values as structured JavaScript rows.
    ///
    /// Entity and attribute order are preserved after first-occurrence
    /// deduplication. Oversized or over-budget reads reject without truncation.
    #[wasm_bindgen(js_name = currentEntities)]
    pub async fn current_entities(
        &self,
        ids: Vec<String>,
        attributes: Vec<String>,
        limit: usize,
    ) -> Result<js_sys::Array, JsValue> {
        let parsed_ids = ids
            .iter()
            .map(|id| {
                crate::EntityId::parse_str(id).map_err(|error| {
                    JsValue::from_str(&format!("current_entities invalid entity id {id}: {error}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let attribute_refs = attributes.iter().map(String::as_str).collect::<Vec<_>>();
        let (ids, attributes) =
            crate::db::normalize_current_entities_request(&parsed_ids, &attribute_refs, limit)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;

        let db = BrowserDb {
            inner: Rc::clone(&self.inner),
        };
        db.ensure_usable()?;
        let guarded = db.begin_paged_read()?;
        let result = self
            .read_current_entities(&db, ids, attributes, limit)
            .await;
        if guarded {
            db.finish_paged_read();
        }
        let facts = result?;
        current_facts_to_js(facts)
    }

    /// Read current source entities that reference one target through one exact attribute.
    #[wasm_bindgen(js_name = refsTo)]
    pub async fn refs_to(
        &self,
        attribute: String,
        value: String,
        limit: usize,
    ) -> Result<js_sys::Array, JsValue> {
        if !attribute.starts_with(':') || !attribute.contains('/') || attribute.contains('\0') {
            return Err(JsValue::from_str(&format!(
                "current_refs attribute must be namespace-qualified: {attribute}"
            )));
        }
        if limit == 0 || limit > crate::db::READ_VIEW_MAX_ROWS {
            return Err(JsValue::from_str(&format!(
                "current_refs limit must be in 1..={}; got {limit}",
                crate::db::READ_VIEW_MAX_ROWS
            )));
        }
        let target = crate::EntityId::parse_str(&value).map_err(|error| {
            JsValue::from_str(&format!("current_refs invalid target id {value}: {error}"))
        })?;
        let db = BrowserDb {
            inner: Rc::clone(&self.inner),
        };
        db.ensure_usable()?;
        let guarded = db.begin_paged_read()?;
        let result = self.read_current_refs(&db, &attribute, target, limit).await;
        if guarded {
            db.finish_paged_read();
        }
        let sources = result?;
        let bytes = sources.len().saturating_mul(36);
        if bytes > BROWSER_READ_VIEW_MAX_RESULT_BYTES {
            return Err(JsValue::from_str(&format!(
                "current_refs result exceeds {BROWSER_READ_VIEW_MAX_RESULT_BYTES} bytes; result was rejected without truncation"
            )));
        }
        let result = js_sys::Array::new();
        for source in sources {
            result.push(&JsValue::from_str(&source.to_string()));
        }
        Ok(result)
    }
}

impl BrowserReadView {
    async fn read_current_entities(
        &self,
        db: &BrowserDb,
        ids: Vec<crate::EntityId>,
        attributes: Vec<String>,
        limit: usize,
    ) -> Result<Vec<crate::db::CurrentFact>, JsValue> {
        let as_of = crate::query::datalog::types::AsOf::Counter(self.tx_count);
        let valid_time = match self.valid_at {
            ValidAt::Timestamp(timestamp) => crate::graph::storage::CurrentValidTime::At(timestamp),
            ValidAt::AnyValidTime => crate::graph::storage::CurrentValidTime::Any,
            ValidAt::Slot(_) => {
                return Err(JsValue::from_str(
                    "internal: browser read view retained a valid-time slot",
                ));
            }
        };
        let mut history_entries = 0usize;
        let mut facts = Vec::new();

        for entity in ids {
            for attribute in &attributes {
                let mut cursor = self
                    .inner
                    .borrow()
                    .fact_storage
                    .current_entity_attribute_cursor(entity, attribute, Some(&as_of), valid_time)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
                loop {
                    let remaining = crate::db::CURRENT_ENTITIES_MAX_HISTORY_ENTRIES
                        .saturating_sub(history_entries);
                    if remaining == 0 {
                        return Err(JsValue::from_str(&format!(
                            "current_entities history work exceeds {} entries; use raw Datalog or maintenance context",
                            crate::db::CURRENT_ENTITIES_MAX_HISTORY_ENTRIES
                        )));
                    }
                    let mut values = Vec::new();
                    let step = self
                        .inner
                        .borrow()
                        .fact_storage
                        .step_current_entity_attribute_cursor(
                            &mut cursor,
                            crate::db::CURRENT_ENTITIES_STEP_ENTRIES.min(remaining),
                            &mut |value| {
                                values.push(value.clone());
                                Ok(())
                            },
                        );
                    match step {
                        Ok(step) => {
                            let (entries, complete) = match step {
                                crate::graph::storage::CurrentEntityAttributeStep::Yielded {
                                    entries,
                                } => (entries, false),
                                crate::graph::storage::CurrentEntityAttributeStep::Complete {
                                    entries,
                                } => (entries, true),
                            };
                            history_entries = history_entries.saturating_add(entries);
                            if facts.len().saturating_add(values.len()) > limit {
                                return Err(JsValue::from_str(&format!(
                                    "current_entities result exceeds limit {limit}; result was rejected without truncation"
                                )));
                            }
                            facts.extend(values.into_iter().map(|value| crate::db::CurrentFact {
                                entity,
                                attribute: attribute.clone(),
                                value,
                            }));
                            if complete {
                                break;
                            }
                            if history_entries >= crate::db::CURRENT_ENTITIES_MAX_HISTORY_ENTRIES {
                                return Err(JsValue::from_str(&format!(
                                    "current_entities history work exceeds {} entries; use raw Datalog or maintenance context",
                                    crate::db::CURRENT_ENTITIES_MAX_HISTORY_ENTRIES
                                )));
                            }
                            db.evict_aggregate_staging();
                            yield_browser_task().await?;
                        }
                        Err(error) => match page_not_resident_id(&error) {
                            Some(page_id) => {
                                db.fetch_and_stage_page(page_id).await?;
                                db.evict_aggregate_staging();
                            }
                            None => return Err(JsValue::from_str(&error.to_string())),
                        },
                    }
                }
            }
        }
        Ok(facts)
    }

    async fn read_current_refs(
        &self,
        db: &BrowserDb,
        attribute: &str,
        target: crate::EntityId,
        limit: usize,
    ) -> Result<Vec<crate::EntityId>, JsValue> {
        let as_of = crate::query::datalog::types::AsOf::Counter(self.tx_count);
        let valid_time = match self.valid_at {
            ValidAt::Timestamp(timestamp) => crate::graph::storage::CurrentValidTime::At(timestamp),
            ValidAt::AnyValidTime => crate::graph::storage::CurrentValidTime::Any,
            ValidAt::Slot(_) => {
                return Err(JsValue::from_str(
                    "internal: browser read view retained a valid-time slot",
                ));
            }
        };
        let mut cursor = self
            .inner
            .borrow()
            .fact_storage
            .current_refs_cursor(attribute, target, Some(&as_of), valid_time)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let mut history_entries = 0usize;
        let mut sources = Vec::new();
        loop {
            let remaining =
                crate::db::CURRENT_REFS_MAX_HISTORY_ENTRIES.saturating_sub(history_entries);
            if remaining == 0 {
                return Err(JsValue::from_str(&format!(
                    "current_refs history work exceeds {} entries; use raw Datalog or maintenance context",
                    crate::db::CURRENT_REFS_MAX_HISTORY_ENTRIES
                )));
            }
            let mut emitted = Vec::new();
            let step = self.inner.borrow().fact_storage.step_current_refs_cursor(
                &mut cursor,
                crate::db::CURRENT_REFS_STEP_ENTRIES.min(remaining),
                &mut |source| {
                    emitted.push(source);
                    Ok(())
                },
            );
            match step {
                Ok(step) => {
                    let (entries, complete) = match step {
                        crate::graph::storage::CurrentRefsStep::Yielded { entries } => {
                            (entries, false)
                        }
                        crate::graph::storage::CurrentRefsStep::Complete { entries } => {
                            (entries, true)
                        }
                    };
                    history_entries = history_entries.saturating_add(entries);
                    if sources.len().saturating_add(emitted.len()) > limit {
                        return Err(JsValue::from_str(&format!(
                            "current_refs result exceeds limit {limit}; result was rejected without truncation"
                        )));
                    }
                    sources.extend(emitted);
                    if complete {
                        return Ok(sources);
                    }
                    if history_entries >= crate::db::CURRENT_REFS_MAX_HISTORY_ENTRIES {
                        return Err(JsValue::from_str(&format!(
                            "current_refs history work exceeds {} entries; use raw Datalog or maintenance context",
                            crate::db::CURRENT_REFS_MAX_HISTORY_ENTRIES
                        )));
                    }
                    db.evict_aggregate_staging();
                    yield_browser_task().await?;
                }
                Err(error) => match page_not_resident_id(&error) {
                    Some(page_id) => {
                        db.fetch_and_stage_page(page_id).await?;
                        db.evict_aggregate_staging();
                    }
                    None => return Err(JsValue::from_str(&error.to_string())),
                },
            }
        }
    }
}

fn current_facts_to_js(facts: Vec<crate::db::CurrentFact>) -> Result<js_sys::Array, JsValue> {
    let rows = js_sys::Array::new();
    let mut result_bytes = 0usize;
    for fact in facts {
        let value_json = to_tagged_json(&fact.value).to_string();
        result_bytes = result_bytes
            .saturating_add(36)
            .saturating_add(fact.attribute.len())
            .saturating_add(value_json.len());
        if result_bytes > BROWSER_READ_VIEW_MAX_RESULT_BYTES {
            return Err(JsValue::from_str(&format!(
                "current_entities result exceeds {BROWSER_READ_VIEW_MAX_RESULT_BYTES} bytes; result was rejected without truncation"
            )));
        }
        let row = js_sys::Object::new();
        js_sys::Reflect::set(
            &row,
            &JsValue::from_str("entity"),
            &JsValue::from_str(&fact.entity.to_string()),
        )?;
        js_sys::Reflect::set(
            &row,
            &JsValue::from_str("attribute"),
            &JsValue::from_str(&fact.attribute),
        )?;
        let value = js_sys::JSON::parse(&value_json)?;
        js_sys::Reflect::set(&row, &JsValue::from_str("value"), &value)?;
        rows.push(&row);
    }
    Ok(rows)
}

impl BrowserDb {
    fn create_read_view(
        &self,
        as_of: Option<u64>,
        valid_at: ValidAt,
    ) -> Result<BrowserReadView, JsValue> {
        self.ensure_usable()?;
        let current = self.inner.borrow().fact_storage.current_tx_count();
        let tx_count = as_of.unwrap_or(current);
        if tx_count > current {
            return Err(JsValue::from_str(&format!(
                "read-view as_of {tx_count} is newer than current transaction {current}"
            )));
        }
        Ok(BrowserReadView {
            inner: Rc::clone(&self.inner),
            tx_count,
            valid_at,
        })
    }

    fn ensure_usable(&self) -> Result<(), JsValue> {
        let inner = self.inner.borrow();
        if inner.durability_poisoned {
            return Err(JsValue::from_str(
                "BrowserDb durability state is uncertain after a failed write; discard this handle and reopen",
            ));
        }
        if inner.mutation_in_flight {
            return Err(JsValue::from_str(
                "BrowserDb mutation is awaiting durability; await it before querying or exporting from this handle",
            ));
        }
        if inner.paged_read_in_flight {
            return Err(JsValue::from_str(
                "BrowserDb paged read is awaiting IndexedDB; await it before starting another operation on this handle",
            ));
        }
        Ok(())
    }

    /// Mark an async paged read in flight. Eager/in-memory reads stay entirely
    /// synchronous and need no guard because JavaScript cannot interleave them.
    fn begin_paged_read(&self) -> Result<bool, JsValue> {
        let mut inner = self.inner.borrow_mut();
        if !inner.paged {
            return Ok(false);
        }
        if inner.durability_poisoned {
            return Err(JsValue::from_str(
                "BrowserDb durability state is uncertain after a failed write; discard this handle and reopen",
            ));
        }
        if inner.mutation_in_flight || inner.paged_read_in_flight {
            return Err(JsValue::from_str(
                "BrowserDb operation already in progress; await it before starting a paged read",
            ));
        }
        inner.paged_read_in_flight = true;
        Ok(true)
    }

    fn finish_paged_read(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.paged_read_in_flight = false;
        inner
            .pfs
            .with_backend_mut(BrowserBufferBackend::evict_all_clean_unpinned);
    }

    fn begin_mutation(&self) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        if inner.durability_poisoned {
            return Err(JsValue::from_str(
                "BrowserDb durability state is uncertain after a failed write; discard this handle and reopen",
            ));
        }
        if inner.mutation_in_flight {
            return Err(JsValue::from_str(
                "BrowserDb mutation already in progress; await it before starting another write, checkpoint, import, or maintenance call",
            ));
        }
        if inner.paged_read_in_flight {
            return Err(JsValue::from_str(
                "BrowserDb paged read is awaiting IndexedDB; await it before starting a mutation",
            ));
        }
        inner.mutation_in_flight = true;
        inner.mutation_failure_rolled_back = false;
        inner.mutation_advanced = false;
        Ok(())
    }

    fn finish_mutation(&self, failed: bool) {
        let mut inner = self.inner.borrow_mut();
        inner.mutation_in_flight = false;
        inner.durability_poisoned |=
            failed && inner.mutation_advanced && !inner.mutation_failure_rolled_back;
        inner.mutation_failure_rolled_back = false;
        inner.mutation_advanced = false;
    }

    async fn execute_read_command(&self, command: DatalogCommand) -> Result<QueryResult, JsValue> {
        let demand_batch_pages = match &command {
            DatalogCommand::Query(query) => {
                let plan = QueryAccessPlan::for_query(query);
                if plan.is_full_scan() {
                    self.prefetch_full_scan_pages().await?;
                }
                plan.browser_demand_batch_pages()
            }
            _ => 1,
        };

        let mut aggregate_session = match &command {
            DatalogCommand::Query(query) => {
                let inner = self.inner.borrow();
                DatalogExecutor::new_with_rules_and_functions(
                    inner.fact_storage.clone(),
                    inner.rules.clone(),
                    inner.functions.clone(),
                )
                .owned_attribute_aggregate_session(query)
                .map_err(|error| JsValue::from_str(&error.to_string()))?
            }
            _ => None,
        };

        let mut requested = HashSet::new();
        loop {
            let result = if let Some(session) = aggregate_session.as_mut() {
                match session.step(BROWSER_AGGREGATE_ENTRY_BUDGET) {
                    Ok(OwnedAggregateStep::Complete(result)) => return Ok(result),
                    Ok(OwnedAggregateStep::Yielded { entries }) => {
                        if entries == 0 {
                            return Err(JsValue::from_str(
                                "aggregate cursor yielded without advancing",
                            ));
                        }
                        self.evict_aggregate_staging();
                        yield_browser_task().await?;
                        continue;
                    }
                    Err(error) => Err(error),
                }
            } else {
                let inner = self.inner.borrow();
                DatalogExecutor::new_with_rules_and_functions(
                    inner.fact_storage.clone(),
                    inner.rules.clone(),
                    inner.functions.clone(),
                )
                .execute(command.clone())
            };
            match result {
                Ok(result) => return Ok(result),
                Err(error) => match page_not_resident_id(&error) {
                    Some(page_id) => {
                        if aggregate_session.is_none() && !requested.insert(page_id) {
                            return Err(JsValue::from_str(&format!(
                                "paged query requested page {page_id} again after it was staged"
                            )));
                        }
                        self.fetch_and_stage_page_window(page_id, demand_batch_pages)
                            .await?;
                        if aggregate_session.is_some() {
                            self.evict_aggregate_staging();
                        }
                    }
                    None => return Err(JsValue::from_str(&error.to_string())),
                },
            }
        }
    }

    async fn execute_read_view_command(
        &self,
        command: DatalogCommand,
    ) -> Result<QueryResult, JsValue> {
        if let DatalogCommand::Query(query) = &command
            && let Some(result) = self.try_execute_projected_read_view_query(query).await?
        {
            return Ok(result);
        }
        self.execute_read_command(command).await
    }

    async fn try_execute_projected_read_view_query(
        &self,
        query: &crate::query::datalog::types::DatalogQuery,
    ) -> Result<Option<QueryResult>, JsValue> {
        let (executor, reader, captured_watermark) = {
            let inner = self.inner.borrow();
            (
                DatalogExecutor::new_with_rules_and_functions(
                    inner.fact_storage.clone(),
                    inner.rules.clone(),
                    inner.functions.clone(),
                ),
                inner.pfs.projection_reader(),
                inner.fact_storage.current_projection_watermark(),
            )
        };
        let Some(eligibility) = executor
            .projected_attribute_aggregate_session(query)
            .map_err(|error| JsValue::from_str(&error.to_string()))?
        else {
            return Ok(None);
        };
        #[cfg(any(test, feature = "bench-internals"))]
        crate::storage::current_projection_image::note_projection_route_attempt();
        if captured_watermark.1 != eligibility.tx_count() {
            #[cfg(any(test, feature = "bench-internals"))]
            crate::storage::current_projection_image::note_projection_ledger_fallback();
            return Ok(None);
        }
        let descriptors = reader.scan_descriptors(
            eligibility.attribute(),
            eligibility.tx_count(),
            eligibility.valid_at(),
        );
        for descriptor in descriptors {
            let Some(mut aggregate) = executor
                .projected_attribute_aggregate_session(query)
                .map_err(|error| JsValue::from_str(&error.to_string()))?
            else {
                return Ok(None);
            };
            let tail = if descriptor.identity.tx_count() < eligibility.tx_count() {
                let admission = loop {
                    let prepared = {
                        let mut inner = self.inner.borrow_mut();
                        let storage = inner.fact_storage.clone();
                        inner.projection_tail_cache.prepare(
                            &storage,
                            descriptor.identity,
                            &descriptor.attribute,
                            descriptor.valid_time_floor,
                            eligibility.tx_count(),
                        )
                    };
                    match prepared {
                        Ok(admission) => break admission,
                        Err(error) => match page_not_resident_id(&error) {
                            Some(page_id) => {
                                self.fetch_and_stage_page_window(page_id, BROWSER_IDB_BATCH_PAGES)
                                    .await?;
                                yield_browser_task().await?;
                            }
                            None => return Err(JsValue::from_str(&error.to_string())),
                        },
                    }
                };
                match admission {
                    crate::graph::current_projection::CurrentProjectionTailAdmission::Ready {
                        overlay,
                        diagnostics,
                    } => {
                        #[cfg(any(test, feature = "bench-internals"))]
                        crate::storage::current_projection_image::note_projection_tail(
                            diagnostics,
                            &overlay,
                        );
                        aggregate
                            .note_scanned_rows(
                                diagnostics
                                    .tail_facts
                                    .saturating_add(diagnostics.history_entries),
                            )
                            .map_err(|error| JsValue::from_str(&error.to_string()))?;
                        Some(overlay)
                    }
                    crate::graph::current_projection::CurrentProjectionTailAdmission::OverBudget => {
                        #[cfg(any(test, feature = "bench-internals"))]
                        crate::storage::current_projection_image::note_projection_tail_budget_fallback();
                        continue;
                    }
                }
            } else {
                None
            };
            let mut scan = loop {
                let mut read_page = |page_id| reader.read_page(page_id);
                match crate::storage::current_projection_image::CurrentProjectionScan::open(
                    descriptor.clone(),
                    &mut read_page,
                ) {
                    Ok(scan) => break Some(scan),
                    Err(error) => match page_not_resident_id(&error) {
                        Some(page_id) => {
                            self.fetch_and_stage_page_window(page_id, BROWSER_IDB_BATCH_PAGES)
                                .await?;
                        }
                        None => {
                            #[cfg(any(test, feature = "bench-internals"))]
                            crate::storage::current_projection_image::note_projection_corrupt_candidate();
                            break None;
                        }
                    },
                }
            };
            let Some(mut scan) = scan.take() else {
                continue;
            };
            loop {
                let remaining = aggregate.remaining_source_budget();
                if remaining == 0 {
                    aggregate
                        .note_scanned_rows(1)
                        .map_err(|error| JsValue::from_str(&error.to_string()))?;
                }
                let mut semantic_error = None;
                let mut read_page = |page_id| reader.read_page(page_id);
                let scan_started = scan.rows_scanned();
                let step = scan.step(
                    remaining.min(BROWSER_AGGREGATE_ENTRY_BUDGET),
                    aggregate.valid_at(),
                    &mut read_page,
                    &mut |entity, encoded| {
                        if tail
                            .as_ref()
                            .is_some_and(|overlay| overlay.contains_entity(entity))
                        {
                            #[cfg(any(test, feature = "bench-internals"))]
                            crate::storage::current_projection_image::note_projection_base_row_suppressed();
                        } else if semantic_error.is_none()
                            && let Err(error) = aggregate.push_encoded(entity, encoded)
                        {
                            semantic_error = Some(error);
                        }
                        Ok(())
                    },
                );
                if let Some(error) = semantic_error {
                    return Err(JsValue::from_str(&error.to_string()));
                }
                let step = match step {
                    Ok(step) => step,
                    Err(error) => match page_not_resident_id(&error) {
                        Some(page_id) => {
                            aggregate
                                .note_scanned_rows(scan.rows_scanned().saturating_sub(scan_started))
                                .map_err(|error| JsValue::from_str(&error.to_string()))?;
                            self.fetch_and_stage_page_window(page_id, BROWSER_IDB_BATCH_PAGES)
                                .await?;
                            continue;
                        }
                        None => {
                            #[cfg(any(test, feature = "bench-internals"))]
                            crate::storage::current_projection_image::note_projection_corrupt_candidate();
                            break;
                        }
                    },
                };
                let (rows, complete) = match step {
                    crate::storage::current_projection_image::CurrentProjectionScanStep::Yielded {
                        rows,
                    } => (rows, false),
                    crate::storage::current_projection_image::CurrentProjectionScanStep::Complete {
                        rows,
                    } => (rows, true),
                };
                aggregate
                    .note_scanned_rows(rows)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
                if complete {
                    if let Some(overlay) = &tail {
                        let mut semantic_error = None;
                        let overlay_rows = overlay
                            .visit_at(aggregate.valid_at(), &mut |entity, encoded| {
                                #[cfg(any(test, feature = "bench-internals"))]
                                crate::storage::current_projection_image::note_projection_overlay_row_emitted();
                                if semantic_error.is_none()
                                    && let Err(error) = aggregate.push_encoded(entity, encoded)
                                {
                                    semantic_error = Some(error);
                                }
                                Ok(())
                            })
                            .map_err(|error| JsValue::from_str(&error.to_string()))?;
                        if let Some(error) = semantic_error {
                            return Err(JsValue::from_str(&error.to_string()));
                        }
                        aggregate
                            .note_scanned_rows(overlay_rows)
                            .map_err(|error| JsValue::from_str(&error.to_string()))?;
                    }
                    let watermark = self
                        .inner
                        .borrow()
                        .fact_storage
                        .current_projection_watermark();
                    if watermark != captured_watermark {
                        #[cfg(any(test, feature = "bench-internals"))]
                        crate::storage::current_projection_image::note_projection_ledger_fallback();
                        return Ok(None);
                    }
                    return aggregate
                        .finish()
                        .map(Some)
                        .map_err(|error| JsValue::from_str(&error.to_string()));
                }
                if rows == 0 {
                    break;
                }
                self.evict_aggregate_staging();
                yield_browser_task().await?;
            }
        }
        #[cfg(any(test, feature = "bench-internals"))]
        crate::storage::current_projection_image::note_projection_ledger_fallback();
        Ok(None)
    }

    async fn fetch_and_stage_page(&self, page_id: u64) -> Result<(), JsValue> {
        let idb = self
            .inner
            .borrow()
            .idb
            .as_ref()
            .ok_or_else(|| JsValue::from_str("paged read is missing its IndexedDB source"))?
            .clone_handle();
        let page = idb.load_page(page_id).await?;
        self.verify_and_stage_pages(vec![(page_id, page)])
    }

    async fn fetch_and_stage_page_window(
        &self,
        required_page: u64,
        batch_pages: u64,
    ) -> Result<(), JsValue> {
        if batch_pages <= 1 {
            return self.fetch_and_stage_page(required_page).await;
        }
        let (idb, logical_page_count) = {
            let inner = self.inner.borrow();
            let idb = inner
                .idb
                .as_ref()
                .ok_or_else(|| JsValue::from_str("paged read is missing its IndexedDB source"))?
                .clone_handle();
            let logical_page_count = inner
                .pfs
                .with_backend(BrowserBufferBackend::page_count_raw)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            (idb, logical_page_count)
        };
        let start = required_page / batch_pages * batch_pages;
        let count = logical_page_count.saturating_sub(start).min(batch_pages);
        let pages = idb.load_page_window(required_page, start, count).await?;

        let mut verified = Vec::with_capacity(pages.len());
        for (page_id, page) in pages {
            let verification = self
                .inner
                .borrow()
                .pfs
                .verify_browser_fetched_page(page_id, &page);
            match verification {
                Ok(()) => verified.push((page_id, page)),
                Err(error) if page_id == required_page => {
                    return Err(JsValue::from_str(&error.to_string()));
                }
                Err(_) => {}
            }
        }
        if !verified
            .iter()
            .any(|(page_id, _)| *page_id == required_page)
        {
            return Err(JsValue::from_str(&format!(
                "required IndexedDB page {required_page} did not pass verification"
            )));
        }
        self.inner
            .borrow_mut()
            .pfs
            .with_backend_mut(|backend| backend.stage_clean_pages(verified))
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    fn verify_and_stage_pages(&self, pages: Vec<(u64, Vec<u8>)>) -> Result<(), JsValue> {
        {
            let inner = self.inner.borrow();
            for (page_id, page) in &pages {
                inner
                    .pfs
                    .verify_browser_fetched_page(*page_id, page)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
            }
        }
        self.inner
            .borrow_mut()
            .pfs
            .with_backend_mut(|backend| backend.stage_clean_pages(pages))
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    async fn prefetch_full_scan_pages(&self) -> Result<(), JsValue> {
        let (idb, range) = {
            let inner = self.inner.borrow();
            if !inner.paged {
                return Ok(());
            }
            let logical_page_count = inner
                .pfs
                .with_backend(BrowserBufferBackend::page_count_raw)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            if logical_page_count == 0 {
                return Ok(());
            }
            let range = inner
                .pfs
                .browser_base_fact_range()
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            let idb = inner
                .idb
                .as_ref()
                .ok_or_else(|| JsValue::from_str("paged scan is missing its IndexedDB source"))?
                .clone_handle();
            (idb, range)
        };

        let mut start = range.start_page();
        while start < range.end_page() {
            let count = (range.end_page() - start).min(BROWSER_IDB_BATCH_PAGES);
            let pages = idb.load_page_range(start, count).await?;
            self.verify_and_stage_pages(pages)?;
            start = start
                .checked_add(count)
                .ok_or_else(|| JsValue::from_str("full-scan page range overflow"))?;
        }
        Ok(())
    }

    async fn prefetch_projection_source_pages(&self) -> Result<(), JsValue> {
        let (idb, range) = {
            let inner = self.inner.borrow();
            if !inner.paged {
                return Ok(());
            }
            let logical_page_count = inner
                .pfs
                .with_backend(BrowserBufferBackend::page_count_raw)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            if logical_page_count == 0 {
                return Ok(());
            }
            let page0 = inner
                .pfs
                .with_backend(|backend| backend.read_page_raw(0))
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            let plan = BrowserV11BootstrapPlan::from_page0(&page0)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            let idb = inner
                .idb
                .as_ref()
                .ok_or_else(|| {
                    JsValue::from_str("projection rebuild is missing its IndexedDB source")
                })?
                .clone_handle();
            (idb, plan.base_covered_range())
        };

        let mut start = range.start_page();
        while start < range.end_page() {
            let count = (range.end_page() - start).min(BROWSER_IDB_BATCH_PAGES);
            let pages = idb.load_page_range(start, count).await?;
            self.verify_and_stage_pages(pages)?;
            start = start
                .checked_add(count)
                .ok_or_else(|| JsValue::from_str("projection source page range overflow"))?;
        }
        Ok(())
    }

    fn evict_sparse_staging(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.paged {
            inner
                .pfs
                .with_backend_mut(BrowserBufferBackend::evict_all_clean_unpinned);
        }
    }

    fn evict_aggregate_staging(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.paged {
            inner.pfs.with_backend_mut(|backend| {
                backend.evict_clean_unpinned_to(BROWSER_AGGREGATE_RESIDENT_PAGE_LIMIT);
            });
        }
    }

    async fn flush_dirty_pages_or_restore(
        &self,
        idb: IndexedDbBackend,
        pages: Vec<(u64, Vec<u8>)>,
    ) -> Result<(), JsValue> {
        let write_error = match idb.write_pages(pages).await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };

        // IndexedDB readwrite transactions are atomic. Reopen the previous
        // durable image through the same mode as the live handle. The paged
        // path deliberately reloads only v11 authority metadata, so a rejected
        // write on a 1M graph cannot regress to an O(total) recovery copy.
        let mode = self.inner.borrow().open_mode;
        let (restored, restored_paged) = open_persistent_storage(&idb, mode)
            .await
            .map_err(|recovery_error| {
                JsValue::from_str(&format!(
                    "IndexedDB write failed and previous durable graph could not be reopened: write={}; reopen={}",
                    js_value_message(&write_error),
                    js_value_message(&recovery_error),
                ))
            })?;
        let restored_storage = restored.storage().clone();

        let mut inner = self.inner.borrow_mut();
        inner.pfs = restored;
        inner.fact_storage = restored_storage;
        inner.paged = restored_paged;
        inner.projection_tail_cache.clear();
        inner.mutation_failure_rolled_back = true;
        drop(inner);
        Err(write_error)
    }

    /// Apply a batch of pre-materialized facts to the in-memory store and
    /// flush dirty pages to IndexedDB (if present).
    ///
    /// The `RefCell` borrow is fully released before the `.await` so that no
    /// borrow is held across the async boundary.
    async fn apply_write(
        &self,
        facts: Vec<crate::graph::types::Fact>,
        is_retract: bool,
    ) -> Result<String, JsValue> {
        use crate::db::VALID_FROM_USE_TX_TIME;
        use crate::graph::types::tx_id_now;

        // ── Sync section: hold borrow, do ALL sync work, collect owned data ──
        let (dirty_pages, result_json) = {
            let mut inner = self.inner.borrow_mut();

            inner.mutation_advanced = true;
            let tx_count = inner.fact_storage.allocate_tx_count();
            let tx_id = tx_id_now();

            let stamped: Vec<crate::graph::types::Fact> = facts
                .into_iter()
                .map(|mut f| {
                    f.tx_id = tx_id;
                    f.tx_count = tx_count;
                    if f.valid_from == VALID_FROM_USE_TX_TIME {
                        f.valid_from = tx_id.cast_signed();
                    }
                    f
                })
                .collect();

            for fact in &stamped {
                inner
                    .fact_storage
                    .load_fact(fact.clone())
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
            }

            inner.pfs.mark_dirty();
            inner
                .pfs
                .save()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            // Collect dirty pages as owned Vec<(u64, Vec<u8>)> — no borrows escape
            let dirty_pages = take_dirty_pages(&mut inner.pfs)?;
            if inner.paged {
                configure_sparse_authority(&mut inner.pfs)?;
            }

            let decision = inner.pfs.delta_maintenance_decision();
            let durability = if inner.idb.is_some() {
                "published"
            } else {
                "memory"
            };
            let mut json = serde_json::json!({
                "tx_id": tx_id,
                "tx_count": tx_count,
                "durability": durability,
                "maintenance_pending": !matches!(
                    decision,
                    DeltaMaintenanceDecision::ContinueDeltaAppend
                ),
                "advice": browser_write_advice(decision),
            });
            let result_key = if is_retract {
                "retracted"
            } else {
                "transacted"
            };
            let object = json
                .as_object_mut()
                .ok_or_else(|| JsValue::from_str("browser write result is not a JSON object"))?;
            object.insert(result_key.to_string(), serde_json::json!(tx_id));

            (dirty_pages, json.to_string())
        };
        // ── Borrow dropped here ───────────────────────────────────────────────

        // ── Async section: flush to IDB (no RefCell borrow held) ─────────────
        if !dirty_pages.is_empty() {
            let idb = self
                .inner
                .borrow()
                .idb
                .as_ref()
                .map(IndexedDbBackend::clone_handle);
            if let Some(idb) = idb {
                self.flush_dirty_pages_or_restore(idb, dirty_pages).await?;
            }
        }

        self.evict_sparse_staging();

        Ok(result_json)
    }

    /// Stamp and publish one fully prepared mixed write batch.
    async fn apply_atomic_write(
        &self,
        prepared: crate::db::MaterializedAtomicWrite,
    ) -> Result<String, JsValue> {
        use crate::db::VALID_FROM_USE_TX_TIME;
        use crate::graph::types::tx_id_now;
        use crate::storage::packed_pages::MAX_FACT_BYTES;

        #[cfg(feature = "bench-internals")]
        let mutation_started = js_sys::Date::now();
        #[cfg(feature = "bench-internals")]
        let preparation_ms = prepared.benchmark_preparation_ms;
        let (dirty_pages, result_json) = {
            let mut inner = self.inner.borrow_mut();
            let tx_id = tx_id_now();
            let tx_count = inner.fact_storage.current_tx_count().saturating_add(1);
            let mut stamped = prepared.facts;

            for (index, fact) in stamped.iter_mut().enumerate() {
                fact.tx_id = tx_id;
                fact.tx_count = tx_count;
                if fact.valid_from == VALID_FROM_USE_TX_TIME {
                    fact.valid_from = tx_id.cast_signed();
                }
                let encoded = postcard::to_allocvec(fact)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
                if encoded.len() > MAX_FACT_BYTES {
                    return Err(JsValue::from_str(&format!(
                        "executeAtomic fact {} serialised size {} bytes exceeds maximum slot size {} bytes",
                        index,
                        encoded.len(),
                        MAX_FACT_BYTES
                    )));
                }
            }

            // No semantic failure remains after this boundary. Claim one
            // transaction counter and publish every materialized fact together.
            inner.mutation_advanced = true;
            let allocated_tx_count = inner.fact_storage.allocate_tx_count();
            if allocated_tx_count != tx_count {
                return Err(JsValue::from_str(
                    "executeAtomic transaction counter changed while the mutation guard was held",
                ));
            }

            for fact in stamped {
                inner
                    .fact_storage
                    .load_fact(fact)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
            }

            inner.pfs.mark_dirty();
            inner
                .pfs
                .save()
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            let dirty_pages = take_dirty_pages(&mut inner.pfs)?;
            if inner.paged {
                configure_sparse_authority(&mut inner.pfs)?;
            }

            let decision = inner.pfs.delta_maintenance_decision();
            let durability = if inner.idb.is_some() {
                "published"
            } else {
                "memory"
            };
            let fact_count = prepared
                .transacted_fact_count
                .saturating_add(prepared.retracted_fact_count);
            let json = serde_json::json!({
                "atomic": true,
                "command_count": prepared.command_count,
                "fact_count": fact_count,
                "transacted_fact_count": prepared.transacted_fact_count,
                "retracted_fact_count": prepared.retracted_fact_count,
                "tx_id": tx_id,
                "tx_count": tx_count,
                "transacted": tx_id,
                "durability": durability,
                "maintenance_pending": !matches!(
                    decision,
                    DeltaMaintenanceDecision::ContinueDeltaAppend
                ),
                "advice": browser_write_advice(decision),
            })
            .to_string();

            (dirty_pages, json)
        };

        #[cfg(feature = "bench-internals")]
        let mutation_ms = js_sys::Date::now() - mutation_started;
        #[cfg(feature = "bench-internals")]
        let publication_started = js_sys::Date::now();

        if !dirty_pages.is_empty() {
            let idb = self
                .inner
                .borrow()
                .idb
                .as_ref()
                .map(IndexedDbBackend::clone_handle);
            if let Some(idb) = idb {
                self.flush_dirty_pages_or_restore(idb, dirty_pages).await?;
            }
        }

        self.evict_sparse_staging();
        #[cfg(feature = "bench-internals")]
        let result_json = {
            let publication_ms = js_sys::Date::now() - publication_started;
            let mut result: serde_json::Value = serde_json::from_str(&result_json)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            result["benchmark"] = serde_json::json!({
                "schema": "vicia.browser-atomic-write-stages.v1",
                "preparation_ms": preparation_ms,
                "mutation_ms": mutation_ms,
                "publication_ms": publication_ms,
            });
            result.to_string()
        };
        Ok(result_json)
    }
}

async fn open_persistent_storage(
    idb: &IndexedDbBackend,
    mode: BrowserOpenMode,
) -> Result<(PersistentFactStorage<BrowserBufferBackend>, bool), JsValue> {
    let Some(page0) = idb.load_page_if_present(0).await? else {
        let numeric_pages = idb.count_numeric_pages().await?;
        if numeric_pages != 0 {
            return Err(JsValue::from_str(
                "IndexedDB contains page records but published page 0 is missing",
            ));
        }
        let page0 =
            crate::storage::header_extension::build_header_page(crate::storage::FileHeader::new())
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
        idb.replace_all_pages(vec![(0, page0.clone())]).await?;
        let buffer = if mode.is_paged() {
            BrowserBufferBackend::load_sparse_pages(
                HashMap::from([(0, page0)]),
                1,
                HashSet::from([0]),
            )
        } else {
            Ok(BrowserBufferBackend::load_pages(HashMap::from([(
                0, page0,
            )])))
        }
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let pfs = PersistentFactStorage::new(buffer, 256)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        return Ok((pfs, mode.is_paged()));
    };

    let header = crate::storage::FileHeader::from_bytes(&page0)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    if header.version == crate::storage::FORMAT_VERSION
        || header.version == crate::storage::MAX_READABLE_FORMAT_VERSION
    {
        match open_current_sparse_storage(idb, page0).await {
            Ok(mut pfs) => {
                if mode == BrowserOpenMode::EagerCompatibility {
                    prefetch_eager_compatibility_pages(idb, &mut pfs).await?;
                }
                return Ok((pfs, mode.is_paged()));
            }
            Err(error) if mode == BrowserOpenMode::EagerCompatibility => {
                // Preserve the published eager API for unusual but valid
                // metadata footprints outside the bounded paged policy. Both
                // paths still construct the same PersistentFactStorage core.
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }

    open_eager_or_migrate_storage(idb, mode).await
}

async fn open_current_sparse_storage(
    idb: &IndexedDbBackend,
    page0: Vec<u8>,
) -> Result<PersistentFactStorage<BrowserBufferBackend>, JsValue> {
    let plan = BrowserV11BootstrapPlan::from_page0(&page0)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let mut pages = HashMap::from([(0u64, page0)]);
    let mut pinned = HashSet::from([0u64]);

    for range in plan.required_ranges() {
        let loaded = load_required_range(idb, *range).await?;
        insert_bootstrap_pages(&mut pages, &mut pinned, loaded);
    }
    for candidate in plan.manifest_candidates() {
        if let Some(loaded) = load_optional_range(idb, candidate.manifest_range()).await? {
            insert_bootstrap_pages(&mut pages, &mut pinned, loaded);
        }
    }
    for range in plan
        .projection_catalog_ranges()
        .map_err(|error| JsValue::from_str(&error.to_string()))?
    {
        if let Some(loaded) = load_optional_range(idb, range).await? {
            insert_bootstrap_pages(&mut pages, &mut pinned, loaded);
        }
    }

    let mut backend =
        BrowserBufferBackend::load_sparse_pages(pages, plan.published_page_count(), pinned)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let resident_plan = plan
        .plan_resident_metadata(&backend)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;

    for range in resident_plan.candidate_segment_ranges() {
        if let Some(loaded) = load_optional_range(idb, range).await? {
            for (page_id, page) in &loaded {
                resident_plan
                    .verify_fetched_published_page(*page_id, page)
                    .map_err(|error| JsValue::from_str(&error.to_string()))?;
            }
            backend
                .stage_clean_pages(loaded)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
        }
    }

    let pins = authority_pin_ids(&plan, &resident_plan, &backend);
    backend
        .replace_pinned_pages(pins)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    PersistentFactStorage::new(backend, 256).map_err(|error| JsValue::from_str(&error.to_string()))
}

async fn open_eager_or_migrate_storage(
    idb: &IndexedDbBackend,
    mode: BrowserOpenMode,
) -> Result<(PersistentFactStorage<BrowserBufferBackend>, bool), JsValue> {
    let existing = idb.load_all_pages().await?;
    let existing_page_count = u64::try_from(existing.len())
        .map_err(|_| JsValue::from_str("IndexedDB page count exceeds u64"))?;
    let buffer = BrowserBufferBackend::load_pages(existing);
    let mut pfs = PersistentFactStorage::new(buffer, 256)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    pfs.with_backend_mut(BrowserBufferBackend::retain_declared_prefix)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    if mode.is_paged() {
        // A previous compatible format can be read eagerly, but its index nodes do
        // not satisfy the current sparse decoder. Recompact it once while the
        // complete image is resident, then atomically publish the replacement
        // pages before exposing a bounded paged handle.
        pfs.run_idle_delta_maintenance()
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
    }
    let declared_page_count = pfs
        .with_backend_mut(BrowserBufferBackend::retain_declared_prefix)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let migration_pages = take_dirty_pages(&mut pfs)?;
    if !migration_pages.is_empty() {
        if declared_page_count < existing_page_count {
            let published_pages = pfs.with_backend(BrowserBufferBackend::all_pages);
            idb.replace_all_pages(published_pages).await?;
        } else {
            idb.write_pages(migration_pages).await?;
        }
    }

    let became_sparse = if mode.is_paged() {
        configure_sparse_authority(&mut pfs).map_err(|error| {
            JsValue::from_str(&format!(
                "openPaged cannot silently fall back to eager residency: {}",
                js_value_message(&error),
            ))
        })?;
        pfs.with_backend_mut(BrowserBufferBackend::evict_all_clean_unpinned);
        true
    } else {
        false
    };
    Ok((pfs, became_sparse))
}

async fn prefetch_eager_compatibility_pages(
    idb: &IndexedDbBackend,
    pfs: &mut PersistentFactStorage<BrowserBufferBackend>,
) -> Result<(), JsValue> {
    let published_page_count = pfs
        .browser_published_page_count()
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let present = idb.load_all_pages().await?;
    let complete_pages = present.into_iter().filter(|(page_id, page)| {
        *page_id < published_page_count && page.len() == crate::storage::PAGE_SIZE
    });
    pfs.with_backend_mut(|backend| backend.stage_clean_pages(complete_pages))
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

async fn load_required_range(
    idb: &IndexedDbBackend,
    range: BrowserPageRange,
) -> Result<Vec<(u64, Vec<u8>)>, JsValue> {
    let mut loaded = Vec::new();
    let mut start = range.start_page();
    while start < range.end_page() {
        let count = (range.end_page() - start).min(BROWSER_IDB_BATCH_PAGES);
        loaded.extend(idb.load_page_range(start, count).await?);
        start = start
            .checked_add(count)
            .ok_or_else(|| JsValue::from_str("required browser page range overflow"))?;
    }
    Ok(loaded)
}

async fn load_optional_range(
    idb: &IndexedDbBackend,
    range: BrowserPageRange,
) -> Result<Option<Vec<(u64, Vec<u8>)>>, JsValue> {
    let mut loaded = Vec::new();
    let mut start = range.start_page();
    while start < range.end_page() {
        let count = (range.end_page() - start).min(BROWSER_IDB_BATCH_PAGES);
        let Some(chunk) = idb.load_page_range_if_complete(start, count).await? else {
            return Ok(None);
        };
        loaded.extend(chunk);
        start = start
            .checked_add(count)
            .ok_or_else(|| JsValue::from_str("optional browser page range overflow"))?;
    }
    Ok(Some(loaded))
}

fn insert_bootstrap_pages(
    pages: &mut HashMap<u64, Vec<u8>>,
    pinned: &mut HashSet<u64>,
    loaded: Vec<(u64, Vec<u8>)>,
) {
    for (page_id, page) in loaded {
        pages.insert(page_id, page);
        pinned.insert(page_id);
    }
}

fn authority_pin_ids(
    plan: &BrowserV11BootstrapPlan,
    resident_plan: &crate::storage::persistent_facts::BrowserV11ResidentPlan,
    backend: &BrowserBufferBackend,
) -> HashSet<u64> {
    let mut pins = HashSet::from([0u64]);
    for range in plan.required_ranges() {
        insert_resident_range_ids(&mut pins, *range, backend);
    }
    for candidate in resident_plan.manifest_candidates() {
        insert_resident_range_ids(&mut pins, candidate.manifest_range(), backend);
        for range in candidate.segment_ranges() {
            insert_resident_range_ids(&mut pins, *range, backend);
        }
    }
    pins
}

fn insert_resident_range_ids(
    pins: &mut HashSet<u64>,
    range: BrowserPageRange,
    backend: &BrowserBufferBackend,
) {
    for page_id in range.start_page()..range.end_page() {
        if backend.is_page_resident(page_id) {
            pins.insert(page_id);
        }
    }
}

fn configure_sparse_authority(
    pfs: &mut PersistentFactStorage<BrowserBufferBackend>,
) -> Result<(), JsValue> {
    let pins = sparse_authority_pin_ids(pfs)?;
    pfs.with_backend_mut(|backend| {
        if backend.is_sparse() {
            backend.replace_pinned_pages(pins)
        } else {
            backend.configure_sparse_residency(pins).map(|_| ())
        }
    })
    .map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Prove that a complete resident candidate has the exact metadata lineage and
/// page prefix required by a later sparse open, without changing its live mode.
fn validate_sparse_authority(
    pfs: &PersistentFactStorage<BrowserBufferBackend>,
) -> Result<(), JsValue> {
    let pins = sparse_authority_pin_ids(pfs)?;
    pfs.with_backend(|backend| backend.validate_sparse_residency(pins))
        .map(|_| ())
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

fn sparse_authority_pin_ids(
    pfs: &PersistentFactStorage<BrowserBufferBackend>,
) -> Result<HashSet<u64>, JsValue> {
    let logical_page_count = pfs
        .with_backend(BrowserBufferBackend::page_count_raw)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    if logical_page_count == 0 {
        return Ok(HashSet::new());
    }
    let page0 = pfs
        .with_backend(|backend| backend.read_page_raw(0))
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let plan = BrowserV11BootstrapPlan::from_page0(&page0)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let selected_manifest = pfs
        .browser_selected_manifest_identity()
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let pins = pfs.with_backend(|backend| {
        let resident_plan = plan.plan_resident_metadata(backend)?;
        if let Some((selected_slot, selected_generation)) = selected_manifest
            && !resident_plan.manifest_candidates().iter().any(|candidate| {
                candidate.slot() == selected_slot
                    && candidate.generation() == selected_generation
            })
        {
            anyhow::bail!(
                "Bounded browser planner does not retain the manifest lineage selected by the persistent loader"
            );
        }
        Ok::<_, anyhow::Error>(authority_pin_ids(&plan, &resident_plan, backend))
    });
    pins.map_err(|error| JsValue::from_str(&error.to_string()))
}

// ── JSON serialisation helpers (free functions, not exported to WASM) ────────

fn browser_write_advice(decision: DeltaMaintenanceDecision) -> &'static str {
    match decision {
        DeltaMaintenanceDecision::ContinueDeltaAppend => "none",
        DeltaMaintenanceDecision::ScheduleBackgroundRecompact => "schedule_idle_maintenance",
        DeltaMaintenanceDecision::MaintenanceBackpressure => "reduce_checkpoint_cadence",
    }
}

fn take_dirty_pages(
    pfs: &mut PersistentFactStorage<BrowserBufferBackend>,
) -> Result<Vec<(u64, Vec<u8>)>, JsValue> {
    let dirty_ids = pfs.with_backend_mut(|backend| backend.take_dirty());
    dirty_ids
        .into_iter()
        .map(|id| {
            pfs.with_backend(|backend| backend.read_page_raw(id))
                .map(|data| (id, data))
                .map_err(|error| {
                    JsValue::from_str(&format!("failed to read dirty browser page {id}: {error}"))
                })
        })
        .collect()
}

fn js_value_message(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| "non-string JavaScript error".to_string())
}

fn query_result_to_json(result: QueryResult) -> String {
    use serde_json::{Value as JVal, json};

    let val: JVal = match result {
        QueryResult::Transacted(tx_id) => {
            json!({"transacted": tx_id})
        }
        QueryResult::Retracted(tx_id) => {
            json!({"retracted": tx_id})
        }
        QueryResult::Forgotten { tx_id, count } => {
            json!({"forgotten": count, "tx_id": tx_id})
        }
        QueryResult::Ok => json!({"ok": true}),
        QueryResult::QueryResults { vars, results } => {
            let rows: Vec<Vec<JVal>> = results
                .iter()
                .map(|row| row.iter().map(to_tagged_json).collect())
                .collect();
            json!({"variables": vars, "results": rows})
        }
    };
    val.to_string()
}

#[cfg(all(target_arch = "wasm32", feature = "browser", test))]
mod tests {
    use super::*;
    use crate::gate_e_test_support::{
        BROWSER_FIXTURE, CorruptionCase, NATIVE_FIXTURE, Probe, QueryCase, apply_mutation,
        bounded_selective_read_fixture, corpus, normalize_rows, published_byte_len,
    };
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    const BASE_FACT_PAGE_START_OFFSET: usize = 84 + 12 + 80;

    fn fixture_pages(bytes: &[u8]) -> Vec<(u64, Vec<u8>)> {
        assert!(
            bytes.len().is_multiple_of(crate::storage::PAGE_SIZE),
            "fixture must contain complete pages"
        );
        bytes
            .chunks_exact(crate::storage::PAGE_SIZE)
            .enumerate()
            .map(|(page_id, page)| (u64::try_from(page_id).unwrap(), page.to_vec()))
            .collect()
    }

    async fn build_sparse_v11_fixture(fact_count: usize) -> Vec<u8> {
        let source = BrowserDb::open_in_memory().expect("open sparse fixture source");
        let mut tuples = Vec::with_capacity(fact_count);
        for index in 0..fact_count {
            let entity = if index == fact_count / 2 {
                ":sparse/target".to_string()
            } else {
                format!(":sparse/e{index}")
            };
            tuples.push(format!("[{entity} :sparse/value {index}]"));
        }
        source
            .execute(format!("(transact [{}])", tuples.join(" ")))
            .await
            .expect("write sparse fixture facts");
        source
            .export_graph()
            .expect("export sparse fixture")
            .to_vec()
    }

    async fn build_bounded_selective_read_image() -> Vec<u8> {
        let fixture = bounded_selective_read_fixture();
        let source = BrowserDb::open_in_memory().expect("open bounded selective-read source");
        for command in fixture.setup_commands() {
            source
                .execute(command)
                .await
                .expect("write bounded selective-read fixture");
        }
        source
            .export_graph()
            .expect("export bounded selective-read fixture")
            .to_vec()
    }

    fn browser_query_source_refs(encoded: &str) -> Vec<String> {
        let value: serde_json::Value =
            serde_json::from_str(encoded).expect("source query JSON must parse");
        let rows = value["results"]
            .as_array()
            .expect("source query results must be an array");
        let mut sources = rows
            .iter()
            .map(|row| {
                let row = row.as_array().expect("source query row must be an array");
                assert_eq!(row.len(), 1, "source query must return one column");
                row[0]["$ref"]
                    .as_str()
                    .expect("source query must return a tagged entity ref")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        sources.sort_unstable();
        sources
    }

    fn downgrade_current_fixture_to_v11(bytes: &mut [u8]) {
        let page0 = bytes
            .get(..crate::storage::PAGE_SIZE)
            .expect("fixture must contain page 0");
        let mut header =
            crate::storage::FileHeader::from_bytes(page0).expect("read current fixture header");
        let extension = crate::storage::header_extension::HeaderExtension::read_from_page0(
            header.version,
            page0,
        )
        .expect("read current fixture extension")
        .expect("current fixture must contain an extension");
        header.version = crate::storage::INTEGRITY_FORMAT_VERSION;
        header.header_checksum = crate::storage::persistent_facts::compute_header_checksum(&header);
        let extension = crate::storage::header_extension::HeaderExtension::new(
            extension.primary(),
            extension.secondary(),
        )
        .with_base_fact_page_start(extension.base_fact_page_start())
        .expect("retain v11 base fact page start")
        .with_base_integrity(extension.base_integrity())
        .expect("retain v11 base integrity");
        let v11_page0 =
            crate::storage::header_extension::build_header_page_with_extension(header, extension)
                .expect("build v11 fixture header");
        bytes
            .get_mut(..crate::storage::PAGE_SIZE)
            .expect("fixture must contain mutable page 0")
            .copy_from_slice(&v11_page0);
    }

    fn page_map_version(pages: &std::collections::HashMap<u64, Vec<u8>>) -> u32 {
        let page0 = pages.get(&0).expect("page map must contain page 0");
        u32::from_le_bytes(page0[4..8].try_into().unwrap())
    }

    fn assert_canonical_query_json(case: &QueryCase, encoded: &str) {
        let value: serde_json::Value =
            serde_json::from_str(encoded).expect("browser query JSON must parse");
        let variables: Vec<String> = serde_json::from_value(value["variables"].clone())
            .expect("query variables must decode");
        assert_eq!(variables, case.variables, "query variables: {}", case.id);
        let mut actual: Vec<Vec<serde_json::Value>> =
            serde_json::from_value(value["results"].clone()).expect("query rows must decode");
        let mut expected = case.rows.clone();
        if case.unordered_rows {
            actual = normalize_rows(actual);
            expected = normalize_rows(expected);
        }
        assert_eq!(actual, expected, "query rows: {}", case.id);
    }

    async fn assert_browser_queries(db: &BrowserDb, queries: &[QueryCase]) {
        for case in queries {
            let encoded = db
                .execute(case.datalog.clone())
                .await
                .expect("Gate E browser query must execute");
            assert_canonical_query_json(case, &encoded);
        }
    }

    async fn assert_browser_probe(db: &BrowserDb, probe: &Probe, case_id: &str) {
        let encoded = db
            .execute(probe.datalog.clone())
            .await
            .expect("corruption fallback probe must execute");
        let value: serde_json::Value =
            serde_json::from_str(&encoded).expect("fallback probe JSON must parse");
        let actual: Vec<Vec<serde_json::Value>> =
            serde_json::from_value(value["results"].clone()).expect("fallback rows must decode");
        assert_eq!(actual, probe.rows, "corruption fallback probe: {case_id}");
    }

    async fn assert_rejected_import_preserves_sentinel(
        db: &BrowserDb,
        db_name: &str,
        case: &CorruptionCase,
        mutated: Vec<u8>,
    ) {
        let result = db
            .import_graph(js_sys::Uint8Array::from(mutated.as_slice()))
            .await;
        assert!(
            result.is_err(),
            "browser must reject corruption: {}",
            case.id
        );
        let live = db
            .execute("(query [:find ?v :where [:sentinel :value ?v]])".to_string())
            .await
            .expect("live sentinel query");
        let live: serde_json::Value = serde_json::from_str(&live).expect("live sentinel JSON");
        assert_eq!(live["results"], serde_json::json!([["preserved"]]));

        let reopened = BrowserDb::open(db_name)
            .await
            .expect("reopen sentinel database");
        let durable = reopened
            .execute("(query [:find ?v :where [:sentinel :value ?v]])".to_string())
            .await
            .expect("durable sentinel query");
        let durable: serde_json::Value =
            serde_json::from_str(&durable).expect("durable sentinel JSON");
        assert_eq!(durable["results"], serde_json::json!([["preserved"]]));
    }

    #[wasm_bindgen_test]
    async fn paged_open_and_selective_query_are_bounded_and_warm() {
        let bytes = build_sparse_v11_fixture(400).await;
        let plan = BrowserV11BootstrapPlan::from_page0(&bytes[..crate::storage::PAGE_SIZE])
            .expect("plan sparse fixture");
        assert!(
            plan.base_covered_range().page_count() > 4,
            "fixture needs multiple immutable base pages"
        );

        let db_name = format!("vicia-paged-selective-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open paged seed IDB");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed paged fixture");
        idb.reset_read_counters_for_test();

        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open paged fixture");
        let open_counts = idb.read_counters_for_test();
        assert_eq!(open_counts.full_store_reads, 0);
        {
            let inner = db.inner.borrow();
            assert!(inner.paged);
            inner.pfs.with_backend(|backend| {
                assert!(backend.is_sparse());
                assert!(
                    backend.resident_page_count()
                        < usize::try_from(plan.published_page_count()).unwrap(),
                    "paged open must not retain the complete graph"
                );
                for page_id in
                    plan.base_covered_range().start_page()..plan.base_covered_range().end_page()
                {
                    assert!(
                        !backend.is_page_resident(page_id),
                        "paged open must not fetch immutable base page {page_id}"
                    );
                }
            });
        }

        let query = "(query [:find ?v :where [:sparse/target :sparse/value ?v]])";
        let cold = db
            .execute(query.to_string())
            .await
            .expect("cold selective paged query");
        let cold: serde_json::Value = serde_json::from_str(&cold).expect("cold query JSON");
        assert_eq!(cold["results"], serde_json::json!([[200]]));
        let cold_counts = idb.read_counters_for_test();
        assert!(
            cold_counts.pages_returned > open_counts.pages_returned,
            "cold selective query must fetch its index/fact pages"
        );
        assert_eq!(cold_counts.full_store_reads, 0);

        let warm = db
            .execute(query.to_string())
            .await
            .expect("warm selective paged query");
        let warm: serde_json::Value = serde_json::from_str(&warm).expect("warm query JSON");
        assert_eq!(warm, cold);
        let warm_counts = idb.read_counters_for_test();
        assert_eq!(
            warm_counts.pages_returned, cold_counts.pages_returned,
            "warm repeat must be served by the bounded core page cache"
        );
        let inner = db.inner.borrow();
        inner.pfs.with_backend(|backend| {
            assert_eq!(
                backend.resident_page_count(),
                backend.pinned_page_count(),
                "operation staging pages must be released after the query"
            );
        });
    }

    #[wasm_bindgen_test]
    async fn paged_bounded_selective_read_distinguishes_keyword_and_ref_relationships() {
        let fixture = bounded_selective_read_fixture();
        let bytes = build_bounded_selective_read_image().await;
        let header = crate::storage::FileHeader::from_bytes(
            bytes
                .get(..crate::storage::PAGE_SIZE)
                .expect("bounded fixture must contain page 0"),
        )
        .expect("bounded fixture page 0 must decode");
        let db_name = format!("vicia-paged-bounded-selective-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open bounded fixture IDB");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed bounded fixture pages");
        idb.reset_read_counters_for_test();

        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open bounded paged fixture");
        let open_counts = idb.read_counters_for_test();
        assert_eq!(open_counts.full_store_reads, 0);
        let pinned_tx = db.inner.borrow().fact_storage.current_tx_count();
        let view = db
            .read_view_any_valid_time(pinned_tx)
            .expect("capture any-valid-time view");
        let expected = fixture
            .expected_visible_sources()
            .into_iter()
            .map(|source| source.to_string())
            .collect::<Vec<_>>();

        let raw = view
            .query(
                fixture.keyword_query(),
                crate::db::READ_VIEW_MAX_ROWS,
                BROWSER_READ_VIEW_MAX_RESULT_BYTES,
            )
            .await
            .expect("read keyword-valued owner relationship");
        assert_eq!(browser_query_source_refs(&raw), expected);

        let typed = view
            .refs_to(
                fixture.ref_attribute.clone(),
                fixture.ref_target.to_string(),
                fixture.visible_source_count,
            )
            .await
            .expect("read Ref-valued owner relationship");
        let typed = typed
            .iter()
            .map(|value| {
                value
                    .as_string()
                    .expect("typed source must be a UUID string")
            })
            .collect::<Vec<_>>();
        assert_eq!(typed, expected);

        assert!(
            view.query(
                fixture.keyword_query(),
                fixture.visible_source_count - 1,
                BROWSER_READ_VIEW_MAX_RESULT_BYTES,
            )
            .await
            .is_err(),
            "undersized raw budget must reject the complete browser result"
        );
        assert!(
            view.refs_to(
                fixture.ref_attribute.clone(),
                fixture.ref_target.to_string(),
                fixture.visible_source_count - 1,
            )
            .await
            .is_err(),
            "undersized typed limit must reject the complete browser result"
        );

        db.execute(fixture.post_view_command())
            .await
            .expect("append post-view source");
        let pinned = view
            .refs_to(
                fixture.ref_attribute.clone(),
                fixture.ref_target.to_string(),
                crate::db::READ_VIEW_MAX_ROWS,
            )
            .await
            .expect("reread pinned typed relationship");
        assert_eq!(
            usize::try_from(pinned.length()).unwrap(),
            fixture.visible_source_count,
            "pinned browser view must exclude later writes"
        );

        let counts = idb.read_counters_for_test();
        assert_eq!(counts.full_store_reads, 0);
        assert!(
            counts.pages_returned > open_counts.pages_returned,
            "bounded relationship reads must demand committed pages"
        );
        db.inner.borrow().pfs.with_backend(|backend| {
            assert!(backend.is_sparse());
            assert!(
                u64::try_from(backend.resident_page_count()).unwrap() < header.page_count,
                "bounded relationship reads must not retain the complete image"
            );
        });
    }

    #[wasm_bindgen_test]
    async fn paged_attribute_aggregate_resumes_and_releases_staging() {
        use std::cell::Cell;

        let fact_count = 12_000usize;
        let bytes = build_sparse_v11_fixture(fact_count).await;
        let db_name = format!("vicia-paged-aggregate-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open aggregate seed IDB");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed aggregate fixture");
        idb.reset_read_counters_for_test();
        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open aggregate fixture");

        let input_processed = Rc::new(Cell::new(false));
        let input_processed_in_task = input_processed.clone();
        let task = Closure::once_into_js(move || input_processed_in_task.set(true));
        web_sys::window()
            .expect("browser window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(task.unchecked_ref(), 0)
            .expect("schedule synthetic input task");

        let encoded = db
            .execute("(query [:find (count ?v) (sum ?v) :where [?e :sparse/value ?v]])".to_string())
            .await
            .expect("execute resumable aggregate");
        let result: serde_json::Value = serde_json::from_str(&encoded).expect("aggregate JSON");
        let expected_sum = i64::try_from(fact_count * (fact_count - 1) / 2).unwrap();
        assert_eq!(
            result["results"],
            serde_json::json!([[fact_count, expected_sum]])
        );
        assert!(
            input_processed.get(),
            "browser task must run before the aggregate completes"
        );
        let counters = idb.read_counters_for_test();
        assert_eq!(counters.full_store_reads, 0);
        assert!(counters.pages_returned > 0);
        let inner = db.inner.borrow();
        inner.pfs.with_backend(|backend| {
            assert_eq!(
                backend.resident_page_count(),
                backend.pinned_page_count(),
                "completed aggregate must release staging pages"
            );
        });
    }

    #[wasm_bindgen_test]
    async fn paged_attribute_range_batches_demand_callbacks() {
        let fact_count = 400;
        let bytes = build_sparse_v11_fixture(fact_count).await;
        let db_name = format!("vicia-paged-attribute-batch-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open attribute-batch seed IDB");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed attribute-batch fixture");
        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open attribute-batch fixture");
        idb.reset_read_counters_for_test();

        let result = db
            .execute("(query [:find ?v :where [?e :sparse/value ?v]])".to_string())
            .await
            .expect("execute attribute range query");
        let result: serde_json::Value =
            serde_json::from_str(&result).expect("attribute range JSON");
        assert_eq!(result["results"].as_array().map(Vec::len), Some(fact_count));
        let counts = idb.read_counters_for_test();
        assert_eq!(counts.full_store_reads, 0);
        assert!(counts.pages_returned > counts.transactions);
        assert!(
            counts.pages_requested >= counts.transactions.saturating_mul(8),
            "attribute range reads must amortize IndexedDB callback transactions"
        );
    }

    #[wasm_bindgen_test]
    async fn paged_exact_entity_set_query_avoids_full_fact_prefetch() {
        let fact_count = 4_000;
        let bytes = build_sparse_v11_fixture(fact_count).await;
        let plan = BrowserV11BootstrapPlan::from_page0(&bytes[..crate::storage::PAGE_SIZE])
            .expect("plan entity-set fixture");
        assert!(
            plan.base_fact_range().page_count() > 1,
            "fixture must contain a multi-page fact range"
        );

        let db_name = format!("vicia-paged-entity-set-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open entity-set seed IDB");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed entity-set fixture");
        idb.reset_read_counters_for_test();
        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open paged entity-set fixture");
        idb.reset_read_counters_for_test();

        let entity_indexes: Vec<usize> = (10..138).collect();
        let branches = entity_indexes
            .iter()
            .map(|index| format!("[:sparse/e{index} :sparse/value ?value]"))
            .collect::<Vec<_>>()
            .join(" ");
        let result = db
            .execute(format!("(query [:find ?value :where (or {branches})])"))
            .await
            .expect("execute exact entity-set query");
        let result: serde_json::Value = serde_json::from_str(&result).expect("entity-set JSON");
        let mut values: Vec<i64> = result["results"]
            .as_array()
            .expect("entity-set rows")
            .iter()
            .map(|row| {
                row.as_array()
                    .and_then(|values| values.first())
                    .and_then(serde_json::Value::as_i64)
                    .expect("integer entity-set value")
            })
            .collect();
        values.sort_unstable();
        assert_eq!(
            values,
            entity_indexes
                .iter()
                .map(|index| i64::try_from(*index).unwrap())
                .collect::<Vec<_>>()
        );

        let query_counts = idb.read_counters_for_test();
        assert_eq!(query_counts.full_store_reads, 0);
        assert!(query_counts.pages_returned > 0);
        assert!(
            query_counts.pages_requested > query_counts.transactions,
            "exact entity-set query must batch neighboring demand pages"
        );
        let inner = db.inner.borrow();
        inner.pfs.with_backend(|backend| {
            assert_eq!(
                backend.resident_page_count(),
                backend.pinned_page_count(),
                "entity-set query staging pages must be released"
            );
        });
        drop(inner);

        idb.reset_read_counters_for_test();
        let view_db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open cold paged read-view handle");
        idb.reset_read_counters_for_test();
        let view = view_db.read_view().expect("capture paged read view");
        let viewed = view
            .query(
                format!("(query [:find ?value :where (or {branches})])"),
                entity_indexes.len(),
                32 * 1024,
            )
            .await
            .expect("execute exact entity-set read view");
        let viewed: serde_json::Value = serde_json::from_str(&viewed).expect("read-view JSON");
        assert_eq!(
            viewed["results"].as_array().map(Vec::len),
            Some(entity_indexes.len())
        );
        let view_counts = idb.read_counters_for_test();
        assert_eq!(view_counts.full_store_reads, 0);
        assert!(view_counts.pages_returned > 0);
    }

    #[wasm_bindgen_test]
    async fn paged_query_detects_lazy_base_corruption_and_async_export_is_exact() {
        let bytes = build_sparse_v11_fixture(300).await;
        let plan = BrowserV11BootstrapPlan::from_page0(&bytes[..crate::storage::PAGE_SIZE])
            .expect("plan corruption fixture");
        let fact_page = plan.base_fact_range().start_page();
        let mut corrupted = bytes.clone();
        let byte_offset = usize::try_from(fact_page)
            .expect("fact page fits usize")
            .checked_mul(crate::storage::PAGE_SIZE)
            .and_then(|offset| offset.checked_add(crate::storage::PAGE_SIZE - 1))
            .expect("fact page byte offset");
        corrupted[byte_offset] ^= 0x01;

        let corrupt_name = format!("vicia-paged-corrupt-{}", js_sys::Date::now());
        let corrupt_idb = IndexedDbBackend::open(&corrupt_name)
            .await
            .expect("open corrupt seed");
        corrupt_idb
            .replace_all_pages(fixture_pages(&corrupted))
            .await
            .expect("seed corrupt base page");
        let corrupt_db = BrowserDb::open_paged_from_idb(corrupt_idb.clone_handle())
            .await
            .expect("catalog-only open must defer base-page verification");
        let corrupt_query = corrupt_db
            .execute("(query [:find ?v :where [:sparse/e0 :sparse/value ?v]])".to_string())
            .await;
        assert!(
            corrupt_query.is_err(),
            "first query touching the corrupt fact page must fail closed"
        );
        assert!(
            corrupt_db.export_graph_async().await.is_err(),
            "async export must reject a corrupt immutable base page"
        );

        let exact_name = format!("vicia-paged-export-{}", js_sys::Date::now());
        let exact_idb = IndexedDbBackend::open(&exact_name)
            .await
            .expect("open exact seed");
        exact_idb
            .replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed exact graph");
        exact_idb.reset_read_counters_for_test();
        let exact_db = BrowserDb::open_paged_from_idb(exact_idb.clone_handle())
            .await
            .expect("open exact paged graph");
        assert!(
            exact_db.export_graph().is_err(),
            "sync export must not pretend absent paged bytes are complete"
        );
        let exported = exact_db
            .export_graph_async()
            .await
            .expect("async paged export")
            .to_vec();
        assert_eq!(exported, bytes);
        assert_eq!(
            exact_idb.read_counters_for_test().full_store_reads,
            0,
            "async export must use bounded range reads, not legacy get-all"
        );
    }

    #[wasm_bindgen_test]
    async fn paged_async_export_rejects_post_open_selected_metadata_corruption() {
        let base = build_sparse_v11_fixture(220).await;
        let source = BrowserDb::open_in_memory().expect("open delta export source");
        source
            .import_graph(js_sys::Uint8Array::from(base.as_slice()))
            .await
            .expect("import base export fixture");
        source
            .execute("(transact [[:sparse/delta :value 999]])".to_string())
            .await
            .expect("append selected delta lineage");
        let bytes = source
            .export_graph()
            .expect("export selected delta fixture")
            .to_vec();

        let db_name = format!("vicia-paged-export-metadata-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open metadata corruption seed");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed selected delta fixture");
        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open selected delta fixture paged");
        let authority_ranges = {
            let inner = db.inner.borrow();
            inner
                .pfs
                .begin_browser_export_verification()
                .expect("plan export authority")
                .exact_resident_ranges_for_test()
                .to_vec()
        };
        assert!(
            authority_ranges.len() >= 3,
            "fixture must pin catalog, selected segment, and selected manifest ranges"
        );

        for range in authority_ranges {
            let page_id = range.start_page();
            let original = idb
                .load_page(page_id)
                .await
                .expect("load selected authority page");
            let mut corrupt = original.clone();
            corrupt[crate::storage::PAGE_SIZE - 1] ^= 0x01;
            idb.overwrite_page_without_authority_for_test(page_id, &corrupt)
                .await
                .expect("corrupt selected authority without page-0 publish");
            let error = db
                .export_graph_async()
                .await
                .expect_err("selected metadata corruption must fail export");
            assert!(
                js_value_message(&error).contains("authority page"),
                "metadata corruption must fail at the selected authority boundary: {}",
                js_value_message(&error)
            );
            idb.overwrite_page_without_authority_for_test(page_id, &original)
                .await
                .expect("restore selected authority page");
        }

        assert_eq!(
            db.export_graph_async()
                .await
                .expect("restored authority exports")
                .to_vec(),
            bytes
        );
    }

    #[wasm_bindgen_test]
    async fn repeated_paged_open_terminates_on_two_segment_v11_lineage_without_rewrite() {
        // This is the sanitized shape of the Vetch liveness report: one v11
        // base followed by two selected delta segments. The payload is wholly
        // synthetic; the regression is the IndexedDB callback lifetime across
        // the several range requests needed to reopen that lineage.
        let base = build_sparse_v11_fixture(120).await;
        let source = BrowserDb::open_in_memory().expect("open liveness fixture source");
        source
            .import_graph(js_sys::Uint8Array::from(base.as_slice()))
            .await
            .expect("import liveness fixture base");
        source
            .execute("(transact [[:liveness/first :value 1]])".to_string())
            .await
            .expect("append first liveness segment");
        source
            .execute("(transact [[:liveness/second :value 2]])".to_string())
            .await
            .expect("append second liveness segment");
        let bytes = source
            .export_graph()
            .expect("export liveness fixture")
            .to_vec();

        let db_name = format!("vicia-paged-open-liveness-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open liveness fixture IDB");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed liveness fixture");
        let before = idb
            .load_all_pages()
            .await
            .expect("snapshot liveness fixture before opens");

        for attempt in 0..8 {
            let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
                .await
                .expect("two-segment paged open must terminate");
            let result = db
                .execute(
                    "(query [:find ?first ?second :where [:liveness/first :value ?first] [:liveness/second :value ?second]])"
                        .to_string(),
                )
                .await
                .expect("query selected liveness segments");
            let result: serde_json::Value =
                serde_json::from_str(&result).expect("liveness query JSON");
            assert_eq!(
                result["results"],
                serde_json::json!([[1, 2]]),
                "selected state after paged open attempt {attempt}"
            );
        }

        let after = idb
            .load_all_pages()
            .await
            .expect("snapshot liveness fixture after opens");
        assert_eq!(after, before, "paged opens must not rewrite IndexedDB");
    }

    #[wasm_bindgen_test]
    async fn paged_failed_write_restores_sparse_authority_without_full_reload() {
        let bytes = build_sparse_v11_fixture(250).await;
        let db_name = format!("vicia-paged-write-abort-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open abort seed");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed abort graph");
        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open paged abort graph");
        idb.reset_read_counters_for_test();
        idb.fail_next_write_for_test();

        let rejected = db
            .execute("(transact [[:sparse/rejected :value 999]])".to_string())
            .await;
        let rejected = rejected.expect_err("injected IDB abort must reject write");
        assert!(
            !js_value_message(&rejected).contains("could not be reopened"),
            "paged rollback must reopen the old authority: {}",
            js_value_message(&rejected)
        );
        assert!(
            js_value_message(&rejected).contains("injected IndexedDB"),
            "write must reach the injected IDB abort after sparse save preparation: {}",
            js_value_message(&rejected)
        );
        let old = db
            .execute("(query [:find ?v :where [:sparse/target :sparse/value ?v]])".to_string())
            .await
            .expect("restored old query");
        let old: serde_json::Value = serde_json::from_str(&old).expect("old query JSON");
        assert_eq!(old["results"], serde_json::json!([[125]]));
        let rejected_fact = db
            .execute("(query [:find ?v :where [:sparse/rejected :value ?v]])".to_string())
            .await
            .expect("query rejected fact");
        let rejected_fact: serde_json::Value =
            serde_json::from_str(&rejected_fact).expect("rejected query JSON");
        assert_eq!(rejected_fact["results"], serde_json::json!([]));
        assert_eq!(
            idb.read_counters_for_test().full_store_reads,
            0,
            "paged rollback must reopen authority metadata without full-store reload"
        );
        let inner = db.inner.borrow();
        assert!(!inner.durability_poisoned);
        inner
            .pfs
            .with_backend(|backend| assert!(backend.is_sparse()));
    }

    #[wasm_bindgen_test]
    async fn independent_paged_handle_rejects_newer_publication_before_replace_or_write() {
        let db_name = format!("vicia-paged-stale-handle-{}", js_sys::Date::now());
        let seed = BrowserDb::open_paged(&db_name)
            .await
            .expect("open stale-handle seed");
        seed.execute("(transact [[:stable :value 1]])".to_string())
            .await
            .expect("publish initial graph");
        let old_blob = seed
            .export_graph_async()
            .await
            .expect("export initial graph");
        drop(seed);

        let stale = BrowserDb::open_paged(&db_name)
            .await
            .expect("open future stale handle");
        let writer = BrowserDb::open_paged(&db_name)
            .await
            .expect("open independent writer");
        writer
            .execute("(transact [[:newer :value 2]])".to_string())
            .await
            .expect("publish newer graph");

        let export_error = stale
            .export_graph_async()
            .await
            .expect_err("stale async export must reject");
        assert!(js_value_message(&export_error).contains("reopen"));
        let import_error = stale
            .import_graph(old_blob)
            .await
            .expect_err("stale replace must abort before clear");
        assert!(js_value_message(&import_error).contains("reopen"));
        let write_error = stale
            .execute("(transact [[:stale :value 3]])".to_string())
            .await
            .expect_err("stale writer must not extend newer authority");
        assert!(js_value_message(&write_error).contains("reopen"));

        drop(stale);
        drop(writer);
        let reopened = BrowserDb::open_paged(&db_name)
            .await
            .expect("reopen current authority");
        let newer = reopened
            .execute("(query [:find ?v :where [:newer :value ?v]])".to_string())
            .await
            .expect("query newer publication");
        let newer: serde_json::Value = serde_json::from_str(&newer).expect("newer JSON");
        assert_eq!(newer["results"], serde_json::json!([[2]]));
        let stale_rows = reopened
            .execute("(query [:find ?v :where [:stale :value ?v]])".to_string())
            .await
            .expect("query rejected stale publication");
        let stale_rows: serde_json::Value = serde_json::from_str(&stale_rows).expect("stale JSON");
        assert_eq!(stale_rows["results"], serde_json::json!([]));
    }

    #[wasm_bindgen_test]
    async fn paged_import_converges_to_sparse_live_state() {
        let bytes = build_sparse_v11_fixture(350).await;
        let db_name = format!("vicia-paged-import-{}", js_sys::Date::now());
        let db = BrowserDb::open_paged(&db_name)
            .await
            .expect("open empty paged import target");
        db.import_graph(js_sys::Uint8Array::from(bytes.as_slice()))
            .await
            .expect("import into paged target");
        {
            let inner = db.inner.borrow();
            inner.pfs.with_backend(|backend| {
                assert!(backend.is_sparse());
                assert_eq!(backend.resident_page_count(), backend.pinned_page_count());
                assert!(
                    u64::try_from(backend.resident_page_count()).unwrap()
                        < backend.page_count_raw().unwrap(),
                    "successful import must not retain the full input image"
                );
            });
        }
        let result = db
            .execute("(query [:find ?v :where [:sparse/target :sparse/value ?v]])".to_string())
            .await
            .expect("lazy query after paged import");
        let result: serde_json::Value = serde_json::from_str(&result).expect("import query JSON");
        assert_eq!(result["results"], serde_json::json!([[175]]));
        assert_eq!(
            db.export_graph_async()
                .await
                .expect("async export after paged import")
                .to_vec(),
            bytes
        );
    }

    #[wasm_bindgen_test]
    async fn paged_successive_writes_and_forget_remain_atomic_after_reopen() {
        let bytes = build_sparse_v11_fixture(220).await;
        let db_name = format!("vicia-paged-writes-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open successive-write seed");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed successive-write graph");
        let db = BrowserDb::open_paged(&db_name)
            .await
            .expect("open successive-write paged graph");

        let write: serde_json::Value = serde_json::from_str(
            &db.execute("(transact [[:paged/new :state :active]])".to_string())
                .await
                .expect("paged append"),
        )
        .expect("paged append JSON");
        let write_tx = write["tx_count"].as_u64().expect("write tx_count");
        let forget: serde_json::Value = serde_json::from_str(
            &db.execute("(forget [[:paged/new :state :active]])".to_string())
                .await
                .expect("paged forget"),
        )
        .expect("paged forget JSON");
        assert_eq!(forget["forgotten"], 1);
        assert_eq!(forget["tx_count"].as_u64(), Some(write_tx + 1));

        let current = db
            .execute("(query [:find ?v :where [:paged/new :state ?v]])".to_string())
            .await
            .expect("current after forget");
        let current: serde_json::Value = serde_json::from_str(&current).expect("current JSON");
        assert_eq!(current["results"], serde_json::json!([]));
        drop(db);

        let reopened = BrowserDb::open_paged(&db_name)
            .await
            .expect("reopen successive-write graph");
        let historical = reopened
            .execute(format!(
                "(query [:find ?v :as-of {write_tx} :where [:paged/new :state ?v]])"
            ))
            .await
            .expect("historical after reopen");
        let historical: serde_json::Value =
            serde_json::from_str(&historical).expect("historical JSON");
        assert_eq!(
            historical["results"],
            serde_json::json!([[{"$kw": ":active"}]])
        );
    }

    #[wasm_bindgen_test]
    async fn paged_explicit_full_scan_releases_bulk_staging() {
        let bytes = build_sparse_v11_fixture(180).await;
        let db_name = format!("vicia-paged-full-scan-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open full-scan seed");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed full-scan graph");
        let db = BrowserDb::open_paged_from_idb(idb.clone_handle())
            .await
            .expect("open full-scan paged graph");
        idb.reset_read_counters_for_test();

        let result = db
            .execute("(query [:find ?e :where [?e ?a ?v]])".to_string())
            .await
            .expect("explicit full scan");
        let result: serde_json::Value = serde_json::from_str(&result).expect("full scan JSON");
        assert_eq!(result["results"].as_array().map(Vec::len), Some(180));
        assert!(idb.read_counters_for_test().pages_returned > 0);
        let inner = db.inner.borrow();
        inner.pfs.with_backend(|backend| {
            assert_eq!(backend.resident_page_count(), backend.pinned_page_count());
        });
    }

    #[wasm_bindgen_test]
    async fn paged_forced_maintenance_returns_to_sparse_residency() {
        let bytes = build_sparse_v11_fixture(240).await;
        let db_name = format!("vicia-paged-maintenance-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open maintenance seed");
        idb.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed maintenance graph");
        let db = BrowserDb::open_paged(&db_name)
            .await
            .expect("open paged maintenance graph");
        for index in 0..4 {
            db.execute(format!("(transact [[:paged/delta-{index} :n {index}]])"))
                .await
                .expect("append maintenance delta");
        }

        db.begin_mutation().expect("force maintenance guard");
        let result = maintenance::run_idle_maintenance(&db, true).await;
        db.finish_mutation(false);
        db.evict_sparse_staging();
        let result = result.expect("forced paged maintenance");
        let result: serde_json::Value = serde_json::from_str(&result).expect("maintenance JSON");
        assert_eq!(result["delta"], "recompacted");
        {
            let inner = db.inner.borrow();
            inner.pfs.with_backend(|backend| {
                assert!(backend.is_sparse());
                assert_eq!(backend.resident_page_count(), backend.pinned_page_count());
                assert!(
                    u64::try_from(backend.resident_page_count()).unwrap()
                        < backend.page_count_raw().unwrap()
                );
            });
        }
        let query = db
            .execute("(query [:find ?n :where [:paged/delta-3 :n ?n]])".to_string())
            .await
            .expect("query after paged maintenance");
        let query: serde_json::Value =
            serde_json::from_str(&query).expect("maintenance query JSON");
        assert_eq!(query["results"], serde_json::json!([[3]]));
    }

    #[wasm_bindgen_test]
    async fn gate_e_browser_consumer_matches_both_producers_and_round_trips() {
        let corpus = corpus();
        for source in [NATIVE_FIXTURE, BROWSER_FIXTURE] {
            assert_eq!(
                u32::from_le_bytes(source[4..8].try_into().unwrap()),
                10,
                "frozen Gate E producer fixtures remain v10 migration inputs"
            );
            let db = BrowserDb::open_in_memory().expect("open Gate E browser consumer");
            db.import_graph(js_sys::Uint8Array::from(source))
                .await
                .expect("import Gate E fixture");
            assert_browser_queries(&db, &corpus.queries).await;

            let exported = db.export_graph().expect("export Gate E fixture").to_vec();
            assert_eq!(
                u32::from_le_bytes(exported[4..8].try_into().unwrap()),
                crate::storage::FORMAT_VERSION,
                "first import must publish the current format"
            );
            let reopened = BrowserDb::open_in_memory().expect("open round-trip consumer");
            reopened
                .import_graph(js_sys::Uint8Array::from(exported.as_slice()))
                .await
                .expect("reimport browser export");
            assert_browser_queries(&reopened, &corpus.queries).await;
            assert_eq!(
                reopened
                    .export_graph()
                    .expect("re-export current graph")
                    .to_vec(),
                exported,
                "current-format browser round-trip must be byte exact"
            );
        }
    }

    #[wasm_bindgen_test]
    async fn open_migrates_v10_and_persists_v11_before_return() {
        let db_name = format!("vicia-open-v10-migration-{}", js_sys::Date::now());
        let seed = IndexedDbBackend::open(&db_name)
            .await
            .expect("open seed IDB");
        seed.replace_all_pages(fixture_pages(NATIVE_FIXTURE))
            .await
            .expect("seed frozen v10 fixture");
        assert_eq!(
            page_map_version(&seed.load_all_pages().await.expect("load v10 seed")),
            10
        );

        let db = BrowserDb::open(&db_name)
            .await
            .expect("open must migrate and durably publish v11");
        let corpus = corpus();
        assert_browser_queries(&db, &corpus.queries).await;
        let durable_inspector = IndexedDbBackend::open(&db_name)
            .await
            .expect("reopen migrated inspector");
        let durable = durable_inspector
            .load_all_pages()
            .await
            .expect("load durable migrated pages");
        assert_eq!(page_map_version(&durable), crate::storage::FORMAT_VERSION);
        let declared = u64::from_le_bytes(durable[&0][8..16].try_into().unwrap());
        assert_eq!(
            u64::try_from(durable.len()).unwrap(),
            declared,
            "open must commit every page in the migrated published prefix"
        );
        drop(db);

        let reopened = BrowserDb::open(&db_name)
            .await
            .expect("durable v11 graph must reopen");
        assert_browser_queries(&reopened, &corpus.queries).await;
        let second_open_inspector = IndexedDbBackend::open(&db_name)
            .await
            .expect("reopen second-open inspector");
        assert_eq!(
            second_open_inspector
                .load_all_pages()
                .await
                .expect("load second-open pages"),
            durable,
            "second open must not remigrate, grow, or rewrite the v11 image"
        );
    }

    #[wasm_bindgen_test]
    async fn open_paged_migrates_complete_v10_then_returns_sparse() {
        let db_name = format!("vicia-open-paged-v10-migration-{}", js_sys::Date::now());
        let seed = IndexedDbBackend::open(&db_name)
            .await
            .expect("open paged migration seed");
        seed.replace_all_pages(fixture_pages(NATIVE_FIXTURE))
            .await
            .expect("seed complete v10 fixture");

        let db = BrowserDb::open_paged(&db_name)
            .await
            .expect("complete legacy image must migrate before sparse return");
        {
            let inner = db.inner.borrow();
            assert!(inner.paged);
            inner
                .pfs
                .with_backend(|backend| assert!(backend.is_sparse()));
        }
        assert_browser_queries(&db, &corpus().queries).await;
        let durable = IndexedDbBackend::open(&db_name)
            .await
            .expect("inspect paged migration")
            .load_all_pages()
            .await
            .expect("load paged migration image");
        assert_eq!(page_map_version(&durable), crate::storage::FORMAT_VERSION);
    }

    #[wasm_bindgen_test]
    async fn open_paged_migrates_complete_v11_then_returns_sparse() {
        let db_name = format!("vicia-open-paged-v11-migration-{}", js_sys::Date::now());
        let mut bytes = build_sparse_v11_fixture(240).await;
        downgrade_current_fixture_to_v11(&mut bytes);
        let seed = IndexedDbBackend::open(&db_name)
            .await
            .expect("open v11 migration seed");
        seed.replace_all_pages(fixture_pages(&bytes))
            .await
            .expect("seed complete v11 fixture");

        let db = BrowserDb::open_paged(&db_name)
            .await
            .expect("complete v11 image must upgrade before sparse return");
        {
            let inner = db.inner.borrow();
            assert!(inner.paged);
            inner
                .pfs
                .with_backend(|backend| assert!(backend.is_sparse()));
        }
        let query = db
            .execute("(query [:find ?value :where [:sparse/target :sparse/value ?value]])".into())
            .await
            .expect("query migrated v11 fixture");
        let query: serde_json::Value =
            serde_json::from_str(&query).expect("migrated v11 query JSON");
        assert_eq!(query["results"], serde_json::json!([[120]]));
        let durable = IndexedDbBackend::open(&db_name)
            .await
            .expect("inspect v11 migration")
            .load_all_pages()
            .await
            .expect("load migrated v11 image");
        assert_eq!(page_map_version(&durable), crate::storage::FORMAT_VERSION);
    }

    #[wasm_bindgen_test]
    async fn open_migration_write_abort_preserves_exact_v10_image() {
        let db_name = format!("vicia-open-v10-abort-{}", js_sys::Date::now());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open seed IDB");
        idb.replace_all_pages(fixture_pages(NATIVE_FIXTURE))
            .await
            .expect("seed frozen v10 fixture");
        let before = idb
            .load_all_pages()
            .await
            .expect("load pre-migration pages");
        assert_eq!(page_map_version(&before), 10);

        idb.fail_next_write_for_test();
        let result = BrowserDb::open_from_idb(idb.clone_handle()).await;
        assert!(
            result.is_err(),
            "open must not expose a handle when migration durability aborts"
        );
        assert_eq!(
            idb.load_all_pages().await.expect("load aborted pages"),
            before,
            "aborted migration transaction must preserve the exact v10 authority"
        );

        let recovered = BrowserDb::open(&db_name)
            .await
            .expect("later normal open must still migrate the preserved v10 graph");
        let corpus = corpus();
        assert_browser_queries(&recovered, &corpus.queries).await;
        let recovered_inspector = IndexedDbBackend::open(&db_name)
            .await
            .expect("reopen recovered inspector");
        assert_eq!(
            page_map_version(
                &recovered_inspector
                    .load_all_pages()
                    .await
                    .expect("load recovered pages"),
            ),
            crate::storage::FORMAT_VERSION
        );
    }

    #[wasm_bindgen_test]
    async fn strict_paged_import_migrates_complete_v10_and_reopens_sparse() {
        assert_eq!(
            u32::from_le_bytes(NATIVE_FIXTURE[4..8].try_into().unwrap()),
            crate::storage::header_extension::LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION,
            "strict import premise requires the frozen v10 fixture"
        );
        let db_name = format!("vicia-strict-paged-v10-{}", js_sys::Date::now());
        let db = BrowserDb::open(&db_name)
            .await
            .expect("open strict import target");

        db.import_graph_for_paged_access(js_sys::Uint8Array::from(NATIVE_FIXTURE))
            .await
            .expect("complete v10 graph must migrate and pass strict paged validation");

        let durable = IndexedDbBackend::open(&db_name)
            .await
            .expect("open strict import inspector")
            .load_all_pages()
            .await
            .expect("load strict imported pages");
        assert_eq!(page_map_version(&durable), crate::storage::FORMAT_VERSION);
        drop(db);

        let fresh = BrowserDb::open_paged(&db_name)
            .await
            .expect("fresh openPaged must accept strict import");
        {
            let inner = fresh.inner.borrow();
            assert!(inner.paged, "strict import must reopen through sparse mode");
            inner
                .pfs
                .with_backend(|backend| assert!(backend.is_sparse()));
        }
        assert_browser_queries(&fresh, &corpus().queries).await;
    }

    #[wasm_bindgen_test]
    async fn export_rejects_corrupt_v11_base_page() {
        use crate::storage::StorageBackend;

        let db = BrowserDb::open_in_memory().expect("open in-memory consumer");
        db.import_graph(js_sys::Uint8Array::from(NATIVE_FIXTURE))
            .await
            .expect("import and migrate fixture");
        let exported = db.export_graph().expect("export migrated fixture").to_vec();
        let base_start = u64::from_le_bytes(
            exported[BASE_FACT_PAGE_START_OFFSET..BASE_FACT_PAGE_START_OFFSET + 8]
                .try_into()
                .unwrap(),
        );
        {
            let mut inner = db.inner.borrow_mut();
            inner.pfs.with_backend_mut(|backend| {
                let mut page = backend.read_page_raw(base_start).unwrap();
                page[crate::storage::PAGE_SIZE - 1] ^= 0x01;
                backend.write_page(base_start, &page).unwrap();
            });
        }

        assert!(
            db.export_graph().is_err(),
            "browser export must pass base pages through the v11 catalog boundary"
        );
    }

    #[wasm_bindgen_test]
    async fn gate_e_browser_corruption_contract_matches_shared_corpus() {
        let corpus = corpus();
        let mut ordinal = 0u32;
        for (producer, source) in [("native", NATIVE_FIXTURE), ("browser", BROWSER_FIXTURE)] {
            for case in &corpus.corruptions {
                ordinal += 1;
                let mutated = apply_mutation(source, &case.mutation)
                    .expect("Gate E corruption mutation must apply");
                let db_name = format!(
                    "vicia-gate-e-corruption-{producer}-{}-{}-{ordinal}",
                    case.id,
                    js_sys::Date::now()
                );
                let db = BrowserDb::open(&db_name).await.expect("open sentinel DB");
                db.execute("(transact [[:sentinel :value \"preserved\"]])".to_string())
                    .await
                    .expect("write durable sentinel");

                match case.expected.as_str() {
                    "reject" => {
                        assert_rejected_import_preserves_sentinel(&db, &db_name, case, mutated)
                            .await;
                    }
                    "recover_previous" => {
                        db.import_graph(js_sys::Uint8Array::from(mutated.as_slice()))
                            .await
                            .expect("browser must recover previous manifest");
                        let previous_queries: Vec<QueryCase> = corpus
                            .queries
                            .iter()
                            .filter(|query| query.id != "current_retracted_edge_absent")
                            .cloned()
                            .collect();
                        assert_browser_queries(&db, &previous_queries).await;
                        let probe = case.probe.as_ref().expect("fallback case must carry probe");
                        assert_browser_probe(&db, probe, &case.id).await;
                        let round_trip = db.export_graph();
                        if case.exportable {
                            let fresh =
                                BrowserDb::open_in_memory().expect("open fallback reimport");
                            fresh
                                .import_graph(round_trip.expect("complete fallback must export"))
                                .await
                                .expect("reimport recovered graph");
                            assert_browser_probe(&fresh, probe, &case.id).await;
                        } else {
                            assert!(
                                round_trip.is_err(),
                                "physically truncated fallback must not overclaim exportability: {}",
                                case.id
                            );
                        }
                    }
                    "recover_latest" => {
                        let legacy_published =
                            published_byte_len(source).expect("legacy published length");
                        assert!(
                            mutated.len() > legacy_published,
                            "tail case must grow the physical image"
                        );
                        db.import_graph(js_sys::Uint8Array::from(mutated.as_slice()))
                            .await
                            .expect("browser must ignore unpublished tail");
                        assert_browser_queries(&db, &corpus.queries).await;
                        let exported = db.export_graph().expect("export trimmed graph").to_vec();
                        let published = published_byte_len(&exported)
                            .expect("migrated published source length");
                        assert_eq!(
                            exported.len(),
                            published,
                            "browser export must exclude unpublished tail"
                        );
                        assert_eq!(
                            exported
                                .get(legacy_published..legacy_published + 8)
                                .expect("migration catalog magic must be published"),
                            b"MGPGC001",
                            "v11 catalog must replace, not publish, the legacy tail page"
                        );
                        let idb = IndexedDbBackend::open(&db_name)
                            .await
                            .expect("open IDB probe");
                        assert_eq!(
                            idb.load_all_pages().await.expect("load IDB pages").len(),
                            published / crate::storage::PAGE_SIZE,
                            "atomic import must not persist unpublished tail"
                        );
                    }
                    other => panic!("unknown corruption expectation: {other}"),
                }
            }
        }
    }

    #[wasm_bindgen_test]
    async fn paged_import_preserves_truncated_legacy_recovery_without_eager_open_downgrade() {
        let corpus = corpus();
        let previous_queries: Vec<QueryCase> = corpus
            .queries
            .iter()
            .filter(|query| query.id != "current_retracted_edge_absent")
            .cloned()
            .collect();
        let mut ordinal = 0u32;

        for case in corpus
            .corruptions
            .iter()
            .filter(|case| case.expected == "recover_previous" && !case.exportable)
        {
            ordinal += 1;
            let mutated = apply_mutation(NATIVE_FIXTURE, &case.mutation)
                .expect("truncated recovery mutation must apply");
            let db_name = format!(
                "vicia-paged-truncated-import-{}-{}-{ordinal}",
                case.id,
                js_sys::Date::now()
            );
            let db = BrowserDb::open_paged(&db_name)
                .await
                .expect("open paged recovery target");
            db.import_graph(js_sys::Uint8Array::from(mutated.as_slice()))
                .await
                .expect("paged import must preserve the older recoverable lineage");

            {
                let inner = db.inner.borrow();
                assert_eq!(inner.open_mode, BrowserOpenMode::Paged);
                assert!(
                    !inner.paged,
                    "incomplete v10 recovery must remain eager/read-only, never sparse without integrity metadata"
                );
            }
            assert_browser_queries(&db, &previous_queries).await;
            let probe = case.probe.as_ref().expect("fallback case must carry probe");
            assert_browser_probe(&db, probe, &case.id).await;
            assert!(
                db.export_graph_async().await.is_err(),
                "a physically incomplete recovered image must remain non-exportable"
            );
            drop(db);

            let eager = BrowserDb::open(&db_name)
                .await
                .expect("eager compatibility open must preserve legacy recovery");
            assert_browser_probe(&eager, probe, &case.id).await;
            drop(eager);
            assert!(
                BrowserDb::open_paged(&db_name).await.is_err(),
                "bounded open must reject instead of silently retaining the entire incomplete legacy image"
            );
        }
    }

    #[wasm_bindgen_test]
    async fn strict_paged_import_rejects_truncated_recovery_and_preserves_exact_authority() {
        let corpus = corpus();
        let mut ordinal = 0u32;

        for (producer, source) in [("native", NATIVE_FIXTURE), ("browser", BROWSER_FIXTURE)] {
            for case in corpus
                .corruptions
                .iter()
                .filter(|case| case.expected == "recover_previous" && !case.exportable)
            {
                ordinal += 1;
                let mutated = apply_mutation(source, &case.mutation)
                    .expect("truncated recovery mutation must apply");
                let db_name = format!(
                    "vicia-strict-paged-reject-{producer}-{}-{}-{ordinal}",
                    case.id,
                    js_sys::Date::now()
                );
                let db = BrowserDb::open(&db_name)
                    .await
                    .expect("open strict sentinel target");
                db.execute("(transact [[:sentinel :value \"preserved\"]])".to_string())
                    .await
                    .expect("write strict sentinel");

                let inspector = IndexedDbBackend::open(&db_name)
                    .await
                    .expect("open strict target inspector");
                let before = inspector
                    .load_all_pages()
                    .await
                    .expect("snapshot strict target pages");

                let error = db
                    .import_graph_for_paged_access(js_sys::Uint8Array::from(mutated.as_slice()))
                    .await
                    .expect_err("strict import must reject non-exportable recovery");
                assert!(
                    js_value_message(&error).contains("strict paged import"),
                    "strict rejection must identify the strict boundary: {}",
                    case.id
                );

                let live = db
                    .execute("(query [:find ?v :where [:sentinel :value ?v]])".to_string())
                    .await
                    .expect("query live sentinel after strict rejection");
                let live: serde_json::Value =
                    serde_json::from_str(&live).expect("live sentinel JSON");
                assert_eq!(live["results"], serde_json::json!([["preserved"]]));

                assert_eq!(
                    inspector
                        .load_all_pages()
                        .await
                        .expect("load pages after strict rejection"),
                    before,
                    "strict rejection must preserve exact IndexedDB pages: {producer}/{}",
                    case.id
                );

                let fresh = BrowserDb::open_paged(&db_name)
                    .await
                    .expect("fresh openPaged must retain prior authority");
                {
                    let inner = fresh.inner.borrow();
                    assert!(inner.paged, "fresh prior authority must remain sparse");
                }
                let durable = fresh
                    .execute("(query [:find ?v :where [:sentinel :value ?v]])".to_string())
                    .await
                    .expect("query durable sentinel after strict rejection");
                let durable: serde_json::Value =
                    serde_json::from_str(&durable).expect("durable sentinel JSON");
                assert_eq!(durable["results"], serde_json::json!([["preserved"]]));
            }
        }
    }

    #[wasm_bindgen_test]
    async fn in_memory_transact_and_query() {
        let db = BrowserDb::open_in_memory().expect("open_in_memory");
        let transact_result = db
            .execute(r#"(transact [[:alice :name "Alice"] [:alice :age 30]])"#.to_string())
            .await
            .expect("transact");
        let v: serde_json::Value = serde_json::from_str(&transact_result).unwrap();
        assert!(v.get("transacted").is_some());

        let query_result = db
            .execute(r#"(query [:find ?name :where [:alice :name ?name]])"#.to_string())
            .await
            .expect("query");
        let v: serde_json::Value = serde_json::from_str(&query_result).unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0][0], serde_json::Value::String("Alice".into()));
    }

    #[wasm_bindgen_test]
    async fn atomic_mixed_write_replaces_head_and_shares_transaction_metadata() {
        let db = BrowserDb::open_in_memory().expect("open atomic in-memory database");
        db.execute(r#"(transact [[:head :value "old"]])"#.to_string())
            .await
            .expect("seed old head");

        let receipt = db
            .execute_atomic(vec![
                r#"(retract [[:head :value "old"]])"#.to_string(),
                r#"(transact [[:head :value "new"] [:event :kind "replace"]])"#.to_string(),
            ])
            .await
            .expect("commit mixed atomic write");
        let receipt: serde_json::Value =
            serde_json::from_str(&receipt).expect("atomic receipt JSON");
        assert_eq!(receipt["atomic"], true);
        assert_eq!(receipt["command_count"], 2);
        assert_eq!(receipt["fact_count"], 3);
        assert_eq!(receipt["transacted_fact_count"], 2);
        assert_eq!(receipt["retracted_fact_count"], 1);
        assert_eq!(receipt["transacted"], receipt["tx_id"]);
        assert_eq!(receipt["durability"], "memory");

        let head = db
            .execute("(query [:find ?value :where [:head :value ?value]])".to_string())
            .await
            .expect("query replaced head");
        let head: serde_json::Value = serde_json::from_str(&head).expect("head query JSON");
        assert_eq!(head["results"], serde_json::json!([["new"]]));

        let metadata = db
            .execute(
                r#"(query [:find ?tx ?tc :any-valid-time :where [:head :value "new"] [:head :db/tx-id ?tx] [:head :db/tx-count ?tc] [:event :kind "replace"] [:event :db/tx-id ?tx] [:event :db/tx-count ?tc]])"#
                    .to_string(),
            )
            .await
            .expect("query shared atomic metadata");
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata).expect("metadata query JSON");
        assert_eq!(metadata["results"].as_array().map(Vec::len), Some(1));
        assert_eq!(metadata["results"][0][0], receipt["tx_id"]);
        assert_eq!(metadata["results"][0][1], receipt["tx_count"]);
    }

    #[wasm_bindgen_test]
    async fn atomic_write_rejects_invalid_commands_without_mutation() {
        let db = BrowserDb::open_in_memory().expect("open atomic rejection database");
        db.execute("(transact [[:stable :value :old]])".to_string())
            .await
            .expect("seed stable fact");
        let before_tx_count = db.inner.borrow().fact_storage.current_tx_count();

        let error = db
            .execute_atomic(vec![
                "(transact [[:partial :value :must-not-appear]])".to_string(),
                "(query [:find ?value :where [:stable :value ?value]])".to_string(),
            ])
            .await
            .expect_err("query in atomic write must be rejected");
        assert!(js_value_message(&error).contains("only transact and retract"));
        assert_eq!(
            db.inner.borrow().fact_storage.current_tx_count(),
            before_tx_count,
            "preparation failure must not consume a transaction counter"
        );

        let partial = db
            .execute("(query [:find ?value :where [:partial :value ?value]])".to_string())
            .await
            .expect("query rejected partial fact");
        let partial: serde_json::Value =
            serde_json::from_str(&partial).expect("partial query JSON");
        assert_eq!(partial["results"], serde_json::json!([]));
        let ordering_error = db
            .execute_atomic(vec![
                "(retract [[:stable :value :old]])".to_string(),
                "(transact [[:stable :value :old]])".to_string(),
            ])
            .await
            .expect_err("same-fact ordering must be rejected before mutation");
        assert!(
            js_value_message(&ordering_error).contains("fact order is intentionally undefined")
        );
        assert_eq!(
            db.inner.borrow().fact_storage.current_tx_count(),
            before_tx_count,
            "same-fact rejection must not consume a transaction counter"
        );
        assert!(db.execute_atomic(Vec::new()).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn atomic_indexeddb_failure_restores_previous_live_and_durable_head() {
        let db_name = format!("vicia-atomic-write-abort-{}", js_sys::Date::now());
        let db = BrowserDb::open_paged(&db_name)
            .await
            .expect("open atomic persistent database");
        db.execute(r#"(transact [[:head :value "old"]])"#.to_string())
            .await
            .expect("publish old head");
        let before_tx_count = db.inner.borrow().fact_storage.current_tx_count();
        db.inner
            .borrow()
            .idb
            .as_ref()
            .expect("persistent handle")
            .fail_next_write_for_test();

        let error = db
            .execute_atomic(vec![
                r#"(retract [[:head :value "old"]])"#.to_string(),
                r#"(transact [[:head :value "new"]])"#.to_string(),
            ])
            .await
            .expect_err("injected IndexedDB failure must reject atomic write");
        assert!(js_value_message(&error).contains("injected IndexedDB"));
        assert!(!db.inner.borrow().durability_poisoned);
        assert_eq!(
            db.inner.borrow().fact_storage.current_tx_count(),
            before_tx_count,
            "failed atomic publication must restore the transaction counter"
        );

        let live = db
            .execute("(query [:find ?value :where [:head :value ?value]])".to_string())
            .await
            .expect("query restored live head");
        let live: serde_json::Value = serde_json::from_str(&live).expect("live head JSON");
        assert_eq!(live["results"], serde_json::json!([["old"]]));
        drop(db);

        let reopened = BrowserDb::open_paged(&db_name)
            .await
            .expect("reopen atomic persistent database");
        assert_eq!(
            reopened.inner.borrow().fact_storage.current_tx_count(),
            before_tx_count
        );
        let durable = reopened
            .execute("(query [:find ?value :where [:head :value ?value]])".to_string())
            .await
            .expect("query durable old head");
        let durable: serde_json::Value = serde_json::from_str(&durable).expect("durable head JSON");
        assert_eq!(durable["results"], serde_json::json!([["old"]]));
    }

    #[wasm_bindgen_test]
    async fn per_fact_transaction_metadata_survives_browser_query_planning() {
        let db = BrowserDb::open_in_memory().expect("open in-memory metadata database");
        db.execute(r#"(transact [[:timeline :event/first "one"]])"#.to_string())
            .await
            .expect("write first event fact");
        db.execute(r#"(transact [[:timeline :event/second "two"]])"#.to_string())
            .await
            .expect("write second event fact");

        let query = db
            .execute(
                r#"(query [:find ?first-tx ?second-tx
                            :any-valid-time
                            :where [:timeline :event/first "one"]
                                   [:timeline :db/tx-count ?first-tx]
                                   [:timeline :event/second "two"]
                                   [:timeline :db/tx-count ?second-tx]])"#
                    .to_string(),
            )
            .await
            .expect("query exact fact transaction metadata");
        let query: serde_json::Value = serde_json::from_str(&query).expect("query JSON");
        assert_eq!(query["results"], serde_json::json!([[1, 2]]));

        db.execute(r#"(transact [[:expr-meta :value/n 99]])"#.to_string())
            .await
            .expect("write expression input");
        for query in [
            r#"(query [:find ?expected :any-valid-time
                       :where [:expr-meta :value/n ?n]
                              [(+ ?n 1) ?expected]
                              [:expr-meta :db/tx-count ?expected]])"#,
            r#"(query [:find ?expected :any-valid-time
                       :where [:expr-meta :value/n ?n]
                              [:expr-meta :db/tx-count ?expected]
                              [(+ ?n 1) ?expected]])"#,
        ] {
            let result = db
                .execute(query.to_string())
                .await
                .expect("query expression binding conflict");
            let result: serde_json::Value =
                serde_json::from_str(&result).expect("expression query JSON");
            assert_eq!(result["results"], serde_json::json!([]));
        }

        db.execute(r#"(rule [(derived ?e ?v) [?e :event/first ?v]])"#.to_string())
            .await
            .expect("register derived event rule");
        let derived_only = db
            .execute(
                r#"(query [:find ?tx :any-valid-time
                           :where (derived :timeline "one")
                                  [:timeline :db/tx-count ?tx]])"#
                    .to_string(),
            )
            .await
            .expect("query derived metadata");
        let mixed = db
            .execute(
                r#"(query [:find ?tx :any-valid-time
                           :where [:timeline :event/first "one"]
                                  (derived :timeline "one")
                                  [:timeline :db/tx-count ?tx]])"#
                    .to_string(),
            )
            .await
            .expect("query mixed base and derived metadata");
        let derived_only: serde_json::Value =
            serde_json::from_str(&derived_only).expect("derived query JSON");
        let mixed: serde_json::Value = serde_json::from_str(&mixed).expect("mixed query JSON");
        assert_eq!(
            derived_only["results"]
                .as_array()
                .expect("derived result rows")
                .len(),
            1
        );
        assert_eq!(mixed["results"], derived_only["results"]);
        assert_ne!(derived_only["results"], serde_json::json!([[1]]));
    }

    #[wasm_bindgen_test]
    async fn empty_database_exports_a_canonical_round_trippable_graph() {
        let memory = BrowserDb::open_in_memory().expect("open empty memory database");
        let memory_blob = memory
            .export_graph()
            .expect("export canonical empty memory graph");
        assert_eq!(memory_blob.length(), crate::storage::PAGE_SIZE as u32);
        let memory_round_trip = BrowserDb::open_in_memory().expect("open memory round trip");
        memory_round_trip
            .import_graph(memory_blob)
            .await
            .expect("reimport canonical empty memory graph");

        let db_name = format!("vicia-empty-paged-export-{}", js_sys::Date::now());
        let paged = BrowserDb::open_paged(&db_name)
            .await
            .expect("open empty paged database");
        let paged_blob = paged
            .export_graph_async()
            .await
            .expect("export canonical empty paged graph");
        assert_eq!(paged_blob.length(), crate::storage::PAGE_SIZE as u32);
        let fresh = BrowserDb::open_in_memory().expect("open paged export consumer");
        fresh
            .import_graph(paged_blob)
            .await
            .expect("reimport canonical empty paged graph");
    }

    #[wasm_bindgen_test]
    async fn in_memory_forget_preserves_history() {
        let db = BrowserDb::open_in_memory().expect("open_in_memory");
        db.execute(
            r#"(transact {:valid-from "2026-01-01"} [[:alice :status :active]])"#.to_string(),
        )
        .await
        .expect("transact");

        let forgotten = db
            .execute(r#"(forget {:valid-to "2026-06-01"} [[:alice :status :active]])"#.to_string())
            .await
            .expect("forget");
        let forgotten: serde_json::Value = serde_json::from_str(&forgotten).unwrap();
        assert_eq!(forgotten["forgotten"], 1);
        assert!(forgotten["tx_id"].is_u64());

        let current = db
            .execute(r#"(query [:find ?s :where [:alice :status ?s]])"#.to_string())
            .await
            .expect("current query");
        let current: serde_json::Value = serde_json::from_str(&current).unwrap();
        assert_eq!(current["results"].as_array().unwrap().len(), 0);

        let history = db
            .execute(
                r#"(query [:find ?s :valid-at "2026-03-01" :where [:alice :status ?s]])"#
                    .to_string(),
            )
            .await
            .expect("history query");
        let history: serde_json::Value = serde_json::from_str(&history).unwrap();
        assert_eq!(history["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn empty_query_returns_empty_results() {
        let db = BrowserDb::open_in_memory().expect("open_in_memory");
        let result = db
            .execute(r#"(query [:find ?e :where [?e :name _]])"#.to_string())
            .await
            .expect("query");
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 0);
    }

    #[wasm_bindgen_test]
    async fn export_import_round_trip() {
        let db = BrowserDb::open_in_memory().expect("open");
        db.execute(r#"(transact [[:bob :role "admin"]])"#.to_string())
            .await
            .expect("transact");

        let blob = db.export_graph().expect("export");
        let bytes = blob.to_vec();
        assert_eq!(
            &bytes[0..4],
            b"MGRF",
            "exported blob must start with MGRF magic"
        );

        let db2 = BrowserDb::open_in_memory().expect("open2");
        db2.import_graph(blob).await.expect("import");

        let result = db2
            .execute(r#"(query [:find ?role :where [:bob :role ?role]])"#.to_string())
            .await
            .expect("query after import");
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0][0], serde_json::Value::String("admin".into()));
    }

    #[wasm_bindgen_test]
    async fn export_size_is_page_aligned() {
        let db = BrowserDb::open_in_memory().expect("open");
        db.execute(r#"(transact [[:e :v 1]])"#.to_string())
            .await
            .expect("transact");
        let blob = db.export_graph().expect("export");
        assert_eq!(blob.byte_length() as usize % crate::storage::PAGE_SIZE, 0);
    }

    /// Load the committed binary fixture (produced by `cargo run --example
    /// generate_compat_fixture` from the native build) into a `BrowserDb` via
    /// `import_graph` and verify the known facts are queryable.
    ///
    /// This is the **native → browser** direction of the cross-platform
    /// compatibility test.  The companion native side lives in
    /// `tests/cross_platform_compat_test.rs`.
    #[wasm_bindgen_test]
    async fn native_fixture_readable_by_browser_db() {
        let fixture: &[u8] = include_bytes!("../../tests/fixtures/compat.graph");
        let db = BrowserDb::open_in_memory().expect("open in-memory");
        let arr = js_sys::Uint8Array::from(fixture);
        db.import_graph(arr).await.expect("import native fixture");

        let r = db
            .execute(r#"(query [:find ?name :where [?e :name ?name]])"#.to_string())
            .await
            .expect("query name");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(
            results.len(),
            1,
            "expected 1 name result from native fixture"
        );
        assert_eq!(results[0][0], serde_json::Value::String("Alice".into()));

        let r2 = db
            .execute("(query [:find ?age :where [?e :age ?age]])".to_string())
            .await
            .expect("query age");
        let v2: serde_json::Value = serde_json::from_str(&r2).unwrap();
        let results2 = v2["results"].as_array().unwrap();
        assert_eq!(
            results2.len(),
            1,
            "expected 1 age result from native fixture"
        );
        assert_eq!(results2[0][0], serde_json::Value::Number(30.into()));
    }

    #[wasm_bindgen_test]
    async fn import_unparseable_complete_page_prefix_rejected_keeps_data() {
        let db = BrowserDb::open_in_memory().expect("open");
        db.execute(r#"(transact [[:dana :team "core"]])"#.to_string())
            .await
            .expect("transact");

        let bad = js_sys::Uint8Array::new_with_length((crate::storage::PAGE_SIZE + 1) as u32);
        let result = db.import_graph(bad).await;
        assert!(
            result.is_err(),
            "unparseable complete-page prefix must be rejected despite a partial tail"
        );

        let r = db
            .execute(r#"(query [:find ?t :where [:dana :team ?t]])"#.to_string())
            .await
            .expect("query after failed import");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn import_empty_blob_rejected() {
        let db = BrowserDb::open_in_memory().expect("open");
        db.execute(r#"(transact [[:dana :team "core"]])"#.to_string())
            .await
            .expect("transact");

        let empty = js_sys::Uint8Array::new_with_length(0);
        let result = db.import_graph(empty).await;
        assert!(result.is_err(), "empty blob must be rejected");

        let r = db
            .execute(r#"(query [:find ?t :where [:dana :team ?t]])"#.to_string())
            .await
            .expect("query after failed import");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn import_corrupt_blob_rejects_and_preserves_idb() {
        let db_name = "minigraf-test-import-corrupt";

        let db = BrowserDb::open(db_name).await.expect("open");
        db.execute(r#"(transact [[:erin :lang "rust"]])"#.to_string())
            .await
            .expect("transact");

        let garbage = js_sys::Uint8Array::from(vec![0xABu8; crate::storage::PAGE_SIZE].as_slice());
        let result = db.import_graph(garbage).await;
        assert!(result.is_err(), "bad-magic blob must be rejected");

        // Live handle unaffected.
        let r = db
            .execute(r#"(query [:find ?l :where [:erin :lang ?l]])"#.to_string())
            .await
            .expect("query after failed import");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 1);
        drop(db);

        // Durable state unaffected: a reopen still sees the old fact.
        let db2 = BrowserDb::open(db_name).await.expect("reopen");
        let r2 = db2
            .execute(r#"(query [:find ?l :where [:erin :lang ?l]])"#.to_string())
            .await
            .expect("query after reopen");
        let v2: serde_json::Value = serde_json::from_str(&r2).unwrap();
        assert_eq!(v2["results"].as_array().unwrap().len(), 1);
    }

    /// Regression test for the swap-before-flush ordering defect: when the
    /// IndexedDB flush fails, the live in-memory database must remain on the
    /// OLD data, otherwise later incremental writes tear the durable state.
    #[wasm_bindgen_test]
    async fn import_flush_failure_leaves_live_db_untouched() {
        let db_name = "minigraf-test-import-flush-fail";

        let db = BrowserDb::open(db_name).await.expect("open");
        db.execute(r#"(transact [[:old :marker "keep"]])"#.to_string())
            .await
            .expect("transact old");

        // A valid replacement blob holding a distinguishable fact.
        let src = BrowserDb::open_in_memory().expect("open src");
        src.execute(r#"(transact [[:new :marker "incoming"]])"#.to_string())
            .await
            .expect("transact new");
        let blob = src.export_graph().expect("export");

        // Sever the IDB connection underneath the live handle: creating the
        // flush transaction throws synchronously, so the import must fail.
        db.inner.borrow().idb.as_ref().unwrap().db.close();

        let result = db.import_graph(blob).await;
        assert!(result.is_err(), "import must fail when the IDB flush fails");

        // The live database must still be the OLD one (queries don't touch IDB).
        let r = db
            .execute(r#"(query [:find ?m :where [:old :marker ?m]])"#.to_string())
            .await
            .expect("query old after failed import");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(
            v["results"].as_array().unwrap().len(),
            1,
            "old fact must survive a failed import"
        );

        let r2 = db
            .execute(r#"(query [:find ?m :where [:new :marker ?m]])"#.to_string())
            .await
            .expect("query new after failed import");
        let v2: serde_json::Value = serde_json::from_str(&r2).unwrap();
        assert_eq!(
            v2["results"].as_array().unwrap().len(),
            0,
            "new fact must not be visible after a failed import"
        );
        drop(db);

        // Durable pages intact: a fresh connection still loads the old DB.
        let db2 = BrowserDb::open(db_name).await.expect("reopen db");
        let r3 = db2
            .execute(r#"(query [:find ?m :where [:old :marker ?m]])"#.to_string())
            .await
            .expect("query old after reopen");
        let v3: serde_json::Value = serde_json::from_str(&r3).unwrap();
        assert_eq!(
            v3["results"].as_array().unwrap().len(),
            1,
            "old fact must survive reopen"
        );
    }

    /// Regression test for stale-page residue: importing a smaller graph must
    /// remove the previous database's excess pages from IndexedDB, otherwise
    /// IDB grows without bound across imports and reopened handles export
    /// bloated blobs.
    #[wasm_bindgen_test]
    async fn shrinking_import_removes_stale_idb_pages() {
        let db_name = "minigraf-test-import-shrink";

        let db = BrowserDb::open(db_name).await.expect("open");
        // Enough facts for a multi-page database (~25 facts per 4KB page).
        let tuples: String = (0..1000)
            .map(|i| format!("[:e{} :n {}]", i, i))
            .collect::<Vec<_>>()
            .join(" ");
        db.execute(format!("(transact [{}])", tuples))
            .await
            .expect("bulk transact");

        let idb = IndexedDbBackend::open(db_name).await.expect("idb handle");
        let count_before = idb.load_all_pages().await.expect("load before").len();

        // A small valid blob from a 1-fact database.
        let src = BrowserDb::open_in_memory().expect("open src");
        src.execute(r#"(transact [[:tiny :marker "small"]])"#.to_string())
            .await
            .expect("transact tiny");
        let blob = src.export_graph().expect("export");
        let blob_pages = blob.byte_length() as usize / crate::storage::PAGE_SIZE;
        assert!(
            count_before > blob_pages,
            "premise: old DB must be larger than the import blob"
        );

        db.import_graph(blob).await.expect("shrinking import");

        // Stale pages gone: IDB holds exactly the imported page set.
        let after_inspector = IndexedDbBackend::open(db_name)
            .await
            .expect("reopen after import inspector");
        let count_after = after_inspector
            .load_all_pages()
            .await
            .expect("load after")
            .len();
        assert_eq!(
            count_after, blob_pages,
            "IDB must hold exactly the imported pages"
        );
        drop(db);

        // Reopen: only the new data is visible, and the export is not bloated.
        let db2 = BrowserDb::open(db_name).await.expect("reopen");
        let r = db2
            .execute(r#"(query [:find ?m :where [:tiny :marker ?m]])"#.to_string())
            .await
            .expect("query tiny");
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 1);

        let r2 = db2
            .execute(r#"(query [:find ?n :where [:e0 :n ?n]])"#.to_string())
            .await
            .expect("query old");
        let v2: serde_json::Value = serde_json::from_str(&r2).unwrap();
        assert_eq!(
            v2["results"].as_array().unwrap().len(),
            0,
            "old facts must be gone after import"
        );

        let export_after = db2.export_graph().expect("export after reopen");
        assert_eq!(
            export_after.byte_length() as usize,
            blob_pages * crate::storage::PAGE_SIZE,
            "export after reopen must not include stale pages"
        );
    }

    #[wasm_bindgen_test]
    async fn idle_maintenance_reclaims_pages_and_preserves_temporal_ref_history() {
        let db_name = format!("minigraf-test-maintenance-{}", js_sys::Date::now());
        let source = "00000000-0000-0000-0000-0000000000a1";
        let target = "00000000-0000-0000-0000-0000000000b2";
        let db = BrowserDb::open(&db_name).await.expect("open");

        db.execute(format!(
            "(transact {{:valid-from \"2026-01-01\"}} [[#uuid \"{source}\" :edge/to #uuid \"{target}\"] [#uuid \"{target}\" :name \"target\"]])"
        ))
        .await
        .expect("base transact");
        for index in 0..12 {
            db.execute(format!("(transact [[:event/{index} :seq {index}]])"))
                .await
                .expect("delta transact");
        }
        db.execute(format!(
            "(forget {{:valid-to \"2026-06-01\"}} [[#uuid \"{source}\" :edge/to #uuid \"{target}\"]])"
        ))
        .await
        .expect("forget edge");

        let idb = IndexedDbBackend::open(&db_name).await.expect("idb handle");
        let before_pages = idb.load_all_pages().await.expect("pages before").len();
        let before_tx = db.inner.borrow().fact_storage.current_tx_count();

        db.begin_mutation().expect("maintenance guard");
        let result = maintenance::run_idle_maintenance(&db, true).await;
        db.finish_mutation(false);
        let result = result.expect("forced maintenance");
        let outcome: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(outcome["delta"], "recompacted");
        let after_inspector = IndexedDbBackend::open(&db_name)
            .await
            .expect("reopen after maintenance inspector");
        let after_pages = after_inspector
            .load_all_pages()
            .await
            .expect("pages after")
            .len();
        assert!(
            after_pages < before_pages,
            "compact replacement must remove superseded lineage pages"
        );
        assert_eq!(outcome["after_pages"].as_u64().unwrap(), after_pages as u64);
        assert_eq!(db.inner.borrow().fact_storage.current_tx_count(), before_tx);
        let export = db.export_graph().expect("export compact graph");
        assert_eq!(
            export.byte_length() as usize,
            after_pages * crate::storage::PAGE_SIZE,
            "export and IndexedDB must contain the same contiguous page image"
        );

        let current = db
            .execute(format!(
                "(query [:find ?to :where [#uuid \"{source}\" :edge/to ?to]])"
            ))
            .await
            .expect("current ref query");
        let current: serde_json::Value = serde_json::from_str(&current).unwrap();
        assert_eq!(current["results"].as_array().unwrap().len(), 0);
        let history = db
            .execute(format!(
                "(query [:find ?to :valid-at \"2026-03-01\" :where [#uuid \"{source}\" :edge/to ?to]])"
            ))
            .await
            .expect("historical ref query");
        let history: serde_json::Value = serde_json::from_str(&history).unwrap();
        assert_eq!(history["results"].as_array().unwrap().len(), 1);

        let second = db.run_idle_maintenance().await.expect("second maintenance");
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["delta"], "noop");
        drop(db);

        let reopened = BrowserDb::open(&db_name).await.expect("reopen");
        assert_eq!(
            reopened.inner.borrow().fact_storage.current_tx_count(),
            before_tx
        );
        let reopened_history = reopened
            .execute(format!(
                "(query [:find ?to :valid-at \"2026-03-01\" :where [#uuid \"{source}\" :edge/to ?to]])"
            ))
            .await
            .expect("history after reopen");
        let reopened_history: serde_json::Value = serde_json::from_str(&reopened_history).unwrap();
        assert_eq!(reopened_history["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn projection_maintenance_publishes_and_reopens_v13_atomically() {
        let db_name = format!("minigraf-test-projection-v13-{}", js_sys::Date::now());
        let db = BrowserDb::open_paged(&db_name).await.expect("open paged");
        db.execute(
            r#"(transact [[#uuid "00000000-0000-0000-0000-000000000201" :card/title "A"]
                          [#uuid "00000000-0000-0000-0000-000000000201" :card/rank 9]
                          [#uuid "00000000-0000-0000-0000-000000000202" :card/title "B"]])"#
                .to_owned(),
        )
        .await
        .expect("seed projection ledger");
        let attributes = vec![":card/title".to_owned(), ":card/rank".to_owned()];
        let first: serde_json::Value = serde_json::from_str(
            &db.rebuild_current_projections(attributes.clone())
                .await
                .expect("first projection rebuild"),
        )
        .unwrap();
        assert_eq!(first["attribute_count"], 2);
        assert_eq!(first["row_count"], 3);
        assert_eq!(first["arena_reused"], false);

        let view = db.read_view().expect("projection read view");
        let aggregate: serde_json::Value = serde_json::from_str(
            &view
                .query(
                    "(query [:find (count ?rank) (sum ?rank) :where [?e :card/rank ?rank]])"
                        .to_owned(),
                    10,
                    4_096,
                )
                .await
                .expect("projected browser aggregate"),
        )
        .unwrap();
        assert_eq!(aggregate["results"][0][0], 1);
        assert_eq!(aggregate["results"][0][1], 9);
        let diagnostics = crate::storage::current_projection_image::projection_read_diagnostics();
        assert_eq!(diagnostics.route_attempts, 1);
        assert_eq!(diagnostics.completed_scans, 1);
        assert_eq!(diagnostics.rows_scanned, 1);
        assert_eq!(diagnostics.full_image_decodes, 0);
        assert!(diagnostics.pages_read > 0);

        let second: serde_json::Value = serde_json::from_str(
            &db.rebuild_current_projections(attributes.clone())
                .await
                .expect("second projection rebuild"),
        )
        .unwrap();
        let third: serde_json::Value = serde_json::from_str(
            &db.rebuild_current_projections(attributes)
                .await
                .expect("third projection rebuild"),
        )
        .unwrap();
        assert_eq!(third["arena_reused"], true);
        assert_eq!(third["after_pages"], second["after_pages"]);

        db.execute(
            r#"(transact [[#uuid "00000000-0000-0000-0000-000000000203" :card/rank 4]])"#
                .to_owned(),
        )
        .await
        .expect("append resident projection tail");
        let tail_view = db.read_view().expect("resident-tail read view");
        let tail: serde_json::Value = serde_json::from_str(
            &tail_view
                .query(
                    "(query [:find (count ?rank) (sum ?rank) :where [?e :card/rank ?rank]])"
                        .to_owned(),
                    10,
                    4_096,
                )
                .await
                .expect("resident-tail browser aggregate"),
        )
        .unwrap();
        assert_eq!(tail["results"][0][0], 2);
        assert_eq!(tail["results"][0][1], 13);
        let tail_diagnostics =
            crate::storage::current_projection_image::projection_read_diagnostics();
        assert_eq!(tail_diagnostics.completed_scans, 1);
        assert_eq!(tail_diagnostics.ledger_fallbacks, 0);
        assert_eq!(tail_diagnostics.tail_refreshes, 1);
        assert_eq!(tail_diagnostics.tail_facts_visited, 1);
        assert_eq!(tail_diagnostics.overlay_rows_emitted, 1);

        let exported = db.export_graph_async().await.expect("export v13");
        let bytes = exported.to_vec();
        let header =
            crate::storage::FileHeader::from_bytes(&bytes[..crate::storage::PAGE_SIZE]).unwrap();
        assert_eq!(
            header.version,
            crate::storage::header_extension::PROJECTION_CATALOG_FILE_FORMAT_VERSION
        );
        drop(db);

        let reopened = BrowserDb::open_paged(&db_name).await.expect("reopen v13");
        let result = reopened
            .execute(r#"(query [:find ?title :where [?e :card/title ?title]])"#.to_owned())
            .await
            .expect("ledger query after v13 reopen");
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["results"].as_array().unwrap().len(), 2);

        let import_name = format!("minigraf-test-projection-import-{}", js_sys::Date::now());
        let imported = BrowserDb::open_paged(&import_name)
            .await
            .expect("open import target");
        imported
            .import_graph_for_paged_access(js_sys::Uint8Array::from(bytes.as_slice()))
            .await
            .expect("strict import native-compatible v13 image");
        let imported_result = imported
            .execute(r#"(query [:find ?rank :where [?e :card/rank ?rank]])"#.to_owned())
            .await
            .expect("query imported v13");
        let imported_result: serde_json::Value = serde_json::from_str(&imported_result).unwrap();
        assert_eq!(imported_result["results"].as_array().unwrap().len(), 2);
    }

    #[wasm_bindgen_test]
    async fn projection_publication_failure_keeps_previous_browser_authority() {
        let db_name = format!("minigraf-test-projection-failure-{}", js_sys::Date::now());
        let db = BrowserDb::open_paged(&db_name).await.expect("open paged");
        db.execute(r#"(transact [[:card/a :card/title "A"]])"#.to_owned())
            .await
            .expect("seed ledger");
        let before = db
            .export_graph_async()
            .await
            .expect("export before")
            .to_vec();
        db.inner
            .borrow()
            .idb
            .as_ref()
            .expect("persistent handle")
            .fail_next_write_for_test();

        assert!(
            db.rebuild_current_projections(vec![":card/title".to_owned()])
                .await
                .is_err(),
            "injected publication failure must reject"
        );
        let after = db
            .export_graph_async()
            .await
            .expect("export after")
            .to_vec();
        assert_eq!(after, before);
        drop(db);

        let reopened = BrowserDb::open_paged(&db_name)
            .await
            .expect("reopen old authority");
        let result = reopened
            .execute(r#"(query [:find ?title :where [:card/a :card/title ?title]])"#.to_owned())
            .await
            .expect("query after failed publication");
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn failed_idle_maintenance_preserves_live_and_durable_state() {
        let db_name = format!("minigraf-test-maintenance-fail-{}", js_sys::Date::now());
        let db = BrowserDb::open(&db_name).await.expect("open");
        db.execute(r#"(transact [[:stable :marker "old"]])"#.to_string())
            .await
            .expect("base transact");
        db.execute(r#"(transact [[:stable :seq 2]])"#.to_string())
            .await
            .expect("delta transact");

        db.inner
            .borrow()
            .idb
            .as_ref()
            .unwrap()
            .fail_next_replace_for_test();
        db.begin_mutation().expect("maintenance guard");
        let result = maintenance::run_idle_maintenance(&db, true).await;
        db.finish_mutation(false);
        assert!(
            result.is_err(),
            "an enqueue failure after clear and one put must abort maintenance"
        );

        let live = db
            .execute(r#"(query [:find ?m :where [:stable :marker ?m]])"#.to_string())
            .await
            .expect("live old state");
        let live: serde_json::Value = serde_json::from_str(&live).unwrap();
        assert_eq!(live["results"].as_array().unwrap().len(), 1);
        drop(db);

        let reopened = BrowserDb::open(&db_name).await.expect("reopen old state");
        let durable = reopened
            .execute(r#"(query [:find ?m :where [:stable :marker ?m]])"#.to_string())
            .await
            .expect("durable old state");
        let durable: serde_json::Value = serde_json::from_str(&durable).unwrap();
        assert_eq!(durable["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn browser_mutation_guard_and_poison_state_are_enforced() {
        let db = BrowserDb::open_in_memory().expect("open");
        db.begin_mutation().expect("first mutation guard");
        let overlap = db
            .execute(r#"(transact [[:overlap :v 1]])"#.to_string())
            .await;
        assert!(overlap.is_err(), "overlapping mutation must be rejected");
        assert!(
            db.execute(r#"(query [:find ?v :where [:overlap :v ?v]])"#.to_string())
                .await
                .is_err(),
            "queries must not observe a mutation awaiting durability"
        );
        assert!(
            db.export_graph().is_err(),
            "exports must not promote a mutation awaiting durability"
        );
        db.finish_mutation(false);

        db.execute(r#"(transact [[:person :name "Alice"]])"#.to_string())
            .await
            .expect("seed semantic-error query");
        let semantic_error = db
            .execute(
                r#"(forget [:find ?name ?a ?v :where [?e :name ?name] [?e ?a ?v]])"#.to_string(),
            )
            .await;
        assert!(
            semantic_error.is_err(),
            "invalid forget result shape must be rejected"
        );
        assert!(
            db.execute(r#"(query [:find ?v :where [:overlap :v ?v]])"#.to_string())
                .await
                .is_ok(),
            "pre-write semantic failures must not poison the handle"
        );
        assert!(
            db.export_graph().is_ok(),
            "pre-write semantic failures must not block export"
        );

        db.begin_mutation().expect("poisoning mutation guard");
        db.inner.borrow_mut().mutation_advanced = true;
        db.finish_mutation(true);
        assert!(
            db.execute(r#"(query [:find ?v :where [:overlap :v ?v]])"#.to_string())
                .await
                .is_err(),
            "poisoned handles must reject queries"
        );
        assert!(
            db.run_idle_maintenance().await.is_err(),
            "poisoned handles must reject maintenance"
        );
        assert!(
            db.export_graph().is_err(),
            "poisoned handles must reject export"
        );
    }

    #[wasm_bindgen_test]
    async fn failed_incremental_write_poisoning_prevents_torn_state_promotion() {
        let db_name = format!("minigraf-test-write-poison-{}", js_sys::Date::now());
        let db = BrowserDb::open(&db_name).await.expect("open");
        db.execute(r#"(transact [[:stable :marker "durable"]])"#.to_string())
            .await
            .expect("durable base");

        db.inner.borrow().idb.as_ref().unwrap().db.close();
        let failed = db
            .execute(r#"(transact [[:uncertain :marker "must-not-promote"]])"#.to_string())
            .await;
        assert!(failed.is_err(), "closed IDB must reject the write");
        assert!(
            db.execute(r#"(query [:find ?m :where [:stable :marker ?m]])"#.to_string())
                .await
                .is_err(),
            "the advanced in-memory image must not remain queryable"
        );
        assert!(db.run_idle_maintenance().await.is_err());
        assert!(db.export_graph().is_err());
        drop(db);

        let reopened = BrowserDb::open(&db_name)
            .await
            .expect("reopen durable image");
        let stable = reopened
            .execute(r#"(query [:find ?m :where [:stable :marker ?m]])"#.to_string())
            .await
            .expect("stable query");
        let stable: serde_json::Value = serde_json::from_str(&stable).unwrap();
        assert_eq!(stable["results"].as_array().unwrap().len(), 1);
        let uncertain = reopened
            .execute(r#"(query [:find ?m :where [:uncertain :marker ?m]])"#.to_string())
            .await
            .expect("uncertain query");
        let uncertain: serde_json::Value = serde_json::from_str(&uncertain).unwrap();
        assert_eq!(
            uncertain["results"].as_array().unwrap().len(),
            0,
            "a rejected write must be absent after reopen"
        );
    }

    #[wasm_bindgen_test]
    async fn aborted_incremental_write_restores_previous_live_state() {
        let db_name = format!("minigraf-test-write-rollback-{}", js_sys::Date::now());
        let db = BrowserDb::open(&db_name).await.expect("open");
        db.execute(r#"(transact [[:stable :marker "durable"]])"#.to_string())
            .await
            .expect("durable base");
        db.inner
            .borrow()
            .idb
            .as_ref()
            .unwrap()
            .fail_next_write_for_test();

        let failed = db
            .execute(r#"(transact [[:rejected :marker "absent"]])"#.to_string())
            .await;
        assert!(failed.is_err(), "injected IDB abort must reject the write");

        let stable = db
            .execute(r#"(query [:find ?m :where [:stable :marker ?m]])"#.to_string())
            .await
            .expect("restored stable query");
        let stable: serde_json::Value = serde_json::from_str(&stable).unwrap();
        assert_eq!(stable["results"].as_array().unwrap().len(), 1);
        let rejected = db
            .execute(r#"(query [:find ?m :where [:rejected :marker ?m]])"#.to_string())
            .await
            .expect("restored rejected query");
        let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
        assert_eq!(
            rejected["results"].as_array().unwrap().len(),
            0,
            "rejected write must be absent from restored live state"
        );

        db.execute(r#"(transact [[:after :marker "works"]])"#.to_string())
            .await
            .expect("write after rollback");
        drop(db);
        let reopened = BrowserDb::open(&db_name).await.expect("reopen");
        let after = reopened
            .execute(r#"(query [:find ?m :where [:after :marker ?m]])"#.to_string())
            .await
            .expect("after query");
        let after: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(after["results"].as_array().unwrap().len(), 1);
    }

    #[wasm_bindgen_test]
    async fn browser_write_results_report_order_and_durability() {
        let db_name = format!("minigraf-test-write-result-{}", js_sys::Date::now());
        let db = BrowserDb::open(&db_name).await.expect("open");
        let first = db
            .execute(r#"(transact [[:ordered :value 1]])"#.to_string())
            .await
            .expect("first write");
        let second = db
            .execute(r#"(transact [[:ordered :value 2]])"#.to_string())
            .await
            .expect("second write");
        let retracted = db
            .execute(r#"(retract [[:ordered :value 1]])"#.to_string())
            .await
            .expect("retract");
        let forgotten = db
            .execute(r#"(forget [[:ordered :value 2]])"#.to_string())
            .await
            .expect("matched forget");
        let noop = db
            .execute(r#"(forget [[:missing :value 9]])"#.to_string())
            .await
            .expect("no-match forget");
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        let retracted: serde_json::Value = serde_json::from_str(&retracted).unwrap();
        let forgotten: serde_json::Value = serde_json::from_str(&forgotten).unwrap();
        let noop: serde_json::Value = serde_json::from_str(&noop).unwrap();

        assert_eq!(first["tx_count"], 1);
        assert_eq!(second["tx_count"], 2);
        assert_eq!(retracted["tx_count"], 3);
        assert_eq!(forgotten["tx_count"], 4);
        assert_eq!(first["durability"], "published");
        assert_eq!(second["durability"], "published");
        assert_eq!(retracted["durability"], "published");
        assert_eq!(forgotten["durability"], "published");
        assert_eq!(first["maintenance_pending"], false);
        assert_eq!(second["maintenance_pending"], false);
        assert!(first["transacted"].is_u64());
        assert_eq!(first["transacted"], first["tx_id"]);
        assert_eq!(second["transacted"], second["tx_id"]);
        assert_eq!(retracted["retracted"], retracted["tx_id"]);
        assert_eq!(forgotten["forgotten"], 1);
        assert!(forgotten["tx_id"].is_u64());
        assert_eq!(noop["forgotten"], 0);
        assert!(noop["tx_id"].is_null());
        assert!(noop["tx_count"].is_null());
        assert_eq!(noop["durability"], "noop");
        assert_eq!(noop["maintenance_pending"], false);
        assert_eq!(noop["advice"], "none");
    }

    #[wasm_bindgen_test]
    async fn browser_interactive_ledger_exposes_atomic_writes_and_bounded_views() {
        let ledger = BrowserInteractiveLedger::open_in_memory().expect("open interactive");
        ledger
            .execute_atomic(vec![
                r#"(transact [[:card/a :card/title "A"]])"#.to_owned(),
                r#"(transact [[:card/a :card/status :current]])"#.to_owned(),
            ])
            .await
            .expect("atomic interactive write");
        let view = ledger.read_view().expect("bounded view");
        let result = view
            .query(
                r#"(query [:find ?title :where [:card/a :card/title ?title]])"#.to_owned(),
                4,
                4_096,
            )
            .await
            .expect("bounded query");
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["results"][0][0], "A");
        assert_eq!(view.tx_cursor(), 1);
    }

    #[wasm_bindgen_test]
    async fn browser_capability_constructors_use_paged_persistent_authority() {
        let interactive_name = format!("vicia-interactive-capability-{}", js_sys::Date::now());
        let interactive = BrowserInteractiveLedger::open(&interactive_name)
            .await
            .expect("open paged interactive ledger");
        assert!(interactive.db.inner.borrow().paged);
        assert_eq!(
            interactive.db.inner.borrow().open_mode,
            BrowserOpenMode::Paged
        );

        let maintenance_name = format!("vicia-maintenance-capability-{}", js_sys::Date::now());
        let maintenance = BrowserMaintenanceLedger::open(&maintenance_name)
            .await
            .expect("open paged maintenance ledger");
        assert!(maintenance.db.inner.borrow().paged);
        assert_eq!(
            maintenance.db.inner.borrow().open_mode,
            BrowserOpenMode::Paged
        );
    }

    #[wasm_bindgen_test]
    async fn browser_maintenance_ledger_owns_portability_and_idle_work() {
        let source = BrowserMaintenanceLedger::open_in_memory().expect("open maintenance source");
        let graph = source.export_graph().await.expect("export graph");
        let target = BrowserMaintenanceLedger::open_in_memory().expect("open maintenance target");
        target.import_graph(graph).await.expect("strict import");
        let outcome = target
            .run_idle_maintenance()
            .await
            .expect("idle maintenance");
        let outcome: serde_json::Value = serde_json::from_str(&outcome).unwrap();
        assert_eq!(outcome["checkpoint"], "noop");
        assert_eq!(outcome["delta"], "noop");
    }

    #[wasm_bindgen_test]
    async fn browser_read_view_pins_cursor_and_rejects_incomplete_queries() {
        let db = BrowserDb::open_in_memory().expect("open in memory");
        db.execute(r#"(transact [[:proposal :proposal/status "draft"]])"#.to_string())
            .await
            .expect("seed");
        let view = db.read_view().expect("read view");
        assert_eq!(view.tx_cursor(), 1);

        db.execute(r#"(retract [[:proposal :proposal/status "draft"]])"#.to_string())
            .await
            .expect("retract draft");
        db.execute(r#"(transact [[:proposal :proposal/status "accepted"]])"#.to_string())
            .await
            .expect("accept");

        let pinned = view
            .query(
                "(query [:find ?status :where [:proposal :proposal/status ?status]])".to_string(),
                4,
                4_096,
            )
            .await
            .expect("pinned query");
        let pinned: serde_json::Value = serde_json::from_str(&pinned).unwrap();
        assert_eq!(pinned["results"][0][0], "draft");

        assert!(
            view.query("(query [:find ?e :where [?e ?a ?v]])".to_string(), 4, 4_096,)
                .await
                .is_err(),
            "unindexed foreground query must be rejected"
        );
        assert!(
            view.query(
                "(query [:find ?status :where [:proposal :proposal/status ?status]])".to_string(),
                4,
                1,
            )
            .await
            .is_err(),
            "oversized JSON must be rejected, not truncated"
        );

        let dense = BrowserDb::open_in_memory().expect("open dense fixture");
        let facts = (0..32)
            .map(|index| format!("[:item/{index} :item/group :all]"))
            .collect::<Vec<_>>()
            .join(" ");
        dense
            .execute(format!("(transact [{facts}])"))
            .await
            .expect("seed dense fixture");
        let dense_view = dense.read_view().expect("dense read view");
        assert!(
            dense_view
                .query(
                    "(query [:find ?item :where [?item :item/group :all]])".to_string(),
                    1,
                    4_096,
                )
                .await
                .is_err(),
            "max_rows must reject during selective result generation"
        );
        assert!(
            dense_view
                .query(
                    "(query [:find (count ?item) :where [?item :item/group :all]])".to_string(),
                    1,
                    4_096,
                )
                .await
                .is_err(),
            "max_rows must also bound aggregate source work"
        );
    }

    #[wasm_bindgen_test]
    async fn browser_read_view_returns_structured_current_entities() {
        let entity = uuid::Uuid::from_u128(91);
        let target = uuid::Uuid::from_u128(92);
        let db = BrowserDb::open_in_memory().expect("open in memory");
        db.execute(format!(
            r#"(transact [[#uuid "{entity}" :card/title "Draft"] [#uuid "{entity}" :card/space #uuid "{target}"]])"#
        ))
        .await
        .expect("seed current entities");
        let view = db.read_view().expect("read view");
        db.execute(format!(
            r#"(retract [[#uuid "{entity}" :card/title "Draft"]])"#
        ))
        .await
        .expect("retract after view");

        let rows = view
            .current_entities(
                vec![entity.to_string()],
                vec![":card/title".to_owned(), ":card/space".to_owned()],
                4,
            )
            .await
            .expect("structured current entities");
        assert_eq!(rows.length(), 2);
        let entity_string = entity.to_string();
        let target_string = target.to_string();
        let title = rows.get(0);
        assert_eq!(
            js_sys::Reflect::get(&title, &JsValue::from_str("entity"))
                .unwrap()
                .as_string()
                .as_deref(),
            Some(entity_string.as_str())
        );
        assert_eq!(
            js_sys::Reflect::get(&title, &JsValue::from_str("attribute"))
                .unwrap()
                .as_string()
                .as_deref(),
            Some(":card/title")
        );
        assert_eq!(
            js_sys::Reflect::get(&title, &JsValue::from_str("value"))
                .unwrap()
                .as_string()
                .as_deref(),
            Some("Draft")
        );
        let reference = js_sys::Reflect::get(
            &js_sys::Reflect::get(&rows.get(1), &JsValue::from_str("value")).unwrap(),
            &JsValue::from_str("$ref"),
        )
        .unwrap();
        assert_eq!(
            reference.as_string().as_deref(),
            Some(target_string.as_str())
        );
    }

    #[wasm_bindgen_test]
    async fn browser_read_view_returns_pinned_structured_refs_to() {
        let target = uuid::Uuid::from_u128(191);
        let first = uuid::Uuid::from_u128(2);
        let second = uuid::Uuid::from_u128(1);
        let db = BrowserDb::open_in_memory().expect("open in memory");
        db.execute(format!(
            r#"(transact [[#uuid "{first}" :edge/to #uuid "{target}"] [#uuid "{second}" :edge/to #uuid "{target}"]])"#
        ))
        .await
        .expect("seed refs");
        let view = db.read_view().expect("read view");
        db.execute(format!(
            r#"(retract [[#uuid "{first}" :edge/to #uuid "{target}"]])"#
        ))
        .await
        .expect("retract after view");

        let refs = view
            .refs_to(":edge/to".to_owned(), target.to_string(), 4)
            .await
            .expect("structured refs");
        assert_eq!(refs.length(), 2);
        let first_string = first.to_string();
        let second_string = second.to_string();
        assert_eq!(
            refs.get(0).as_string().as_deref(),
            Some(second_string.as_str())
        );
        assert_eq!(
            refs.get(1).as_string().as_deref(),
            Some(first_string.as_str())
        );
        assert!(
            view.refs_to(":edge/to".to_owned(), target.to_string(), 1)
                .await
                .is_err(),
            "incomplete refs result must be rejected"
        );
    }

    #[wasm_bindgen_test]
    async fn browser_vetch_current_reader_fixture_matches_raw_datalog() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            transaction: String,
        }
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../benchmarks/fixtures/vetch-current-reader.v1.json"
        ))
        .unwrap();
        let card = uuid::Uuid::parse_str("50000000-0000-0000-0000-000000000001").unwrap();
        let space = uuid::Uuid::parse_str("50000000-0000-0000-0000-000000000010").unwrap();
        let db = BrowserDb::open_in_memory().expect("open in memory");
        db.execute(fixture.transaction)
            .await
            .expect("load Vetch fixture");
        let view = db.read_view().expect("read view");

        let entities = view
            .current_entities(
                vec![card.to_string()],
                vec![":vetch_card/title".to_owned()],
                4,
            )
            .await
            .expect("typed current card");
        let typed_title = js_sys::Reflect::get(&entities.get(0), &JsValue::from_str("value"))
            .unwrap()
            .as_string();
        let raw_title = view
            .query(
                format!(
                    r#"(query [:find ?value :where [#uuid "{card}" :vetch_card/title ?value]])"#
                ),
                4,
                4_096,
            )
            .await
            .expect("raw current card");
        let raw_title: serde_json::Value = serde_json::from_str(&raw_title).unwrap();
        assert_eq!(typed_title.as_deref(), raw_title["results"][0][0].as_str());

        let refs = view
            .refs_to(":vetch_card/space".to_owned(), space.to_string(), 4)
            .await
            .expect("typed space membership");
        let raw_refs = view
            .query(
                format!(
                    r#"(query [:find ?source :where [?source :vetch_card/space #uuid "{space}"]])"#
                ),
                4,
                4_096,
            )
            .await
            .expect("raw space membership");
        let raw_refs: serde_json::Value = serde_json::from_str(&raw_refs).unwrap();
        assert_eq!(
            refs.get(0).as_string().as_deref(),
            raw_refs["results"][0][0]["$ref"].as_str()
        );
    }

    #[wasm_bindgen_test]
    async fn idb_persistence_round_trip() {
        let db_name = "minigraf-test-persistence";

        let db1 = BrowserDb::open(db_name).await.expect("open db1");
        db1.execute(r#"(transact [[:carol :dept "eng"]])"#.to_string())
            .await
            .expect("transact");
        drop(db1);

        let db2 = BrowserDb::open(db_name).await.expect("open db2");
        let result = db2
            .execute(r#"(query [:find ?dept :where [:carol :dept ?dept]])"#.to_string())
            .await
            .expect("query after reopen");
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0][0], serde_json::Value::String("eng".into()));
    }
}
