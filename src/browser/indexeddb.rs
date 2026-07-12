//! Async IndexedDB backend for browser WASM.
//!
//! This is NOT a `StorageBackend` implementor — it is async-only.
//! Called directly by `BrowserDb` after synchronous `PersistentFactStorage::save()`.

use js_sys::{Array, Promise, Reflect, Uint8Array, Uint32Array};
#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbKeyRange, IdbRequest, IdbTransaction, IdbTransactionMode};

use crate::storage::PAGE_SIZE;

/// Largest integer that JavaScript numbers can represent without aliasing.
const MAX_SAFE_PAGE_ID: u64 = 9_007_199_254_740_991;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IndexedDbReadCounters {
    /// Read-only IndexedDB transactions opened by this handle or its clones.
    pub(crate) transactions: u64,
    /// Pages explicitly requested through sparse single/range reads.
    pub(crate) pages_requested: u64,
    /// Page values returned by IndexedDB, including full-store reads.
    pub(crate) pages_returned: u64,
    /// Legacy eager full-store reads.
    pub(crate) full_store_reads: u64,
}

type EventHandler = Closure<dyn FnMut(web_sys::Event)>;
type RequestHandlers = Rc<RefCell<Option<(EventHandler, EventHandler)>>>;
type TransactionHandlers = Rc<RefCell<Option<(EventHandler, EventHandler, EventHandler)>>>;

fn release_request_handlers(request: &IdbRequest, handlers: &RequestHandlers) {
    request.set_onsuccess(None);
    request.set_onerror(None);
    let owned = handlers.borrow_mut().take();
    // The winning handler is part of `owned` and is still executing here.
    // Dropping that wasm-bindgen `Closure` re-entrantly can invalidate the
    // active JS callback frame. Move destruction to the next microtask so the
    // browser has returned from the event callback first.
    wasm_bindgen_futures::spawn_local(async move {
        drop(owned);
    });
}

fn release_transaction_handlers(tx: &IdbTransaction, handlers: &TransactionHandlers) {
    tx.set_oncomplete(None);
    tx.set_onerror(None);
    tx.set_onabort(None);
    let owned = handlers.borrow_mut().take();
    // See `release_request_handlers`: transaction callbacks have the same
    // self-owned wasm-bindgen closure lifetime.
    wasm_bindgen_futures::spawn_local(async move {
        drop(owned);
    });
}

/// Converts an `IdbRequest` into a JS `Promise` that resolves with the request result.
fn request_to_promise(request: &IdbRequest) -> Promise {
    let req = request.clone();
    Promise::new(&mut |resolve, reject| {
        let req_ok = req.clone();
        let success_handlers: RequestHandlers = Rc::new(RefCell::new(None));
        let error_handlers = success_handlers.clone();
        let success_owner = success_handlers.clone();
        let on_success: EventHandler = Closure::once(move |_: web_sys::Event| {
            let result = req_ok.result().unwrap_or(JsValue::NULL);
            resolve.call1(&JsValue::NULL, &result).ok();
            release_request_handlers(&req_ok, &success_owner);
        });
        let req_error = req.clone();
        let on_error: EventHandler = Closure::once(move |_: web_sys::Event| {
            reject
                .call1(&JsValue::NULL, &JsValue::from_str("IdbRequest failed"))
                .ok();
            release_request_handlers(&req_error, &error_handlers);
        });
        req.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        req.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        success_handlers
            .borrow_mut()
            .replace((on_success, on_error));
    })
}

/// Converts an `IdbTransaction` completion into a JS `Promise`.
///
/// Hooks `onabort` in addition to `oncomplete`/`onerror`: an abort not caused
/// by a request error (e.g. quota exhaustion at commit) fires only `"abort"`,
/// and without the hook the promise would never settle.
fn transaction_to_promise(tx: &IdbTransaction) -> Promise {
    let tx = tx.clone();
    Promise::new(&mut |resolve, reject| {
        let reject_abort = reject.clone();
        let complete_handlers: TransactionHandlers = Rc::new(RefCell::new(None));
        let error_handlers = complete_handlers.clone();
        let abort_handlers = complete_handlers.clone();
        let complete_owner = complete_handlers.clone();
        let tx_complete = tx.clone();
        let on_complete: EventHandler = Closure::once(move |_: web_sys::Event| {
            resolve.call0(&JsValue::NULL).ok();
            release_transaction_handlers(&tx_complete, &complete_owner);
        });
        let tx_error = tx.clone();
        let on_error: EventHandler = Closure::once(move |_: web_sys::Event| {
            reject
                .call1(&JsValue::NULL, &JsValue::from_str("IdbTransaction failed"))
                .ok();
            release_transaction_handlers(&tx_error, &error_handlers);
        });
        let tx_abort = tx.clone();
        let on_abort: EventHandler = Closure::once(move |_: web_sys::Event| {
            reject_abort
                .call1(&JsValue::NULL, &JsValue::from_str("IdbTransaction aborted"))
                .ok();
            release_transaction_handlers(&tx_abort, &abort_handlers);
        });
        tx.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
        tx.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        tx.set_onabort(Some(on_abort.as_ref().unchecked_ref()));
        complete_handlers
            .borrow_mut()
            .replace((on_complete, on_error, on_abort));
    })
}

