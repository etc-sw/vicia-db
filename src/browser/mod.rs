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

use crate::browser::buffer::BrowserBufferBackend;
use crate::browser::indexeddb::IndexedDbBackend;
use crate::graph::FactStorage;
use crate::json_value::to_tagged_json;
use crate::query::datalog::executor::{DatalogExecutor, QueryResult};
use crate::query::datalog::functions::FunctionRegistry;
use crate::query::datalog::parser::parse_datalog_command;
use crate::query::datalog::rules::RuleRegistry;
use crate::query::datalog::types::DatalogCommand;
use crate::storage::delta_growth::DeltaMaintenanceDecision;
use crate::storage::persistent_facts::PersistentFactStorage;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use wasm_bindgen::prelude::*;

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
}

/// Browser-only Minigraf database handle backed by IndexedDB.
///
/// All public methods return `Promise`s. Use `await` in JavaScript.
///
/// **Not compatible with Node.js.** Use `@minigraf/node` for server-side use.
#[wasm_bindgen]
pub struct BrowserDb {
    inner: Rc<RefCell<BrowserDbInner>>,
}

#[wasm_bindgen]
impl BrowserDb {
    /// Open an in-memory database (no IndexedDB — for testing only).
    ///
    /// Data is lost when the page is closed. Use `BrowserDb.open()` for persistence.
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<BrowserDb, JsValue> {
        let buffer = BrowserBufferBackend::new();
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
            })),
        })
    }

    /// Open or create a database backed by IndexedDB.
    ///
    /// `db_name` is used as both the IndexedDB database name and object store name.
    /// Called as `await BrowserDb.open("mydb")` — NOT `new BrowserDb()`.
    #[wasm_bindgen(js_name = open)]
    pub async fn open(db_name: &str) -> Result<BrowserDb, JsValue> {
        let idb = IndexedDbBackend::open(db_name).await?;
        let existing = idb.load_all_pages().await?;

        let buffer = BrowserBufferBackend::load_pages(existing);
        let mut pfs = PersistentFactStorage::new(buffer, 256)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        pfs.with_backend_mut(BrowserBufferBackend::retain_declared_prefix)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
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
            let result = {
                let inner = self.inner.borrow();
                DatalogExecutor::new_with_rules_and_functions(
                    inner.fact_storage.clone(),
                    inner.rules.clone(),
                    inner.functions.clone(),
                )
                .execute(cmd)
                .map_err(|e| JsValue::from_str(&e.to_string()))?
            };
            return Ok(query_result_to_json(result));
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
                self.begin_mutation()?;
                let result = self.execute_forget(spec).await;
                self.finish_mutation(result.is_err());
                result
            }
            // Handled above; unreachable but required for exhaustiveness.
            DatalogCommand::Query(_) | DatalogCommand::Rule(_) => unreachable!(),
        }
    }

    /// Bulk valid-time closure: resolve the triples, materialize the
    /// retract + truncated re-assert pairs, and apply them as one transaction
    /// (single `tx_count`), then flush dirty pages to IndexedDB.
    async fn execute_forget(
        &self,
        spec: crate::query::datalog::types::ForgetSpec,
    ) -> Result<String, JsValue> {
        use crate::graph::types::tx_id_now;

        // ── Sync section: hold borrow, do ALL sync work, collect owned data ──
        let (dirty_pages, result_json) = {
            let mut inner = self.inner.borrow_mut();

            let now = tx_id_now();
            let closure_time = spec.valid_to.unwrap_or_else(|| now.cast_signed());

            let executor = DatalogExecutor::new_with_rules_and_functions(
                inner.fact_storage.clone(),
                inner.rules.clone(),
                inner.functions.clone(),
            );
            let triples =
                crate::db::Minigraf::resolve_forget_triples(&spec, &executor, closure_time)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let (facts, count) = crate::db::Minigraf::materialize_closure(
                &inner.fact_storage,
                &triples,
                closure_time,
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

            // Nothing matched: no tx_count consumed, nothing to flush.
            if facts.is_empty() {
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
            let tx_id = now;

            for mut fact in facts {
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

            let decision = inner.pfs.delta_maintenance_decision();
            let durability = if inner.idb.is_some() {
                "published"
            } else {
                "memory"
            };
            let json = serde_json::json!({
                "forgotten": count,
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
            (pages, inner.idb.is_some())
        };

        if has_idb && !dirty_pages.is_empty() {
            let idb = self.inner.borrow().idb.as_ref().unwrap().clone_handle();
            self.flush_dirty_pages_or_restore(idb, dirty_pages).await?;
        }
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
        result
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
        let page_count = inner
            .pfs
            .with_backend(BrowserBufferBackend::exportable_page_count)
            .map_err(|e| JsValue::from_str(&e.to_string()))? as usize;

        let mut blob = Vec::with_capacity(page_count * crate::storage::PAGE_SIZE);
        for id in 0..page_count as u64 {
            let page = inner
                .pfs
                .with_backend(|b| b.read_page_raw(id))
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            blob.extend_from_slice(&page);
        }
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
        self.begin_mutation()?;
        let result = self.import_graph_inner(data).await;
        // Import builds and durably replaces before swapping live state, so a
        // rejected import leaves both sides aligned and does not poison the
        // handle.
        self.finish_mutation(false);
        result
    }

    async fn import_graph_inner(&self, data: js_sys::Uint8Array) -> Result<(), JsValue> {
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

        let mut pages = std::collections::HashMap::new();
        for (i, chunk) in bytes[..complete_len]
            .chunks(crate::storage::PAGE_SIZE)
            .enumerate()
        {
            pages.insert(i as u64, chunk.to_vec());
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
        Ok(())
    }
}

impl BrowserDb {
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
        Ok(())
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

    async fn flush_dirty_pages_or_restore(
        &self,
        idb: IndexedDbBackend,
        pages: Vec<(u64, Vec<u8>)>,
    ) -> Result<(), JsValue> {
        let write_error = match idb.write_pages(pages).await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };

        // IndexedDB readwrite transactions are atomic. If the failed commit
        // can still be read, reconstruct the live PFS from that previous
        // durable image before rejecting the operation. This makes quota and
        // transaction-abort failures rejected-before-application from the
        // caller's next observable state.
        let durable_pages = idb.load_all_pages().await.map_err(|recovery_error| {
            JsValue::from_str(&format!(
                "IndexedDB write failed and durable-state reload also failed: write={}; reload={}",
                js_value_message(&write_error),
                js_value_message(&recovery_error),
            ))
        })?;
        let buffer = BrowserBufferBackend::load_pages(durable_pages);
        let mut restored = PersistentFactStorage::new(buffer, 256).map_err(|recovery_error| {
            JsValue::from_str(&format!(
                "IndexedDB write failed and previous durable graph could not be reopened: write={}; reopen={recovery_error}",
                js_value_message(&write_error),
            ))
        })?;
        restored
            .with_backend_mut(BrowserBufferBackend::retain_declared_prefix)
            .map_err(|recovery_error| {
                JsValue::from_str(&format!(
                    "IndexedDB write failed and previous durable prefix was invalid: write={}; reopen={recovery_error}",
                    js_value_message(&write_error),
                ))
            })?;
        let restored_storage = restored.storage().clone();

        let mut inner = self.inner.borrow_mut();
        inner.pfs = restored;
        inner.fact_storage = restored_storage;
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
                        f.valid_from = tx_id as i64;
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
            json[result_key] = serde_json::json!(tx_id);

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

        Ok(result_json)
    }
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
        BROWSER_FIXTURE, CorruptionCase, NATIVE_FIXTURE, Probe, QueryCase, apply_mutation, corpus,
        normalize_rows, published_byte_len,
    };
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

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
    async fn gate_e_browser_consumer_matches_both_producers_and_round_trips() {
        let corpus = corpus();
        for source in [NATIVE_FIXTURE, BROWSER_FIXTURE] {
            let db = BrowserDb::open_in_memory().expect("open Gate E browser consumer");
            db.import_graph(js_sys::Uint8Array::from(source))
                .await
                .expect("import Gate E fixture");
            assert_browser_queries(&db, &corpus.queries).await;

            let exported = db.export_graph().expect("export Gate E fixture").to_vec();
            assert_eq!(
                exported, source,
                "canonical import/export must be byte exact"
            );
            let reopened = BrowserDb::open_in_memory().expect("open round-trip consumer");
            reopened
                .import_graph(js_sys::Uint8Array::from(exported.as_slice()))
                .await
                .expect("reimport browser export");
            assert_browser_queries(&reopened, &corpus.queries).await;
        }
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
                        assert!(
                            mutated.len() > published_byte_len(source).expect("published length"),
                            "tail case must grow the physical image"
                        );
                        db.import_graph(js_sys::Uint8Array::from(mutated.as_slice()))
                            .await
                            .expect("browser must ignore unpublished tail");
                        assert_browser_queries(&db, &corpus.queries).await;
                        let published =
                            published_byte_len(source).expect("published source length");
                        assert_eq!(
                            db.export_graph()
                                .expect("export trimmed graph")
                                .byte_length() as usize,
                            published,
                            "browser export must exclude unpublished tail"
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
        let count_after = idb.load_all_pages().await.expect("load after").len();
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
        let after_pages = idb.load_all_pages().await.expect("pages after").len();
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
