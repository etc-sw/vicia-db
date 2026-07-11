//! Async IndexedDB backend for browser WASM.
//!
//! This is NOT a `StorageBackend` implementor — it is async-only.
//! Called directly by `BrowserDb` after synchronous `PersistentFactStorage::save()`.

use js_sys::{Array, Promise, Reflect, Uint8Array};
#[cfg(test)]
use std::cell::Cell;
use std::collections::HashMap;
#[cfg(test)]
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

/// Converts an `IdbRequest` into a JS `Promise` that resolves with the request result.
fn request_to_promise(request: &IdbRequest) -> Promise {
    let req = request.clone();
    Promise::new(&mut |resolve, reject| {
        let req_ok = req.clone();
        let on_success: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                let result = req_ok.result().unwrap_or(JsValue::NULL);
                resolve.call1(&JsValue::NULL, &result).ok();
            });
        let on_error: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                reject
                    .call1(&JsValue::NULL, &JsValue::from_str("IdbRequest failed"))
                    .ok();
            });
        req.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        req.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_success.forget();
        on_error.forget();
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
        let on_complete: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                resolve.call0(&JsValue::NULL).ok();
            });
        let on_error: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                reject
                    .call1(&JsValue::NULL, &JsValue::from_str("IdbTransaction failed"))
                    .ok();
            });
        let on_abort: Closure<dyn FnMut(web_sys::Event)> =
            Closure::once(move |_: web_sys::Event| {
                reject_abort
                    .call1(&JsValue::NULL, &JsValue::from_str("IdbTransaction aborted"))
                    .ok();
            });
        tx.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
        tx.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        tx.set_onabort(Some(on_abort.as_ref().unchecked_ref()));
        on_complete.forget();
        on_error.forget();
        on_abort.forget();
    })
}