/// Async wrapper around a browser IndexedDB database.
///
/// Object store schema:
/// - name: `<db_name>`
/// - numeric key: page_id (u64 stored as a safe JS number)
/// - numeric value: one 4096-byte page `Uint8Array`
///
/// Page 0 is also the atomic publication authority. Keeping authority inside
/// the existing numeric-page schema preserves rollback compatibility with
/// older browser packages: no metadata record is introduced that an old
/// `getAllKeys()` reader would reject.
pub struct IndexedDbBackend {
    pub(crate) db: IdbDatabase,
    pub(crate) store_name: String,
    /// Exact page-0 bytes pinned when this independent handle opened. Cheap
    /// clones share updates after their own successful commits.
    image_authority: Rc<RefCell<Option<Vec<u8>>>>,
    #[cfg(test)]
    fail_next_write: Rc<Cell<bool>>,
    #[cfg(test)]
    fail_next_replace: Rc<Cell<bool>>,
    #[cfg(test)]
    read_counters: Rc<Cell<IndexedDbReadCounters>>,
}

impl IndexedDbBackend {
    /// Open (or create) an IndexedDB database with a single object store.
    ///
    /// If the object store does not exist, it is created in `onupgradeneeded`.
    /// `db_name` is used as both the database name and the object store name.
    pub async fn open(db_name: &str) -> Result<Self, JsValue> {
        // `globalThis.indexedDB` exists in both Window and WorkerGlobalScope.
        // Avoid `web_sys::window()` so Vetch can run O(total-history) open and
        // maintenance work in a dedicated worker instead of blocking its UI.
        let global = js_sys::global();
        let indexed_db = Reflect::get(&global, &JsValue::from_str("indexedDB"))?;
        if indexed_db.is_null() || indexed_db.is_undefined() {
            return Err(JsValue::from_str("IndexedDB not available"));
        }
        let idb_factory: web_sys::IdbFactory = indexed_db.dyn_into()?;

        let store_name = db_name.to_string();
        let store_name_upgrade = store_name.clone();

        let open_request = idb_factory.open_with_u32(db_name, 1)?;

        // Create the object store if this is a fresh database (version upgrade).
        let on_upgrade: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |event: web_sys::Event| {
                let Some(target) = event.target() else {
                    return;
                };
                let Ok(request): Result<web_sys::IdbOpenDbRequest, _> = target.dyn_into() else {
                    return;
                };
                let Ok(result) = request.result() else {
                    return;
                };
                let Ok(db): Result<IdbDatabase, _> = result.dyn_into() else {
                    return;
                };
                if !db.object_store_names().contains(&store_name_upgrade) {
                    let _ = db.create_object_store(&store_name_upgrade);
                }
            });
        open_request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));

        // Wait for the open to succeed.
        JsFuture::from(request_to_promise(open_request.as_ref())).await?;
        open_request.set_onupgradeneeded(None);
        drop(on_upgrade);

        let db: IdbDatabase = open_request.result()?.dyn_into()?;
        let image_authority = read_page_zero_authority(&db, &store_name).await?;
        Ok(Self {
            db,
            store_name,
            image_authority: Rc::new(RefCell::new(image_authority)),
            #[cfg(test)]
            fail_next_write: Rc::new(Cell::new(false)),
            #[cfg(test)]
            fail_next_replace: Rc::new(Cell::new(false)),
            #[cfg(test)]
            read_counters: Rc::new(Cell::new(IndexedDbReadCounters::default())),
        })
    }

    /// Load one complete page from IndexedDB when it exists.
    ///
    /// An absent record returns `None`, allowing page-0 bootstrap to distinguish
    /// a genuinely fresh object store from a malformed stored value. Present
    /// values must be a `Uint8Array` of exactly [`PAGE_SIZE`] bytes.
    pub async fn load_page_if_present(&self, page_id: u64) -> Result<Option<Vec<u8>>, JsValue> {
        validate_page_id(page_id)?;
        #[cfg(test)]
        self.record_read_for_test(1, false);

        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readonly)?;
        let store = tx.object_store(&self.store_name)?;
        let authority_request = store.get(&JsValue::from_f64(0.0))?;
        let key = JsValue::from_f64(page_id as f64);
        let request = store.get(&key)?;
        let authority_promise = request_to_promise(authority_request.as_ref());
        let page_promise = request_to_promise(request.as_ref());
        let authority_value = JsFuture::from(authority_promise).await?;
        let observed_authority = decode_optional_page_zero(authority_value)?;
        self.ensure_pinned_authority(&observed_authority)?;
        let value = JsFuture::from(page_promise).await?;
        if value.is_undefined() {
            return Ok(None);
        }
        let page = decode_page_value(page_id, value)?;

        #[cfg(test)]
        self.record_returned_pages_for_test(1);
        Ok(Some(page))
    }

    /// Load one required complete page from IndexedDB.
    ///
    /// Missing pages, non-`Uint8Array` values, and values whose length is not
    /// exactly [`PAGE_SIZE`] are rejected. This strict wrapper is the primitive
    /// used after a published page id has become authoritative.
    pub async fn load_page(&self, page_id: u64) -> Result<Vec<u8>, JsValue> {
        self.load_page_if_present(page_id)
            .await?
            .ok_or_else(|| JsValue::from_str(&format!("IndexedDB page {page_id} is missing")))
    }

    /// Count numeric page records without materializing the key set.
    ///
    /// The store remains numeric-only for compatibility with prior browser
    /// packages. Page 0 is read in the same transaction to reject a stale
    /// independent handle.
    pub async fn count_numeric_pages(&self) -> Result<u64, JsValue> {
        #[cfg(test)]
        self.record_read_for_test(0, false);

        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readonly)?;
        let store = tx.object_store(&self.store_name)?;
        let authority_request = store.get(&JsValue::from_f64(0.0))?;
        let range = numeric_page_key_range()?;
        let request = store.count_with_key(range.as_ref())?;
        let authority_promise = request_to_promise(authority_request.as_ref());
        let count_promise = request_to_promise(request.as_ref());
        let authority_value = JsFuture::from(authority_promise).await?;
        let observed_authority = decode_optional_page_zero(authority_value)?;
        self.ensure_pinned_authority(&observed_authority)?;
        let value = JsFuture::from(count_promise).await?;
        decode_numeric_page_count(value)
    }

    /// Load a candidate contiguous page range when every page is complete.
    ///
    /// Authoritative failures (invalid bounds, IDB failures, or changed pinned
    /// page 0) are errors. Missing/interior pages and invalid page values return
    /// `None`, allowing manifest-slot recovery to try an older candidate without
    /// swallowing a stale-image error.
    pub async fn load_page_range_if_complete(
        &self,
        start_page: u64,
        page_count: u64,
    ) -> Result<Option<Vec<(u64, Vec<u8>)>>, JsValue> {
        if page_count == 0 {
            self.verify_current_authority().await?;
            return Ok(Some(Vec::new()));
        }

        let page_count_u32 = u32::try_from(page_count).map_err(|_| {
            JsValue::from_str(
                "IndexedDB page batch exceeds the u32 getAll limit; split it into smaller ranges",
            )
        })?;

        let last_page = start_page
            .checked_add(page_count - 1)
            .ok_or_else(|| JsValue::from_str("IndexedDB page range overflows u64"))?;
        validate_page_id(start_page)?;
        validate_page_id(last_page)?;

        #[cfg(test)]
        self.record_read_for_test(page_count, false);

        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readonly)?;
        let store = tx.object_store(&self.store_name)?;
        let authority_request = store.get(&JsValue::from_f64(0.0))?;
        let lower = JsValue::from_f64(start_page as f64);
        let upper = JsValue::from_f64(last_page as f64);
        let range = IdbKeyRange::bound(&lower, &upper)?;

        // Queue all requests and install their handlers before awaiting. Otherwise a browser
        // may mark the transaction inactive between Promise continuations.
        let keys_request = store.get_all_keys_with_key(range.as_ref())?;
        let values_request = store.get_all_with_key(range.as_ref())?;
        let authority_promise = request_to_promise(authority_request.as_ref());
        let keys_promise = request_to_promise(keys_request.as_ref());
        let values_promise = request_to_promise(values_request.as_ref());
        let authority_value = JsFuture::from(authority_promise).await?;
        let observed_authority = decode_optional_page_zero(authority_value)?;
        self.ensure_pinned_authority(&observed_authority)?;
        let keys_value = JsFuture::from(keys_promise).await?;
        let values_value = JsFuture::from(values_promise).await?;
        let keys: Array = keys_value
            .dyn_into()
            .map_err(|_| JsValue::from_str("IndexedDB page-range keys result is not an Array"))?;
        let values: Array = values_value
            .dyn_into()
            .map_err(|_| JsValue::from_str("IndexedDB page-range values result is not an Array"))?;

        if keys.length() != values.length() {
            return Ok(None);
        }
        if keys.length() != page_count_u32 {
            return Ok(None);
        }

        let capacity = usize::try_from(page_count_u32)
            .map_err(|_| JsValue::from_str("IndexedDB page batch exceeds addressable memory"))?;
        let mut pages = Vec::with_capacity(capacity);
        for offset in 0..page_count_u32 {
            let page_id = start_page + u64::from(offset);
            if validate_page_key(keys.get(offset), page_id).is_err() {
                return Ok(None);
            }
            let Ok(page) = decode_page_value(page_id, values.get(offset)) else {
                return Ok(None);
            };
            pages.push((page_id, page));
        }

        #[cfg(test)]
        self.record_returned_pages_for_test(page_count);
        Ok(Some(pages))
    }

    /// Load a required contiguous batch of complete pages in ascending order.
    ///
    /// This strict wrapper turns an invalid recovery candidate into an error;
    /// use [`Self::load_page_range_if_complete`] when an older candidate may be
    /// tried. Changed page-0 authority always remains an error in both variants.
    pub async fn load_page_range(
        &self,
        start_page: u64,
        page_count: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, JsValue> {
        self.load_page_range_if_complete(start_page, page_count)
            .await?
            .ok_or_else(|| {
                JsValue::from_str(&format!(
                    "IndexedDB page range starting at {start_page} with {page_count} pages is missing or corrupt"
                ))
            })
    }

    /// Load all pages from IndexedDB into a `HashMap<page_id, bytes>`.
    ///
    /// Uses numeric-range `getAllKeys()` + `getAll()` in one read transaction.
    /// Page 0 and both page arrays share the same snapshot, so a replaced image
    /// cannot be mixed in.
    pub async fn load_all_pages(&self) -> Result<HashMap<u64, Vec<u8>>, JsValue> {
        #[cfg(test)]
        self.record_read_for_test(0, true);

        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readonly)?;
        let store = tx.object_store(&self.store_name)?;
        let authority_request = store.get(&JsValue::from_f64(0.0))?;
        let range = numeric_page_key_range()?;

        // Queue all requests while the transaction is active. Awaiting the
        // head request before creating the page requests can let some engines mark
        // the transaction inactive between Promise continuations.
        let keys_req = store.get_all_keys_with_key(range.as_ref())?;
        let vals_req = store.get_all_with_key(range.as_ref())?;
        let authority_promise = request_to_promise(authority_request.as_ref());
        let keys_promise = request_to_promise(keys_req.as_ref());
        let values_promise = request_to_promise(vals_req.as_ref());
        let authority_value = JsFuture::from(authority_promise).await?;
        let observed_authority = decode_optional_page_zero(authority_value)?;
        self.ensure_pinned_authority(&observed_authority)?;
        let keys_val = JsFuture::from(keys_promise).await?;
        let vals_val = JsFuture::from(values_promise).await?;
        let keys_arr: Array = keys_val.dyn_into()?;
        let vals_arr: Array = vals_val.dyn_into()?;

        if keys_arr.length() != vals_arr.length() {
            return Err(JsValue::from_str(
                "IndexedDB numeric page keys and values have different lengths",
            ));
        }
        let capacity = usize::try_from(keys_arr.length())
            .map_err(|_| JsValue::from_str("IndexedDB page set exceeds addressable memory"))?;
        let mut pages = HashMap::with_capacity(capacity);
        for i in 0..keys_arr.length() {
            let page_id = decode_numeric_page_key(keys_arr.get(i))?;
            let page = decode_page_value(page_id, vals_arr.get(i))?;
            pages.insert(page_id, page);
        }
        #[cfg(test)]
        self.record_returned_pages_for_test(u64::from(keys_arr.length()));
        Ok(pages)
    }

    /// Clone the underlying IdbDatabase handle (cheap — it's a JS object reference).
    pub fn clone_handle(&self) -> Self {
        Self {
            db: self.db.clone(),
            store_name: self.store_name.clone(),
            image_authority: self.image_authority.clone(),
            #[cfg(test)]
            fail_next_write: self.fail_next_write.clone(),
            #[cfg(test)]
            fail_next_replace: self.fail_next_replace.clone(),
            #[cfg(test)]
            read_counters: self.read_counters.clone(),
        }
    }

    fn ensure_pinned_authority(&self, observed: &Option<Vec<u8>>) -> Result<(), JsValue> {
        if *self.image_authority.borrow() != *observed {
            return Err(stale_image_error());
        }
        Ok(())
    }

    async fn verify_current_authority(&self) -> Result<(), JsValue> {
        let observed = read_page_zero_authority(&self.db, &self.store_name).await?;
        self.ensure_pinned_authority(&observed)
    }

    #[cfg(test)]
    pub(crate) fn page_zero_authority_for_test(&self) -> Option<Vec<u8>> {
        self.image_authority.borrow().clone()
    }

    /// Reset counters shared by this test handle and all of its clones.
    #[cfg(test)]
    pub(crate) fn reset_read_counters_for_test(&self) {
        self.read_counters.set(IndexedDbReadCounters::default());
    }

    /// Snapshot counters shared by this test handle and all of its clones.
    #[cfg(test)]
    pub(crate) fn read_counters_for_test(&self) -> IndexedDbReadCounters {
        self.read_counters.get()
    }

    #[cfg(test)]
    fn record_read_for_test(&self, pages_requested: u64, full_store_read: bool) {
        let mut counters = self.read_counters.get();
        counters.transactions = counters.transactions.saturating_add(1);
        counters.pages_requested = counters.pages_requested.saturating_add(pages_requested);
        if full_store_read {
            counters.full_store_reads = counters.full_store_reads.saturating_add(1);
        }
        self.read_counters.set(counters);
    }

    #[cfg(test)]
    fn record_returned_pages_for_test(&self, pages_returned: u64) {
        let mut counters = self.read_counters.get();
        counters.pages_returned = counters.pages_returned.saturating_add(pages_returned);
        self.read_counters.set(counters);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_write_for_test(&self) {
        self.fail_next_write.set(true);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_replace_for_test(&self) {
        self.fail_next_replace.set(true);
    }

    /// Simulate storage corruption that bypasses Vicia's page-0 publication
    /// guard. Production writes must never use this path.
    #[cfg(test)]
    pub(crate) async fn overwrite_page_without_authority_for_test(
        &self,
        page_id: u64,
        page: &[u8],
    ) -> Result<(), JsValue> {
        validate_page_id(page_id)?;
        if page.len() != PAGE_SIZE {
            return Err(JsValue::from_str(
                "raw test overwrite requires one complete page",
            ));
        }
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readwrite)?;
        let store = tx.object_store(&self.store_name)?;
        let key = JsValue::from_f64(page_id as f64);
        let value = Uint8Array::from(page);
        store.put_with_key(value.as_ref(), &key)?;
        JsFuture::from(transaction_to_promise(&tx)).await?;
        Ok(())
    }

    /// Write a batch of pages to IndexedDB in a single `readwrite` transaction.
    ///
    /// The transaction's first request reads page 0. Only when it byte-matches
    /// this handle's pinned publication authority are the page puts queued.
    /// Every non-empty Vicia publication must include its next page 0. This
    /// linearizes mutations without changing the legacy numeric-only schema.
    ///
    /// `pages` is a list of `(page_id, page_bytes)` pairs. Empty input is a no-op.
    pub async fn write_pages(&self, pages: Vec<(u64, Vec<u8>)>) -> Result<(), JsValue> {
        if pages.is_empty() {
            return Ok(());
        }
        for (page_id, _) in &pages {
            validate_page_id(*page_id)?;
        }
        let next_authority = required_next_page_zero(&pages)?;
        let committed_authority = next_authority.clone();
        let pinned_authority = self.image_authority.borrow().clone();
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readwrite)?;
        let store = tx.object_store(&self.store_name)?;
        let authority_request = store.get(&JsValue::from_f64(0.0))?;
        let guarded_error = Rc::new(RefCell::new(None));

        let callback_request = authority_request.clone();
        let callback_store = store.clone();
        let callback_tx = tx.clone();
        let callback_error = guarded_error.clone();
        let authority_handlers: RequestHandlers = Rc::new(RefCell::new(None));
        let success_authority_handlers = authority_handlers.clone();
        let error_authority_handlers = authority_handlers.clone();
        #[cfg(test)]
        let fail_next_write = self.fail_next_write.clone();
        let on_authority: EventHandler = Closure::once(move |_: web_sys::Event| {
            let queued = (|| -> Result<(), JsValue> {
                let observed_authority = decode_optional_page_zero(callback_request.result()?)?;
                ensure_expected_authority(&pinned_authority, &observed_authority)?;
                for (page_id, data) in &pages {
                    let key = JsValue::from_f64(*page_id as f64);
                    let array = Uint8Array::from(data.as_slice());
                    callback_store.put_with_key(&array, &key)?;
                    #[cfg(test)]
                    if fail_next_write.replace(false) {
                        return Err(JsValue::from_str(
                            "injected IndexedDB write enqueue failure",
                        ));
                    }
                }
                Ok(())
            })();
            if let Err(error) = queued {
                abort_with_error(&callback_tx, &callback_error, error);
            }
            release_request_handlers(&callback_request, &success_authority_handlers);
        });
        let error_request = authority_request.clone();
        let on_authority_error: EventHandler = Closure::once(move |_: web_sys::Event| {
            release_request_handlers(&error_request, &error_authority_handlers);
        });
        authority_request.set_onsuccess(Some(on_authority.as_ref().unchecked_ref()));
        authority_request.set_onerror(Some(on_authority_error.as_ref().unchecked_ref()));
        authority_handlers
            .borrow_mut()
            .replace((on_authority, on_authority_error));

        await_guarded_transaction(&tx, guarded_error).await?;
        self.image_authority
            .borrow_mut()
            .replace(committed_authority);
        Ok(())
    }

    /// Atomically replace the entire object store contents with `pages`.
    ///
    /// Current page 0 is the transaction's first request. A matching handle
    /// then queues `clear()` and the complete next numeric page set in the same
    /// transaction. Any stale authority, enqueue failure, abort, or quota error
    /// preserves the complete previous image.
    ///
    /// This is the bulk-replace path for `BrowserDb::import_graph()`;
    /// `write_pages` remains the incremental path for `execute`/`checkpoint`.
    pub async fn replace_all_pages(&self, pages: Vec<(u64, Vec<u8>)>) -> Result<(), JsValue> {
        for (page_id, _) in &pages {
            validate_page_id(*page_id)?;
        }
        let next_authority = required_next_page_zero(&pages)?;
        let committed_authority = next_authority.clone();
        let pinned_authority = self.image_authority.borrow().clone();
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readwrite)?;
        let store = tx.object_store(&self.store_name)?;
        let authority_request = store.get(&JsValue::from_f64(0.0))?;
        let guarded_error = Rc::new(RefCell::new(None));

        let callback_request = authority_request.clone();
        let callback_store = store.clone();
        let callback_tx = tx.clone();
        let callback_error = guarded_error.clone();
        let authority_handlers: RequestHandlers = Rc::new(RefCell::new(None));
        let success_authority_handlers = authority_handlers.clone();
        let error_authority_handlers = authority_handlers.clone();
        #[cfg(test)]
        let fail_next_replace = self.fail_next_replace.clone();
        let on_authority: EventHandler = Closure::once(move |_: web_sys::Event| {
            let queued = (|| -> Result<(), JsValue> {
                let observed_authority = decode_optional_page_zero(callback_request.result()?)?;
                ensure_expected_authority(&pinned_authority, &observed_authority)?;
                callback_store.clear()?;
                for (page_id, data) in &pages {
                    let key = JsValue::from_f64(*page_id as f64);
                    let array = Uint8Array::from(data.as_slice());
                    callback_store.put_with_key(&array, &key)?;
                    #[cfg(test)]
                    if fail_next_replace.replace(false) {
                        return Err(JsValue::from_str(
                            "injected IndexedDB replacement enqueue failure",
                        ));
                    }
                }
                Ok(())
            })();
            if let Err(error) = queued {
                abort_with_error(&callback_tx, &callback_error, error);
            }
            release_request_handlers(&callback_request, &success_authority_handlers);
        });
        let error_request = authority_request.clone();
        let on_authority_error: EventHandler = Closure::once(move |_: web_sys::Event| {
            release_request_handlers(&error_request, &error_authority_handlers);
        });
        authority_request.set_onsuccess(Some(on_authority.as_ref().unchecked_ref()));
        authority_request.set_onerror(Some(on_authority_error.as_ref().unchecked_ref()));
        authority_handlers
            .borrow_mut()
            .replace((on_authority, on_authority_error));

        await_guarded_transaction(&tx, guarded_error).await?;
        self.image_authority
            .borrow_mut()
            .replace(committed_authority);
        Ok(())
    }
}

