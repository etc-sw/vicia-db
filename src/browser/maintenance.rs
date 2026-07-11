//! Browser idle-maintenance boundary.
//!
//! Role:
//! - Convert measured delta pressure into a fresh compact graph image and an
//!   atomic IndexedDB replacement.
//!
//! Owns:
//! - Full-history-preserving compact-copy construction, replacement ordering,
//!   and browser maintenance outcome framing.
//!
//! Does not own:
//! - Foreground write cadence, Vetch scheduling, cross-tab Web Locks, or
//!   product retention policy.
//!
//! Allowed dependencies:
//! - BrowserDb state, BrowserBufferBackend, IndexedDbBackend, and the existing
//!   storage delta decision.

use super::buffer::BrowserBufferBackend;
use super::{BrowserDb, configure_sparse_authority};
use crate::storage::delta_growth::DeltaMaintenanceDecision;
use wasm_bindgen::JsValue;

pub(super) async fn run_idle_maintenance(db: &BrowserDb, force: bool) -> Result<String, JsValue> {
    let (decision, before_pages, has_idb) = {
        let inner = db.inner.borrow();
        let pages = inner
            .pfs
            .with_backend(|backend| backend.page_count_raw())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        (
            inner.pfs.delta_maintenance_decision(),
            pages,
            inner.idb.is_some(),
        )
    };

    if !has_idb || (!force && matches!(decision, DeltaMaintenanceDecision::ContinueDeltaAppend)) {
        return Ok(maintenance_json(
            "noop",
            "noop",
            maintenance_advice(decision),
            before_pages,
            before_pages,
        ));
    }

    // Paged selective reads stay bounded, but compaction is the explicit
    // O(total-history) worker operation. Resolve the complete base fact range
    // before entering the synchronous streaming writer; retrying a partially
    // built candidate page-by-page would be quadratic and could duplicate work.
    db.prefetch_full_scan_pages().await?;

    // Build separately from the live page image. Any packing/index error
    // leaves the current handle and IndexedDB untouched.
    let (mut candidate, candidate_storage, compact_pages, idb) = {
        let inner = db.inner.borrow();
        let candidate = inner
            .pfs
            .build_compact_copy(BrowserBufferBackend::new(), 256)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let candidate_storage = candidate.storage().clone();
        let compact_pages = candidate.with_backend(BrowserBufferBackend::all_pages);
        let idb = inner
            .idb
            .as_ref()
            .ok_or_else(|| JsValue::from_str("IndexedDB maintenance handle is missing"))?
            .clone_handle();
        (candidate, candidate_storage, compact_pages, idb)
    };

    let after_pages = u64::try_from(compact_pages.len())
        .map_err(|_| JsValue::from_str("compact page count exceeds u64::MAX"))?;
    let paged = db.inner.borrow().open_mode.is_paged();
    if paged {
        candidate.with_backend_mut(|backend| {
            backend.take_dirty();
        });
        configure_sparse_authority(&mut candidate)?;
        candidate.with_backend_mut(BrowserBufferBackend::evict_all_clean_unpinned);
    }
    idb.replace_all_pages(compact_pages).await?;
    if !paged {
        candidate.with_backend_mut(|backend| {
            backend.take_dirty();
        });
    }

    let mut inner = db.inner.borrow_mut();
    inner.pfs = candidate;
    inner.fact_storage = candidate_storage;
    inner.paged = paged;
    drop(inner);

    Ok(maintenance_json(
        "noop",
        "recompacted",
        maintenance_advice(decision),
        before_pages,
        after_pages,
    ))
}

fn maintenance_advice(decision: DeltaMaintenanceDecision) -> &'static str {
    match decision {
        DeltaMaintenanceDecision::MaintenanceBackpressure => "reduce_checkpoint_cadence",
        DeltaMaintenanceDecision::ContinueDeltaAppend
        | DeltaMaintenanceDecision::ScheduleBackgroundRecompact => "none",
    }
}

fn maintenance_json(
    checkpoint: &str,
    delta: &str,
    advice: &str,
    before_pages: u64,
    after_pages: u64,
) -> String {
    serde_json::json!({
        "checkpoint": checkpoint,
        "delta": delta,
        "advice": advice,
        "before_pages": before_pages,
        "after_pages": after_pages,
    })
    .to_string()
}