/// Async wrapper around a browser IndexedDB database.
///
/// Object store schema:
///   name:  `<db_name>`
///   key:   page_id (u64 stored as JS number — f64, safe up to 2^53)
///   value: 4096-byte Uint8Array
pub struct IndexedDbBackend {
    pub(crate) db: IdbDatabase,
    pub(crate) store_name: String,
    #[cfg(test)]
    fail_next_write: Rc<Cell<bool>>,
    #[cfg(test)]
    fail_next_replace: Rc<Cell<bool>>,
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
                let target = event.target().unwrap();
                let request: web_sys::IdbOpenDbRequest = target.dyn_into().unwrap();
                let db: IdbDatabase = request.result().unwrap().dyn_into().unwrap();
                if !db.object_store_names().contains(&store_name_upgrade) {
                    db.create_object_store(&store_name_upgrade).unwrap();
                }
            });
        open_request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
        on_upgrade.forget();

        // Wait for the open to succeed.
        JsFuture::from(request_to_promise(open_request.as_ref())).await?;

        let db: IdbDatabase = open_request.result()?.dyn_into()?;
        Ok(Self {
            db,
            store_name,
            #[cfg(test)]
            fail_next_write: Rc::new(Cell::new(false)),
            #[cfg(test)]
            fail_next_replace: Rc::new(Cell::new(false)),
        })
    }

    /// Load all pages from IndexedDB into a `HashMap<page_id, bytes>`.
    ///
    /// Uses `getAllKeys()` + `getAll()` in a single read transaction, then zips
    /// the two result arrays. Both calls share the same `IdbTransaction` to
    /// guarantee consistency (no writes can interleave between them).
    pub async fn load_all_pages(&self) -> Result<HashMap<u64, Vec<u8>>, JsValue> {
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readonly)?;
        let store = tx.object_store(&self.store_name)?;

        // Queue both requests while the transaction is active. Awaiting the
        // first request before creating the second can let some engines mark
        // the transaction inactive between Promise continuations.
        let keys_req = store.get_all_keys()?;
        let vals_req = store.get_all()?;
        let keys_val = JsFuture::from(request_to_promise(keys_req.as_ref())).await?;
        let vals_val = JsFuture::from(request_to_promise(vals_req.as_ref())).await?;
        let keys_arr: Array = keys_val.dyn_into()?;
        let vals_arr: Array = vals_val.dyn_into()?;

        let mut pages = HashMap::with_capacity(keys_arr.length() as usize);
        for i in 0..keys_arr.length() {
            let key = keys_arr.get(i);
            let page_id = key
                .as_f64()
                .ok_or_else(|| JsValue::from_str("page_id is not a number"))?
                as u64;
            let val = vals_arr.get(i);
            let arr: Uint8Array = val.dyn_into()?;
            pages.insert(page_id, arr.to_vec());
        }
        Ok(pages)
    }

    /// Clone the underlying IdbDatabase handle (cheap — it's a JS object reference).
    pub fn clone_handle(&self) -> Self {
        Self {
            db: self.db.clone(),
            store_name: self.store_name.clone(),
            #[cfg(test)]
            fail_next_write: self.fail_next_write.clone(),
            #[cfg(test)]
            fail_next_replace: self.fail_next_replace.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_write_for_test(&self) {
        self.fail_next_write.set(true);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_replace_for_test(&self) {
        self.fail_next_replace.set(true);
    }

    /// Write a batch of pages to IndexedDB in a single `readwrite` transaction.
    ///
    /// All `put` operations are queued synchronously on the store, then we wait
    /// for the transaction's `oncomplete` event. If any put fails, the transaction
    /// is aborted and an error is returned.
    ///
    /// `pages` is a list of `(page_id, page_bytes)` pairs. Empty input is a no-op.
    pub async fn write_pages(&self, pages: Vec<(u64, Vec<u8>)>) -> Result<(), JsValue> {
        if pages.is_empty() {
            return Ok(());
        }
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readwrite)?;
        let store = tx.object_store(&self.store_name)?;

        let queued = (|| -> Result<(), JsValue> {
            for (page_id, data) in &pages {
                let key = JsValue::from_f64(*page_id as f64);
                let arr = Uint8Array::from(data.as_slice());
                store.put_with_key(&arr, &key)?;
                #[cfg(test)]
                if self.fail_next_write.replace(false) {
                    return Err(JsValue::from_str(
                        "injected IndexedDB write enqueue failure",
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = queued {
            // A synchronous error after earlier puts were queued must not let
            // that prefix auto-commit after this Rust future returns.
            let _ = tx.abort();
            return Err(error);
        }

        // Wait for the transaction to commit. The IDB transaction commits
        // automatically once all put requests have been processed and no
        // new requests are made. We wait here to ensure durability before
        // returning to the caller.
        JsFuture::from(transaction_to_promise(&tx)).await?;
        Ok(())
    }

    /// Atomically replace the entire object store contents with `pages`.
    ///
    /// `clear()` and all `put` operations run in ONE `readwrite` transaction:
    /// if anything fails (including quota exhaustion at commit), the whole
    /// transaction aborts and the previous store contents remain fully intact.
    /// Requests within an IndexedDB transaction execute in FIFO order, so the
    /// clear is guaranteed to precede the puts.
    ///
    /// This is the bulk-replace path for `BrowserDb::import_graph()`;
    /// `write_pages` remains the incremental path for `execute`/`checkpoint`.
    pub async fn replace_all_pages(&self, pages: Vec<(u64, Vec<u8>)>) -> Result<(), JsValue> {
        let tx = self
            .db
            .transaction_with_str_and_mode(&self.store_name, IdbTransactionMode::Readwrite)?;
        let store = tx.object_store(&self.store_name)?;

        let queued = (|| -> Result<(), JsValue> {
            store.clear()?;
            for (page_id, data) in &pages {
                let key = JsValue::from_f64(*page_id as f64);
                let arr = Uint8Array::from(data.as_slice());
                store.put_with_key(&arr, &key)?;
                #[cfg(test)]
                if self.fail_next_replace.replace(false) {
                    return Err(JsValue::from_str(
                        "injected IndexedDB replacement enqueue failure",
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = queued {
            // In particular, never allow `clear()` plus a successfully queued
            // prefix to commit as a partial replacement.
            let _ = tx.abort();
            return Err(error);
        }

        JsFuture::from(transaction_to_promise(&tx)).await?;
        Ok(())
    }
}