async fn read_page_zero_authority(
    db: &IdbDatabase,
    store_name: &str,
) -> Result<Option<Vec<u8>>, JsValue> {
    let tx = db.transaction_with_str_and_mode(store_name, IdbTransactionMode::Readonly)?;
    let store = tx.object_store(store_name)?;
    let request = store.get(&JsValue::from_f64(0.0))?;
    let value = JsFuture::from(request_to_promise(request.as_ref())).await?;
    decode_optional_page_zero(value)
}

fn decode_optional_page_zero(value: JsValue) -> Result<Option<Vec<u8>>, JsValue> {
    if value.is_undefined() {
        return Ok(None);
    }
    decode_page_value(0, value).map(Some)
}

fn ensure_expected_authority(
    expected: &Option<Vec<u8>>,
    observed: &Option<Vec<u8>>,
) -> Result<(), JsValue> {
    if expected != observed {
        return Err(stale_image_error());
    }
    Ok(())
}

fn stale_image_error() -> JsValue {
    JsValue::from_str(
        "IndexedDB page-0 publication authority changed after this handle opened; reopen the database handle",
    )
}

fn required_next_page_zero(pages: &[(u64, Vec<u8>)]) -> Result<Vec<u8>, JsValue> {
    let mut page_zero = None;
    for (page_id, page) in pages {
        if *page_id != 0 {
            continue;
        }
        if page_zero.is_some() {
            return Err(JsValue::from_str(
                "IndexedDB publication contains duplicate page 0",
            ));
        }
        if page.len() != PAGE_SIZE {
            return Err(JsValue::from_str(&format!(
                "IndexedDB publication page 0 has invalid length {} (expected {PAGE_SIZE})",
                page.len(),
            )));
        }
        page_zero = Some(page.clone());
    }
    page_zero.ok_or_else(|| {
        JsValue::from_str("Every non-empty IndexedDB publication must include page 0")
    })
}

