//! Browser WASM support: `BrowserDb` async façade backed by IndexedDB.
//!
//! This module is only compiled for `wasm32-unknown-unknown` with the `browser`
//! feature enabled. It is **not** compatible with Node.js, Deno, Bun, or any
//! server-side runtime. For server-side Node.js, use `@minigraf/node` (Phase 8.3).

/// Synchronous in-memory page buffer with dirty-page tracking.
pub mod buffer;
/// Async IndexedDB backend for browser WASM persistence.
pub mod indexeddb;

use crate::browser::buffer::BrowserBufferBackend;
use crate::browser::indexeddb::IndexedDbBackend;
use crate::graph::FactStorage;
use crate::query::datalog::executor::{DatalogExecutor, QueryResult};
use crate::query::datalog::functions::FunctionRegistry;
use crate::query::datalog::parser::parse_datalog_command;
use crate::query::datalog::rules::RuleRegistry;
use crate::query::datalog::types::DatalogCommand;
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
        let pfs = PersistentFactStorage::new(buffer, 256)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let fact_storage = pfs.storage().clone();

        Ok(BrowserDb {
            inner: Rc::new(RefCell::new(BrowserDbInner {
                fact_storage,
                rules: Arc::new(RwLock::new(RuleRegistry::new())),
                functions: Arc::new(RwLock::new(FunctionRegistry::with_builtins())),
                pfs,
                idb: Some(idb),
            })),
        })
    }

    /// Execute a Datalog command string and return a JSON-encoded result.
    ///
    /// Returns a `Promise<string>` in JavaScript. The JSON shape is:
    /// - Query: `{"variables": [...], "results": [[...], ...]}`
    /// - Transact: `{"transacted": <tx_id>}`
    /// - Retract: `{"retracted": <tx_id>}`
    /// - Rule: `{"ok": true}`
    #[wasm_bindgen(js_name = execute)]
    pub async fn execute(&self, datalog: String) -> Result<String, JsValue> {
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
                self.apply_write(facts, false).await
            }
            DatalogCommand::Retract(tx) => {
                let facts = crate::db::Minigraf::materialize_retraction(&tx)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                self.apply_write(facts, true).await
            }
            // Handled above; unreachable but required for exhaustiveness.
            DatalogCommand::Query(_) | DatalogCommand::Rule(_) => unreachable!(),
        }
    }

    /// Flush all dirty pages to IndexedDB.
    ///
    /// Write-through means individual `execute()` calls already flush dirty
    /// pages, and `import_graph()` performs its own atomic flush, so
    /// `checkpoint()` is only needed after explicit bulk ops.
    /// No-op for in-memory databases.
    pub async fn checkpoint(&self) -> Result<(), JsValue> {
        let (dirty_pages, has_idb) = {
            let mut inner = self.inner.borrow_mut();
            inner
                .pfs
                .save()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let dirty_ids = inner.pfs.with_backend_mut(|b| b.take_dirty());
            let pages: Vec<(u64, Vec<u8>)> = dirty_ids
                .into_iter()
                .filter_map(|id| {
                    inner
                        .pfs
                        .with_backend(|b| b.read_page_raw(id).ok().map(|d| (id, d)))
                })
                .collect();
            (pages, inner.idb.is_some())
        };

        if has_idb && !dirty_pages.is_empty() {
            let idb = self.inner.borrow().idb.as_ref().unwrap().clone_handle();
            idb.write_pages(dirty_pages).await?;
        }
        Ok(())
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
        let inner = self.inner.borrow();
        let page_count = inner
            .pfs
            .with_backend(|b| b.page_count_raw())
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
    /// Calling `execute()` while an import is in flight is undefined behaviour;
    /// single-writer discipline is caller policy.
    #[wasm_bindgen(js_name = importGraph)]
    pub async fn import_graph(&self, data: js_sys::Uint8Array) -> Result<(), JsValue> {
        let bytes = data.to_vec();
        if bytes.is_empty() {
            return Err(JsValue::from_str(
                "import data is empty — a valid .graph blob has at least a header page",
            ));
        }
        if bytes.len() % crate::storage::PAGE_SIZE != 0 {
            return Err(JsValue::from_str(
                "import data length is not a multiple of PAGE_SIZE",
            ));
        }

        let mut pages = std::collections::HashMap::new();
        for (i, chunk) in bytes.chunks(crate::storage::PAGE_SIZE).enumerate() {
            pages.insert(i as u64, chunk.to_vec());
        }

        // Build the replacement storage locally — no live state is touched yet,
        // so any parse/validation failure leaves the database unchanged.
        let buffer = BrowserBufferBackend::load_pages_all_dirty(pages);
        let mut new_pfs = PersistentFactStorage::new(buffer, 256)
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
            let dirty_ids = inner.pfs.with_backend_mut(|b| b.take_dirty());
            let dirty_pages: Vec<(u64, Vec<u8>)> = dirty_ids
                .into_iter()
                .filter_map(|id| {
                    inner
                        .pfs
                        .with_backend(|b| b.read_page_raw(id).ok().map(|d| (id, d)))
                })
                .collect();

            let json = if is_retract {
                format!(r#"{{"retracted":{}}}"#, tx_id)
            } else {
                format!(r#"{{"transacted":{}}}"#, tx_id)
            };

            (dirty_pages, json)
        };
        // ── Borrow dropped here ───────────────────────────────────────────────

        // ── Async section: flush to IDB (no RefCell borrow held) ─────────────
        if !dirty_pages.is_empty() {
            let has_idb = self.inner.borrow().idb.is_some();
            if has_idb {
                let idb = self.inner.borrow().idb.as_ref().unwrap().clone_handle();
                idb.write_pages(dirty_pages).await?;
            }
        }

        Ok(result_json)
    }
}

// ── JSON serialisation helpers (free functions, not exported to WASM) ────────

fn query_result_to_json(result: QueryResult) -> String {
    use serde_json::{Value as JVal, json};

    let val: JVal = match result {
        QueryResult::Transacted(tx_id) => {
            json!({"transacted": tx_id})
        }
        QueryResult::Retracted(tx_id) => {
            json!({"retracted": tx_id})
        }
        QueryResult::Ok => json!({"ok": true}),
        QueryResult::QueryResults { vars, results } => {
            let rows: Vec<Vec<JVal>> = results
                .iter()
                .map(|row| row.iter().map(value_to_json).collect())
                .collect();
            json!({"variables": vars, "results": rows})
        }
    };
    val.to_string()
}

fn value_to_json(v: &crate::graph::types::Value) -> serde_json::Value {
    use crate::graph::types::Value;
    use serde_json::Value as JVal;
    match v {
        Value::String(s) => JVal::String(s.clone()),
        Value::Integer(i) => JVal::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(JVal::Number)
            .unwrap_or(JVal::Null),
        Value::Boolean(b) => JVal::Bool(*b),
        Value::Ref(uuid) => JVal::String(uuid.to_string()),
        Value::Keyword(k) => JVal::String(k.clone()),
        Value::Null => JVal::Null,
    }
}

#[cfg(all(target_arch = "wasm32", feature = "browser", test))]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

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
    async fn import_invalid_length_rejected_keeps_data() {
        let db = BrowserDb::open_in_memory().expect("open");
        db.execute(r#"(transact [[:dana :team "core"]])"#.to_string())
            .await
            .expect("transact");

        let bad = js_sys::Uint8Array::new_with_length((crate::storage::PAGE_SIZE + 1) as u32);
        let result = db.import_graph(bad).await;
        assert!(result.is_err(), "non-page-aligned blob must be rejected");

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