fn abort_with_error(
    tx: &IdbTransaction,
    guarded_error: &Rc<RefCell<Option<JsValue>>>,
    error: JsValue,
) {
    let mut error_slot = guarded_error.borrow_mut();
    if error_slot.is_none() {
        *error_slot = Some(error);
    }
    drop(error_slot);
    let _ = tx.abort();
}

async fn await_guarded_transaction(
    tx: &IdbTransaction,
    guarded_error: Rc<RefCell<Option<JsValue>>>,
) -> Result<(), JsValue> {
    match JsFuture::from(transaction_to_promise(tx)).await {
        Ok(_) => Ok(()),
        Err(transaction_error) => {
            let specific_error = guarded_error.borrow_mut().take();
            Err(specific_error.unwrap_or(transaction_error))
        }
    }
}

fn numeric_page_key_range() -> Result<IdbKeyRange, JsValue> {
    let lower = JsValue::from_f64(0.0);
    let upper = JsValue::from_f64(MAX_SAFE_PAGE_ID as f64);
    IdbKeyRange::bound(&lower, &upper)
}

fn decode_numeric_page_count(value: JsValue) -> Result<u64, JsValue> {
    let count = value
        .as_f64()
        .ok_or_else(|| JsValue::from_str("IndexedDB numeric page count is not a number"))?;
    if !count.is_finite() || count.fract() != 0.0 || count < 0.0 {
        return Err(JsValue::from_str(&format!(
            "IndexedDB numeric page count {count} is not a non-negative finite integer"
        )));
    }
    if count > f64::from(u32::MAX) {
        return Err(JsValue::from_str(
            "IndexedDB numeric page count exceeds the Web IDL u32 limit",
        ));
    }
    let singleton = Array::of1(&value);
    let count_u32 = Uint32Array::new(singleton.as_ref()).get_index(0);
    if f64::from(count_u32) != count {
        return Err(JsValue::from_str(
            "IndexedDB numeric page count could not be represented exactly",
        ));
    }
    Ok(u64::from(count_u32))
}

fn decode_numeric_page_key(key: JsValue) -> Result<u64, JsValue> {
    let actual = key
        .as_f64()
        .ok_or_else(|| JsValue::from_str("IndexedDB page key is not a number"))?;
    if !actual.is_finite()
        || actual.fract() != 0.0
        || actual < 0.0
        || actual > MAX_SAFE_PAGE_ID as f64
    {
        return Err(JsValue::from_str(&format!(
            "IndexedDB page key {actual} is not a non-negative safe integer"
        )));
    }
    Ok(validated_safe_integer_to_u64(actual))
}

/// Convert only after `decode_numeric_page_key` has proved the value is a
/// non-negative integer within JavaScript's exact integer range.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn validated_safe_integer_to_u64(value: f64) -> u64 {
    value as u64
}

fn validate_page_id(page_id: u64) -> Result<(), JsValue> {
    if page_id > MAX_SAFE_PAGE_ID {
        return Err(JsValue::from_str(&format!(
            "IndexedDB page id {page_id} exceeds JavaScript's maximum safe integer"
        )));
    }
    Ok(())
}

fn validate_page_key(key: JsValue, expected_page_id: u64) -> Result<(), JsValue> {
    let actual_page_id = decode_numeric_page_key(key)?;
    if actual_page_id != expected_page_id {
        return Err(JsValue::from_str(&format!(
            "IndexedDB page range is not contiguous: expected page {expected_page_id}, found page {actual_page_id}"
        )));
    }
    Ok(())
}

fn decode_page_value(page_id: u64, value: JsValue) -> Result<Vec<u8>, JsValue> {
    let array: Uint8Array = value.dyn_into().map_err(|_| {
        JsValue::from_str(&format!(
            "IndexedDB page {page_id} is not stored as a Uint8Array"
        ))
    })?;
    let bytes = array.to_vec();
    if bytes.len() != PAGE_SIZE {
        return Err(JsValue::from_str(&format!(
            "IndexedDB page {page_id} has invalid length {} (expected {PAGE_SIZE})",
            bytes.len()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    fn page(byte: u8) -> Vec<u8> {
        vec![byte; PAGE_SIZE]
    }

    #[wasm_bindgen_test]
    async fn sparse_reads_are_batched_and_counted_across_clones() {
        let db_name = format!("vicia-idb-sparse-read-{}", js_sys::Math::random());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open sparse-read test database");
        idb.replace_all_pages(vec![(0, page(10)), (1, page(11)), (2, page(12))])
            .await
            .expect("seed sparse-read test pages");
        assert_eq!(idb.page_zero_authority_for_test(), Some(page(10)));
        let eager_pages = idb.load_all_pages().await.expect("load numeric pages");
        assert_eq!(eager_pages.len(), 3);
        idb.reset_read_counters_for_test();

        assert_eq!(
            idb.count_numeric_pages()
                .await
                .expect("count numeric pages"),
            3,
            "the compatibility store contains only numeric page records"
        );
        assert_eq!(idb.load_page(0).await.expect("load page zero"), page(10));
        let clone = idb.clone_handle();
        let range = clone
            .load_page_range(0, 3)
            .await
            .expect("load contiguous page range");
        assert_eq!(
            range
                .iter()
                .map(|(page_id, _)| *page_id)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(range[1].1, page(11));

        let counters = idb.read_counters_for_test();
        assert_eq!(counters.transactions, 3);
        assert_eq!(counters.pages_requested, 4);
        assert_eq!(counters.pages_returned, 4);
        assert_eq!(counters.full_store_reads, 0);
    }

    #[wasm_bindgen_test]
    async fn legacy_unfiltered_reader_sees_only_numeric_page_records() {
        let db_name = format!("vicia-idb-legacy-reader-{}", js_sys::Math::random());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open legacy-reader database");
        idb.replace_all_pages(vec![(0, page(14)), (1, page(15))])
            .await
            .expect("publish numeric-only image");

        // This is the exact unfiltered shape used by the previously published
        // browser package. A string/compound metadata key would make that
        // reader reject the database during rollback.
        let tx = idb
            .db
            .transaction_with_str_and_mode(&idb.store_name, IdbTransactionMode::Readonly)
            .expect("open legacy reader transaction");
        let store = tx
            .object_store(&idb.store_name)
            .expect("open legacy reader store");
        let keys_request = store.get_all_keys().expect("queue unfiltered keys");
        let values_request = store.get_all().expect("queue unfiltered values");
        let keys: Array = JsFuture::from(request_to_promise(keys_request.as_ref()))
            .await
            .expect("read unfiltered keys")
            .dyn_into()
            .expect("keys array");
        let values: Array = JsFuture::from(request_to_promise(values_request.as_ref()))
            .await
            .expect("read unfiltered values")
            .dyn_into()
            .expect("values array");
        assert_eq!(keys.length(), 2);
        assert_eq!(keys.length(), values.length());
        for index in 0..keys.length() {
            assert!(
                keys.get(index).as_f64().is_some(),
                "legacy key must be numeric"
            );
            let page: Uint8Array = values
                .get(index)
                .dyn_into()
                .expect("legacy value must be a Uint8Array");
            assert_eq!(page.length(), PAGE_SIZE as u32);
        }
    }

    #[wasm_bindgen_test]
    async fn sparse_reads_reject_missing_and_invalid_length_pages() {
        let db_name = format!("vicia-idb-sparse-invalid-{}", js_sys::Math::random());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open sparse-invalid test database");
        idb.replace_all_pages(vec![
            (0, page(20)),
            (2, page(22)),
            (3, vec![23; PAGE_SIZE - 1]),
        ])
        .await
        .expect("seed sparse-invalid test pages");

        assert!(
            idb.load_page_if_present(1)
                .await
                .expect("optional missing page read")
                .is_none(),
            "optional missing page must be distinguishable"
        );
        assert!(idb.load_page(1).await.is_err(), "required page must fail");
        assert!(
            idb.load_page_range_if_complete(0, 3)
                .await
                .expect("invalid candidate read remains non-authoritative")
                .is_none(),
            "interior range hole must invalidate only the candidate"
        );
        assert!(
            idb.load_page_range(0, 3).await.is_err(),
            "interior range hole must fail"
        );
        assert!(
            idb.load_page(3).await.is_err(),
            "short page value must fail"
        );
        assert!(
            idb.load_page_range_if_complete(3, 1)
                .await
                .expect("short candidate page remains non-authoritative")
                .is_none(),
            "short page must invalidate only the recovery candidate"
        );

        let tx = idb
            .db
            .transaction_with_str_and_mode(&idb.store_name, IdbTransactionMode::Readwrite)
            .expect("open raw-value test transaction");
        let store = tx
            .object_store(&idb.store_name)
            .expect("open raw-value test store");
        store
            .put_with_key(&JsValue::from_str("not-a-page"), &JsValue::from_f64(4.0))
            .expect("queue non-page value");
        JsFuture::from(transaction_to_promise(&tx))
            .await
            .expect("commit raw-value test image");
        assert!(
            idb.load_page_range_if_complete(4, 1)
                .await
                .expect("non-Uint8 candidate remains non-authoritative")
                .is_none(),
            "non-Uint8 value must invalidate only the recovery candidate"
        );
        assert!(
            idb.load_page_range(MAX_SAFE_PAGE_ID, 2).await.is_err(),
            "unsafe JavaScript page id must fail"
        );
        assert!(
            idb.load_page_range(0, u64::from(u32::MAX) + 1)
                .await
                .is_err(),
            "oversized getAll batch must request explicit caller chunking"
        );
    }

    #[wasm_bindgen_test]
    async fn replacement_advances_shared_authority_and_invalidates_independent_handle() {
        let db_name = format!("vicia-idb-image-replace-{}", js_sys::Math::random());
        let seed = IndexedDbBackend::open(&db_name)
            .await
            .expect("open replacement seed handle");
        seed.replace_all_pages(vec![(0, page(30)), (1, page(31))])
            .await
            .expect("publish initial image");
        assert_eq!(seed.page_zero_authority_for_test(), Some(page(30)));

        let old_handle = IndexedDbBackend::open(&db_name)
            .await
            .expect("open independent old handle");
        let replacing_handle = IndexedDbBackend::open(&db_name)
            .await
            .expect("open replacing handle");
        let shared_clone = replacing_handle.clone_handle();
        replacing_handle
            .replace_all_pages(vec![(0, page(40)), (1, page(41)), (2, page(42))])
            .await
            .expect("publish replacement image");

        assert_eq!(
            replacing_handle.page_zero_authority_for_test(),
            Some(page(40))
        );
        assert_eq!(shared_clone.page_zero_authority_for_test(), Some(page(40)));
        assert_eq!(
            shared_clone
                .load_page(1)
                .await
                .expect("clone reads new image"),
            page(41)
        );

        let stale_candidate = old_handle.load_page_range_if_complete(0, 3).await;
        let stale_error = stale_candidate
            .expect_err("stale range must be an error, never a missing candidate")
            .as_string()
            .expect("stale error must be a string");
        assert!(stale_error.contains("reopen"));
        assert!(old_handle.load_page(0).await.is_err());
        let stale_write = old_handle.write_pages(vec![(0, page(90))]).await;
        let stale_write_error = stale_write
            .expect_err("stale incremental writer must abort before publish")
            .as_string()
            .expect("stale write error must be a string");
        assert!(stale_write_error.contains("reopen"));
        let stale_replace = old_handle.replace_all_pages(vec![(0, page(91))]).await;
        let stale_replace_error = stale_replace
            .expect_err("stale replacement must abort before clear")
            .as_string()
            .expect("stale replacement error must be a string");
        assert!(stale_replace_error.contains("reopen"));

        let reopened = IndexedDbBackend::open(&db_name)
            .await
            .expect("reopen replacement image");
        assert_eq!(reopened.page_zero_authority_for_test(), Some(page(40)));
        assert_eq!(
            reopened
                .load_page(0)
                .await
                .expect("stale writers preserve replacement image"),
            page(40)
        );
        assert_eq!(
            reopened
                .count_numeric_pages()
                .await
                .expect("count replacement pages"),
            3
        );
        let all_pages = reopened
            .load_all_pages()
            .await
            .expect("load replacement numeric pages");
        assert_eq!(all_pages.len(), 3, "the store must remain numeric-only");
    }

    #[wasm_bindgen_test]
    async fn incremental_write_advances_authority_and_invalidates_independent_handle() {
        let db_name = format!("vicia-idb-image-write-{}", js_sys::Math::random());
        let writer = IndexedDbBackend::open(&db_name)
            .await
            .expect("open incremental writer");
        writer
            .replace_all_pages(vec![(0, page(50)), (1, page(51))])
            .await
            .expect("publish initial incremental image");
        let old_handle = IndexedDbBackend::open(&db_name)
            .await
            .expect("open pre-write independent handle");
        let shared_clone = writer.clone_handle();

        writer
            .write_pages(vec![(0, page(53)), (1, page(52))])
            .await
            .expect("publish incremental page mutation");
        assert_eq!(writer.page_zero_authority_for_test(), Some(page(53)));
        assert_eq!(shared_clone.page_zero_authority_for_test(), Some(page(53)));
        assert_eq!(
            shared_clone
                .load_page(1)
                .await
                .expect("shared clone reads incremental image"),
            page(52)
        );
        let stale_error = old_handle
            .load_page_if_present(1)
            .await
            .expect_err("independent handle must reject changed authority")
            .as_string()
            .expect("stale error must be a string");
        assert!(stale_error.contains("reopen"));
    }

    #[wasm_bindgen_test]
    async fn aborted_replacement_preserves_authority_and_complete_previous_image() {
        let db_name = format!("vicia-idb-image-abort-{}", js_sys::Math::random());
        let idb = IndexedDbBackend::open(&db_name)
            .await
            .expect("open replacement-abort handle");
        idb.replace_all_pages(vec![(0, page(60)), (1, page(61))])
            .await
            .expect("publish pre-abort image");
        let authority_before = idb.page_zero_authority_for_test();
        idb.fail_next_replace_for_test();

        let replacement = idb
            .replace_all_pages(vec![(0, page(70)), (1, page(71)), (2, page(72))])
            .await;
        assert!(replacement.is_err(), "injected replacement must abort");
        assert_eq!(idb.page_zero_authority_for_test(), authority_before);
        assert_eq!(
            idb.load_page(1).await.expect("old live image after abort"),
            page(61)
        );

        let reopened = IndexedDbBackend::open(&db_name)
            .await
            .expect("reopen image after aborted replacement");
        assert_eq!(reopened.page_zero_authority_for_test(), authority_before);
        assert_eq!(
            reopened
                .count_numeric_pages()
                .await
                .expect("count preserved pages"),
            2
        );
        assert_eq!(
            reopened
                .load_page(1)
                .await
                .expect("durable old image after abort"),
            page(61)
        );
    }
}
