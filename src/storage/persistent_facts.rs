use crate::graph::FactStorage;
/// Persistent fact storage that integrates StorageBackend with Datalog facts.
///
/// This module bridges the gap between high-level fact operations and
/// low-level page-based storage backends.
use crate::graph::types::{
    EntityId, Fact, RETRACT_ALL_VALID_FROM, TxId, VALID_TIME_FOREVER, Value,
};
use crate::storage::FACT_PAGE_FORMAT_PACKED;
#[cfg(not(target_arch = "wasm32"))]
use crate::storage::backend::file::FileBackend;
use crate::storage::btree_v6::{
    BtreeBuildOptions, MutexStorageBackend, OnDiskIndexReader, btree_entries,
    build_btree_from_key_entries, build_btree_with_options, merge_sorted_iters, stream_all_entries,
};
use crate::storage::cache::PageCache;
use crate::storage::delta_growth::{DeltaGrowthMetrics, DeltaMaintenanceDecision};
use crate::storage::delta_index::{DeltaIndexEntries, KeyedIndexReader, LayeredIndexReader};
use crate::storage::delta_manifest::{
    DeltaBaseIdentity, DeltaManifest, DeltaManifestSegment, PersistedManifestRecoveryReason,
    PersistedManifestSelection, read_manifest_from_descriptor, write_manifest_pages,
};
use crate::storage::delta_segment::{DeltaSegment, write_segment_pages};
use crate::storage::header_extension::{
    BasePageIntegrityDescriptor, HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName,
    HeaderManifestSlotRecoveryReason, HeaderManifestSlotSelection, build_header_page,
    build_header_page_with_extension, select_header_manifest_slot_from_page0,
};
use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, VaetKey, encode_value};
use crate::storage::packed_pages::{PackedFactPacker, pack_facts, visit_fact_refs_in_pages};
use crate::storage::page_integrity::{
    BasePageIntegrityCatalog, catalog_crc32,
    compute_page_checksum as compute_integrity_page_checksum,
};
use crate::storage::{
    CommittedFactReader, CommittedIndexReader, FileHeader, PAGE_SIZE, StorageBackend,
};
use anyhow::Result;
use crc32fast::Hasher;
use serde::Serialize;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[cfg(feature = "bench-internals")]
use std::cell::Cell;
#[cfg(feature = "bench-internals")]
use std::time::Instant;

#[cfg(feature = "bench-internals")]
/// Repository-only ownership counters for a full base construction.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointConstructionDiagnostics {
    /// Maximum completed fact pages retained before writing.
    pub peak_fact_pages_in_memory: u64,
    /// Maximum typed entries retained for one index.
    pub peak_typed_entries: u64,
    /// Maximum pending fact-position references retained for sorting.
    pub peak_sort_reference_entries: u64,
    /// Maximum bytes owned by the pending fact-position sort buffer.
    pub peak_sort_reference_bytes: u64,
    /// Canonical value bytes cached once across all pending index sorts.
    pub cached_value_bytes: u64,
    /// Candidate fact pages visited across index passes.
    pub fact_page_visits: u64,
    /// Maximum serialized entries retained by a B-tree frontier.
    pub peak_serialized_entries: u64,
    /// Maximum serialized payload bytes retained by a B-tree frontier.
    pub peak_serialized_bytes: u64,
    /// Time spent streaming and writing candidate fact pages.
    pub fact_packing_micros: u64,
    /// Time spent loading committed index entries before an incremental rebuild.
    pub committed_index_read_micros: u64,
    /// Time spent building and sorting pending entries for all indexes.
    pub pending_index_sort_micros: u64,
    /// Time spent syncing newly packed fact pages before index construction.
    pub fact_sync_micros: u64,
    /// Time spent collecting and sorting EAVT entries.
    pub eavt_collect_sort_micros: u64,
    /// Time spent serializing and writing the EAVT tree.
    pub eavt_build_micros: u64,
    /// Time spent collecting and sorting AEVT entries.
    pub aevt_collect_sort_micros: u64,
    /// Time spent serializing and writing the AEVT tree.
    pub aevt_build_micros: u64,
    /// Time spent collecting and sorting AVET entries.
    pub avet_collect_sort_micros: u64,
    /// Time spent serializing and writing the AVET tree.
    pub avet_build_micros: u64,
    /// Time spent collecting and sorting VAET entries.
    pub vaet_collect_sort_micros: u64,
    /// Time spent serializing and writing the VAET tree.
    pub vaet_build_micros: u64,
    /// Time spent syncing candidate fact and index pages before cataloging.
    pub data_sync_micros: u64,
    /// Time spent hashing and writing the base integrity catalog.
    pub integrity_catalog_micros: u64,
    /// Time spent assembling the candidate header page.
    pub header_assembly_micros: u64,
    /// Time spent writing the publication header page.
    pub publish_write_micros: u64,
    /// Time spent durably syncing the published header and candidate pages.
    pub publish_sync_micros: u64,
    /// Time spent installing readers and clearing the published overlay.
    pub publish_finalize_micros: u64,
}

#[cfg(feature = "bench-internals")]
thread_local! {
    static CHECKPOINT_DIAGNOSTICS: Cell<CheckpointConstructionDiagnostics> =
        const { Cell::new(CheckpointConstructionDiagnostics {
            peak_fact_pages_in_memory: 0,
            peak_typed_entries: 0,
            peak_sort_reference_entries: 0,
            peak_sort_reference_bytes: 0,
            cached_value_bytes: 0,
            fact_page_visits: 0,
            peak_serialized_entries: 0,
            peak_serialized_bytes: 0,
            fact_packing_micros: 0,
            committed_index_read_micros: 0,
            pending_index_sort_micros: 0,
            fact_sync_micros: 0,
            eavt_collect_sort_micros: 0,
            eavt_build_micros: 0,
            aevt_collect_sort_micros: 0,
            aevt_build_micros: 0,
            avet_collect_sort_micros: 0,
            avet_build_micros: 0,
            vaet_collect_sort_micros: 0,
            vaet_build_micros: 0,
            data_sync_micros: 0,
            integrity_catalog_micros: 0,
            header_assembly_micros: 0,
            publish_write_micros: 0,
            publish_sync_micros: 0,
            publish_finalize_micros: 0,
        }) };
}

#[cfg(feature = "bench-internals")]
fn checkpoint_elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(feature = "bench-internals")]
fn update_checkpoint_diagnostics(update: impl FnOnce(&mut CheckpointConstructionDiagnostics)) {
    let mut diagnostics = CHECKPOINT_DIAGNOSTICS.get();
    update(&mut diagnostics);
    CHECKPOINT_DIAGNOSTICS.set(diagnostics);
}

#[cfg(feature = "bench-internals")]
pub(crate) fn checkpoint_construction_diagnostics() -> CheckpointConstructionDiagnostics {
    let mut diagnostics = CHECKPOINT_DIAGNOSTICS.get();
    let (entries, bytes) = crate::storage::btree_v6::build_diagnostics();
    diagnostics.peak_serialized_entries = entries;
    diagnostics.peak_serialized_bytes = bytes;
    diagnostics
}

#[cfg(feature = "bench-internals")]
fn reset_checkpoint_construction_diagnostics() {
    CHECKPOINT_DIAGNOSTICS.set(CheckpointConstructionDiagnostics::default());
    crate::storage::btree_v6::reset_build_diagnostics();
}

#[cfg(feature = "bench-internals")]
fn observe_checkpoint_typed_entries(entries: usize) {
    let mut diagnostics = CHECKPOINT_DIAGNOSTICS.get();
    diagnostics.peak_typed_entries = diagnostics
        .peak_typed_entries
        .max(u64::try_from(entries).unwrap_or(u64::MAX));
    CHECKPOINT_DIAGNOSTICS.set(diagnostics);
}

#[cfg(not(feature = "bench-internals"))]
fn observe_checkpoint_typed_entries(_entries: usize) {}

fn normalize_legacy_retractions(facts: &mut [Fact]) {
    for fact in facts {
        if !fact.asserted {
            fact.valid_from = RETRACT_ALL_VALID_FROM;
            fact.valid_to = VALID_TIME_FOREVER;
        }
    }
}

/// Compute the legacy v4 CRC32 checksum over all facts.
///
/// Sorts facts by `(tx_count, entity_bytes, attribute)` before hashing to
/// produce a stable total order independent of Vec insertion order.
fn compute_index_checksum(facts: &[Fact]) -> Result<u32> {
    let mut sorted: Vec<&Fact> = facts.iter().collect();
    sorted.sort_by(|a, b| {
        a.tx_count
            .cmp(&b.tx_count)
            .then_with(|| a.entity.as_bytes().cmp(b.entity.as_bytes()))
            .then_with(|| a.attribute.as_str().cmp(b.attribute.as_str()))
    });
    let mut hasher = Hasher::new();
    for fact in sorted {
        let bytes = postcard::to_allocvec(fact)?;
        hasher.update(&bytes);
    }
    Ok(hasher.finalize())
}

/// Return the exclusive end of a legacy one-fact-per-page range.
///
/// v1-v3 publish only header + fact pages. v4 may append four paged-blob
/// indexes, whose first non-zero root starts immediately after the facts.
/// `node_count` was the canonical fact count in every one-per-page writer and
/// prevents a corrupt root from silently shortening or extending migration.
fn legacy_one_per_page_fact_end(header: &FileHeader) -> Result<u64> {
    let expected_end = header
        .node_count
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("Legacy fact page range overflow"))?;
    let first_index_page = [
        header.eavt_root_page,
        header.aevt_root_page,
        header.avet_root_page,
        header.vaet_root_page,
    ]
    .into_iter()
    .filter(|page_id| *page_id > 0)
    .min();
    let fact_end = first_index_page.unwrap_or(header.page_count);

    if fact_end > header.page_count || fact_end != expected_end {
        anyhow::bail!("Legacy one-per-page fact range does not match node_count and index roots");
    }
    Ok(fact_end)
}

/// CommittedFactReader backed by a PageCache + shared backend.
///
/// Resolves FactRefs to Fact objects by reading packed pages from the backend
/// through the page cache. Used after loading (or migrating) a v5/v6 file so that indexes can
/// resolve committed facts without keeping the entire fact list in memory.
// Fields are read inside CommittedFactReader trait methods that are accessed via
// dyn dispatch — Rust's dead-code lint does not track trait-impl field reads
// when the impl is behind dyn dispatch.
struct CommittedFactLoaderImpl<B: StorageBackend> {
    #[allow(dead_code)]
    page_cache: Arc<PageCache>,
    /// Pre-built adapter reused on every `resolve()` call.
    /// Avoids an `Arc::clone` per call: the adapter is constructed once and
    /// holds the `Arc<Mutex<B>>` for the lifetime of this reader.
    /// Backend mutex is still only acquired on cache misses (see `MutexStorageBackend`).
    backend_adapter: MutexStorageBackend<B>,
    committed_fact_pages: Arc<AtomicU64>,
    #[allow(dead_code)]
    committed_fact_page_start: Arc<AtomicU64>,
}

impl<B: StorageBackend + 'static> crate::storage::CommittedFactReader
    for CommittedFactLoaderImpl<B>
{
    fn resolve(
        &self,
        fact_ref: crate::storage::index::FactRef,
    ) -> anyhow::Result<crate::graph::types::Fact> {
        // backend_adapter is pre-built at construction time — no Arc::clone per call.
        // Backend mutex is only acquired inside adapter.read_page() on a cache miss.
        let page = self
            .page_cache
            .get_or_load(fact_ref.page_id, &self.backend_adapter)?;
        crate::storage::packed_pages::read_slot(&page, fact_ref.slot_index)
    }

    fn stream_all(&self) -> anyhow::Result<Vec<crate::graph::types::Fact>> {
        let n = self.committed_fact_pages.load(Ordering::SeqCst);
        let first_fact_page = self.committed_fact_page_start.load(Ordering::SeqCst);
        crate::storage::packed_pages::read_all_from_pages(&self.backend_adapter, first_fact_page, n)
    }

    fn for_each_fact(
        &self,
        visit: &mut dyn FnMut(crate::graph::types::Fact) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let n = self.committed_fact_pages.load(Ordering::SeqCst);
        let first_fact_page = self.committed_fact_page_start.load(Ordering::SeqCst);
        crate::storage::packed_pages::for_each_from_pages(
            &self.backend_adapter,
            first_fact_page,
            n,
            visit,
        )
    }

    fn for_each_fact_since(
        &self,
        since_tx_count: u64,
        visit: &mut dyn FnMut(crate::graph::types::Fact) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let n = self.committed_fact_pages.load(Ordering::SeqCst);
        if n == 0 {
            return Ok(());
        }
        let first_fact_page = self.committed_fact_page_start.load(Ordering::SeqCst);

        // Packed fact pages hold facts in nondecreasing tx_count order, so
        // "page max tx_count > since" is monotone across the fact-page range.
        // Binary search the first tail page through the page cache: O(log n)
        // page reads instead of a committed full scan.
        let mut lo = 0u64;
        let mut hi = n;
        while lo < hi {
            let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
            let page_id = first_fact_page.saturating_add(mid);
            let page = self
                .page_cache
                .get_or_load(page_id, &self.backend_adapter)?;
            match crate::storage::packed_pages::last_tx_count(&page)? {
                Some(max_tx) if max_tx > since_tx_count => hi = mid,
                Some(_) => lo = mid.saturating_add(1),
                None => {
                    // Non-packed or empty page inside the fact range — the
                    // monotone probe is unreliable here; fall back to the
                    // correct full-scan filter.
                    return self.for_each_fact(&mut |fact| {
                        if fact.tx_count > since_tx_count {
                            visit(fact)?;
                        }
                        Ok(())
                    });
                }
            }
        }
        if lo >= n {
            return Ok(());
        }

        // Stream only the tail pages; the boundary page may still hold
        // leading facts at or below `since`, so keep the filter.
        crate::storage::packed_pages::for_each_from_pages(
            &self.backend_adapter,
            first_fact_page.saturating_add(lo),
            n.saturating_sub(lo),
            &mut |fact| {
                if fact.tx_count > since_tx_count {
                    visit(fact)?;
                }
                Ok(())
            },
        )
    }

    fn committed_page_count(&self) -> u64 {
        self.committed_fact_pages.load(Ordering::SeqCst)
    }
}

struct LayeredFactLoaderImpl {
    base: Arc<dyn CommittedFactReader>,
    delta_facts: Arc<RwLock<BTreeMap<FactRef, Fact>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointOutcome {
    Noop,
    FullRebuild,
    FullRebuildFromVisibleDelta,
    DeltaSegment,
}

impl CheckpointOutcome {
    pub(crate) fn permits_wal_retire(self) -> bool {
        !matches!(self, Self::Noop)
    }
}

struct RecompactCandidate {
    header: FileHeader,
    header_page: Vec<u8>,
    base_fact_page_start: u64,
    fact_page_count: u64,
    checkpoint_tx_count: u64,
    base_integrity: Arc<BasePageIntegrityCatalog>,
}

struct BaseIntegrityWrite {
    catalog: Arc<BasePageIntegrityCatalog>,
    descriptor: BasePageIntegrityDescriptor,
    aggregate_checksum: u32,
    published_page_count: u64,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
const MAX_BROWSER_BOOTSTRAP_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

/// One bounded, contiguous page range needed by sparse browser storage.
///
/// The range is always half-open: `[start_page, end_page)`. A zero-page range
/// is used only for the canonical empty base-fact range.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BrowserPageRange {
    start_page: u64,
    page_count: u64,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl BrowserPageRange {
    fn bounded(
        label: &str,
        start_page: u64,
        page_count: u64,
        published_page_count: u64,
    ) -> Result<Self> {
        let end_page = start_page
            .checked_add(page_count)
            .ok_or_else(|| anyhow::anyhow!("{label} page range overflow"))?;
        if end_page > published_page_count {
            anyhow::bail!("{label} page range exceeds published page count");
        }
        Ok(Self {
            start_page,
            page_count,
        })
    }

    pub(crate) fn start_page(self) -> u64 {
        self.start_page
    }

    pub(crate) fn page_count(self) -> u64 {
        self.page_count
    }

    pub(crate) fn end_page(self) -> u64 {
        // Every instance is constructed through `bounded`, which checked the
        // addition. Keep this accessor infallible for browser range loops.
        self.start_page + self.page_count
    }

    fn contains(self, page_id: u64) -> bool {
        page_id >= self.start_page && page_id < self.end_page()
    }

    fn overlaps(self, other: Self) -> bool {
        self.start_page < other.end_page() && other.start_page < self.end_page()
    }
}

/// One page-0 manifest candidate whose payload range is safe to fetch.
///
/// Candidates remain ordered newest-first, but sparse bootstrap deliberately
/// keeps every valid slot so a missing newest manifest or segment can recover
/// through the previous published lineage.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BrowserManifestCandidate {
    slot: HeaderManifestSlotName,
    descriptor: HeaderManifestSlot,
    manifest_range: BrowserPageRange,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl BrowserManifestCandidate {
    pub(crate) fn slot(self) -> HeaderManifestSlotName {
        self.slot
    }

    pub(crate) fn generation(self) -> u64 {
        self.descriptor.generation()
    }

    pub(crate) fn manifest_range(self) -> BrowserPageRange {
        self.manifest_range
    }
}

/// A valid published image that the bounded browser bootstrap deliberately
/// does not support. This is distinct from a missing or corrupt candidate:
/// falling back to an older manifest would silently expose stale committed
/// state, so paged open must fail and eager compatibility may retry the full
/// native-equivalent loader.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Debug)]
struct BrowserSparseUnsupported(String);

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl std::fmt::Display for BrowserSparseUnsupported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl std::error::Error for BrowserSparseUnsupported {}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn browser_sparse_unsupported(message: impl Into<String>) -> anyhow::Error {
    BrowserSparseUnsupported(message.into()).into()
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn is_browser_sparse_unsupported(error: &anyhow::Error) -> bool {
    error.downcast_ref::<BrowserSparseUnsupported>().is_some()
}

/// Page-0-derived first phase of a sparse browser v11 open.
///
/// This phase performs no backend reads beyond the supplied page 0 and makes
/// no attacker-sized allocation. It identifies the integrity catalog that is
/// mandatory for every open and each independently recoverable manifest
/// payload that the async IndexedDB layer may fetch next.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Clone, Debug)]
pub(crate) struct BrowserV11BootstrapPlan {
    header: FileHeader,
    extension: HeaderExtension,
    page0_checksum: u32,
    base_covered_range: BrowserPageRange,
    base_fact_range: BrowserPageRange,
    required_ranges: Vec<BrowserPageRange>,
    manifest_candidates: Vec<BrowserManifestCandidate>,
}

/// One decoded manifest and the segment ranges that must be resident before
/// synchronous core loading can try that lineage.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowserManifestSegmentPlan {
    slot: HeaderManifestSlotName,
    generation: u64,
    manifest_range: BrowserPageRange,
    segment_ranges: Vec<BrowserPageRange>,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl BrowserManifestSegmentPlan {
    pub(crate) fn slot(&self) -> HeaderManifestSlotName {
        self.slot
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn manifest_range(&self) -> BrowserPageRange {
        self.manifest_range
    }

    pub(crate) fn segment_ranges(&self) -> &[BrowserPageRange] {
        &self.segment_ranges
    }
}

/// Metadata-validated second phase of a sparse browser v11 open.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Clone, Debug)]
pub(crate) struct BrowserV11ResidentPlan {
    published_page_count: u64,
    page0_checksum: u32,
    base_integrity: Option<Arc<BasePageIntegrityCatalog>>,
    manifest_candidates: Vec<BrowserManifestSegmentPlan>,
}

/// Streaming verifier for a paged browser export.
///
/// The output blob itself is necessarily O(total), but this cursor adds only
/// O(selected-segment-count) ranges. Immutable base pages are checked against
/// the generation-bound catalog, while catalog/selected manifest/selected
/// segment bytes must match the exact resident bytes validated at open.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
pub(crate) struct BrowserPublishedExportVerifier {
    published_page_count: u64,
    next_page_id: u64,
    exact_resident_ranges: Vec<BrowserPageRange>,
    next_exact_range_index: usize,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl BrowserPublishedExportVerifier {
    pub(crate) fn published_page_count(&self) -> u64 {
        self.published_page_count
    }

    pub(crate) fn finish(self) -> Result<()> {
        if self.next_page_id != self.published_page_count {
            anyhow::bail!(
                "Browser export ended at page {}, expected {}",
                self.next_page_id,
                self.published_page_count
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn exact_resident_ranges_for_test(&self) -> &[BrowserPageRange] {
        &self.exact_resident_ranges
    }
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl BrowserV11ResidentPlan {
    pub(crate) fn published_page_count(&self) -> u64 {
        self.published_page_count
    }

    pub(crate) fn manifest_candidates(&self) -> &[BrowserManifestSegmentPlan] {
        &self.manifest_candidates
    }

    /// Return the exact-range union across every valid manifest candidate.
    ///
    /// Only identical ranges are deduplicated. Adjacent ranges remain separate:
    /// combining an older complete segment with a missing newest segment would
    /// turn one optional range failure into loss of the recoverable lineage.
    pub(crate) fn candidate_segment_ranges(&self) -> Vec<BrowserPageRange> {
        let mut ranges: Vec<BrowserPageRange> = self
            .manifest_candidates
            .iter()
            .flat_map(|candidate| candidate.segment_ranges.iter().copied())
            .collect();
        ranges.sort_unstable();
        ranges.dedup();
        ranges
    }

    /// Validate one asynchronously fetched page before it enters the
    /// synchronous resident backend. Immutable base pages are checked against
    /// the generation-bound v11 catalog; other published pages are bounded and
    /// later validated by their manifest/segment codecs as complete payloads.
    pub(crate) fn verify_fetched_published_page(&self, page_id: u64, page: &[u8]) -> Result<()> {
        if page_id >= self.published_page_count {
            anyhow::bail!("Fetched page is outside the published page range");
        }
        if page.len() != PAGE_SIZE {
            anyhow::bail!(
                "Fetched page {page_id} has invalid length {} (expected {PAGE_SIZE})",
                page.len()
            );
        }
        if page_id == 0 && crc32fast::hash(page) != self.page0_checksum {
            anyhow::bail!("Fetched page 0 changed after sparse bootstrap planning");
        }
        if let Some(catalog) = &self.base_integrity {
            let covered = BrowserPageRange::bounded(
                "Base integrity coverage",
                catalog.covered_page_start(),
                catalog.covered_page_count(),
                self.published_page_count,
            )?;
            if covered.contains(page_id) {
                catalog.verify_page(page_id, page)?;
            }
        }
        Ok(())
    }
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
impl BrowserV11BootstrapPlan {
    /// Validate page 0 and plan the bounded metadata reads for a sparse v11
    /// browser open.
    pub(crate) fn from_page0(page0: &[u8]) -> Result<Self> {
        if page0.len() != PAGE_SIZE {
            anyhow::bail!(
                "Browser v11 bootstrap requires exactly {PAGE_SIZE} page-0 bytes, got {}",
                page0.len()
            );
        }

        let header = FileHeader::from_bytes(page0)?;
        header.validate()?;
        if header.version < crate::storage::INTEGRITY_FORMAT_VERSION
            || header.version > crate::storage::FORMAT_VERSION
        {
            anyhow::bail!(
                "Sparse browser bootstrap requires a paged-ready v{}..=v{} format, got v{}",
                crate::storage::INTEGRITY_FORMAT_VERSION,
                crate::storage::FORMAT_VERSION,
                header.version
            );
        }
        if header.header_checksum != 0 {
            let computed = compute_header_checksum_from_bytes(page0);
            if computed != header.header_checksum {
                anyhow::bail!("Header checksum mismatch during sparse browser bootstrap");
            }
        }

        let extension = HeaderExtension::read_from_page0(header.version, page0)?
            .ok_or_else(|| anyhow::anyhow!("v11 database is missing its header extension"))?;
        let integrity_descriptor = validate_v11_base_integrity_descriptor(&header, &extension)?;

        let base_fact_range = BrowserPageRange::bounded(
            "Base fact",
            extension.base_fact_page_start(),
            header.fact_page_count,
            header.page_count,
        )?;
        let base_covered_range = match integrity_descriptor {
            Some(descriptor) => BrowserPageRange::bounded(
                "Base integrity coverage",
                descriptor.covered_page_start(),
                descriptor.covered_page_count(),
                header.page_count,
            )?,
            None => BrowserPageRange::bounded(
                "Empty base integrity coverage",
                extension.base_fact_page_start(),
                0,
                header.page_count,
            )?,
        };

        let required_ranges: Vec<BrowserPageRange> = integrity_descriptor
            .map(|descriptor| {
                BrowserPageRange::bounded(
                    "Base integrity catalog",
                    descriptor.catalog_page_start(),
                    descriptor.catalog_page_count(),
                    header.page_count,
                )
            })
            .transpose()?
            .into_iter()
            .collect();

        let manifest_candidates = plan_browser_manifest_candidates(
            &header,
            &extension,
            base_covered_range,
            required_ranges.first().copied(),
        )?;

        Ok(Self {
            header,
            extension,
            page0_checksum: crc32fast::hash(page0),
            base_covered_range,
            base_fact_range,
            required_ranges,
            manifest_candidates,
        })
    }

    pub(crate) fn published_page_count(&self) -> u64 {
        self.header.page_count
    }

    pub(crate) fn base_covered_range(&self) -> BrowserPageRange {
        self.base_covered_range
    }

    pub(crate) fn base_fact_range(&self) -> BrowserPageRange {
        self.base_fact_range
    }

    /// Mandatory metadata ranges, excluding page 0. For a non-empty v11 base
    /// this is exactly the integrity catalog range.
    pub(crate) fn required_ranges(&self) -> &[BrowserPageRange] {
        &self.required_ranges
    }

    /// Independently recoverable manifest payloads in newest-first order.
    pub(crate) fn manifest_candidates(&self) -> &[BrowserManifestCandidate] {
        &self.manifest_candidates
    }

    pub(crate) fn candidate_manifest_ranges(&self) -> Vec<BrowserPageRange> {
        self.manifest_candidates
            .iter()
            .map(|candidate| candidate.manifest_range)
            .collect()
    }

    /// Decode every resident manifest candidate and plan its segment ranges.
    ///
    /// The supplied backend only needs page 0, the `required_ranges()`, and any
    /// manifest ranges that were successfully fetched. A missing or corrupt
    /// newest manifest is skipped so an older valid candidate can still be
    /// planned, matching normal persistent-load fallback semantics.
    pub(crate) fn plan_resident_metadata<B: StorageBackend>(
        &self,
        backend: &B,
    ) -> Result<BrowserV11ResidentPlan> {
        let resident_page0 = backend.read_page(0)?;
        if resident_page0.len() != PAGE_SIZE
            || crc32fast::hash(&resident_page0) != self.page0_checksum
        {
            anyhow::bail!("Resident page 0 changed during sparse browser bootstrap");
        }

        let base_integrity = load_v11_base_integrity(backend, &self.header, &self.extension)?;

        let mut manifest_plans = Vec::new();
        for candidate in &self.manifest_candidates {
            let Ok(manifest) =
                read_manifest_from_descriptor(backend, &self.header, candidate.descriptor)
            else {
                continue;
            };
            if manifest
                .base_identity()
                .validate_against_header(&self.header)
                .is_err()
            {
                continue;
            }

            let plan = match plan_browser_manifest_segments(
                &self.header,
                *candidate,
                &manifest,
                self.base_covered_range,
                self.required_ranges.first().copied(),
            ) {
                Ok(plan) => plan,
                Err(error) if is_browser_sparse_unsupported(&error) => return Err(error),
                Err(_) => continue,
            };
            manifest_plans.push(plan);
        }

        if !self.manifest_candidates.is_empty() && manifest_plans.is_empty() {
            anyhow::bail!("No valid delta manifest remains for sparse browser recovery");
        }

        Ok(BrowserV11ResidentPlan {
            published_page_count: self.header.page_count,
            page0_checksum: self.page0_checksum,
            base_integrity,
            manifest_candidates: manifest_plans,
        })
    }
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn plan_browser_manifest_candidates(
    header: &FileHeader,
    extension: &HeaderExtension,
    base_covered_range: BrowserPageRange,
    catalog_range: Option<BrowserPageRange>,
) -> Result<Vec<BrowserManifestCandidate>> {
    let slots = [
        (HeaderManifestSlotName::Primary, extension.primary()),
        (HeaderManifestSlotName::Secondary, extension.secondary()),
    ];
    let has_non_empty_slot = slots.iter().any(|(_, descriptor)| !descriptor.is_empty());
    let mut candidates = Vec::new();

    for (slot, descriptor) in slots {
        if !descriptor.is_selectable() {
            continue;
        }
        validate_browser_manifest_resource_policy(descriptor)?;
        let manifest_range = match validate_browser_manifest_descriptor(header, descriptor) {
            Ok(range) => range,
            Err(error) if is_browser_sparse_unsupported(&error) => return Err(error),
            Err(_) => continue,
        };
        if manifest_range.overlaps(base_covered_range)
            || catalog_range.is_some_and(|catalog| manifest_range.overlaps(catalog))
        {
            return Err(browser_sparse_unsupported(
                "Delta manifest layout overlaps immutable base metadata; bounded browser open is unsupported",
            ));
        }
        candidates.push(BrowserManifestCandidate {
            slot,
            descriptor,
            manifest_range,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .generation()
            .cmp(&left.generation())
            .then_with(|| match (left.slot, right.slot) {
                (HeaderManifestSlotName::Primary, HeaderManifestSlotName::Secondary) => {
                    std::cmp::Ordering::Less
                }
                (HeaderManifestSlotName::Secondary, HeaderManifestSlotName::Primary) => {
                    std::cmp::Ordering::Greater
                }
                _ => std::cmp::Ordering::Equal,
            })
    });

    if has_non_empty_slot && candidates.is_empty() {
        anyhow::bail!("No bounded selectable delta manifest remains in page 0");
    }
    Ok(candidates)
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn validate_browser_manifest_descriptor(
    header: &FileHeader,
    descriptor: HeaderManifestSlot,
) -> Result<BrowserPageRange> {
    BrowserPageRange::bounded(
        "Delta manifest",
        descriptor.manifest_page_start(),
        descriptor.manifest_page_count(),
        header.page_count,
    )
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn validate_browser_manifest_resource_policy(descriptor: HeaderManifestSlot) -> Result<()> {
    let manifest_len = usize::try_from(descriptor.manifest_len()).map_err(|_| {
        browser_sparse_unsupported("Delta manifest length exceeds browser memory limits")
    })?;
    if manifest_len > MAX_BROWSER_BOOTSTRAP_MANIFEST_BYTES {
        return Err(browser_sparse_unsupported(format!(
            "Delta manifest exceeds the supported {}-byte browser bootstrap metadata limit",
            MAX_BROWSER_BOOTSTRAP_MANIFEST_BYTES,
        )));
    }
    let canonical_page_count = manifest_len.div_ceil(PAGE_SIZE);
    let descriptor_page_count =
        usize::try_from(descriptor.manifest_page_count()).map_err(|_| {
            browser_sparse_unsupported("Delta manifest page count exceeds browser memory limits")
        })?;
    if canonical_page_count != descriptor_page_count {
        return Err(browser_sparse_unsupported(
            "Non-canonical delta manifest page count is unsupported by bounded browser open",
        ));
    }
    Ok(())
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn plan_browser_manifest_segments(
    header: &FileHeader,
    candidate: BrowserManifestCandidate,
    manifest: &DeltaManifest,
    base_covered_range: BrowserPageRange,
    catalog_range: Option<BrowserPageRange>,
) -> Result<BrowserManifestSegmentPlan> {
    let mut segment_ranges = Vec::new();
    segment_ranges
        .try_reserve_exact(manifest.segments().len())
        .map_err(|_| {
            browser_sparse_unsupported("Delta segment range plan exceeds browser memory limits")
        })?;

    for descriptor in manifest.segments() {
        let segment_range = BrowserPageRange::bounded(
            "Delta segment",
            descriptor.segment_page_start(),
            descriptor.segment_page_count(),
            header.page_count,
        )?;
        if segment_range.end_page() > candidate.manifest_range.start_page() {
            return Err(browser_sparse_unsupported(
                "Delta segment topology reaches its publishing manifest; bounded browser open is unsupported",
            ));
        }
        if segment_range.overlaps(base_covered_range)
            || catalog_range.is_some_and(|catalog| segment_range.overlaps(catalog))
        {
            return Err(browser_sparse_unsupported(
                "Delta segment topology overlaps immutable base metadata; bounded browser open is unsupported",
            ));
        }
        segment_ranges.push(segment_range);
    }

    Ok(BrowserManifestSegmentPlan {
        slot: candidate.slot,
        generation: candidate.generation(),
        manifest_range: candidate.manifest_range,
        segment_ranges,
    })
}

impl LayeredFactLoaderImpl {
    fn new(
        base: Arc<dyn CommittedFactReader>,
        delta_facts: Arc<RwLock<BTreeMap<FactRef, Fact>>>,
    ) -> Self {
        Self { base, delta_facts }
    }
}

impl CommittedFactReader for LayeredFactLoaderImpl {
    fn resolve(&self, fact_ref: FactRef) -> anyhow::Result<Fact> {
        let delta_facts = self
            .delta_facts
            .read()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(fact) = delta_facts.get(&fact_ref) {
            return Ok(fact.clone());
        }
        drop(delta_facts);
        self.base.resolve(fact_ref)
    }

    fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
        let mut facts = self.base.stream_all()?;
        let delta_facts = self
            .delta_facts
            .read()
            .unwrap_or_else(|error| error.into_inner());
        facts.extend(delta_facts.values().cloned());
        Ok(facts)
    }

    fn for_each_fact(
        &self,
        visit: &mut dyn FnMut(Fact) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.base.for_each_fact(visit)?;
        let delta_facts: Vec<Fact> = self
            .delta_facts
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect();
        for fact in delta_facts {
            visit(fact)?;
        }
        Ok(())
    }

    fn for_each_fact_since(
        &self,
        since_tx_count: u64,
        visit: &mut dyn FnMut(Fact) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        // The base skips its own committed pages via the tx-ordered page
        // probe; delta facts already live in memory, so the filter below is
        // proportional to the delta size, not the committed graph size.
        self.base.for_each_fact_since(since_tx_count, visit)?;
        let delta_facts = self
            .delta_facts
            .read()
            .unwrap_or_else(|error| error.into_inner());
        for fact in delta_facts.values() {
            if fact.tx_count > since_tx_count {
                visit(fact.clone())?;
            }
        }
        Ok(())
    }

    fn committed_page_count(&self) -> u64 {
        self.base.committed_page_count()
    }
}

/// V1 fact format (Phase 3, before bi-temporal fields were added).
///
/// Used only during migration from v1 → v2 file format.
#[derive(Debug, serde::Deserialize)]
struct FactV1 {
    entity: crate::graph::types::EntityId,
    attribute: crate::graph::types::Attribute,
    value: crate::graph::types::Value,
    tx_id: crate::graph::types::TxId,
    asserted: bool,
}

/// Persistent fact storage with serialization support.
///
/// Architecture:
/// - Page 0: versioned header, delta-manifest slots, and base-integrity authority.
/// - Packed fact pages plus on-disk EAVT/AEVT/AVET/VAET B+trees.
/// - Optional append-only delta segments and manifests between recompactions.
/// - An in-file, generation-bound checksum catalog for every immutable base page.
///
/// Committed facts and indexes are resolved lazily through a bounded page cache;
/// only pending writes live in the mutable in-memory [`FactStorage`]. Full base
/// publication and copy-on-write recompact write data and integrity metadata
/// before page 0, while delta checkpoints preserve the selected base identity.
pub struct PersistentFactStorage<B: StorageBackend + 'static> {
    backend: Arc<Mutex<B>>,
    page_cache: Arc<PageCache>,
    storage: FactStorage,
    dirty: bool,
    last_checkpointed_tx_count: u64,
    header_manifest_selection: HeaderManifestSlotSelection,
    delta_manifest_selection: PersistedManifestSelection,
    /// Shared committed delta state for the selected manifest. A checkpoint
    /// extends these maps with only the newly published segment instead of
    /// rereading or rematerializing the complete selected lineage.
    resident_delta_segment_count: usize,
    resident_delta_facts: Option<Arc<RwLock<BTreeMap<FactRef, Fact>>>>,
    resident_delta_indexes: Option<Arc<RwLock<DeltaIndexEntries>>>,
    committed_fact_pages: Arc<AtomicU64>,
    committed_fact_page_start: Arc<AtomicU64>,
    base_integrity: Option<Arc<BasePageIntegrityCatalog>>,
    loaded_format_version: u32,
    write_blocked_legacy: bool,
    btree_build_options: BtreeBuildOptions,
}

impl<B: StorageBackend + 'static> PersistentFactStorage<B> {
    /// Create a new persistent storage with the given backend.
    ///
    /// If the backend already contains data, loads it.
    /// Otherwise, initializes a new empty fact storage.
    ///
    /// `page_cache_capacity` controls the LRU page cache size (in pages).
    /// A value of 256 means at most 256 x 4KB = 1MB of cached pages.
    pub fn new(backend: B, page_cache_capacity: usize) -> Result<Self> {
        Self::new_with_btree_options(backend, page_cache_capacity, BtreeBuildOptions::default())
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn new_with_btree_fill_percent(
        backend: B,
        page_cache_capacity: usize,
        fill_percent: u8,
    ) -> Result<Self> {
        Self::new_with_btree_options(
            backend,
            page_cache_capacity,
            BtreeBuildOptions::new(fill_percent)?,
        )
    }

    fn new_with_btree_options(
        backend: B,
        page_cache_capacity: usize,
        btree_build_options: BtreeBuildOptions,
    ) -> Result<Self> {
        let backend = Arc::new(Mutex::new(backend));
        let page_cache = Arc::new(PageCache::new(page_cache_capacity));
        let committed_fact_pages = Arc::new(AtomicU64::new(0));
        let committed_fact_page_start = Arc::new(AtomicU64::new(1));
        let mut persistent = PersistentFactStorage {
            backend,
            page_cache,
            storage: FactStorage::new(),
            dirty: false,
            last_checkpointed_tx_count: 0,
            header_manifest_selection: HeaderManifestSlotSelection::NoDeltaManifest,
            delta_manifest_selection: PersistedManifestSelection::NoDeltaManifest,
            resident_delta_segment_count: 0,
            resident_delta_facts: None,
            resident_delta_indexes: None,
            committed_fact_pages,
            committed_fact_page_start,
            base_integrity: None,
            loaded_format_version: crate::storage::FORMAT_VERSION,
            write_blocked_legacy: false,
            btree_build_options,
        };

        // Try to load existing data.
        //
        // The load condition combines two checks:
        // - `!is_new`: FileBackend reports false when the file existed on disk
        //   (even with only a header page, page_count == 1). This ensures
        //   migrate_v5_to_v6 runs for a v5 file that has no fact pages.
        // - `page_count > 1`: catches MemoryBackend, which always reports
        //   is_new == true; page count > 1 means facts were previously saved.
        let (is_new_backend, page_count) = {
            let b = persistent
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            (b.is_new(), b.page_count()?)
        };
        if !is_new_backend || page_count > 1 {
            persistent.load()?;
        } else {
            // New database: FileBackend already wrote the initial header;
            // MemoryBackend starts empty. Nothing to save yet.
        }

        Ok(persistent)
    }

    fn build_btree(
        &self,
        entries: impl Iterator<Item = (Vec<u8>, Vec<u8>)>,
        backend: &mut dyn StorageBackend,
        start_page_id: u64,
    ) -> Result<(u64, u64)> {
        build_btree_with_options(
            entries,
            backend,
            &self.page_cache,
            start_page_id,
            self.btree_build_options,
        )
    }

    fn build_btree_keys<K: Serialize>(
        &self,
        entries: impl Iterator<Item = (K, FactRef)>,
        backend: &mut dyn StorageBackend,
        start_page_id: u64,
    ) -> Result<(u64, u64)> {
        build_btree_from_key_entries(
            entries,
            backend,
            &self.page_cache,
            start_page_id,
            self.btree_build_options,
        )
    }

    fn base_fact_loader(&self) -> Arc<dyn CommittedFactReader> {
        let backend_adapter = match &self.base_integrity {
            Some(base_integrity) => {
                MutexStorageBackend::verified(self.backend.clone(), base_integrity.clone())
            }
            None => MutexStorageBackend::new(self.backend.clone()),
        };
        Arc::new(CommittedFactLoaderImpl {
            backend_adapter,
            page_cache: self.page_cache.clone(),
            committed_fact_pages: self.committed_fact_pages.clone(),
            committed_fact_page_start: self.committed_fact_page_start.clone(),
        })
    }

    fn wire_committed_readers(
        &mut self,
        header: &FileHeader,
        delta_segments: Vec<DeltaSegment>,
    ) -> Result<()> {
        let mut delta_facts = BTreeMap::new();
        let mut eavt = Vec::new();
        let mut aevt = Vec::new();
        let mut avet = Vec::new();
        let mut vaet = Vec::new();
        for segment in &delta_segments {
            let payload = segment.payload();
            delta_facts.extend(payload.facts().iter().cloned());
            eavt.extend(payload.eavt.iter().cloned());
            aevt.extend(payload.aevt.iter().cloned());
            avet.extend(payload.avet.iter().cloned());
            vaet.extend(payload.vaet.iter().cloned());
        }

        let resident_delta_facts =
            (!delta_segments.is_empty()).then(|| Arc::new(RwLock::new(delta_facts)));
        let resident_delta_indexes = (!delta_segments.is_empty()).then(|| {
            Arc::new(RwLock::new(DeltaIndexEntries::from_entries(
                eavt, aevt, avet, vaet,
            )))
        });
        let base_loader = self.base_fact_loader();
        let fact_reader: Arc<dyn CommittedFactReader> = match &resident_delta_facts {
            Some(delta_facts) => {
                Arc::new(LayeredFactLoaderImpl::new(base_loader, delta_facts.clone()))
            }
            None => base_loader,
        };

        if header.eavt_root_page == 0 {
            self.storage.publish_committed_readers(fact_reader, None);
            self.resident_delta_segment_count = delta_segments.len();
            self.resident_delta_facts = resident_delta_facts;
            self.resident_delta_indexes = resident_delta_indexes;
            return Ok(());
        }

        let base_index_reader = Arc::new(match &self.base_integrity {
            Some(base_integrity) => OnDiskIndexReader::new_verified(
                self.backend.clone(),
                self.page_cache.clone(),
                base_integrity.clone(),
                header.eavt_root_page,
                header.aevt_root_page,
                header.avet_root_page,
                header.vaet_root_page,
            ),
            None => OnDiskIndexReader::new(
                self.backend.clone(),
                self.page_cache.clone(),
                header.eavt_root_page,
                header.aevt_root_page,
                header.avet_root_page,
                header.vaet_root_page,
            ),
        });
        let index_reader: Arc<dyn CommittedIndexReader> = match &resident_delta_indexes {
            Some(delta_indexes) => {
                let base_keyed_reader: Arc<dyn KeyedIndexReader> = base_index_reader;
                Arc::new(LayeredIndexReader::new_shared(
                    base_keyed_reader,
                    delta_indexes.clone(),
                ))
            }
            None => base_index_reader,
        };
        self.storage
            .publish_committed_readers(fact_reader, Some(index_reader));
        self.resident_delta_segment_count = delta_segments.len();
        self.resident_delta_facts = resident_delta_facts;
        self.resident_delta_indexes = resident_delta_indexes;
        Ok(())
    }

    fn append_resident_delta_segment(&mut self, segment: &DeltaSegment) -> Result<()> {
        let resident_delta_facts = self
            .resident_delta_facts
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Resident delta fact state is missing"))?;
        let resident_delta_indexes = self
            .resident_delta_indexes
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Resident delta index state is missing"))?;
        let next_segment_count = self
            .resident_delta_segment_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Resident delta segment count overflow"))?;
        let payload = segment.payload();
        self.storage.publish_incremental_committed(|| {
            resident_delta_facts
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .extend(payload.facts().iter().cloned());
            resident_delta_indexes
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .extend_from_entries(&payload.eavt, &payload.aevt, &payload.avet, &payload.vaet);
        });
        self.resident_delta_segment_count = next_segment_count;
        Ok(())
    }

    fn load_usable_delta_selection(
        backend: &B,
        header: &FileHeader,
        page0: &[u8],
    ) -> Result<(
        HeaderManifestSlotSelection,
        PersistedManifestSelection,
        Vec<DeltaSegment>,
    )> {
        let Some(extension) = HeaderExtension::read_from_page0(header.version, page0)? else {
            return Ok((
                HeaderManifestSlotSelection::NoDeltaManifest,
                PersistedManifestSelection::NoDeltaManifest,
                Vec::new(),
            ));
        };

        let primary = extension.primary();
        let secondary = extension.secondary();
        let has_invalid_slot = (!primary.is_empty() && !primary.is_selectable())
            || (!secondary.is_empty() && !secondary.is_selectable());

        let mut candidates = Vec::new();
        if primary.is_selectable() {
            candidates.push((HeaderManifestSlotName::Primary, primary));
        }
        if secondary.is_selectable() {
            candidates.push((HeaderManifestSlotName::Secondary, secondary));
        }

        if candidates.is_empty() {
            if has_invalid_slot {
                return Ok((
                    HeaderManifestSlotSelection::RecoveryRequired {
                        reason: HeaderManifestSlotRecoveryReason::CorruptManifestSlot,
                    },
                    PersistedManifestSelection::RecoveryRequired {
                        reason: PersistedManifestRecoveryReason::CorruptManifestSlot,
                    },
                    Vec::new(),
                ));
            }
            return Ok((
                HeaderManifestSlotSelection::NoDeltaManifest,
                PersistedManifestSelection::NoDeltaManifest,
                Vec::new(),
            ));
        }

        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.1.generation()));
        for (slot, descriptor) in candidates {
            let Ok(manifest) = read_manifest_from_descriptor(backend, header, descriptor) else {
                continue;
            };
            let Ok(delta_segments) =
                Self::load_delta_segments_from_manifest(backend, header, &manifest)
            else {
                continue;
            };
            return Ok((
                HeaderManifestSlotSelection::Use { slot, descriptor },
                PersistedManifestSelection::Use { slot, manifest },
                delta_segments,
            ));
        }

        Ok((
            HeaderManifestSlotSelection::RecoveryRequired {
                reason: HeaderManifestSlotRecoveryReason::CorruptManifestSlot,
            },
            PersistedManifestSelection::RecoveryRequired {
                reason: PersistedManifestRecoveryReason::NoValidManifest,
            },
            Vec::new(),
        ))
    }

    /// Upgrade a complete v10 published image by appending integrity metadata
    /// and publishing only page 0. Existing base, delta, and manifest bytes are
    /// preserved exactly. If the published prefix is physically incomplete but
    /// recoverable through an older manifest, leave it on v10 and make the
    /// handle read-only rather than filling holes or changing export behavior.
    fn migrate_v10_to_v11(
        &mut self,
        header: &FileHeader,
        page0: &[u8],
        delta_selection: &PersistedManifestSelection,
    ) -> Result<bool> {
        let extension = HeaderExtension::read_from_page0(header.version, page0)?
            .ok_or_else(|| anyhow::anyhow!("v10 migration requires a header extension"))?;
        let base_page_end = match delta_selection {
            PersistedManifestSelection::NoDeltaManifest => header.page_count,
            PersistedManifestSelection::Use { manifest, .. } => {
                manifest.base_identity().page_count()
            }
            PersistedManifestSelection::RecoveryRequired { .. } => {
                anyhow::bail!("Cannot migrate a v10 graph that requires manifest recovery")
            }
        };
        let base_page_start = extension.base_fact_page_start();
        if base_page_end < base_page_start {
            anyhow::bail!("v10 base page range is invalid");
        }

        let covered_page_count = base_page_end.saturating_sub(base_page_start);
        if covered_page_count > 0 {
            // Validate the eager metadata footprint before checking a sparse
            // published prefix or scanning any base page.
            BasePageIntegrityCatalog::encoded_len_for_page_count(covered_page_count)?;
        }

        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;

        // A fallback-safe v10 image may intentionally have missing newest
        // manifest pages. Migrating such an image would either create sparse
        // published holes or silently turn them into zero pages on native.
        // Preserve the exact v10 recovery state and block writes instead.
        let published_prefix_complete = backend.has_complete_page_prefix(header.page_count)?;

        if covered_page_count == 0 {
            let roots_are_empty = header.eavt_root_page == 0
                && header.aevt_root_page == 0
                && header.avet_root_page == 0
                && header.vaet_root_page == 0;
            let delta_is_empty = match delta_selection {
                PersistedManifestSelection::NoDeltaManifest => true,
                PersistedManifestSelection::Use { manifest, .. } => manifest.segments().is_empty(),
                PersistedManifestSelection::RecoveryRequired { .. } => false,
            };
            if base_page_start != 1
                || header.fact_page_count != 0
                || header.node_count != 0
                || !roots_are_empty
                || !delta_is_empty
            {
                anyhow::bail!("Non-canonical empty v10 base cannot migrate to v11");
            }

            let mut migrated_header = *header;
            migrated_header.version = crate::storage::FORMAT_VERSION;
            migrated_header.page_count = 1;
            migrated_header.header_checksum = compute_header_checksum(&migrated_header);
            let migrated_page0 = build_header_page(migrated_header)?;
            backend.write_page(0, &migrated_page0)?;
            backend.sync()?;
            return Ok(true);
        }

        if !published_prefix_complete {
            if covered_page_count > 0 {
                let aggregate =
                    compute_page_checksum(&*backend, base_page_start, covered_page_count)?;
                if aggregate != header.index_checksum {
                    anyhow::bail!("Base checksum mismatch during v10 recovery");
                }
            }
            drop(backend);
            self.write_blocked_legacy = true;
            return Ok(false);
        }

        let integrity = write_base_integrity_catalog(
            &mut *backend,
            1,
            base_page_start,
            covered_page_count,
            header.page_count,
            (covered_page_count > 0).then_some(header.index_checksum),
        )?;

        let mut migrated_header = *header;
        migrated_header.version = crate::storage::FORMAT_VERSION;
        migrated_header.page_count = integrity.published_page_count;
        migrated_header.header_checksum = compute_header_checksum(&migrated_header);
        let migrated_extension = HeaderExtension::new(extension.primary(), extension.secondary())
            .with_base_fact_page_start(base_page_start)?
            .with_base_integrity(integrity.descriptor)?;
        let migrated_page0 = build_header_page_with_extension(migrated_header, migrated_extension)?;

        // Data and catalog were synced by write_base_integrity_catalog. Page 0
        // is the only publish write and remains v10 on every earlier failure.
        backend.write_page(0, &migrated_page0)?;
        backend.sync()?;
        drop(backend);
        Ok(true)
    }

    fn build_next_manifest_extension(
        header: &FileHeader,
        page0: &[u8],
        current_selection: HeaderManifestSlotSelection,
        descriptor: HeaderManifestSlot,
    ) -> Result<(HeaderExtension, HeaderManifestSlotName)> {
        let extension = HeaderExtension::read_from_page0(header.version, page0)?
            .unwrap_or_else(HeaderExtension::empty);

        match current_selection {
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Primary,
                ..
            } => Ok((
                HeaderExtension::new(extension.primary(), descriptor)
                    .with_base_fact_page_start(extension.base_fact_page_start())?
                    .with_base_integrity(extension.base_integrity())?,
                HeaderManifestSlotName::Secondary,
            )),
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Secondary,
                ..
            } => Ok((
                HeaderExtension::new(descriptor, extension.secondary())
                    .with_base_fact_page_start(extension.base_fact_page_start())?
                    .with_base_integrity(extension.base_integrity())?,
                HeaderManifestSlotName::Primary,
            )),
            HeaderManifestSlotSelection::NoDeltaManifest
            | HeaderManifestSlotSelection::RecoveryRequired { .. } => Ok((
                HeaderExtension::new(descriptor, HeaderManifestSlot::empty())
                    .with_base_fact_page_start(extension.base_fact_page_start())?
                    .with_base_integrity(extension.base_integrity())?,
                HeaderManifestSlotName::Primary,
            )),
        }
    }

    fn load_delta_segments_from_manifest(
        backend: &B,
        header: &FileHeader,
        manifest: &DeltaManifest,
    ) -> Result<Vec<DeltaSegment>> {
        manifest
            .segments()
            .iter()
            .map(|descriptor| Self::read_delta_segment_from_descriptor(backend, header, descriptor))
            .collect()
    }

    fn read_delta_segment_from_descriptor(
        backend: &B,
        header: &FileHeader,
        descriptor: &DeltaManifestSegment,
    ) -> Result<DeltaSegment> {
        let segment_end = descriptor
            .segment_page_start()
            .checked_add(descriptor.segment_page_count())
            .ok_or_else(|| anyhow::anyhow!("Delta segment page range overflow"))?;
        if segment_end > header.page_count {
            anyhow::bail!("Delta segment page range out of bounds");
        }

        let page_count = usize::try_from(descriptor.segment_page_count())
            .map_err(|_| anyhow::anyhow!("Delta segment page count exceeds usize"))?;
        let capacity = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or_else(|| anyhow::anyhow!("Delta segment page capacity overflow"))?;
        let mut bytes = Vec::with_capacity(capacity);
        for offset in 0..descriptor.segment_page_count() {
            let page_id = descriptor
                .segment_page_start()
                .checked_add(offset)
                .ok_or_else(|| anyhow::anyhow!("Delta segment page id overflow"))?;
            let page = backend.read_page(page_id)?;
            if page.len() != PAGE_SIZE {
                anyhow::bail!("Delta segment page has invalid size");
            }
            bytes.extend_from_slice(&page);
        }

        let segment = DeltaSegment::decode_from_page_bytes(&bytes)?;
        let segment_header = segment.header();
        if segment_header.fact_page_start != descriptor.fact_page_start()
            || segment_header.fact_page_count != descriptor.fact_page_count()
            || segment_header.low_tx_count != descriptor.low_tx_count()
            || segment_header.high_tx_count != descriptor.high_tx_count()
        {
            anyhow::bail!("Delta segment header does not match manifest descriptor");
        }
        Ok(segment)
    }

    /// Build a compact, contiguous copy of the complete visible fact log on a
    /// fresh backend.
    ///
    /// The source is streamed into packed pages and the four index-entry
    /// buffers; it is never materialized as an additional `Vec<Fact>`. Every
    /// full-history identity field is preserved because facts are copied
    /// verbatim. This is the browser physical-reclaim primitive: the caller
    /// can atomically replace IndexedDB with the returned page image, then swap
    /// the live handle only after that durable replacement succeeds.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn build_compact_copy<C: StorageBackend + 'static>(
        &self,
        backend: C,
        page_cache_capacity: usize,
    ) -> Result<PersistentFactStorage<C>> {
        let checkpoint_tx_count = self.storage.current_tx_count();
        let source = self.storage.clone();
        let mut candidate = PersistentFactStorage::new(backend, page_cache_capacity)?;
        candidate.write_fresh_base_from_source(checkpoint_tx_count, |visit| {
            source.for_each_fact(visit)
        })?;
        Ok(candidate)
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn write_fresh_base_from_source(
        &mut self,
        checkpoint_tx_count: u64,
        source: impl FnOnce(&mut dyn FnMut(Fact) -> Result<()>) -> Result<()>,
    ) -> Result<()> {
        {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            if !backend.is_new() || backend.page_count()? != 0 {
                anyhow::bail!("Compact copy destination backend must be empty");
            }
        }

        let mut packer = PackedFactPacker::new(1);
        let mut index_entries = new_index_entries_with_capacity(0);
        let mut node_count = 0u64;
        source(&mut |fact| {
            let fact_ref = packer.push(&fact)?;
            push_index_entries_for_fact(&mut index_entries, &fact, fact_ref);
            node_count = node_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("fact count exceeds u64::MAX"))?;
            Ok(())
        })?;

        let fact_pages = packer.finish();
        let num_fact_pages = u64::try_from(fact_pages.len())
            .map_err(|_| anyhow::anyhow!("fact page count exceeds u64::MAX"))?;
        sort_index_entries(&mut index_entries);
        let (eavt_entries, aevt_entries, avet_entries, vaet_entries) = index_entries;

        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        for (i, page_data) in fact_pages.iter().enumerate() {
            let page_offset =
                u64::try_from(i).map_err(|_| anyhow::anyhow!("page index {i} exceeds u64::MAX"))?;
            let page_id = 1u64
                .checked_add(page_offset)
                .ok_or_else(|| anyhow::anyhow!("page id overflow writing fact pages"))?;
            backend.write_page(page_id, page_data)?;
        }

        let index_start = 1u64
            .checked_add(num_fact_pages)
            .ok_or_else(|| anyhow::anyhow!("page count overflow computing index start"))?;
        let (eavt_root, next1) = self.build_btree(
            btree_entries(eavt_entries.into_iter())?.into_iter(),
            &mut *backend,
            index_start,
        )?;
        let (aevt_root, next2) = self.build_btree(
            btree_entries(aevt_entries.into_iter())?.into_iter(),
            &mut *backend,
            next1,
        )?;
        let (avet_root, next3) = self.build_btree(
            btree_entries(avet_entries.into_iter())?.into_iter(),
            &mut *backend,
            next2,
        )?;
        let (vaet_root, next4) = self.build_btree(
            btree_entries(vaet_entries.into_iter())?.into_iter(),
            &mut *backend,
            next3,
        )?;
        let total_data_pages = next4.saturating_sub(1);
        backend.sync()?;
        let integrity =
            write_base_integrity_catalog(&mut *backend, 1, 1, total_data_pages, next4, None)?;
        let mut header = FileHeader::new();
        header.page_count = integrity.published_page_count;
        header.node_count = node_count;
        header.last_checkpointed_tx_count = checkpoint_tx_count;
        header.eavt_root_page = eavt_root;
        header.aevt_root_page = aevt_root;
        header.avet_root_page = avet_root;
        header.vaet_root_page = vaet_root;
        header.index_checksum = integrity.aggregate_checksum;
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        header.fact_page_count = num_fact_pages;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page_with_base_integrity(header, 1, integrity.descriptor)?;
        let manifest_selection =
            select_header_manifest_slot_from_page0(header.version, &header_page)?;
        backend.write_page(0, &header_page)?;
        backend.sync()?;
        drop(backend);

        self.header_manifest_selection = manifest_selection;
        self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
        self.committed_fact_pages
            .store(num_fact_pages, Ordering::SeqCst);
        self.committed_fact_page_start.store(1, Ordering::SeqCst);
        self.base_integrity = Some(integrity.catalog);
        self.last_checkpointed_tx_count = checkpoint_tx_count;
        self.storage.restore_tx_counter_from(checkpoint_tx_count);
        self.dirty = false;
        self.wire_committed_readers(&header, Vec::new())?;
        self.storage.post_checkpoint_clear();
        Ok(())
    }

    /// Publish decoded legacy facts as a copy-on-write v11 base.
    ///
    /// The candidate starts after the complete legacy published image, so an
    /// interrupted migration leaves the old page-0 authority and every page it
    /// references untouched. Only the final page-0 write switches formats.
    fn publish_legacy_facts_as_v11(
        &mut self,
        header: &FileHeader,
        mut facts: Vec<Fact>,
    ) -> Result<()> {
        if header.version < 9 {
            normalize_legacy_retractions(&mut facts);
        }
        let fact_count = u64::try_from(facts.len())
            .map_err(|_| anyhow::anyhow!("legacy fact count exceeds u64::MAX"))?;
        if fact_count != header.node_count {
            anyhow::bail!(
                "Legacy fact count does not match header node_count; refusing partial migration"
            );
        }
        let max_tx = facts
            .iter()
            .map(|fact| fact.tx_count)
            .max()
            .unwrap_or(0)
            .max(header.last_checkpointed_tx_count);

        {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            if !backend.has_complete_page_prefix(header.page_count)? {
                anyhow::bail!(
                    "Legacy published page range is physically incomplete; refusing sparse COW migration"
                );
            }
        }

        self.storage.clear()?;
        self.storage.restore_tx_counter_from(max_tx);

        let candidate =
            self.write_cow_candidate_from_source(header.page_count, 1, max_tx, move |visit| {
                for fact in facts {
                    visit(fact)?;
                }
                Ok(())
            })?;
        self.publish_recompact_candidate(candidate)?;
        Ok(())
    }

    fn write_recompact_candidate_from_visible_facts(&mut self) -> Result<RecompactCandidate> {
        let checkpoint_tx_count = self.storage.current_tx_count();
        let base_generation = self
            .base_integrity
            .as_ref()
            .map(|catalog| catalog.base_generation())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Base integrity generation overflow"))?;

        let base_fact_page_start = {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            let curr_header_page = backend.read_page(0)?;
            let curr_header = FileHeader::from_bytes(&curr_header_page)?;
            curr_header.validate()?;
            curr_header.page_count
        };
        let source = self.storage.clone();
        self.write_cow_candidate_from_source(
            base_fact_page_start,
            base_generation,
            checkpoint_tx_count,
            move |visit| source.for_each_fact(visit),
        )
    }

    fn write_cow_candidate_from_source(
        &mut self,
        base_fact_page_start: u64,
        base_generation: u64,
        checkpoint_tx_count: u64,
        source: impl FnOnce(&mut dyn FnMut(Fact) -> Result<()>) -> Result<()>,
    ) -> Result<RecompactCandidate> {
        if base_fact_page_start == 0 {
            anyhow::bail!("Recompact base candidate cannot start on page 0");
        }

        #[cfg(feature = "bench-internals")]
        reset_checkpoint_construction_diagnostics();
        #[cfg(feature = "bench-internals")]
        let fact_packing_started = Instant::now();

        let mut packer = PackedFactPacker::new(base_fact_page_start);
        let mut node_count = 0u64;
        let mut written_fact_pages = 0u64;
        self.page_cache.invalidate_from(base_fact_page_start);
        let candidate_backend = self.backend.clone();
        source(&mut |fact| {
            packer.push(&fact)?;
            let completed = packer.take_completed_pages();
            if !completed.is_empty() {
                let mut backend = candidate_backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
                for page in completed {
                    let page_id = base_fact_page_start
                        .checked_add(written_fact_pages)
                        .ok_or_else(|| anyhow::anyhow!("fact page id overflow"))?;
                    backend.write_page(page_id, &page)?;
                    written_fact_pages = written_fact_pages
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("fact page count overflow"))?;
                }
            }
            node_count = node_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("fact count exceeds u64::MAX"))?;
            Ok(())
        })?;

        let final_pages = packer.finish();

        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;

        for page_data in final_pages {
            let page_id = base_fact_page_start
                .checked_add(written_fact_pages)
                .ok_or_else(|| anyhow::anyhow!("page id overflow writing recompact facts"))?;
            backend.write_page(page_id, &page_data)?;
            written_fact_pages = written_fact_pages
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("fact page count overflow"))?;
        }
        let num_fact_pages = written_fact_pages;

        #[cfg(feature = "bench-internals")]
        CHECKPOINT_DIAGNOSTICS.set(CheckpointConstructionDiagnostics {
            peak_fact_pages_in_memory: 1,
            fact_page_visits: num_fact_pages.saturating_mul(4),
            fact_packing_micros: checkpoint_elapsed_micros(fact_packing_started),
            ..CheckpointConstructionDiagnostics::default()
        });

        let index_start = base_fact_page_start
            .checked_add(num_fact_pages)
            .ok_or_else(|| {
                anyhow::anyhow!("page count overflow computing recompact index_start")
            })?;
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut eavt_entries = Vec::with_capacity(usize::try_from(node_count).unwrap_or(0));
        visit_fact_refs_in_pages(
            &*backend,
            base_fact_page_start,
            num_fact_pages,
            &mut |fact, fact_ref| {
                eavt_entries.push((eavt_key(&fact), fact_ref));
                Ok(())
            },
        )?;
        eavt_entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        observe_checkpoint_typed_entries(eavt_entries.len());
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.eavt_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (eavt_root, next1) =
            self.build_btree_keys(eavt_entries.into_iter(), &mut *backend, index_start)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.eavt_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut aevt_entries = Vec::with_capacity(usize::try_from(node_count).unwrap_or(0));
        visit_fact_refs_in_pages(
            &*backend,
            base_fact_page_start,
            num_fact_pages,
            &mut |fact, fact_ref| {
                aevt_entries.push((aevt_key(&fact), fact_ref));
                Ok(())
            },
        )?;
        aevt_entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        observe_checkpoint_typed_entries(aevt_entries.len());
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.aevt_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (aevt_root, next2) =
            self.build_btree_keys(aevt_entries.into_iter(), &mut *backend, next1)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.aevt_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut avet_entries = Vec::with_capacity(usize::try_from(node_count).unwrap_or(0));
        visit_fact_refs_in_pages(
            &*backend,
            base_fact_page_start,
            num_fact_pages,
            &mut |fact, fact_ref| {
                avet_entries.push((avet_key(&fact), fact_ref));
                Ok(())
            },
        )?;
        avet_entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        observe_checkpoint_typed_entries(avet_entries.len());
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.avet_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (avet_root, next3) =
            self.build_btree_keys(avet_entries.into_iter(), &mut *backend, next2)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.avet_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut vaet_entries = Vec::new();
        visit_fact_refs_in_pages(
            &*backend,
            base_fact_page_start,
            num_fact_pages,
            &mut |fact, fact_ref| {
                if let Some(key) = vaet_key(&fact) {
                    vaet_entries.push((key, fact_ref));
                }
                Ok(())
            },
        )?;
        vaet_entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        observe_checkpoint_typed_entries(vaet_entries.len());
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.vaet_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (vaet_root, next4) =
            self.build_btree_keys(vaet_entries.into_iter(), &mut *backend, next3)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.vaet_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        let total_data_pages = next4.saturating_sub(base_fact_page_start);
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.sync()?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.data_sync_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let integrity = write_base_integrity_catalog(
            &mut *backend,
            base_generation,
            base_fact_page_start,
            total_data_pages,
            next4,
            None,
        )?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.integrity_catalog_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut header = FileHeader::new();
        header.page_count = integrity.published_page_count;
        header.node_count = node_count;
        header.last_checkpointed_tx_count = checkpoint_tx_count;
        header.eavt_root_page = eavt_root;
        header.aevt_root_page = aevt_root;
        header.avet_root_page = avet_root;
        header.vaet_root_page = vaet_root;
        header.index_checksum = integrity.aggregate_checksum;
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        header.fact_page_count = num_fact_pages;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page_with_base_integrity(
            header,
            base_fact_page_start,
            integrity.descriptor,
        )?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.header_assembly_micros = checkpoint_elapsed_micros(phase_started);
        });
        Ok(RecompactCandidate {
            header,
            header_page,
            base_fact_page_start,
            fact_page_count: num_fact_pages,
            checkpoint_tx_count,
            base_integrity: integrity.catalog,
        })
    }

    fn publish_recompact_candidate(
        &mut self,
        candidate: RecompactCandidate,
    ) -> Result<CheckpointOutcome> {
        let manifest_selection = select_header_manifest_slot_from_page0(
            candidate.header.version,
            &candidate.header_page,
        )?;
        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.write_page(0, &candidate.header_page)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.publish_write_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.sync()?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.publish_sync_micros = checkpoint_elapsed_micros(phase_started);
        });
        drop(backend);

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        self.header_manifest_selection = manifest_selection;
        self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
        self.committed_fact_pages
            .store(candidate.fact_page_count, Ordering::SeqCst);
        self.committed_fact_page_start
            .store(candidate.base_fact_page_start, Ordering::SeqCst);
        self.base_integrity = Some(candidate.base_integrity);
        self.loaded_format_version = candidate.header.version;
        self.last_checkpointed_tx_count = candidate.checkpoint_tx_count;
        self.dirty = false;
        self.wire_committed_readers(&candidate.header, Vec::new())?;
        self.storage.post_checkpoint_clear();
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.publish_finalize_micros = checkpoint_elapsed_micros(phase_started);
        });
        Ok(CheckpointOutcome::FullRebuildFromVisibleDelta)
    }

    /// Load all facts from the backend into memory.
    fn load(&mut self) -> Result<()> {
        let (header, raw_header_bytes) = {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            let header_page = backend.read_page(0)?;
            let h = FileHeader::from_bytes(&header_page)?;
            h.validate()?;
            (h, header_page)
        };
        self.loaded_format_version = header.version;

        // Migrate v1 → v2 if needed
        if header.version < 2 {
            return self.migrate_v1_to_v2();
        }

        // Migrate v5 → v6 (paged-blob indexes → on-disk B+tree)
        if header.version == 5 {
            return self.migrate_v5_to_v6(&header);
        }

        // For v7+ files, validate header checksum using raw bytes from disk
        // For older versions (v6 and earlier), header_checksum is 0 so validation is skipped
        if header.version >= 7 && header.header_checksum != 0 {
            let computed = compute_header_checksum_from_bytes(&raw_header_bytes);
            if header.header_checksum != computed {
                anyhow::bail!(
                    "Header checksum mismatch: possible file corruption. Database may be damaged."
                );
            }
        }

        let header_extension = HeaderExtension::read_from_page0(header.version, &raw_header_bytes)?;
        let base_fact_page_start = header_extension
            .as_ref()
            .map(HeaderExtension::base_fact_page_start)
            .unwrap_or(1);
        if (base_fact_page_start == 0 || base_fact_page_start >= header.page_count.max(1))
            && header.fact_page_count > 0
        {
            anyhow::bail!("Header base fact page start is out of bounds");
        }
        self.committed_fact_page_start
            .store(base_fact_page_start, Ordering::SeqCst);
        self.base_integrity = if header.version >= crate::storage::INTEGRITY_FORMAT_VERSION {
            let extension = header_extension
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("v11 database is missing its header extension"))?;
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            load_v11_base_integrity(&*backend, &header, extension)?
        } else {
            None
        };

        let (header_manifest_selection, delta_manifest_selection, selected_delta_segments) = {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            Self::load_usable_delta_selection(&*backend, &header, &raw_header_bytes)?
        };
        if let PersistedManifestSelection::RecoveryRequired { reason } = delta_manifest_selection {
            let reason = match reason {
                PersistedManifestRecoveryReason::CorruptManifestSlot => "corrupt manifest slot",
                PersistedManifestRecoveryReason::NoValidManifest => "no valid manifest",
            };
            anyhow::bail!("Delta manifest recovery required: {reason}");
        }
        if header.version == 10
            && self.migrate_v10_to_v11(&header, &raw_header_bytes, &delta_manifest_selection)?
        {
            return self.load();
        }
        self.header_manifest_selection = header_manifest_selection;
        self.delta_manifest_selection = delta_manifest_selection;
        let selected_delta_manifest = self.delta_manifest_selection.manifest().cloned();
        let selected_delta_has_segments = selected_delta_manifest
            .as_ref()
            .is_some_and(|manifest| !manifest.segments().is_empty());

        // Store last_checkpointed_tx_count from header (0 for v2 files)
        self.last_checkpointed_tx_count = header.last_checkpointed_tx_count;

        // Clear existing storage
        self.storage.clear()?;

        let fact_page_format = header.fact_page_format;

        if fact_page_format == 0
            || fact_page_format == crate::storage::FACT_PAGE_FORMAT_ONE_PER_PAGE
        {
            let facts = self.read_one_per_page_legacy(&header)?;
            return self.publish_legacy_facts_as_v11(&header, facts);
        }

        // Packed fact-page format (v6+).
        let num_fact_pages =
            if header.version >= 10 || (header.version >= 6 && header.fact_page_count > 0) {
                header.fact_page_count
            } else {
                let first_index_page = [
                    header.eavt_root_page,
                    header.aevt_root_page,
                    header.avet_root_page,
                    header.vaet_root_page,
                ]
                .iter()
                .filter(|&&p| p > 0)
                .copied()
                .min()
                .unwrap_or(header.page_count);
                first_index_page.saturating_sub(1)
            };
        self.committed_fact_pages
            .store(num_fact_pages, Ordering::SeqCst);

        // v6-v9 packed images predate the page-local catalog but do carry an
        // aggregate checksum. Validate that legacy authority before any
        // migration write; otherwise a decodable bit flip could be rebuilt and
        // blessed as a fresh v11 base. Historical files may checksum all base
        // pages or fact pages only, so accept either exact legacy rule. The
        // decoded facts are then appended as a COW candidate; no page selected
        // by the legacy header is overwritten before page 0 publishes v11.
        if (6..10).contains(&header.version) {
            let has_index_root = [
                header.eavt_root_page,
                header.aevt_root_page,
                header.avet_root_page,
                header.vaet_root_page,
            ]
            .into_iter()
            .any(|page_id| page_id > 0);
            if num_fact_pages == 0 && (header.node_count > 0 || has_index_root) {
                anyhow::bail!(
                    "Non-empty legacy metadata declares no fact pages; refusing data-loss migration"
                );
            }

            if num_fact_pages > 0 {
                BasePageIntegrityCatalog::encoded_len_for_page_count(num_fact_pages)?;
                let backend = self
                    .backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
                let fact_checksum =
                    compute_page_checksum(&*backend, base_fact_page_start, num_fact_pages)?;
                if fact_checksum != header.index_checksum {
                    let full_page_count = header.page_count.saturating_sub(base_fact_page_start);
                    BasePageIntegrityCatalog::encoded_len_for_page_count(full_page_count)?;
                    let full_checksum =
                        compute_page_checksum(&*backend, base_fact_page_start, full_page_count)?;
                    if full_checksum != header.index_checksum {
                        anyhow::bail!(
                            "Legacy base checksum mismatch; refusing to migrate corrupt v{} data",
                            header.version
                        );
                    }
                }
            }

            let facts = if num_fact_pages > 0 {
                let backend = self
                    .backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
                crate::storage::packed_pages::read_all_from_pages(
                    &*backend,
                    base_fact_page_start,
                    num_fact_pages,
                )?
            } else {
                Vec::new()
            };
            return self.publish_legacy_facts_as_v11(&header, facts);
        }

        // Compute page-based checksum to verify data integrity.
        // New files (post-fix): checksum covers ALL pages (facts + indexes).
        // Old files (pre-fix): checksum covers only fact pages.
        // Try full checksum first; fall back to fact-only for backwards compat.
        let needs_format_upgrade =
            header.version < crate::storage::INTEGRITY_FORMAT_VERSION && !self.write_blocked_legacy;
        let needs_rebuild = if selected_delta_has_segments {
            if needs_format_upgrade {
                anyhow::bail!("Delta manifest requires the current file format");
            }
            if header.eavt_root_page == 0 {
                anyhow::bail!("Delta manifest requires base index roots");
            }
            let Some(manifest) = selected_delta_manifest.as_ref() else {
                anyhow::bail!("Delta manifest selection is missing selected manifest");
            };
            manifest.base_identity().validate_against_header(&header)?;
            false
        } else if needs_format_upgrade && num_fact_pages > 0 {
            true
        } else if num_fact_pages == 0 || header.eavt_root_page == 0 {
            num_fact_pages > 0 // rebuild if facts exist but no index root
        } else if header.version >= crate::storage::INTEGRITY_FORMAT_VERSION {
            false // v11+ base pages are verified lazily through the catalog.
        } else {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            let stored = header.index_checksum;
            // Total data pages: pages 1 through page_count-1 (everything except header)
            let total_data_pages = header.page_count.saturating_sub(base_fact_page_start);
            let full_checksum =
                compute_page_checksum(&*backend, base_fact_page_start, total_data_pages)?;
            if full_checksum == stored {
                false // new-style checksum matches: facts + indexes verified
            } else {
                // Fall back: old files stored checksum over fact pages only
                let fact_checksum =
                    compute_page_checksum(&*backend, base_fact_page_start, num_fact_pages)?;
                if fact_checksum == stored {
                    false // old-style checksum matches: facts verified, indexes unprotected
                } else {
                    true // neither matches: corruption detected, rebuild
                }
            }
        };

        // Register CommittedFactReader on FactStorage (before WAL replay).
        // If a delta manifest is visible, this base reader is replaced below
        // with a layered reader after the segment payloads are verified.
        self.storage.set_committed_reader(self.base_fact_loader());

        // Restore tx_counter from header
        self.storage
            .restore_tx_counter_from(header.last_checkpointed_tx_count);

        if needs_rebuild {
            // Rebuild indexes by re-reading all packed facts. This covers checksum
            // mismatches and older on-disk format semantics.
            let mut all_facts = {
                let backend = self
                    .backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
                crate::storage::packed_pages::read_all_from_pages(
                    &*backend,
                    base_fact_page_start,
                    num_fact_pages,
                )?
            };
            if header.version < 9 {
                normalize_legacy_retractions(&mut all_facts);
            }

            // Re-pack to derive correct FactRefs, and rewrite fact pages when
            // upgrading semantics so the v9 sentinel is durable on disk.
            let (fact_pages, real_refs) = pack_facts(&all_facts, 1)?;
            let num_fact_pages = u64::try_from(fact_pages.len())
                .map_err(|_| anyhow::anyhow!("fact page count exceeds u64::MAX"))?;

            // Build sorted index entries
            let (eavt_entries, aevt_entries, avet_entries, vaet_entries) =
                build_sorted_index_entries(&all_facts, &real_refs);

            // Fix up tx_counter from actual facts
            let max_tx = all_facts.iter().map(|f| f.tx_count).max().unwrap_or(0);
            self.storage.restore_tx_counter_from(max_tx);

            // Build v6 B+tree indexes directly
            let index_start = 1u64
                .checked_add(num_fact_pages)
                .ok_or_else(|| anyhow::anyhow!("page count overflow computing index_start"))?;
            let mut backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            self.page_cache.invalidate_from(1);
            for (i, page_data) in fact_pages.iter().enumerate() {
                let page_offset = u64::try_from(i)
                    .map_err(|_| anyhow::anyhow!("page index {i} exceeds u64::MAX"))?;
                let page_id = 1u64
                    .checked_add(page_offset)
                    .ok_or_else(|| anyhow::anyhow!("page id overflow writing fact pages"))?;
                backend.write_page(page_id, page_data)?;
            }
            let (eavt_root, next1) = self.build_btree(
                btree_entries(eavt_entries.into_iter())?.into_iter(),
                &mut *backend,
                index_start,
            )?;
            let (aevt_root, next2) = self.build_btree(
                btree_entries(aevt_entries.into_iter())?.into_iter(),
                &mut *backend,
                next1,
            )?;
            let (avet_root, next3) = self.build_btree(
                btree_entries(avet_entries.into_iter())?.into_iter(),
                &mut *backend,
                next2,
            )?;
            let (vaet_root, next4) = self.build_btree(
                btree_entries(vaet_entries.into_iter())?.into_iter(),
                &mut *backend,
                next3,
            )?;

            // Publish a v11 base only after fact/index pages and their catalog
            // are durable.
            let total_data_pages = next4.saturating_sub(1);
            backend.sync()?;
            let integrity =
                write_base_integrity_catalog(&mut *backend, 1, 1, total_data_pages, next4, None)?;

            let mut new_header = FileHeader::new();
            new_header.page_count = integrity.published_page_count;
            new_header.node_count = all_facts.len() as u64;
            new_header.last_checkpointed_tx_count = max_tx;
            new_header.eavt_root_page = eavt_root;
            new_header.aevt_root_page = aevt_root;
            new_header.avet_root_page = avet_root;
            new_header.vaet_root_page = vaet_root;
            new_header.index_checksum = integrity.aggregate_checksum;
            new_header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
            new_header.fact_page_count = num_fact_pages;

            let write_checksum = compute_header_checksum(&new_header);
            new_header.header_checksum = write_checksum;

            let header_page =
                build_header_page_with_base_integrity(new_header, 1, integrity.descriptor)?;
            let manifest_selection =
                select_header_manifest_slot_from_page0(new_header.version, &header_page)?;
            backend.write_page(0, &header_page)?;
            backend.sync()?;
            drop(backend);

            self.header_manifest_selection = manifest_selection;
            self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
            self.last_checkpointed_tx_count = max_tx;
            self.committed_fact_pages
                .store(num_fact_pages, Ordering::SeqCst);
            self.committed_fact_page_start.store(1, Ordering::SeqCst);
            self.base_integrity = Some(integrity.catalog);

            self.wire_committed_readers(&new_header, Vec::new())?;
        } else {
            // No rebuild needed - validate header checksum for v7+ files
            // Re-read header from disk to get any updates from rebuild path
            if header.version >= 7 && header.header_checksum != 0 {
                let backend = self
                    .backend
                    .lock()
                    .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
                let current_header_bytes = backend.read_page(0)?;
                let current_header = FileHeader::from_bytes(&current_header_bytes)?;
                let computed = compute_header_checksum_from_bytes(&current_header_bytes);
                if current_header.header_checksum != computed {
                    anyhow::bail!(
                        "Header checksum mismatch: possible file corruption. Database may be damaged."
                    );
                }
            }

            if header.eavt_root_page != 0 {
                self.wire_committed_readers(&header, selected_delta_segments)?;
            }
        }
        // else: empty DB — indexes are empty by default, nothing to do.

        self.dirty = false;
        Ok(())
    }

    /// Read only the fact range from a legacy one-per-page image.
    ///
    /// v4 index-blob pages follow the facts and are deliberately ignored: the
    /// fact log plus its CRC is the migration authority and v11 rebuilds all
    /// indexes from it.
    fn read_one_per_page_legacy(&self, header: &FileHeader) -> Result<Vec<Fact>> {
        let fact_page_end = legacy_one_per_page_fact_end(header)?;
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        let capacity = usize::try_from(header.node_count)
            .map_err(|_| anyhow::anyhow!("Legacy fact count exceeds memory limits"))?;
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(capacity)
            .map_err(|_| anyhow::anyhow!("Legacy fact allocation exceeds memory limits"))?;
        for page_id in 1..fact_page_end {
            let page = backend.read_page(page_id)?;
            let fact = postcard::from_bytes::<Fact>(&page).map_err(|error| {
                anyhow::anyhow!("Failed to deserialize legacy fact at page {page_id}: {error}")
            })?;
            facts.push(fact);
        }
        if header.version == 4 && compute_index_checksum(&facts)? != header.index_checksum {
            anyhow::bail!("Legacy v4 fact checksum mismatch; refusing corrupt migration");
        }
        Ok(facts)
    }

    /// Migrate a v1 file (Phase 3 format, no bi-temporal fields) to v2.
    ///
    /// V1 facts only have (entity, attribute, value, tx_id, asserted).
    /// V2 facts add tx_count, valid_from, valid_to.
    ///
    /// Migration strategy:
    /// - Sort v1 facts by tx_id ascending
    /// - Group facts with the same tx_id into the same tx_count (monotonic counter)
    /// - Set valid_from = tx_id as i64 (wall-clock approximation)
    /// - Set valid_to = VALID_TIME_FOREVER (open-ended)
    /// - Write the migrated data back in v2 format
    fn migrate_v1_to_v2(&mut self) -> Result<()> {
        use crate::graph::types::VALID_TIME_FOREVER;

        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        let header_page = backend.read_page(0)?;
        let header = FileHeader::from_bytes(&header_page)?;
        let fact_page_end = legacy_one_per_page_fact_end(&header)?;

        // Read all v1 facts. Any missing or undecodable page is corruption;
        // migration must not publish a partially reconstructed database.
        let mut v1_facts: Vec<FactV1> = Vec::new();
        for page_id in 1..fact_page_end {
            let page = backend.read_page(page_id)?;
            let fact = postcard::from_bytes::<FactV1>(&page).map_err(|error| {
                anyhow::anyhow!("Failed to deserialize v1 fact at page {page_id}: {error}")
            })?;
            v1_facts.push(fact);
        }
        drop(backend);

        // Sort by tx_id ascending so we can group them
        v1_facts.sort_by_key(|f| f.tx_id);

        // Assign tx_count, grouping facts with the same tx_id into the same tx_count
        let mut tx_count: u64 = 0;
        let mut prev_tx_id: Option<crate::graph::types::TxId> = None;
        let mut migrated: Vec<Fact> = Vec::new();

        for v1 in v1_facts {
            if prev_tx_id != Some(v1.tx_id) {
                tx_count = tx_count.saturating_add(1);
                prev_tx_id = Some(v1.tx_id);
            }
            let valid_from = v1.tx_id.cast_signed();
            let mut fact = Fact::with_valid_time(
                v1.entity,
                v1.attribute,
                v1.value,
                v1.tx_id,
                tx_count,
                valid_from,
                VALID_TIME_FOREVER,
            );
            // Preserve the asserted flag (with_valid_time sets asserted=true by default)
            fact.asserted = v1.asserted;
            if !fact.asserted {
                fact.valid_from = RETRACT_ALL_VALID_FROM;
                fact.valid_to = VALID_TIME_FOREVER;
            }
            migrated.push(fact);
        }

        self.publish_legacy_facts_as_v11(&header, migrated)
    }

    /// Migrate a v5 packed base and paged-blob indexes to a COW v11 base.
    fn migrate_v5_to_v6(&mut self, header: &FileHeader) -> Result<()> {
        let roots = [
            header.eavt_root_page,
            header.aevt_root_page,
            header.avet_root_page,
            header.vaet_root_page,
        ];
        let has_index_root = roots.iter().any(|page_id| *page_id > 0);
        let first_index_page = roots
            .iter()
            .filter(|&&page_id| page_id > 0)
            .copied()
            .min()
            .unwrap_or(header.page_count);
        let num_fact_pages = first_index_page.saturating_sub(1);

        if num_fact_pages == 0 && (header.node_count > 0 || has_index_root) {
            anyhow::bail!(
                "Non-empty v5 metadata declares no fact pages; refusing data-loss migration"
            );
        }

        // Validate the calculated range and checksum before writing an
        // append-only candidate. A missing first page is corruption, not an
        // empty database.
        if num_fact_pages > 0 {
            BasePageIntegrityCatalog::encoded_len_for_page_count(num_fact_pages)?;
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            backend
                .read_page(1)
                .map_err(|error| anyhow::anyhow!("Cannot read first v5 fact page: {error}"))?;
            let fact_checksum = compute_page_checksum(&*backend, 1, num_fact_pages)?;
            if fact_checksum != header.index_checksum {
                anyhow::bail!("Legacy base checksum mismatch; refusing to migrate corrupt v5 data");
            }
        }

        let facts = if num_fact_pages > 0 {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            crate::storage::packed_pages::read_all_from_pages(&*backend, 1, num_fact_pages)?
        } else {
            Vec::new()
        };
        self.publish_legacy_facts_as_v11(header, facts)
    }

    /// Consume this storage and return the underlying backend.
    ///
    /// Useful in tests to inspect or reuse the backend after saving.
    /// Any dirty (unsaved) changes are saved before the backend is returned.
    ///
    /// Returns an error if the backend Arc has multiple references.
    #[allow(dead_code)]
    pub fn into_backend(mut self) -> Result<B> {
        // Save pending changes before giving up ownership
        if self.dirty {
            let _ = self.save();
        }
        let backend_arc = self.backend.clone();
        // Suppress the Drop impl so we don't double-save.
        self.dirty = false;
        drop(self);
        match Arc::try_unwrap(backend_arc) {
            Ok(mutex) => Ok(mutex
                .into_inner()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?),
            Err(_) => Err(anyhow::anyhow!(
                "into_backend: backend Arc has multiple owners"
            )),
        }
    }

    fn try_save_delta_segment(
        &mut self,
        pending_facts: &[Fact],
    ) -> Result<Option<CheckpointOutcome>> {
        if pending_facts.is_empty() {
            return Ok(None);
        }
        let selected_delta_manifest = match &self.delta_manifest_selection {
            PersistedManifestSelection::NoDeltaManifest => None,
            PersistedManifestSelection::Use { manifest, .. } => Some(manifest.clone()),
            PersistedManifestSelection::RecoveryRequired { .. } => return Ok(None),
        };

        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        let curr_header_page = match backend.read_page(0) {
            Ok(bytes) => bytes,
            Err(_) if backend.is_new() => return Ok(None),
            Err(e) => anyhow::bail!("Failed to read header from existing file: {}", e),
        };
        let curr_header = FileHeader::from_bytes(&curr_header_page)?;
        if curr_header.version < crate::storage::INTEGRITY_FORMAT_VERSION
            || curr_header.version > crate::storage::FORMAT_VERSION
            || curr_header.fact_page_format != FACT_PAGE_FORMAT_PACKED
            || curr_header.eavt_root_page == 0
            || curr_header.aevt_root_page == 0
            || curr_header.avet_root_page == 0
            || curr_header.vaet_root_page == 0
        {
            return Ok(None);
        }

        let (base_identity, mut manifest_segments) = if let Some(manifest) =
            selected_delta_manifest.as_ref()
        {
            if manifest.segments().len() != self.resident_delta_segment_count {
                anyhow::bail!("Resident delta segment count does not match the selected manifest");
            }
            (manifest.base_identity(), manifest.segments().to_vec())
        } else {
            if self.resident_delta_segment_count != 0 {
                anyhow::bail!("Resident delta segments exist without a selected manifest");
            }
            (DeltaBaseIdentity::from_header(&curr_header), Vec::new())
        };

        let segment_page_start = curr_header.page_count;
        let segment = DeltaSegment::from_facts(pending_facts.to_vec(), segment_page_start)?;
        self.page_cache.invalidate_from(segment_page_start);
        let segment_page_count = write_segment_pages(&mut *backend, segment_page_start, &segment)?;
        let manifest_segment = DeltaManifestSegment::from_segment_header(
            segment_page_start,
            segment_page_count,
            segment.header(),
        )?;
        let generation = self
            .storage
            .current_tx_count()
            .max(curr_header.last_checkpointed_tx_count.saturating_add(1))
            .max(1);
        manifest_segments.push(manifest_segment);
        let manifest = DeltaManifest::new(generation, base_identity, manifest_segments)?;
        let manifest_page_start = segment_page_start
            .checked_add(segment_page_count)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest page start overflow"))?;
        let descriptor = write_manifest_pages(&mut *backend, manifest_page_start, &manifest)?;
        let (extension, published_slot) = Self::build_next_manifest_extension(
            &curr_header,
            &curr_header_page,
            self.header_manifest_selection,
            descriptor,
        )?;
        let next_page_count = manifest_page_start
            .checked_add(descriptor.manifest_page_count())
            .ok_or_else(|| anyhow::anyhow!("Delta checkpoint page count overflow"))?;

        // New segment and manifest pages must be durable before page 0 publishes
        // the descriptor that makes them visible.
        backend.sync()?;

        let mut header = curr_header;
        header.page_count = next_page_count;
        let pending_len = u64::try_from(pending_facts.len())
            .map_err(|_| anyhow::anyhow!("pending fact count exceeds u64::MAX"))?;
        header.node_count = curr_header
            .node_count
            .checked_add(pending_len)
            .ok_or_else(|| anyhow::anyhow!("node_count overflow"))?;
        header.last_checkpointed_tx_count = self.storage.current_tx_count();
        header.index_checksum = curr_header.index_checksum;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page_with_extension(header, extension)?;
        backend.write_page(0, &header_page)?;
        backend.sync()?;
        drop(backend);

        self.header_manifest_selection = HeaderManifestSlotSelection::Use {
            slot: published_slot,
            descriptor,
        };
        self.delta_manifest_selection = PersistedManifestSelection::Use {
            slot: published_slot,
            manifest,
        };
        self.last_checkpointed_tx_count = self.storage.current_tx_count();
        self.dirty = false;
        if self.resident_delta_segment_count == 0 {
            self.wire_committed_readers(&header, vec![segment])?;
        } else {
            self.append_resident_delta_segment(&segment)?;
        }
        self.storage.post_checkpoint_clear();
        Ok(Some(CheckpointOutcome::DeltaSegment))
    }

    /// Save all facts from memory to the backend using packed pages and v6 on-disk B+tree indexes.
    pub fn save(&mut self) -> Result<CheckpointOutcome> {
        if !self.dirty {
            return Ok(CheckpointOutcome::Noop);
        }
        if self.write_blocked_legacy {
            anyhow::bail!(
                "Legacy graph recovery is read-only until a complete published image is repaired"
            );
        }

        // ── Step A: read current header + stream old B+tree entries BEFORE overwriting ──
        let pending_facts = self.storage.get_pending_facts();
        if let Some(outcome) = self.try_save_delta_segment(&pending_facts)? {
            return Ok(outcome);
        }
        if matches!(
            self.delta_manifest_selection,
            PersistedManifestSelection::Use { .. }
        ) {
            let candidate = self.write_recompact_candidate_from_visible_facts()?;
            return self.publish_recompact_candidate(candidate);
        }
        #[cfg(feature = "bench-internals")]
        reset_checkpoint_construction_diagnostics();

        let base_generation = self
            .base_integrity
            .as_ref()
            .map(|catalog| catalog.base_generation())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Base integrity generation overflow"))?;
        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;

        let old_fact_page_count = self.committed_fact_pages.load(Ordering::SeqCst);
        let base_fact_page_start = self.committed_fact_page_start.load(Ordering::SeqCst);
        let new_fact_start = base_fact_page_start
            .checked_add(old_fact_page_count)
            .ok_or_else(|| anyhow::anyhow!("page count overflow computing new_fact_start"))?;

        let curr_header = match backend.read_page(0) {
            Ok(bytes) => FileHeader::from_bytes(&bytes)?,
            Err(_) if backend.is_new() => FileHeader::new(),
            Err(e) => anyhow::bail!("Failed to read header from existing file: {}", e),
        };

        // A full save republishes the existing base under a new generation.
        // Verify the entire old generation before any page can be overwritten,
        // otherwise latent corruption could be checksummed and blessed as new.
        if let Some(base_integrity) = &self.base_integrity {
            verify_base_integrity_pages(&*backend, base_integrity)?;
        }

        // Stream committed B+tree entries BEFORE writing new pages that may overlap
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let committed_eavt: Vec<(EavtKey, FactRef)> = if curr_header.eavt_root_page != 0 {
            stream_all_entries(curr_header.eavt_root_page, &*backend, &self.page_cache)?
        } else {
            Vec::new()
        };
        let committed_aevt: Vec<(AevtKey, FactRef)> = if curr_header.aevt_root_page != 0 {
            stream_all_entries(curr_header.aevt_root_page, &*backend, &self.page_cache)?
        } else {
            Vec::new()
        };
        let committed_avet: Vec<(AvetKey, FactRef)> = if curr_header.avet_root_page != 0 {
            stream_all_entries(curr_header.avet_root_page, &*backend, &self.page_cache)?
        } else {
            Vec::new()
        };
        let committed_vaet: Vec<(VaetKey, FactRef)> = if curr_header.vaet_root_page != 0 {
            stream_all_entries(curr_header.vaet_root_page, &*backend, &self.page_cache)?
        } else {
            Vec::new()
        };
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.committed_index_read_micros = checkpoint_elapsed_micros(phase_started);
        });

        // Invalidate cached pages that will be overwritten (old index pages)
        self.page_cache.invalidate_from(new_fact_start);

        // ── Step B: pack pending facts as new appended pages ────────────────────
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (new_pages, new_fact_refs) = pack_facts(&pending_facts, new_fact_start)?;
        for (i, page_data) in new_pages.iter().enumerate() {
            let page_offset =
                u64::try_from(i).map_err(|_| anyhow::anyhow!("page index {i} exceeds u64::MAX"))?;
            let page_id = new_fact_start
                .checked_add(page_offset)
                .ok_or_else(|| anyhow::anyhow!("page id overflow writing fact pages"))?;
            backend.write_page(page_id, page_data)?;
        }
        let new_pages_len = u64::try_from(new_pages.len())
            .map_err(|_| anyhow::anyhow!("new page count exceeds u64::MAX"))?;
        let new_total_fact_pages = old_fact_page_count
            .checked_add(new_pages_len)
            .ok_or_else(|| anyhow::anyhow!("fact page count overflow"))?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.fact_packing_micros = checkpoint_elapsed_micros(phase_started);
            diagnostics.peak_fact_pages_in_memory = new_pages_len;
        });

        // Sync fact pages to disk before building indexes on top of them.
        // Without this, a crash during index build could leave partially-flushed
        // fact pages that the old header's index roots would try to traverse.
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.sync()?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.fact_sync_micros = checkpoint_elapsed_micros(phase_started);
        });

        // ── Step C: prepare one reusable pending fact-reference sort buffer ─────
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut pending_order = PendingIndexOrder::new(&pending_facts, &new_fact_refs);
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.pending_index_sort_micros = checkpoint_elapsed_micros(phase_started);
            diagnostics.peak_typed_entries = u64::from(!pending_facts.is_empty());
            diagnostics.peak_sort_reference_entries =
                u64::try_from(pending_facts.len()).unwrap_or(u64::MAX);
            diagnostics.peak_sort_reference_bytes = u64::try_from(
                pending_facts
                    .len()
                    .saturating_mul(std::mem::size_of::<usize>()),
            )
            .unwrap_or(u64::MAX);
            diagnostics.cached_value_bytes =
                u64::try_from(pending_order.cached_value_bytes()).unwrap_or(u64::MAX);
        });

        // ── Step D: merge committed + pending entries, build new B+trees ─────────
        let index_start = base_fact_page_start
            .checked_add(new_total_fact_pages)
            .ok_or_else(|| anyhow::anyhow!("page count overflow computing index_start"))?;

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        pending_order.sort_eavt();
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.eavt_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (eavt_root, next1) = if committed_eavt.is_empty() {
            self.build_btree_keys(
                pending_order.borrowed_eavt_entries(),
                &mut *backend,
                index_start,
            )?
        } else {
            let entries =
                merge_sorted_iters(committed_eavt.into_iter(), pending_order.eavt_entries());
            self.build_btree_keys(entries, &mut *backend, index_start)?
        };
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.eavt_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        pending_order.sort_aevt();
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.aevt_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (aevt_root, next2) = if committed_aevt.is_empty() {
            self.build_btree_keys(pending_order.borrowed_aevt_entries(), &mut *backend, next1)?
        } else {
            let entries =
                merge_sorted_iters(committed_aevt.into_iter(), pending_order.aevt_entries());
            self.build_btree_keys(entries, &mut *backend, next1)?
        };
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.aevt_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        pending_order.sort_avet();
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.avet_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (avet_root, next3) = if committed_avet.is_empty() {
            self.build_btree_keys(pending_order.borrowed_avet_entries(), &mut *backend, next2)?
        } else {
            let entries =
                merge_sorted_iters(committed_avet.into_iter(), pending_order.avet_entries());
            self.build_btree_keys(entries, &mut *backend, next2)?
        };
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.avet_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        pending_order.sort_vaet();
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.vaet_collect_sort_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let (vaet_root, next4) = if committed_vaet.is_empty() {
            self.build_btree_keys(pending_order.borrowed_vaet_entries(), &mut *backend, next3)?
        } else {
            let entries =
                merge_sorted_iters(committed_vaet.into_iter(), pending_order.vaet_entries());
            self.build_btree_keys(entries, &mut *backend, next3)?
        };
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.vaet_build_micros = checkpoint_elapsed_micros(phase_started);
        });

        // Sync index pages to disk before writing the header.
        // The header update is the atomic commit point: once it's durable,
        // recovery uses the new root pages. All data those roots reference
        // must already be on stable storage.
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.sync()?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.data_sync_micros = checkpoint_elapsed_micros(phase_started);
        });

        let total_data_pages = next4.saturating_sub(base_fact_page_start);
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let integrity = write_base_integrity_catalog(
            &mut *backend,
            base_generation,
            base_fact_page_start,
            total_data_pages,
            next4,
            None,
        )?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.integrity_catalog_micros = checkpoint_elapsed_micros(phase_started);
        });

        // ── Step E: write header (last write = crash-safe boundary) ─────────────
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        let mut header = FileHeader::new(); // current format
        header.page_count = integrity.published_page_count;
        let pending_len = u64::try_from(pending_facts.len())
            .map_err(|_| anyhow::anyhow!("pending fact count exceeds u64::MAX"))?;
        header.node_count = curr_header
            .node_count
            .checked_add(pending_len)
            .ok_or_else(|| anyhow::anyhow!("node_count overflow"))?;
        header.last_checkpointed_tx_count = self.storage.current_tx_count();
        header.eavt_root_page = eavt_root;
        header.aevt_root_page = aevt_root;
        header.avet_root_page = avet_root;
        header.vaet_root_page = vaet_root;
        header.index_checksum = integrity.aggregate_checksum;
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        header.fact_page_count = new_total_fact_pages;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page_with_base_integrity(
            header,
            base_fact_page_start,
            integrity.descriptor,
        )?;
        let manifest_selection =
            select_header_manifest_slot_from_page0(header.version, &header_page)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.header_assembly_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.write_page(0, &header_page)?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.publish_write_micros = checkpoint_elapsed_micros(phase_started);
        });
        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        backend.sync()?;
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.publish_sync_micros = checkpoint_elapsed_micros(phase_started);
        });
        drop(backend);

        #[cfg(feature = "bench-internals")]
        let phase_started = Instant::now();
        self.header_manifest_selection = manifest_selection;
        self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
        self.committed_fact_pages
            .store(new_total_fact_pages, Ordering::SeqCst);
        self.committed_fact_page_start
            .store(base_fact_page_start, Ordering::SeqCst);
        self.base_integrity = Some(integrity.catalog);
        self.last_checkpointed_tx_count = self.storage.current_tx_count();
        self.dirty = false;

        // ── Step F: wire verified committed readers ──────────────────────────────
        self.wire_committed_readers(&header, Vec::new())?;

        // Clear pending — all data now on disk
        self.storage.post_checkpoint_clear();
        #[cfg(feature = "bench-internals")]
        update_checkpoint_diagnostics(|diagnostics| {
            diagnostics.publish_finalize_micros = checkpoint_elapsed_micros(phase_started);
        });

        Ok(CheckpointOutcome::FullRebuild)
    }

    /// Get a reference to the underlying fact storage
    pub fn storage(&self) -> &FactStorage {
        &self.storage
    }

    /// The `last_checkpointed_tx_count` recorded in the on-disk header.
    ///
    /// Used by WAL replay to skip entries already present in the main file.
    pub fn last_checkpointed_tx_count(&self) -> u64 {
        self.last_checkpointed_tx_count
    }

    #[cfg(test)]
    pub(crate) fn header_manifest_selection(&self) -> HeaderManifestSlotSelection {
        self.header_manifest_selection
    }

    #[cfg(test)]
    pub(crate) fn delta_manifest_selection(&self) -> &PersistedManifestSelection {
        &self.delta_manifest_selection
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn delta_maintenance_decision(&self) -> DeltaMaintenanceDecision {
        if self.loaded_format_version < crate::storage::FORMAT_VERSION {
            return DeltaMaintenanceDecision::ScheduleBackgroundRecompact;
        }
        match &self.delta_manifest_selection {
            PersistedManifestSelection::Use { manifest, .. } => {
                DeltaGrowthMetrics::from_manifest(manifest).decide()
            }
            PersistedManifestSelection::NoDeltaManifest
            | PersistedManifestSelection::RecoveryRequired { .. } => {
                DeltaMaintenanceDecision::ContinueDeltaAppend
            }
        }
    }

    /// (visible delta segment count, visible delta page growth) for the A6
    /// session `status` op. Zero for both when no delta manifest is selected.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn delta_growth_snapshot(&self) -> (u64, u64) {
        match &self.delta_manifest_selection {
            PersistedManifestSelection::Use { manifest, .. } => {
                let metrics = DeltaGrowthMetrics::from_manifest(manifest);
                (
                    metrics.visible_delta_segment_count(),
                    metrics.visible_delta_page_growth(),
                )
            }
            PersistedManifestSelection::NoDeltaManifest
            | PersistedManifestSelection::RecoveryRequired { .. } => (0, 0),
        }
    }

    /// Fold the selected visible delta into a fresh base through the existing
    /// full-rebuild path.
    ///
    /// This is intentionally private and not called from `checkpoint()`. The
    /// public scheduling boundary is [`crate::Minigraf::run_idle_maintenance`],
    /// which checkpoints pending writes before invoking this path.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn recompact_visible_delta(&mut self) -> Result<CheckpointOutcome> {
        if !self.storage.get_pending_facts().is_empty() {
            anyhow::bail!("Cannot recompact visible delta while pending facts are uncheckpointed");
        }

        match &self.delta_manifest_selection {
            PersistedManifestSelection::NoDeltaManifest
                if self.loaded_format_version < crate::storage::FORMAT_VERSION =>
            {
                let candidate = self.write_recompact_candidate_from_visible_facts()?;
                self.publish_recompact_candidate(candidate)
            }
            PersistedManifestSelection::NoDeltaManifest => Ok(CheckpointOutcome::Noop),
            PersistedManifestSelection::RecoveryRequired { reason } => {
                anyhow::bail!("Cannot recompact delta manifest requiring recovery: {reason:?}");
            }
            PersistedManifestSelection::Use { .. } => {
                let candidate = self.write_recompact_candidate_from_visible_facts()?;
                self.publish_recompact_candidate(candidate)
            }
        }
    }

    /// Run scheduled delta maintenance from an idle/background caller.
    ///
    /// This is intentionally not called from `save()`/foreground checkpoint.
    /// The caller must first make pending writes durable; this method preserves
    /// the same pending-facts guard as explicit recompact.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn run_idle_delta_maintenance(&mut self) -> Result<CheckpointOutcome> {
        match self.delta_maintenance_decision() {
            DeltaMaintenanceDecision::ContinueDeltaAppend => Ok(CheckpointOutcome::Noop),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
            | DeltaMaintenanceDecision::MaintenanceBackpressure => self.recompact_visible_delta(),
        }
    }

    /// Mark storage as dirty (needs saving)
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Force the dirty flag to true regardless of current state.
    ///
    /// Used by checkpoint to ensure save() always writes even if no new
    /// facts have been added since the last save.
    pub fn force_dirty(&mut self) {
        self.mark_dirty();
    }

    /// Check if storage has unsaved changes
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Suppress the fallback auto-save in [`Drop`] when the owning database
    /// explicitly keeps WAL entries pending (the `usize::MAX` benchmark
    /// sentinel). The WAL remains the durable authority and will replay on the
    /// next open.
    pub(crate) fn suppress_drop_save(&mut self) {
        self.dirty = false;
    }

    /// Inspect the logical packed-fact range selected by the loaded v11 base.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn browser_base_fact_range(&self) -> Result<BrowserPageRange> {
        let published_page_count = self.browser_published_page_count()?;
        BrowserPageRange::bounded(
            "Loaded browser base fact",
            self.committed_fact_page_start.load(Ordering::SeqCst),
            self.committed_fact_pages.load(Ordering::SeqCst),
            published_page_count,
        )
    }

    /// Inspect page 0's declared, published page count without equating it to
    /// the number of pages currently resident in a sparse backend.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn browser_published_page_count(&self) -> Result<u64> {
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned during browser inspection"))?;
        let page0 = backend.read_page(0)?;
        let header = FileHeader::from_bytes(&page0)?;
        header.validate()?;
        Ok(header.page_count)
    }

    /// Return the exact manifest lineage selected by the normal persistent
    /// loader. Browser sparse planning must retain this same slot/generation;
    /// a planner that only supports an older candidate must fail instead of
    /// evicting pages owned by the live selected lineage.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn browser_selected_manifest_identity(
        &self,
    ) -> Result<Option<(HeaderManifestSlotName, u64)>> {
        match (
            self.header_manifest_selection,
            &self.delta_manifest_selection,
        ) {
            (
                HeaderManifestSlotSelection::NoDeltaManifest,
                PersistedManifestSelection::NoDeltaManifest,
            ) => Ok(None),
            (
                HeaderManifestSlotSelection::Use { slot, descriptor },
                PersistedManifestSelection::Use {
                    slot: persisted_slot,
                    manifest,
                },
            ) if slot == *persisted_slot && descriptor.generation() == manifest.generation() => {
                Ok(Some((slot, descriptor.generation())))
            }
            _ => anyhow::bail!(
                "Persistent manifest selection is not a single consistent browser authority"
            ),
        }
    }

    /// Start a streaming verification of an asynchronously re-read browser
    /// image. The selected metadata ranges are derived from the same manifest
    /// selection that wired this live PFS, never from a newly chosen fallback.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn begin_browser_export_verification(
        &self,
    ) -> Result<BrowserPublishedExportVerifier> {
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned during browser export setup"))?;
        let page0 = backend.read_page(0)?;
        let header = FileHeader::from_bytes(&page0)?;
        header.validate()?;
        if header.version < crate::storage::INTEGRITY_FORMAT_VERSION
            || header.version > crate::storage::FORMAT_VERSION
        {
            anyhow::bail!("Paged browser export requires a paged-ready file format");
        }
        let extension = HeaderExtension::read_from_page0(header.version, &page0)?
            .ok_or_else(|| anyhow::anyhow!("Paged browser export requires header metadata"))?;
        let integrity = validate_v11_base_integrity_descriptor(&header, &extension)?;

        let mut exact_resident_ranges = Vec::new();
        if let Some(descriptor) = integrity {
            exact_resident_ranges.push(BrowserPageRange::bounded(
                "Base integrity catalog",
                descriptor.catalog_page_start(),
                descriptor.catalog_page_count(),
                header.page_count,
            )?);
        }

        match (
            self.header_manifest_selection,
            &self.delta_manifest_selection,
        ) {
            (
                HeaderManifestSlotSelection::NoDeltaManifest,
                PersistedManifestSelection::NoDeltaManifest,
            ) => {}
            (
                HeaderManifestSlotSelection::Use { slot, descriptor },
                PersistedManifestSelection::Use {
                    slot: persisted_slot,
                    manifest,
                },
            ) if slot == *persisted_slot && descriptor.generation() == manifest.generation() => {
                exact_resident_ranges.push(BrowserPageRange::bounded(
                    "Selected delta manifest",
                    descriptor.manifest_page_start(),
                    descriptor.manifest_page_count(),
                    header.page_count,
                )?);
                exact_resident_ranges
                    .try_reserve(manifest.segments().len())
                    .map_err(|_| anyhow::anyhow!("Browser export segment plan exceeds memory"))?;
                for segment in manifest.segments() {
                    exact_resident_ranges.push(BrowserPageRange::bounded(
                        "Selected delta segment",
                        segment.segment_page_start(),
                        segment.segment_page_count(),
                        header.page_count,
                    )?);
                }
            }
            _ => anyhow::bail!(
                "Persistent manifest selection is not a single consistent browser export authority"
            ),
        }

        exact_resident_ranges.sort_unstable();
        exact_resident_ranges.dedup();
        for range in &exact_resident_ranges {
            for page_id in range.start_page()..range.end_page() {
                backend.read_page(page_id).map_err(|error| {
                    anyhow::anyhow!(
                        "Validated browser export authority page {page_id} is not resident: {error}"
                    )
                })?;
            }
        }

        Ok(BrowserPublishedExportVerifier {
            published_page_count: header.page_count,
            next_page_id: 0,
            exact_resident_ranges,
            next_exact_range_index: 0,
        })
    }

    /// Verify one ascending batch freshly read from IndexedDB.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn verify_browser_export_batch(
        &self,
        verifier: &mut BrowserPublishedExportVerifier,
        pages: &[(u64, Vec<u8>)],
    ) -> Result<()> {
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned during browser export"))?;
        let resident_page0 = backend.read_page(0)?;
        for (page_id, page) in pages {
            if *page_id != verifier.next_page_id {
                anyhow::bail!(
                    "Browser export page order mismatch: expected {}, found {}",
                    verifier.next_page_id,
                    page_id
                );
            }
            verify_browser_published_page_bytes(
                &resident_page0,
                self.base_integrity.as_deref(),
                *page_id,
                page,
            )?;
            while verifier
                .exact_resident_ranges
                .get(verifier.next_exact_range_index)
                .is_some_and(|range| range.end_page() <= *page_id)
            {
                verifier.next_exact_range_index = verifier
                    .next_exact_range_index
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("Browser export range cursor overflow"))?;
            }
            let requires_exact_match = verifier
                .exact_resident_ranges
                .get(verifier.next_exact_range_index)
                .is_some_and(|range| range.contains(*page_id));
            if requires_exact_match {
                let expected = backend.read_page(*page_id)?;
                if expected != *page {
                    anyhow::bail!(
                        "Browser export authority page {page_id} changed after it was validated"
                    );
                }
            }
            verifier.next_page_id = verifier
                .next_page_id
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Browser export page id overflow"))?;
        }
        Ok(())
    }

    /// Verify bytes returned by an external async browser read before staging
    /// them in the synchronous storage backend.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn verify_browser_fetched_page(&self, page_id: u64, page: &[u8]) -> Result<()> {
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned during browser page verify"))?;
        let page0 = backend.read_page(0)?;
        verify_browser_published_page_bytes(&page0, self.base_integrity.as_deref(), page_id, page)
    }

    /// Run a closure with read access to the underlying storage backend.
    ///
    /// Used by the browser WASM layer to read pages after `save()` without
    /// exposing the `Arc<Mutex<B>>` directly.
    #[cfg(all(target_arch = "wasm32", feature = "browser"))]
    #[allow(clippy::unwrap_used)] // poison may contain a partial mutation; fail-stop instead of recovering it
    pub(crate) fn with_backend<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&B) -> R,
    {
        let guard = self.backend.lock().unwrap();
        f(&*guard)
    }

    /// Read one published browser page, verifying immutable base pages against
    /// the selected generation catalog in the same backend read.
    #[cfg(all(target_arch = "wasm32", feature = "browser"))]
    pub(crate) fn read_published_page(&self, page_id: u64) -> Result<Vec<u8>> {
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned during browser export"))?;
        let page0 = backend.read_page(0)?;
        let page = backend.read_page(page_id)?;
        verify_browser_published_page_bytes(
            &page0,
            self.base_integrity.as_deref(),
            page_id,
            &page,
        )?;
        Ok(page)
    }

    /// Run a closure with mutable access to the underlying storage backend.
    ///
    /// Used by the browser WASM layer to drain dirty pages after `save()`.
    #[cfg(all(target_arch = "wasm32", feature = "browser"))]
    #[allow(clippy::unwrap_used)] // poison may contain a partial mutation; fail-stop instead of recovering it
    pub(crate) fn with_backend_mut<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut B) -> R,
    {
        let mut guard = self.backend.lock().unwrap();
        f(&mut *guard)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PersistentFactStorage<FileBackend> {
    /// Copy the exact page-0-published image through the already-open backend.
    pub(crate) fn copy_published_image_to(&mut self, destination: &mut File) -> Result<u64> {
        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned during backup"))?;
        if let Some(base_integrity) = &self.base_integrity {
            verify_base_integrity_pages(&*backend, base_integrity)?;
        }
        backend.copy_published_image_to(destination)
    }
}

impl<B: StorageBackend + 'static> Drop for PersistentFactStorage<B> {
    fn drop(&mut self) {
        // Auto-save on drop
        if self.dirty {
            let _ = self.save();
        }
    }
}

fn write_base_integrity_catalog(
    backend: &mut dyn StorageBackend,
    base_generation: u64,
    covered_page_start: u64,
    covered_page_count: u64,
    catalog_page_start: u64,
    expected_aggregate_checksum: Option<u32>,
) -> Result<BaseIntegrityWrite> {
    if covered_page_count == 0 {
        let catalog = Arc::new(BasePageIntegrityCatalog::build(0, 1, Vec::new())?);
        return Ok(BaseIntegrityWrite {
            catalog,
            descriptor: BasePageIntegrityDescriptor::empty(),
            aggregate_checksum: expected_aggregate_checksum.unwrap_or(0),
            published_page_count: catalog_page_start,
        });
    }
    let covered_page_end = covered_page_start
        .checked_add(covered_page_count)
        .ok_or_else(|| anyhow::anyhow!("Base integrity covered page range overflow"))?;
    if catalog_page_start < covered_page_end {
        anyhow::bail!("Base integrity catalog overlaps covered base pages");
    }

    // Reject unsupported metadata sizes before reading the base or allocating
    // its checksum vector. This is both the writer bound and the migration
    // bound for attacker-controlled legacy headers.
    BasePageIntegrityCatalog::encoded_len_for_page_count(covered_page_count)?;

    let checksum_capacity = usize::try_from(covered_page_count)
        .map_err(|_| anyhow::anyhow!("Base integrity page count exceeds memory limits"))?;
    let mut page_checksums = Vec::new();
    page_checksums
        .try_reserve_exact(checksum_capacity)
        .map_err(|_| anyhow::anyhow!("Base integrity catalog allocation exceeds memory limits"))?;
    let mut aggregate = Hasher::new();
    for offset in 0..covered_page_count {
        let page_id = covered_page_start
            .checked_add(offset)
            .ok_or_else(|| anyhow::anyhow!("Base integrity page id overflow"))?;
        let page = backend.read_page(page_id)?;
        if page.len() != PAGE_SIZE {
            anyhow::bail!(
                "Base integrity page {} has invalid length {}",
                page_id,
                page.len()
            );
        }
        aggregate.update(&page);
        page_checksums.push(compute_integrity_page_checksum(
            base_generation,
            page_id,
            &page,
        )?);
    }

    let aggregate_checksum = aggregate.finalize();
    if let Some(expected) = expected_aggregate_checksum
        && aggregate_checksum != expected
    {
        anyhow::bail!(
            "Base checksum mismatch during v11 migration: expected {expected:#010x}, got {aggregate_checksum:#010x}"
        );
    }

    let catalog = Arc::new(BasePageIntegrityCatalog::build(
        base_generation,
        covered_page_start,
        page_checksums,
    )?);
    let encoded = catalog.encode()?;
    let catalog_len = u64::try_from(encoded.len())
        .map_err(|_| anyhow::anyhow!("Base integrity catalog length exceeds u64"))?;
    let catalog_page_count = catalog_len
        .checked_add(PAGE_SIZE as u64 - 1)
        .ok_or_else(|| anyhow::anyhow!("Base integrity catalog page count overflow"))?
        / PAGE_SIZE as u64;
    let catalog_checksum = catalog_crc32(&encoded);
    let descriptor = BasePageIntegrityDescriptor::new(
        base_generation,
        covered_page_start,
        covered_page_count,
        catalog_page_start,
        catalog_page_count,
        catalog_len,
        catalog_checksum,
    )?;

    for offset in 0..catalog_page_count {
        let page_id = catalog_page_start
            .checked_add(offset)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog page id overflow"))?;
        let byte_start_u64 = offset
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog byte offset overflow"))?;
        let byte_start = usize::try_from(byte_start_u64)
            .map_err(|_| anyhow::anyhow!("Base integrity catalog byte offset exceeds usize"))?;
        let byte_end = byte_start.saturating_add(PAGE_SIZE).min(encoded.len());
        let mut page = vec![0u8; PAGE_SIZE];
        page.get_mut(..byte_end.saturating_sub(byte_start))
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog page slice out of bounds"))?
            .copy_from_slice(
                encoded
                    .get(byte_start..byte_end)
                    .ok_or_else(|| anyhow::anyhow!("Base integrity catalog bytes out of bounds"))?,
            );
        backend.write_page(page_id, &page)?;
    }

    // Catalog bytes must be durable and readable before page 0 can publish
    // their descriptor.
    backend.sync()?;
    let decoded = read_base_integrity_catalog(backend, descriptor)?;
    if decoded.as_ref() != catalog.as_ref() {
        anyhow::bail!("Base integrity catalog read-back does not match written catalog");
    }

    Ok(BaseIntegrityWrite {
        catalog,
        descriptor,
        aggregate_checksum,
        published_page_count: descriptor.catalog_page_end()?,
    })
}

fn read_base_integrity_catalog(
    backend: &dyn StorageBackend,
    descriptor: BasePageIntegrityDescriptor,
) -> Result<Arc<BasePageIntegrityCatalog>> {
    if descriptor.is_empty() {
        anyhow::bail!("Base integrity catalog descriptor is empty");
    }
    if !descriptor.checksum_valid() {
        anyhow::bail!("Base integrity catalog descriptor checksum mismatch");
    }

    let expected_len =
        BasePageIntegrityCatalog::encoded_len_for_page_count(descriptor.covered_page_count())?;
    let descriptor_len = usize::try_from(descriptor.catalog_len())
        .map_err(|_| anyhow::anyhow!("Base integrity catalog length exceeds memory limits"))?;
    if descriptor_len != expected_len {
        anyhow::bail!("Base integrity catalog length does not match its covered page count");
    }

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_len)
        .map_err(|_| anyhow::anyhow!("Base integrity catalog allocation exceeds memory limits"))?;
    let mut remaining = expected_len;
    for offset in 0..descriptor.catalog_page_count() {
        let page_id = descriptor
            .catalog_page_start()
            .checked_add(offset)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog page id overflow"))?;
        let page = backend.read_page(page_id)?;
        if page.len() != PAGE_SIZE {
            anyhow::bail!(
                "Base integrity catalog page {} has invalid length {}",
                page_id,
                page.len()
            );
        }
        let payload_len = remaining.min(PAGE_SIZE);
        bytes.extend_from_slice(
            page.get(..payload_len)
                .ok_or_else(|| anyhow::anyhow!("Base integrity catalog page is truncated"))?,
        );
        if page
            .get(payload_len..)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog padding is truncated"))?
            .iter()
            .any(|byte| *byte != 0)
        {
            anyhow::bail!("Base integrity catalog padding must be zero");
        }
        remaining = remaining.saturating_sub(payload_len);
    }
    if remaining != 0 || bytes.len() != expected_len {
        anyhow::bail!("Base integrity catalog is truncated");
    }
    if catalog_crc32(&bytes) != descriptor.catalog_checksum() {
        anyhow::bail!("Base integrity catalog checksum mismatch");
    }

    let catalog = BasePageIntegrityCatalog::decode(&bytes)?;
    if catalog.base_generation() != descriptor.base_generation()
        || catalog.covered_page_start() != descriptor.covered_page_start()
        || catalog.covered_page_count() != descriptor.covered_page_count()
    {
        anyhow::bail!("Base integrity catalog identity does not match page 0 descriptor");
    }
    Ok(Arc::new(catalog))
}

fn verify_base_integrity_pages(
    backend: &dyn StorageBackend,
    catalog: &BasePageIntegrityCatalog,
) -> Result<()> {
    for offset in 0..catalog.covered_page_count() {
        let page_id = catalog
            .covered_page_start()
            .checked_add(offset)
            .ok_or_else(|| anyhow::anyhow!("Base integrity page id overflow"))?;
        let page = backend.read_page(page_id)?;
        catalog.verify_page(page_id, &page)?;
    }
    Ok(())
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn verify_browser_published_page_bytes(
    resident_page0: &[u8],
    base_integrity: Option<&BasePageIntegrityCatalog>,
    page_id: u64,
    page: &[u8],
) -> Result<()> {
    if resident_page0.len() != PAGE_SIZE {
        anyhow::bail!("Resident browser page 0 has invalid length");
    }
    let header = FileHeader::from_bytes(resident_page0)?;
    header.validate()?;
    if page_id >= header.page_count {
        anyhow::bail!("Fetched page is outside the published page range");
    }
    if page.len() != PAGE_SIZE {
        anyhow::bail!(
            "Fetched page {page_id} has invalid length {} (expected {PAGE_SIZE})",
            page.len()
        );
    }
    if page_id == 0 {
        if page != resident_page0 {
            anyhow::bail!("Fetched page 0 does not match the loaded publication authority");
        }
        return Ok(());
    }
    if let Some(catalog) = base_integrity {
        let covered_end = catalog
            .covered_page_start()
            .checked_add(catalog.covered_page_count())
            .ok_or_else(|| anyhow::anyhow!("Base integrity page range overflow"))?;
        if page_id >= catalog.covered_page_start() && page_id < covered_end {
            catalog.verify_page(page_id, page)?;
        }
    }
    Ok(())
}

fn load_v11_base_integrity(
    backend: &dyn StorageBackend,
    header: &FileHeader,
    extension: &HeaderExtension,
) -> Result<Option<Arc<BasePageIntegrityCatalog>>> {
    let Some(descriptor) = validate_v11_base_integrity_descriptor(header, extension)? else {
        return Ok(None);
    };
    read_base_integrity_catalog(backend, descriptor).map(Some)
}

/// Validate page-0's complete v11 base layout without reading or allocating
/// integrity-catalog payload bytes.
fn validate_v11_base_integrity_descriptor(
    header: &FileHeader,
    extension: &HeaderExtension,
) -> Result<Option<BasePageIntegrityDescriptor>> {
    let descriptor = extension.base_integrity();
    if descriptor.is_empty() {
        let roots_are_empty = header.eavt_root_page == 0
            && header.aevt_root_page == 0
            && header.avet_root_page == 0
            && header.vaet_root_page == 0;
        if header.fact_page_count == 0
            && header.node_count == 0
            && roots_are_empty
            && extension.base_fact_page_start() == 1
            && header.page_count == 1
        {
            return Ok(None);
        }
        anyhow::bail!("Non-canonical empty or unprotected v11 database");
    }
    if descriptor.covered_page_start() != extension.base_fact_page_start() {
        anyhow::bail!("Base integrity coverage does not match base fact page start");
    }
    if descriptor.catalog_page_end()? > header.page_count {
        anyhow::bail!("Base integrity catalog exceeds published page count");
    }
    let expected_catalog_len =
        BasePageIntegrityCatalog::encoded_len_for_page_count(descriptor.covered_page_count())?;
    let descriptor_catalog_len = usize::try_from(descriptor.catalog_len())
        .map_err(|_| anyhow::anyhow!("Base integrity catalog length exceeds memory limits"))?;
    if descriptor_catalog_len != expected_catalog_len {
        anyhow::bail!("Base integrity catalog length does not match its covered page count");
    }
    let covered_page_end = descriptor.covered_page_end()?;
    let fact_page_end = extension
        .base_fact_page_start()
        .checked_add(header.fact_page_count)
        .ok_or_else(|| anyhow::anyhow!("Base fact page range overflow"))?;
    if fact_page_end > covered_page_end {
        anyhow::bail!("Base fact page range exceeds integrity coverage");
    }
    if header.fact_page_count == 0 {
        anyhow::bail!("Protected v11 base must declare at least one packed fact page");
    }
    let roots = [
        ("EAVT", header.eavt_root_page),
        ("AEVT", header.aevt_root_page),
        ("AVET", header.avet_root_page),
        ("VAET", header.vaet_root_page),
    ];
    for (name, root) in roots {
        if root == 0 || root < descriptor.covered_page_start() || root >= covered_page_end {
            anyhow::bail!("{name} root is outside base integrity coverage");
        }
    }
    Ok(Some(descriptor))
}

/// Compute CRC32 checksum over a range of pages on the backend.
fn compute_page_checksum(
    backend: &dyn StorageBackend,
    first_page: u64,
    num_pages: u64,
) -> Result<u32> {
    let mut hasher = Hasher::new();
    for i in 0..num_pages {
        let page_id = first_page
            .checked_add(i)
            .ok_or_else(|| anyhow::anyhow!("page id overflow in checksum computation"))?;
        let page = backend.read_page(page_id)?;
        hasher.update(&page);
    }
    Ok(hasher.finalize())
}

/// Compute CRC32 checksum over header bytes 0-79 (header_checksum field zeroed).
pub fn compute_header_checksum(header: &FileHeader) -> u32 {
    let mut bytes = header.to_bytes();
    // Zero out bytes 80–83 (the header_checksum field) before hashing.
    // The header is exactly 84 bytes (guaranteed by FileHeader::to_bytes).
    if let Some(b) = bytes.get_mut(80) {
        *b = 0;
    }
    if let Some(b) = bytes.get_mut(81) {
        *b = 0;
    }
    if let Some(b) = bytes.get_mut(82) {
        *b = 0;
    }
    if let Some(b) = bytes.get_mut(83) {
        *b = 0;
    }
    let mut hasher = Hasher::new();
    if let Some(slice) = bytes.get(..80) {
        hasher.update(slice);
    }
    hasher.finalize()
}

/// Compute CRC32 checksum over raw header bytes 0-79 (bytes 80-83 zeroed).
fn compute_header_checksum_from_bytes(bytes: &[u8]) -> u32 {
    let mut data = bytes.to_vec();
    if data.len() < 84 {
        data.resize(84, 0);
    }
    // Zero out bytes 80–83 (the header_checksum field) before hashing.
    if let Some(b) = data.get_mut(80) {
        *b = 0;
    }
    if let Some(b) = data.get_mut(81) {
        *b = 0;
    }
    if let Some(b) = data.get_mut(82) {
        *b = 0;
    }
    if let Some(b) = data.get_mut(83) {
        *b = 0;
    }
    let mut hasher = Hasher::new();
    if let Some(slice) = data.get(..80) {
        hasher.update(slice);
    }
    hasher.finalize()
}

fn build_header_page_with_base_integrity(
    header: FileHeader,
    base_fact_page_start: u64,
    base_integrity: BasePageIntegrityDescriptor,
) -> Result<Vec<u8>> {
    if header.version >= crate::storage::INTEGRITY_FORMAT_VERSION {
        let extension = HeaderExtension::empty()
            .with_base_fact_page_start(base_fact_page_start)?
            .with_base_integrity(base_integrity)?;
        build_header_page_with_extension(header, extension)
    } else {
        build_header_page(header)
    }
}

type SortedIndexEntries = (
    Vec<(EavtKey, FactRef)>,
    Vec<(AevtKey, FactRef)>,
    Vec<(AvetKey, FactRef)>,
    Vec<(VaetKey, FactRef)>,
);

fn new_index_entries_with_capacity(capacity: usize) -> SortedIndexEntries {
    (
        Vec::with_capacity(capacity),
        Vec::with_capacity(capacity),
        Vec::with_capacity(capacity),
        Vec::new(),
    )
}

fn eavt_key(fact: &Fact) -> EavtKey {
    eavt_key_with_value_bytes(fact, encode_value(&fact.value))
}

fn eavt_key_with_value_bytes(fact: &Fact, value_bytes: Vec<u8>) -> EavtKey {
    EavtKey {
        entity: fact.entity,
        attribute: fact.attribute.clone(),
        valid_from: fact.valid_from,
        valid_to: fact.valid_to,
        tx_count: fact.tx_count,
        value_bytes,
        tx_id: fact.tx_id,
        asserted: fact.asserted,
    }
}

fn aevt_key(fact: &Fact) -> AevtKey {
    aevt_key_with_value_bytes(fact, encode_value(&fact.value))
}

fn aevt_key_with_value_bytes(fact: &Fact, value_bytes: Vec<u8>) -> AevtKey {
    AevtKey {
        attribute: fact.attribute.clone(),
        entity: fact.entity,
        valid_from: fact.valid_from,
        valid_to: fact.valid_to,
        tx_count: fact.tx_count,
        value_bytes,
        tx_id: fact.tx_id,
        asserted: fact.asserted,
    }
}

fn avet_key(fact: &Fact) -> AvetKey {
    avet_key_with_value_bytes(fact, encode_value(&fact.value))
}

fn avet_key_with_value_bytes(fact: &Fact, value_bytes: Vec<u8>) -> AvetKey {
    AvetKey {
        attribute: fact.attribute.clone(),
        value_bytes,
        valid_from: fact.valid_from,
        valid_to: fact.valid_to,
        entity: fact.entity,
        tx_count: fact.tx_count,
        tx_id: fact.tx_id,
        asserted: fact.asserted,
    }
}

fn vaet_key(fact: &Fact) -> Option<VaetKey> {
    let Value::Ref(target) = &fact.value else {
        return None;
    };
    Some(VaetKey {
        ref_target: *target,
        attribute: fact.attribute.clone(),
        valid_from: fact.valid_from,
        valid_to: fact.valid_to,
        source_entity: fact.entity,
        tx_count: fact.tx_count,
        tx_id: fact.tx_id,
        asserted: fact.asserted,
    })
}

struct PendingIndexOrder<'a> {
    facts: &'a [Fact],
    refs: &'a [FactRef],
    value_bytes: Vec<Vec<u8>>,
    order: Vec<usize>,
}

// Initial base construction can serialize these borrowed views immediately into
// the B-tree page frontier. Keeping the owned key types for committed/pending
// merges avoids changing their ordering contract while removing three copies of
// every pending attribute and canonical value from the common empty-base path.
#[derive(Serialize)]
struct BorrowedEavtKey<'a> {
    entity: EntityId,
    attribute: &'a str,
    valid_from: i64,
    valid_to: i64,
    tx_count: u64,
    value_bytes: &'a [u8],
    tx_id: TxId,
    asserted: bool,
}

#[derive(Serialize)]
struct BorrowedAevtKey<'a> {
    attribute: &'a str,
    entity: EntityId,
    valid_from: i64,
    valid_to: i64,
    tx_count: u64,
    value_bytes: &'a [u8],
    tx_id: TxId,
    asserted: bool,
}

#[derive(Serialize)]
struct BorrowedAvetKey<'a> {
    attribute: &'a str,
    value_bytes: &'a [u8],
    valid_from: i64,
    valid_to: i64,
    entity: EntityId,
    tx_count: u64,
    tx_id: TxId,
    asserted: bool,
}

#[derive(Serialize)]
struct BorrowedVaetKey<'a> {
    ref_target: EntityId,
    attribute: &'a str,
    valid_from: i64,
    valid_to: i64,
    source_entity: EntityId,
    tx_count: u64,
    tx_id: TxId,
    asserted: bool,
}

fn required_index<T>(slice: &[T], index: usize) -> &T {
    match slice.get(index) {
        Some(value) => value,
        None => unreachable!("pending sort index must originate from the fact slice"),
    }
}

impl<'a> PendingIndexOrder<'a> {
    fn new(facts: &'a [Fact], refs: &'a [FactRef]) -> Self {
        debug_assert_eq!(facts.len(), refs.len());
        Self {
            facts,
            refs,
            value_bytes: facts.iter().map(|fact| encode_value(&fact.value)).collect(),
            order: Vec::with_capacity(facts.len()),
        }
    }

    fn reset_all(&mut self) {
        self.order.clear();
        self.order.extend(0..self.facts.len());
    }

    fn sort_eavt(&mut self) {
        self.reset_all();
        let facts = self.facts;
        let values = &self.value_bytes;
        self.order.sort_unstable_by(|&left, &right| {
            let left_fact = required_index(facts, left);
            let right_fact = required_index(facts, right);
            (
                left_fact.entity,
                &left_fact.attribute,
                left_fact.valid_from,
                left_fact.valid_to,
                left_fact.tx_count,
                required_index(values, left),
                left_fact.tx_id,
                left_fact.asserted,
            )
                .cmp(&(
                    right_fact.entity,
                    &right_fact.attribute,
                    right_fact.valid_from,
                    right_fact.valid_to,
                    right_fact.tx_count,
                    required_index(values, right),
                    right_fact.tx_id,
                    right_fact.asserted,
                ))
        });
    }

    fn sort_aevt(&mut self) {
        self.reset_all();
        let facts = self.facts;
        let values = &self.value_bytes;
        self.order.sort_unstable_by(|&left, &right| {
            let left_fact = required_index(facts, left);
            let right_fact = required_index(facts, right);
            (
                &left_fact.attribute,
                left_fact.entity,
                left_fact.valid_from,
                left_fact.valid_to,
                left_fact.tx_count,
                required_index(values, left),
                left_fact.tx_id,
                left_fact.asserted,
            )
                .cmp(&(
                    &right_fact.attribute,
                    right_fact.entity,
                    right_fact.valid_from,
                    right_fact.valid_to,
                    right_fact.tx_count,
                    required_index(values, right),
                    right_fact.tx_id,
                    right_fact.asserted,
                ))
        });
    }

    fn sort_avet(&mut self) {
        self.reset_all();
        let facts = self.facts;
        let values = &self.value_bytes;
        self.order.sort_unstable_by(|&left, &right| {
            let left_fact = required_index(facts, left);
            let right_fact = required_index(facts, right);
            (
                &left_fact.attribute,
                required_index(values, left),
                left_fact.valid_from,
                left_fact.valid_to,
                left_fact.entity,
                left_fact.tx_count,
                left_fact.tx_id,
                left_fact.asserted,
            )
                .cmp(&(
                    &right_fact.attribute,
                    required_index(values, right),
                    right_fact.valid_from,
                    right_fact.valid_to,
                    right_fact.entity,
                    right_fact.tx_count,
                    right_fact.tx_id,
                    right_fact.asserted,
                ))
        });
    }

    fn sort_vaet(&mut self) {
        self.order.clear();
        self.order.extend(
            self.facts
                .iter()
                .enumerate()
                .filter_map(|(index, fact)| matches!(fact.value, Value::Ref(_)).then_some(index)),
        );
        let facts = self.facts;
        self.order.sort_unstable_by(|&left, &right| {
            let left_fact = required_index(facts, left);
            let right_fact = required_index(facts, right);
            let Value::Ref(left_target) = &left_fact.value else {
                unreachable!("VAET order contains only Ref values")
            };
            let Value::Ref(right_target) = &right_fact.value else {
                unreachable!("VAET order contains only Ref values")
            };
            (
                left_target,
                &left_fact.attribute,
                left_fact.valid_from,
                left_fact.valid_to,
                left_fact.entity,
                left_fact.tx_count,
                left_fact.tx_id,
                left_fact.asserted,
            )
                .cmp(&(
                    right_target,
                    &right_fact.attribute,
                    right_fact.valid_from,
                    right_fact.valid_to,
                    right_fact.entity,
                    right_fact.tx_count,
                    right_fact.tx_id,
                    right_fact.asserted,
                ))
        });
    }

    fn eavt_entries(&self) -> impl Iterator<Item = (EavtKey, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            let value_bytes = required_index(&self.value_bytes, index).clone();
            (
                eavt_key_with_value_bytes(fact, value_bytes),
                *required_index(self.refs, index),
            )
        })
    }

    fn borrowed_eavt_entries(&self) -> impl Iterator<Item = (BorrowedEavtKey<'_>, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            (
                BorrowedEavtKey {
                    entity: fact.entity,
                    attribute: &fact.attribute,
                    valid_from: fact.valid_from,
                    valid_to: fact.valid_to,
                    tx_count: fact.tx_count,
                    value_bytes: required_index(&self.value_bytes, index).as_slice(),
                    tx_id: fact.tx_id,
                    asserted: fact.asserted,
                },
                *required_index(self.refs, index),
            )
        })
    }

    fn aevt_entries(&self) -> impl Iterator<Item = (AevtKey, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            let value_bytes = required_index(&self.value_bytes, index).clone();
            (
                aevt_key_with_value_bytes(fact, value_bytes),
                *required_index(self.refs, index),
            )
        })
    }

    fn borrowed_aevt_entries(&self) -> impl Iterator<Item = (BorrowedAevtKey<'_>, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            (
                BorrowedAevtKey {
                    attribute: &fact.attribute,
                    entity: fact.entity,
                    valid_from: fact.valid_from,
                    valid_to: fact.valid_to,
                    tx_count: fact.tx_count,
                    value_bytes: required_index(&self.value_bytes, index).as_slice(),
                    tx_id: fact.tx_id,
                    asserted: fact.asserted,
                },
                *required_index(self.refs, index),
            )
        })
    }

    fn avet_entries(&self) -> impl Iterator<Item = (AvetKey, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            let value_bytes = required_index(&self.value_bytes, index).clone();
            (
                avet_key_with_value_bytes(fact, value_bytes),
                *required_index(self.refs, index),
            )
        })
    }

    fn borrowed_avet_entries(&self) -> impl Iterator<Item = (BorrowedAvetKey<'_>, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            (
                BorrowedAvetKey {
                    attribute: &fact.attribute,
                    value_bytes: required_index(&self.value_bytes, index).as_slice(),
                    valid_from: fact.valid_from,
                    valid_to: fact.valid_to,
                    entity: fact.entity,
                    tx_count: fact.tx_count,
                    tx_id: fact.tx_id,
                    asserted: fact.asserted,
                },
                *required_index(self.refs, index),
            )
        })
    }

    fn vaet_entries(&self) -> impl Iterator<Item = (VaetKey, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            let key = match vaet_key(fact) {
                Some(key) => key,
                None => unreachable!("VAET order contains only Ref values"),
            };
            (key, *required_index(self.refs, index))
        })
    }

    fn borrowed_vaet_entries(&self) -> impl Iterator<Item = (BorrowedVaetKey<'_>, FactRef)> + '_ {
        self.order.iter().copied().map(|index| {
            let fact = required_index(self.facts, index);
            let Value::Ref(ref_target) = &fact.value else {
                unreachable!("VAET order contains only Ref values")
            };
            (
                BorrowedVaetKey {
                    ref_target: *ref_target,
                    attribute: &fact.attribute,
                    valid_from: fact.valid_from,
                    valid_to: fact.valid_to,
                    source_entity: fact.entity,
                    tx_count: fact.tx_count,
                    tx_id: fact.tx_id,
                    asserted: fact.asserted,
                },
                *required_index(self.refs, index),
            )
        })
    }

    #[cfg(feature = "bench-internals")]
    fn cached_value_bytes(&self) -> usize {
        self.value_bytes.iter().map(Vec::len).sum()
    }
}

fn push_index_entries_for_fact(entries: &mut SortedIndexEntries, fact: &Fact, fact_ref: FactRef) {
    let value_bytes = encode_value(&fact.value);
    entries.0.push((
        EavtKey {
            entity: fact.entity,
            attribute: fact.attribute.clone(),
            valid_from: fact.valid_from,
            valid_to: fact.valid_to,
            tx_count: fact.tx_count,
            value_bytes: value_bytes.clone(),
            tx_id: fact.tx_id,
            asserted: fact.asserted,
        },
        fact_ref,
    ));
    entries.1.push((
        AevtKey {
            attribute: fact.attribute.clone(),
            entity: fact.entity,
            valid_from: fact.valid_from,
            valid_to: fact.valid_to,
            tx_count: fact.tx_count,
            value_bytes: value_bytes.clone(),
            tx_id: fact.tx_id,
            asserted: fact.asserted,
        },
        fact_ref,
    ));
    entries.2.push((
        AvetKey {
            attribute: fact.attribute.clone(),
            value_bytes,
            valid_from: fact.valid_from,
            valid_to: fact.valid_to,
            entity: fact.entity,
            tx_count: fact.tx_count,
            tx_id: fact.tx_id,
            asserted: fact.asserted,
        },
        fact_ref,
    ));
    if let Value::Ref(target) = &fact.value {
        entries.3.push((
            VaetKey {
                ref_target: *target,
                attribute: fact.attribute.clone(),
                valid_from: fact.valid_from,
                valid_to: fact.valid_to,
                source_entity: fact.entity,
                tx_count: fact.tx_count,
                tx_id: fact.tx_id,
                asserted: fact.asserted,
            },
            fact_ref,
        ));
    }
}

fn sort_index_entries(entries: &mut SortedIndexEntries) {
    entries.0.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    entries.1.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    entries.2.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    entries.3.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
}

/// Build sorted index entry vecs for a slice of facts and their corresponding FactRefs.
///
/// Returns `(eavt_entries, aevt_entries, avet_entries, vaet_entries)`, each sorted by their
/// respective key type. The `vaet` vec only contains entries whose value is a `Value::Ref`.
fn build_sorted_index_entries(facts: &[Fact], refs: &[FactRef]) -> SortedIndexEntries {
    let mut entries = new_index_entries_with_capacity(facts.len());
    for (fact, &fact_ref) in facts.iter().zip(refs.iter()) {
        push_index_entries_for_fact(&mut entries, fact, fact_ref);
    }
    sort_index_entries(&mut entries);
    entries
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::graph::types::{FactRecord, Value};
    use crate::storage::PAGE_SIZE;
    use crate::storage::backend::{FileBackend, MemoryBackend};
    use std::io::Write;
    use std::time::Instant;
    use uuid::Uuid;

    #[test]
    fn pending_reference_order_matches_owned_key_order_for_all_value_kinds() {
        let entities = [Uuid::from_u128(3), Uuid::from_u128(1), Uuid::from_u128(2)];
        let values = vec![
            Value::String("zeta".to_string()),
            Value::Integer(-7),
            Value::Float(f64::NAN),
            Value::Boolean(true),
            Value::Keyword(":kind/value".to_string()),
            Value::Null,
            Value::Ref(Uuid::from_u128(9)),
        ];
        let mut facts = Vec::new();
        for (index, value) in values.into_iter().enumerate() {
            let entity = entities[index % entities.len()];
            let mut fact = Fact::with_valid_time(
                entity,
                if index % 2 == 0 { ":z/attr" } else { ":a/attr" }.to_string(),
                value,
                100 + index as u64,
                10 + index as u64,
                -20 + index as i64,
                200 + index as i64,
            );
            if index == 5 {
                fact.asserted = false;
            }
            facts.push(fact);
        }
        let refs: Vec<FactRef> = (0..facts.len())
            .map(|index| FactRef {
                page_id: 40 + index as u64,
                slot_index: index as u16,
            })
            .collect();
        let eager = build_sorted_index_entries(&facts, &refs);
        let mut order = PendingIndexOrder::new(&facts, &refs);

        order.sort_eavt();
        assert_eq!(order.eavt_entries().collect::<Vec<_>>(), eager.0);
        order.sort_aevt();
        assert_eq!(order.aevt_entries().collect::<Vec<_>>(), eager.1);
        order.sort_avet();
        assert_eq!(order.avet_entries().collect::<Vec<_>>(), eager.2);
        order.sort_vaet();
        assert_eq!(order.vaet_entries().collect::<Vec<_>>(), eager.3);
    }

    #[test]
    fn borrowed_pending_keys_build_byte_identical_index_pages() {
        let facts = vec![
            Fact::with_valid_time(
                Uuid::from_u128(2),
                ":person/name".to_string(),
                Value::String("beta".to_string()),
                12,
                2,
                100,
                400,
            ),
            Fact::with_valid_time(
                Uuid::from_u128(1),
                ":person/score".to_string(),
                Value::Float(-3.5),
                11,
                1,
                50,
                500,
            ),
            Fact::retract_with_valid_time(
                Uuid::from_u128(2),
                ":person/friend".to_string(),
                Value::Ref(Uuid::from_u128(9)),
                13,
                3,
                200,
                300,
            ),
        ];
        let refs = vec![
            FactRef {
                page_id: 8,
                slot_index: 1,
            },
            FactRef {
                page_id: 8,
                slot_index: 0,
            },
            FactRef {
                page_id: 9,
                slot_index: 0,
            },
        ];
        let eager = build_sorted_index_entries(&facts, &refs);
        let mut eager_backend = MemoryBackend::new();
        let eager_cache = PageCache::new(64);
        let (_, next1) = build_btree_from_key_entries(
            eager.0.into_iter(),
            &mut eager_backend,
            &eager_cache,
            1,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        let (_, next2) = build_btree_from_key_entries(
            eager.1.into_iter(),
            &mut eager_backend,
            &eager_cache,
            next1,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        let (_, next3) = build_btree_from_key_entries(
            eager.2.into_iter(),
            &mut eager_backend,
            &eager_cache,
            next2,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        let (_, eager_end) = build_btree_from_key_entries(
            eager.3.into_iter(),
            &mut eager_backend,
            &eager_cache,
            next3,
            BtreeBuildOptions::default(),
        )
        .unwrap();

        let mut reference_backend = MemoryBackend::new();
        let reference_cache = PageCache::new(64);
        let mut order = PendingIndexOrder::new(&facts, &refs);
        order.sort_eavt();
        let (_, next1) = build_btree_from_key_entries(
            order.borrowed_eavt_entries(),
            &mut reference_backend,
            &reference_cache,
            1,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        order.sort_aevt();
        let (_, next2) = build_btree_from_key_entries(
            order.borrowed_aevt_entries(),
            &mut reference_backend,
            &reference_cache,
            next1,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        order.sort_avet();
        let (_, next3) = build_btree_from_key_entries(
            order.borrowed_avet_entries(),
            &mut reference_backend,
            &reference_cache,
            next2,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        order.sort_vaet();
        let (_, reference_end) = build_btree_from_key_entries(
            order.borrowed_vaet_entries(),
            &mut reference_backend,
            &reference_cache,
            next3,
            BtreeBuildOptions::default(),
        )
        .unwrap();

        assert_eq!(reference_end, eager_end);
        for page_id in 1..eager_end {
            assert_eq!(
                reference_backend.read_page(page_id).unwrap(),
                eager_backend.read_page(page_id).unwrap(),
                "reference-sorted index page must match eager bytes"
            );
        }
    }

    #[test]
    fn test_persistent_fact_storage_new() {
        let backend = MemoryBackend::new();
        let storage = PersistentFactStorage::new(backend, 256).unwrap();

        // Should be able to create new storage
        assert_eq!(storage.storage().fact_count(), 0);
    }

    #[test]
    fn test_persistent_fact_storage_save_load() {
        // Create separate scopes to test persistence
        let alice = Uuid::new_v4();

        // First session: create and save facts
        {
            let backend = MemoryBackend::new();
            let mut storage = PersistentFactStorage::new(backend, 256).unwrap();

            storage
                .storage()
                .transact(
                    vec![
                        (
                            alice,
                            ":person/name".to_string(),
                            Value::String("Alice".to_string()),
                        ),
                        (alice, ":person/age".to_string(), Value::Integer(30)),
                    ],
                    None,
                )
                .unwrap();

            storage.mark_dirty();
            storage.save().unwrap();

            // Verify facts are persisted
            assert_eq!(storage.storage().fact_count(), 2);
        }

        // Note: In a real scenario, we'd reopen the same file.
        // MemoryBackend doesn't persist across instances, so this test
        // mainly validates the save/load mechanism.
    }

    #[test]
    fn test_persistent_fact_storage_auto_save() {
        let backend = MemoryBackend::new();

        let alice = Uuid::new_v4();

        // Create storage in a scope so it drops
        {
            let mut storage = PersistentFactStorage::new(backend, 256).unwrap();
            storage
                .storage()
                .transact(
                    vec![(
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    )],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            // Drop happens here, should auto-save
        }

        // Load into new storage - backend is consumed, need to create a new test
        // This test verifies the pattern, actual persistence is tested above
    }

    // -----------------------------------------------------------------------
    // Migration helpers
    // -----------------------------------------------------------------------

    /// Build a MemoryBackend that contains a v1-format file with two FactV1 facts.
    fn make_v1_backend() -> MemoryBackend {
        use crate::storage::{MAGIC_NUMBER, PAGE_SIZE};

        let alice = Uuid::new_v4();

        #[derive(serde::Serialize)]
        struct FactV1Ser {
            entity: Uuid,
            attribute: String,
            value: Value,
            tx_id: u64,
            asserted: bool,
        }

        let fact1 = FactV1Ser {
            entity: alice,
            attribute: ":person/name".to_string(),
            value: Value::String("Alice".to_string()),
            tx_id: 1000,
            asserted: true,
        };
        let fact2 = FactV1Ser {
            entity: alice,
            attribute: ":person/age".to_string(),
            value: Value::Integer(30),
            tx_id: 1000,
            asserted: true,
        };

        let mut backend = MemoryBackend::new();

        // Write v1 header (version=1, page_count=3)
        let mut header_bytes = vec![0u8; PAGE_SIZE];
        header_bytes[0..4].copy_from_slice(&MAGIC_NUMBER);
        header_bytes[4..8].copy_from_slice(&1u32.to_le_bytes()); // version = 1
        header_bytes[8..16].copy_from_slice(&3u64.to_le_bytes()); // page_count = 3
        header_bytes[16..24].copy_from_slice(&2u64.to_le_bytes()); // node_count = 2 facts
        backend.write_page(0, &header_bytes).unwrap();

        // Write facts (one per page)
        for (i, fact) in [&fact1, &fact2].iter().enumerate() {
            let data = postcard::to_allocvec(*fact).unwrap();
            let mut page = vec![0u8; PAGE_SIZE];
            page[..data.len()].copy_from_slice(&data);
            backend.write_page((i + 1) as u64, &page).unwrap();
        }

        backend
    }

    fn write_single_fact_v11(path: &std::path::Path) -> Uuid {
        let entity = Uuid::new_v4();
        let mut storage =
            PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
        storage
            .storage()
            .transact(
                vec![(
                    entity,
                    ":integrity/name".to_string(),
                    Value::String("Source A".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        storage.save().unwrap();
        drop(storage);
        entity
    }

    fn write_three_generation_v11_memory() -> (MemoryBackend, [Uuid; 3]) {
        let backend = MemoryBackend::new();
        let inspection = backend.clone();
        let mut storage = PersistentFactStorage::new(backend, 256).unwrap();
        let entities = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];

        for (index, entity) in entities.iter().copied().enumerate() {
            storage
                .storage()
                .transact(
                    vec![(
                        entity,
                        ":bootstrap/generation".to_string(),
                        Value::Integer(index as i64),
                    )],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            storage.save().unwrap();
        }
        drop(storage);
        (inspection, entities)
    }

    fn stage_browser_range(
        source: &MemoryBackend,
        destination: &mut MemoryBackend,
        range: BrowserPageRange,
    ) {
        for page_id in range.start_page()..range.end_page() {
            destination
                .write_page(page_id, &source.read_page(page_id).unwrap())
                .unwrap();
        }
    }

    fn stage_browser_ranges(
        source: &MemoryBackend,
        destination: &mut MemoryBackend,
        ranges: impl IntoIterator<Item = BrowserPageRange>,
    ) {
        for range in ranges {
            stage_browser_range(source, destination, range);
        }
    }

    #[test]
    fn browser_v11_bootstrap_plans_bounded_metadata_and_every_lineage() {
        let (source, _) = write_three_generation_v11_memory();
        let page0 = source.read_page(0).unwrap();
        let bootstrap = BrowserV11BootstrapPlan::from_page0(&page0).unwrap();

        assert_eq!(
            bootstrap.published_page_count(),
            FileHeader::from_bytes(&page0).unwrap().page_count
        );
        assert!(bootstrap.base_fact_range().page_count() > 0);
        assert!(
            bootstrap.base_covered_range().page_count() >= bootstrap.base_fact_range().page_count()
        );
        assert_eq!(bootstrap.required_ranges().len(), 1);
        assert_eq!(bootstrap.manifest_candidates().len(), 2);
        assert_eq!(bootstrap.candidate_manifest_ranges().len(), 2);
        assert!(
            bootstrap.manifest_candidates()[0].generation()
                > bootstrap.manifest_candidates()[1].generation()
        );
        assert_eq!(
            bootstrap.manifest_candidates()[0].slot(),
            HeaderManifestSlotName::Secondary
        );
        assert_eq!(
            bootstrap.manifest_candidates()[1].slot(),
            HeaderManifestSlotName::Primary
        );

        let mut resident_metadata = MemoryBackend::new();
        resident_metadata.write_page(0, &page0).unwrap();
        stage_browser_ranges(
            &source,
            &mut resident_metadata,
            bootstrap.required_ranges().iter().copied(),
        );
        stage_browser_ranges(
            &source,
            &mut resident_metadata,
            bootstrap.candidate_manifest_ranges(),
        );

        let resident = bootstrap
            .plan_resident_metadata(&resident_metadata)
            .unwrap();
        assert_eq!(
            resident.published_page_count(),
            bootstrap.published_page_count()
        );
        assert_eq!(resident.manifest_candidates().len(), 2);
        assert_eq!(resident.candidate_segment_ranges().len(), 2);
        assert_eq!(resident.manifest_candidates()[0].segment_ranges().len(), 2);
        assert_eq!(resident.manifest_candidates()[1].segment_ranges().len(), 1);
        assert_eq!(
            resident.manifest_candidates()[0].manifest_range(),
            bootstrap.manifest_candidates()[0].manifest_range()
        );
        assert_eq!(
            resident.manifest_candidates()[0].slot(),
            HeaderManifestSlotName::Secondary
        );
        assert!(
            resident.manifest_candidates()[0].generation()
                > resident.manifest_candidates()[1].generation()
        );

        let base_page_id = bootstrap.base_covered_range().start_page();
        let base_page = source.read_page(base_page_id).unwrap();
        resident
            .verify_fetched_published_page(base_page_id, &base_page)
            .unwrap();
        let mut corrupt_base_page = base_page;
        corrupt_base_page[PAGE_SIZE - 1] ^= 0x01;
        assert!(
            resident
                .verify_fetched_published_page(base_page_id, &corrupt_base_page)
                .is_err(),
            "externally fetched base bytes must be generation-verified"
        );
        assert!(
            resident
                .verify_fetched_published_page(
                    bootstrap.published_page_count(),
                    &vec![0; PAGE_SIZE]
                )
                .is_err(),
            "external reads must remain inside page 0's publication boundary"
        );

        let loaded = PersistentFactStorage::new(source, 256).unwrap();
        assert_eq!(
            loaded.browser_base_fact_range().unwrap(),
            bootstrap.base_fact_range()
        );
        assert_eq!(
            loaded.browser_published_page_count().unwrap(),
            bootstrap.published_page_count()
        );
        loaded
            .verify_browser_fetched_page(0, &page0)
            .expect("loaded page-0 authority must verify byte-exact");
    }

    #[test]
    fn browser_v11_sparse_segments_preserve_previous_manifest_fallback() {
        let (source, entities) = write_three_generation_v11_memory();
        let page0 = source.read_page(0).unwrap();
        let bootstrap = BrowserV11BootstrapPlan::from_page0(&page0).unwrap();

        let mut metadata = MemoryBackend::new();
        metadata.write_page(0, &page0).unwrap();
        stage_browser_ranges(
            &source,
            &mut metadata,
            bootstrap.required_ranges().iter().copied(),
        );
        stage_browser_ranges(
            &source,
            &mut metadata,
            bootstrap.candidate_manifest_ranges(),
        );
        let resident = bootstrap.plan_resident_metadata(&metadata).unwrap();
        assert_eq!(resident.manifest_candidates().len(), 2);

        // Pin the complete base and metadata, but stage only the older
        // lineage's segment set. Page 0 still points at the newer slot, so the
        // normal loader must try it, observe its missing segment, and fall back.
        stage_browser_range(&source, &mut metadata, bootstrap.base_covered_range());
        stage_browser_ranges(
            &source,
            &mut metadata,
            resident.manifest_candidates()[1]
                .segment_ranges()
                .iter()
                .copied(),
        );

        let recovered = PersistentFactStorage::new(metadata, 256)
            .expect("missing newest segment must recover through older manifest");
        let visible = recovered.storage().get_all_facts().unwrap();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|fact| fact.entity == entities[0]));
        assert!(visible.iter().any(|fact| fact.entity == entities[1]));
        assert!(visible.iter().all(|fact| fact.entity != entities[2]));
    }

    #[test]
    fn browser_v11_bootstrap_skips_missing_newest_manifest_payload() {
        let (source, _) = write_three_generation_v11_memory();
        let page0 = source.read_page(0).unwrap();
        let bootstrap = BrowserV11BootstrapPlan::from_page0(&page0).unwrap();
        let older = bootstrap.manifest_candidates()[1];

        let mut partial = MemoryBackend::new();
        partial.write_page(0, &page0).unwrap();
        stage_browser_ranges(
            &source,
            &mut partial,
            bootstrap.required_ranges().iter().copied(),
        );
        stage_browser_range(&source, &mut partial, older.manifest_range());

        let resident = bootstrap.plan_resident_metadata(&partial).unwrap();
        assert_eq!(resident.manifest_candidates().len(), 1);
        assert_eq!(
            resident.manifest_candidates()[0].generation(),
            older.generation()
        );
    }

    #[test]
    fn browser_v11_bootstrap_rejects_oversized_manifest_before_fetch() {
        let (source, _) = write_three_generation_v11_memory();
        let page0 = source.read_page(0).unwrap();
        let mut header = FileHeader::from_bytes(&page0).unwrap();
        let extension = HeaderExtension::read_from_page0(header.version, &page0)
            .unwrap()
            .unwrap();
        let manifest_len = u64::try_from(MAX_BROWSER_BOOTSTRAP_MANIFEST_BYTES).unwrap() + 1;
        let manifest_page_count = manifest_len.div_ceil(PAGE_SIZE as u64);
        let manifest_page_start = header.page_count;
        let malicious = HeaderManifestSlot::new(
            99,
            manifest_page_start,
            manifest_page_count,
            manifest_len,
            0,
        )
        .unwrap();
        header.page_count = manifest_page_start + manifest_page_count;
        header.header_checksum = compute_header_checksum(&header);
        let malicious_extension = HeaderExtension::new(malicious, extension.primary())
            .with_base_fact_page_start(extension.base_fact_page_start())
            .unwrap()
            .with_base_integrity(extension.base_integrity())
            .unwrap();
        let malicious_page0 =
            build_header_page_with_extension(header, malicious_extension).unwrap();

        assert!(
            BrowserV11BootstrapPlan::from_page0(&malicious_page0).is_err(),
            "unsupported newest metadata must not silently fall back to an older manifest"
        );
    }

    #[test]
    fn browser_v11_bootstrap_accepts_canonical_empty_database() {
        let page0 = build_header_page(FileHeader::new()).unwrap();
        let plan = BrowserV11BootstrapPlan::from_page0(&page0).unwrap();

        assert_eq!(plan.published_page_count(), 1);
        assert_eq!(plan.base_fact_range().page_count(), 0);
        assert_eq!(plan.base_covered_range().page_count(), 0);
        assert!(plan.required_ranges().is_empty());
        assert!(plan.manifest_candidates().is_empty());

        let mut backend = MemoryBackend::new();
        backend.write_page(0, &page0).unwrap();
        let resident = plan.plan_resident_metadata(&backend).unwrap();
        assert!(resident.manifest_candidates().is_empty());
    }

    fn read_header_and_extension(path: &std::path::Path) -> (FileHeader, HeaderExtension) {
        let bytes = std::fs::read(path).unwrap();
        let page0 = bytes.get(..PAGE_SIZE).expect("graph must contain page 0");
        let header = FileHeader::from_bytes(page0).unwrap();
        let extension = HeaderExtension::read_from_page0(header.version, page0)
            .unwrap()
            .expect("current graph must contain a header extension");
        (header, extension)
    }

    fn flip_page_byte(path: &std::path::Path, page_id: u64, byte_offset: usize) {
        let mut bytes = std::fs::read(path).unwrap();
        let page_start = usize::try_from(page_id).unwrap() * PAGE_SIZE;
        let offset = page_start + byte_offset;
        let byte = bytes.get_mut(offset).expect("target page byte must exist");
        *byte ^= 0x01;
        std::fs::write(path, bytes).unwrap();
    }

    fn downgrade_current_base_to_v10(path: &std::path::Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let (mut header, extension) = read_header_and_extension(path);
        let integrity = extension.base_integrity();
        let base_end = integrity.covered_page_end().unwrap();
        assert_eq!(
            integrity.catalog_page_start(),
            base_end,
            "fresh v11 fixture must place the catalog immediately after its base"
        );

        header.version = 10;
        header.page_count = base_end;
        header.header_checksum = compute_header_checksum(&header);
        let v10_extension = HeaderExtension::empty()
            .with_base_fact_page_start(extension.base_fact_page_start())
            .unwrap();
        let page0 = build_header_page_with_extension(header, v10_extension).unwrap();
        bytes.get_mut(..PAGE_SIZE).unwrap().copy_from_slice(&page0);
        bytes.truncate(usize::try_from(base_end).unwrap() * PAGE_SIZE);
        std::fs::write(path, bytes).unwrap();
    }

    fn downgrade_current_base_to_v11(path: &std::path::Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let (mut header, extension) = read_header_and_extension(path);

        header.version = crate::storage::INTEGRITY_FORMAT_VERSION;
        header.header_checksum = compute_header_checksum(&header);
        let v11_extension = HeaderExtension::new(extension.primary(), extension.secondary())
            .with_base_fact_page_start(extension.base_fact_page_start())
            .unwrap()
            .with_base_integrity(extension.base_integrity())
            .unwrap();
        let page0 = build_header_page_with_extension(header, v11_extension).unwrap();
        bytes.get_mut(..PAGE_SIZE).unwrap().copy_from_slice(&page0);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn test_load_preserves_original_tx_id() {
        let mut pfs = PersistentFactStorage::new(MemoryBackend::new(), 256).unwrap();

        let alice = Uuid::new_v4();
        pfs.storage()
            .transact(
                vec![(
                    alice,
                    ":person/name".to_string(),
                    Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();

        let original_tx_id = pfs.storage().get_all_facts().unwrap()[0].tx_id;

        pfs.mark_dirty();
        pfs.save().unwrap();

        // Reload from the same backend
        let backend = pfs.into_backend().unwrap();
        let pfs2 = PersistentFactStorage::new(backend, 256).unwrap();
        let loaded_tx_id = pfs2.storage().get_all_facts().unwrap()[0].tx_id;

        assert_eq!(
            original_tx_id, loaded_tx_id,
            "tx_id must survive save/load round-trip"
        );
    }

    #[test]
    fn test_migrate_v1_to_v2_assigns_defaults() {
        use crate::graph::types::VALID_TIME_FOREVER;

        let backend = make_v1_backend();
        let pfs = PersistentFactStorage::new(backend, 256).unwrap();
        let facts = pfs.storage().get_all_facts().unwrap();

        assert_eq!(facts.len(), 2);
        // Both facts share tx_id=1000 → same tx_count
        assert_eq!(
            facts[0].tx_count, facts[1].tx_count,
            "facts from the same tx_id batch must get the same tx_count"
        );
        assert_eq!(
            facts[0].valid_to, VALID_TIME_FOREVER,
            "migrated fact must have open-ended valid_to"
        );
        assert_eq!(
            facts[0].valid_from, 1000_i64,
            "migrated fact valid_from must equal original tx_id"
        );
    }

    #[test]
    fn corrupt_v1_fact_rejects_migration_without_publishing() {
        let mut backend = make_v1_backend();
        backend.write_page(1, &vec![0xFF; PAGE_SIZE]).unwrap();
        let inspection = backend.clone();

        let result = PersistentFactStorage::new(backend, 256);
        assert!(result.is_err(), "undecodable v1 fact must reject migration");
        let header = FileHeader::from_bytes(&inspection.read_page(0).unwrap()).unwrap();
        assert_eq!(
            header.version, 1,
            "failed v1 migration must not publish v11"
        );
    }

    #[test]
    fn corrupt_v4_fact_rejects_migration_without_publishing() {
        let mut backend = MemoryBackend::new();
        let mut header = FileHeader::new();
        header.version = 4;
        header.page_count = 2;
        header.node_count = 1;
        header.fact_page_format = crate::storage::FACT_PAGE_FORMAT_ONE_PER_PAGE;
        let mut page0 = header.to_bytes();
        page0.resize(PAGE_SIZE, 0);
        backend.write_page(0, &page0).unwrap();
        backend.write_page(1, &vec![0xFF; PAGE_SIZE]).unwrap();
        let inspection = backend.clone();

        let result = PersistentFactStorage::new(backend, 256);
        assert!(result.is_err(), "undecodable v4 fact must reject migration");
        let header = FileHeader::from_bytes(&inspection.read_page(0).unwrap()).unwrap();
        assert_eq!(
            header.version, 4,
            "failed v4 migration must not publish v11"
        );
    }

    #[test]
    fn corrupt_v7_packed_base_rejects_migration_without_publishing() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let mut corrupt = include_bytes!("../../tests/fixtures/compat.graph").to_vec();
        assert_eq!(u32::from_le_bytes(corrupt[4..8].try_into().unwrap()), 7);
        corrupt[PAGE_SIZE * 2 - 1] ^= 0x01;
        std::fs::write(path, &corrupt).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(
            result.is_err(),
            "corrupt v7 packed base must not be rebuilt into v11"
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            corrupt,
            "failed v7 migration must preserve the legacy image byte-exact"
        );
    }

    #[test]
    fn valid_v7_migration_appends_cow_base_and_preserves_legacy_pages() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let legacy = include_bytes!("../../tests/fixtures/compat.graph").to_vec();
        let legacy_header = FileHeader::from_bytes(&legacy[..PAGE_SIZE]).unwrap();
        assert_eq!(legacy_header.version, 7);
        assert_eq!(
            usize::try_from(legacy_header.page_count).unwrap() * PAGE_SIZE,
            legacy.len()
        );
        std::fs::write(path, &legacy).unwrap();

        let migrated = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
            .expect("valid v7 graph must migrate through an append-only candidate");
        assert_eq!(
            u64::try_from(migrated.storage().get_all_facts().unwrap().len()).unwrap(),
            legacy_header.node_count
        );
        drop(migrated);

        let current = std::fs::read(path).unwrap();
        let current_header = FileHeader::from_bytes(&current[..PAGE_SIZE]).unwrap();
        let extension = HeaderExtension::read_from_page0(current_header.version, &current)
            .unwrap()
            .expect("migrated v11 header must contain its extension");
        assert_eq!(current_header.version, crate::storage::FORMAT_VERSION);
        assert_eq!(current_header.node_count, legacy_header.node_count);
        assert_eq!(extension.base_fact_page_start(), legacy_header.page_count);
        assert_eq!(
            &current[PAGE_SIZE..legacy.len()],
            &legacy[PAGE_SIZE..],
            "v7 migration must leave every legacy non-header page byte-exact"
        );
    }

    #[test]
    fn v9_cow_migration_preserves_scoped_retraction_identity() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = Uuid::from_u128(0x900);
        let value = Value::String("windowed".to_string());
        let facts = vec![
            Fact::with_valid_time(
                entity,
                ":legacy/window".to_string(),
                value.clone(),
                10,
                1,
                100,
                200,
            ),
            Fact::with_valid_time(
                entity,
                ":legacy/window".to_string(),
                value.clone(),
                20,
                2,
                300,
                400,
            ),
            Fact::retract_with_valid_time(
                entity,
                ":legacy/window".to_string(),
                value.clone(),
                30,
                3,
                100,
                200,
            ),
        ];
        {
            let mut storage =
                PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
            for fact in facts.iter().cloned() {
                storage.storage().load_fact(fact).unwrap();
            }
            storage.storage().restore_tx_counter_from(3);
            storage.mark_dirty();
            storage.save().unwrap();
        }

        let mut legacy = std::fs::read(path).unwrap();
        let current_header = FileHeader::from_bytes(&legacy[..PAGE_SIZE]).unwrap();
        let current_extension =
            HeaderExtension::read_from_page0(current_header.version, &legacy[..PAGE_SIZE])
                .unwrap()
                .expect("current fixture must have a v11 extension");
        assert_eq!(current_extension.base_fact_page_start(), 1);
        let base_end = current_extension
            .base_integrity()
            .covered_page_end()
            .unwrap();
        let mut v9_header = current_header;
        v9_header.version = 9;
        v9_header.page_count = base_end;
        v9_header.header_checksum = compute_header_checksum(&v9_header);
        let v9_page0 = build_header_page(v9_header).unwrap();
        legacy[..PAGE_SIZE].copy_from_slice(&v9_page0);
        legacy.truncate(usize::try_from(base_end).unwrap() * PAGE_SIZE);
        std::fs::write(path, &legacy).unwrap();

        let migrated = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
            .expect("v9 scoped retraction graph must migrate");
        let mut migrated_facts = migrated.storage().get_all_facts().unwrap();
        migrated_facts.sort_by_key(|fact| fact.tx_count);
        assert_eq!(migrated_facts.len(), 3);
        assert_eq!(
            migrated_facts
                .iter()
                .map(|fact| (fact.tx_count, fact.valid_from, fact.valid_to, fact.asserted))
                .collect::<Vec<_>>(),
            vec![
                (1, 100, 200, true),
                (2, 300, 400, true),
                (3, 100, 200, false),
            ]
        );
        assert!(migrated_facts.iter().all(|fact| {
            fact.entity == entity && fact.attribute == ":legacy/window" && fact.value == value
        }));
        let backend = migrated.into_backend().unwrap();
        assert_eq!(
            FileHeader::from_bytes(&backend.read_page(0).unwrap())
                .unwrap()
                .node_count,
            3
        );
    }

    #[test]
    fn test_save_writes_current_header_and_tx_watermark() {
        use crate::storage::FORMAT_VERSION;

        let backend = MemoryBackend::new();
        let mut pfs = PersistentFactStorage::new(backend, 256).unwrap();
        let alice = Uuid::new_v4();
        pfs.storage()
            .transact(
                vec![(
                    alice,
                    ":name".to_string(),
                    crate::graph::types::Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();
        pfs.mark_dirty();
        pfs.save().unwrap();

        // Read back the header and verify version and last_checkpointed_tx_count
        let backend = pfs.into_backend().unwrap();
        let header_page = backend.read_page(0).unwrap();
        let header = crate::storage::FileHeader::from_bytes(&header_page).unwrap();
        assert_eq!(header.version, FORMAT_VERSION);
        assert_eq!(header.last_checkpointed_tx_count, 1); // one transact call
    }

    #[test]
    fn test_last_checkpointed_tx_count_getter() {
        let backend = MemoryBackend::new();
        let pfs = PersistentFactStorage::new(backend, 256).unwrap();
        // Fresh database: no checkpoint yet
        assert_eq!(pfs.last_checkpointed_tx_count(), 0);
    }

    #[test]
    fn test_indexes_survive_save_load_roundtrip() {
        use crate::graph::types::Value;
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;
        use uuid::Uuid;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        // Save phase
        {
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            pfs.storage()
                .transact(
                    vec![
                        (
                            alice,
                            ":name".to_string(),
                            Value::String("Alice".to_string()),
                        ),
                        (alice, ":friend".to_string(), Value::Ref(bob)),
                    ],
                    None,
                )
                .unwrap();
            pfs.dirty = true;
            pfs.save().unwrap();
        }

        // Load phase — indexes must be accessible via on-disk B+tree
        {
            let pfs = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            // v6: indexes live on disk via CommittedIndexReader, not in pending RAM
            let alice_facts = pfs.storage().get_facts_by_entity(&alice).unwrap();
            assert_eq!(
                alice_facts.len(),
                2,
                "EAVT must resolve 2 entries after reload"
            );
            // Check that Ref-valued fact is accessible
            let ref_facts: Vec<_> = alice_facts
                .iter()
                .filter(|f| matches!(&f.value, crate::graph::types::Value::Ref(_)))
                .collect();
            assert_eq!(
                ref_facts.len(),
                1,
                "Ref fact must be accessible after reload"
            );
        }
    }

    #[test]
    fn test_compute_index_checksum_stable() {
        use crate::graph::types::{Fact, VALID_TIME_FOREVER, Value};
        use uuid::Uuid;

        let e = Uuid::new_v4();
        let facts = vec![
            Fact::with_valid_time(
                e,
                ":a".to_string(),
                Value::Integer(1),
                100,
                2,
                0,
                VALID_TIME_FOREVER,
            ),
            Fact::with_valid_time(
                e,
                ":b".to_string(),
                Value::Integer(2),
                200,
                1,
                0,
                VALID_TIME_FOREVER,
            ),
        ];
        let c1 = compute_index_checksum(&facts).unwrap();
        // Reversed order — same checksum (deterministic sort applied inside)
        let facts_reversed = vec![facts[1].clone(), facts[0].clone()];
        let c2 = compute_index_checksum(&facts_reversed).unwrap();
        assert_eq!(c1, c2, "Checksum must be order-independent");
    }

    #[test]
    fn test_migrate_v1_tx_counter_set_correctly() {
        let backend = make_v1_backend();
        let pfs = PersistentFactStorage::new(backend, 256).unwrap();

        let alice = Uuid::new_v4();
        pfs.storage()
            .transact(
                vec![(alice, ":new/fact".to_string(), Value::Boolean(true))],
                None,
            )
            .unwrap();

        let new_fact = pfs
            .storage()
            .get_all_facts()
            .unwrap()
            .into_iter()
            .find(|f| f.attribute == ":new/fact")
            .unwrap();

        // After migrating 1 unique tx_id (tx_count=1), next tx should get tx_count=2
        assert_eq!(
            new_fact.tx_count, 2,
            "first new transaction after migration must get tx_count=2"
        );
    }

    #[test]
    fn test_save_writes_packed_pages() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        {
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            let mut tuples = Vec::new();
            for i in 0u64..50 {
                tuples.push((alice, format!(":attr{}", i), Value::Integer(i as i64)));
            }
            tuples.push((alice, ":friend".to_string(), Value::Ref(bob)));
            pfs.storage().transact(tuples, None).unwrap();
            pfs.mark_dirty();
            pfs.save().unwrap();
        }

        // Verify: header says current format, fact_page_format = PACKED
        {
            let backend = FileBackend::open(&path).unwrap();
            let header_bytes = backend.read_page(0).unwrap();
            let header = crate::storage::FileHeader::from_bytes(&header_bytes).unwrap();
            assert_eq!(header.version, crate::storage::FORMAT_VERSION);
            assert_eq!(
                header.fact_page_format,
                crate::storage::FACT_PAGE_FORMAT_PACKED
            );
            // 51 facts @ ~25/page = ~3 pages (far fewer than 51)
            let fact_page_count = header.eavt_root_page.saturating_sub(1);
            assert!(
                fact_page_count <= 5,
                "got {} fact pages (expected <=5)",
                fact_page_count
            );
        }
    }

    #[test]
    fn test_current_save_stores_nonzero_base_checksum() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();

        {
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            pfs.storage()
                .transact(
                    vec![(
                        alice,
                        ":name".to_string(),
                        Value::String("Alice".to_string()),
                    )],
                    None,
                )
                .unwrap();
            pfs.mark_dirty();
            pfs.save().unwrap();
        }

        {
            let backend = FileBackend::open(&path).unwrap();
            let header_bytes = backend.read_page(0).unwrap();
            let header = crate::storage::FileHeader::from_bytes(&header_bytes).unwrap();
            // Checksum should be non-zero for a non-empty DB
            assert_ne!(header.index_checksum, 0, "checksum must be set");
        }
    }

    #[test]
    fn test_v4_database_migrates_to_current_on_open() {
        use crate::storage::backend::FileBackend;
        use crate::storage::{FACT_PAGE_FORMAT_PACKED, PAGE_SIZE};
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();

        // Write a "v4-style" file: version=4, fact_page_format byte = 0 (legacy padding)
        {
            use crate::storage::FileHeader;
            let fact = crate::graph::types::Fact::with_valid_time(
                alice,
                ":name".to_string(),
                Value::String("Alice".to_string()),
                1u64,
                1u64,
                0i64,
                i64::MAX,
            );
            let mut backend = FileBackend::open(&path).unwrap();

            // Write fact at page 1 (one-per-page format)
            let data = postcard::to_allocvec(&fact).unwrap();
            let mut page = vec![0u8; PAGE_SIZE];
            page[..data.len()].copy_from_slice(&data);
            backend.write_page(1, &page).unwrap();

            // Write v4 header (fact_page_format byte will be 0)
            let mut header = FileHeader::new();
            header.version = 4;
            header.page_count = 2;
            header.node_count = 1;
            header.index_checksum = compute_index_checksum(std::slice::from_ref(&fact)).unwrap();
            header.fact_page_format = 0;
            let mut hbytes = header.to_bytes();
            hbytes.resize(PAGE_SIZE, 0);
            backend.write_page(0, &hbytes).unwrap();
            backend.sync().unwrap();
        }

        // Open — should auto-migrate to the current format
        {
            let pfs = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            assert_eq!(
                pfs.storage().fact_count(),
                1,
                "migrated fact must be loaded"
            );
        }

        // Verify file is now current format
        {
            let backend = FileBackend::open(&path).unwrap();
            let header_bytes = backend.read_page(0).unwrap();
            let header = crate::storage::FileHeader::from_bytes(&header_bytes).unwrap();
            assert_eq!(
                header.version,
                crate::storage::FORMAT_VERSION,
                "file must be upgraded to current format"
            );
            assert_eq!(header.fact_page_format, FACT_PAGE_FORMAT_PACKED);
        }
    }

    #[test]
    fn indexed_v4_migrates_by_fact_boundary_without_touching_legacy_pages() {
        use crate::storage::btree::write_all_indexes;
        use crate::storage::index::FactRef;
        use std::collections::BTreeMap;

        let entity = Uuid::from_u128(0x44);
        let fact = Fact::with_valid_time(
            entity,
            ":legacy/name".to_string(),
            Value::String("indexed v4".to_string()),
            7,
            1,
            7,
            VALID_TIME_FOREVER,
        );
        let facts = vec![fact.clone(), fact.clone()];
        let fact_refs = vec![
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
            FactRef {
                page_id: 2,
                slot_index: 0,
            },
        ];
        let mut backend = MemoryBackend::new();
        backend
            .write_page(0, &build_header_page(FileHeader::new()).unwrap())
            .unwrap();
        let encoded_fact = postcard::to_allocvec(&fact).unwrap();
        let mut fact_page = vec![0u8; PAGE_SIZE];
        fact_page[..encoded_fact.len()].copy_from_slice(&encoded_fact);
        backend.write_page(1, &fact_page).unwrap();
        backend.write_page(2, &fact_page).unwrap();

        let (eavt, aevt, avet, vaet) = build_sorted_index_entries(&facts, &fact_refs);
        let roots = write_all_indexes(
            &eavt.into_iter().collect::<BTreeMap<_, _>>(),
            &aevt.into_iter().collect::<BTreeMap<_, _>>(),
            &avet.into_iter().collect::<BTreeMap<_, _>>(),
            &vaet.into_iter().collect::<BTreeMap<_, _>>(),
            &mut backend,
            3,
        )
        .unwrap();
        let legacy_page_count = backend.page_count().unwrap();
        let mut header = FileHeader::new();
        header.version = 4;
        header.page_count = legacy_page_count;
        header.node_count = 2;
        header.last_checkpointed_tx_count = 1;
        header.eavt_root_page = roots.0;
        header.aevt_root_page = roots.1;
        header.avet_root_page = roots.2;
        header.vaet_root_page = roots.3;
        header.index_checksum = compute_index_checksum(&facts).unwrap();
        header.fact_page_format = 0;
        let page0 = build_header_page(header).unwrap();
        backend.write_page(0, &page0).unwrap();
        let legacy_pages: Vec<Vec<u8>> = (1..legacy_page_count)
            .map(|page_id| backend.read_page(page_id).unwrap())
            .collect();

        let migrated = PersistentFactStorage::new(backend, 256)
            .expect("indexed v4 graph must migrate through its fact boundary");
        assert_eq!(
            migrated
                .storage()
                .get_facts_by_entity(&entity)
                .unwrap()
                .len(),
            2,
            "duplicate legacy ledger rows must remain distinct after migration"
        );
        let backend = migrated.into_backend().unwrap();
        let current_page0 = backend.read_page(0).unwrap();
        let current_header = FileHeader::from_bytes(&current_page0).unwrap();
        let extension = HeaderExtension::read_from_page0(current_header.version, &current_page0)
            .unwrap()
            .expect("migrated v11 header must contain its extension");
        assert_eq!(current_header.version, crate::storage::FORMAT_VERSION);
        assert_eq!(current_header.node_count, 2);
        assert_eq!(
            extension.base_fact_page_start(),
            legacy_page_count,
            "legacy migration must append the candidate after the old authority"
        );
        for (offset, expected) in legacy_pages.iter().enumerate() {
            let page_id = u64::try_from(offset).unwrap() + 1;
            assert_eq!(
                backend.read_page(page_id).unwrap(),
                *expected,
                "legacy page must remain byte-exact until page-0 publication"
            );
        }
    }

    #[test]
    fn legacy_cow_rejects_incomplete_huge_published_prefix_before_sparse_write() {
        let fact = Fact::with_valid_time(
            Uuid::from_u128(0x4400),
            ":legacy/name".to_string(),
            Value::String("bounded prefix".to_string()),
            7,
            1,
            7,
            VALID_TIME_FOREVER,
        );
        let mut fact_page = vec![0u8; PAGE_SIZE];
        let encoded = postcard::to_allocvec(&fact).unwrap();
        fact_page[..encoded.len()].copy_from_slice(&encoded);
        let mut header = FileHeader::new();
        header.version = 4;
        header.page_count = 3_604_123_350;
        header.node_count = 1;
        header.eavt_root_page = 2;
        header.index_checksum = compute_index_checksum(std::slice::from_ref(&fact)).unwrap();
        header.fact_page_format = 0;
        let page0 = build_header_page(header).unwrap();
        let mut backend = MemoryBackend::new();
        backend.write_page(0, &page0).unwrap();
        backend.write_page(1, &fact_page).unwrap();
        let inspection = backend.clone();

        let started = Instant::now();
        let result = PersistentFactStorage::new(backend, 256);
        assert!(
            result.is_err(),
            "incomplete legacy prefix must not choose its claimed end as a COW write offset"
        );
        assert!(started.elapsed().as_secs() < 2);
        assert_eq!(inspection.page_count().unwrap(), 2);
        assert_eq!(inspection.read_page(0).unwrap(), page0);
        assert_eq!(inspection.read_page(1).unwrap(), fact_page);
    }

    #[test]
    fn test_current_format_reopen_wires_index_readers() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();

        // Save in the current format.
        {
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            pfs.storage()
                .transact(
                    vec![(
                        alice,
                        ":name".to_string(),
                        Value::String("Alice".to_string()),
                    )],
                    None,
                )
                .unwrap();
            pfs.mark_dirty();
            pfs.save().unwrap();
        }

        // Reload — CommittedFactReader should be wired, fact accessible
        {
            let pfs = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            assert_eq!(pfs.storage().fact_count(), 1);
            // Query by entity should work via index
            let facts = pfs.storage().get_facts_by_entity(&alice).unwrap();
            assert_eq!(facts.len(), 1);
            assert_eq!(facts[0].entity, alice);
        }
    }

    // ── v6 on-disk B+tree tests ─────────────────────────────────────────────

    #[test]
    fn test_save_writes_current_header() {
        let backend = MemoryBackend::new();
        let mut storage = PersistentFactStorage::new(backend, 256).unwrap();
        storage
            .storage()
            .transact(
                vec![(
                    Uuid::new_v4(),
                    ":name".to_string(),
                    Value::String("x".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        storage.save().unwrap();

        let backend = storage.into_backend().unwrap();
        let header_page = backend.read_page(0).unwrap();
        let header = crate::storage::FileHeader::from_bytes(&header_page).unwrap();
        assert_eq!(
            header.version,
            crate::storage::FORMAT_VERSION,
            "save() must write current header"
        );
        assert_eq!(header.to_bytes().len(), 84, "header must be 84 bytes");
        assert!(header.fact_page_count > 0, "fact_page_count must be set");
        assert!(
            header.eavt_root_page > 0,
            "eavt_root must be set after save"
        );
    }

    #[test]
    fn test_save_writes_v11_empty_manifest_slots() {
        use crate::storage::header_extension::{
            HeaderManifestSlotSelection, select_header_manifest_slot_from_page0,
        };

        let backend = MemoryBackend::new();
        let mut storage = PersistentFactStorage::new(backend, 256).unwrap();
        storage
            .storage()
            .transact(
                vec![(
                    Uuid::new_v4(),
                    ":name".to_string(),
                    Value::String("x".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        storage.save().unwrap();

        let backend = storage.into_backend().unwrap();
        let header_page = backend.read_page(0).unwrap();
        let header = crate::storage::FileHeader::from_bytes(&header_page).unwrap();
        let selection = select_header_manifest_slot_from_page0(header.version, &header_page)
            .expect("v11 empty manifest slots should decode");

        assert_eq!(header.version, crate::storage::FORMAT_VERSION);
        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
    }

    #[test]
    fn test_reopen_reads_v11_empty_manifest_slots_as_no_delta_manifest() {
        use crate::storage::backend::FileBackend;
        use crate::storage::header_extension::HeaderManifestSlotSelection;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        {
            let mut storage =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            storage
                .storage()
                .transact(
                    vec![(
                        Uuid::new_v4(),
                        ":name".to_string(),
                        Value::String("x".to_string()),
                    )],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            storage.save().unwrap();
        }

        let reopened = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
        assert!(matches!(
            reopened.header_manifest_selection(),
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
    }

    #[test]
    fn v11_fact_page_corruption_fails_on_first_selective_read() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = write_single_fact_v11(path);
        let (_, extension) = read_header_and_extension(path);
        let fact_page = extension.base_fact_page_start();

        // Flip padding so the packed fact remains decodable. Integrity, not
        // deserialization luck, must reject the page when it is first touched.
        flip_page_byte(path, fact_page, PAGE_SIZE - 1);

        let reopened = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
            .expect("v11 open must stay bounded and defer base-page reads");
        let error = reopened.storage().get_facts_by_entity(&entity).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Base page integrity checksum mismatch"),
            "selective fact read must fail at the verified page boundary"
        );
    }

    #[test]
    fn v11_index_root_corruption_fails_on_first_selective_read() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = write_single_fact_v11(path);
        let (header, _) = read_header_and_extension(path);

        flip_page_byte(path, header.eavt_root_page, PAGE_SIZE - 1);

        let reopened = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
            .expect("v11 open must not scan the base index");
        let error = reopened.storage().get_facts_by_entity(&entity).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Base page integrity checksum mismatch"),
            "selective index read must fail at the verified page boundary"
        );
    }

    #[test]
    fn v11_catalog_payload_corruption_rejects_open_without_rewrite() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let (_, extension) = read_header_and_extension(path);
        flip_page_byte(path, extension.base_integrity().catalog_page_start(), 0);
        let corrupted = std::fs::read(path).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(result.is_err(), "corrupt catalog must reject open");
        assert_eq!(
            std::fs::read(path).unwrap(),
            corrupted,
            "rejected catalog must not rewrite its source"
        );
    }

    #[test]
    fn v11_descriptor_corruption_rejects_open_without_rewrite() {
        use crate::storage::header_extension::{
            HEADER_EXTENSION_OFFSET, LEGACY_HEADER_EXTENSION_LEN,
        };

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let mut bytes = std::fs::read(path).unwrap();
        let descriptor_offset = HEADER_EXTENSION_OFFSET + LEGACY_HEADER_EXTENSION_LEN;
        bytes[descriptor_offset] ^= 0x01;
        std::fs::write(path, &bytes).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(result.is_err(), "corrupt descriptor must reject open");
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "rejected descriptor must not rewrite its source"
        );
    }

    #[test]
    fn v11_missing_catalog_page_rejects_open_without_rewrite() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let (_, extension) = read_header_and_extension(path);
        let truncate_at = extension.base_integrity().catalog_page_start() * PAGE_SIZE as u64;
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(truncate_at)
            .unwrap();
        let truncated = std::fs::read(path).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(result.is_err(), "missing catalog page must reject open");
        assert_eq!(
            std::fs::read(path).unwrap(),
            truncated,
            "rejected truncated graph must not be rewritten"
        );
    }

    #[test]
    fn v11_catalog_length_must_match_covered_page_count_before_read() {
        let descriptor = BasePageIntegrityDescriptor::new(1, 1, 1, 2, 1, 48, 0)
            .expect("structurally canonical descriptor should build");
        let backend = MemoryBackend::new();
        let error = read_base_integrity_catalog(&backend, descriptor).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("length does not match its covered page count"),
            "coverage must bound catalog allocation before any page read"
        );
    }

    #[test]
    fn oversized_v11_catalog_rejects_before_backend_read_or_allocation() {
        struct NoReadBackend {
            reads: Arc<std::sync::atomic::AtomicU64>,
        }

        impl StorageBackend for NoReadBackend {
            fn write_page(&mut self, _page_id: u64, _data: &[u8]) -> Result<()> {
                anyhow::bail!("unexpected write")
            }

            fn read_page(&self, _page_id: u64) -> Result<Vec<u8>> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("unexpected read")
            }

            fn sync(&mut self) -> Result<()> {
                Ok(())
            }

            fn page_count(&self) -> Result<u64> {
                Ok(0)
            }

            fn close(&mut self) -> Result<()> {
                Ok(())
            }

            fn backend_name(&self) -> &'static str {
                "no-read"
            }

            fn is_new(&self) -> bool {
                false
            }
        }

        let covered = crate::storage::page_integrity::MAX_BASE_INTEGRITY_COVERED_PAGES + 1;
        let catalog_len = 40u64 + covered * 4;
        let catalog_pages = catalog_len.div_ceil(PAGE_SIZE as u64);
        let descriptor = BasePageIntegrityDescriptor::new(
            1,
            1,
            covered,
            1 + covered,
            catalog_pages,
            catalog_len,
            0,
        )
        .expect("descriptor layout itself should be representable");
        let reads = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let backend = NoReadBackend {
            reads: reads.clone(),
        };

        let error = read_base_integrity_catalog(&backend, descriptor).unwrap_err();
        assert!(error.to_string().contains("supported"));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "unsupported catalog must reject before any backend read"
        );
    }

    #[test]
    fn noncanonical_empty_v11_header_rejects_without_sparse_write() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let mut header = FileHeader::new();
        header.page_count = 100;
        header.header_checksum = compute_header_checksum(&header);
        let extension = HeaderExtension::empty()
            .with_base_fact_page_start(100)
            .unwrap();
        let page0 = build_header_page_with_extension(header, extension).unwrap();
        std::fs::write(path, &page0).unwrap();
        let before = std::fs::read(path).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(
            result.is_err(),
            "empty v11 authority must be the canonical one-page shape"
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "rejected empty header must not create a sparse future-write target"
        );
    }

    #[test]
    fn protected_v11_base_requires_a_declared_fact_page() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let mut bytes = std::fs::read(path).unwrap();
        let mut header = FileHeader::from_bytes(&bytes[..PAGE_SIZE]).unwrap();
        header.fact_page_count = 0;
        header.header_checksum = compute_header_checksum(&header);
        bytes[..84].copy_from_slice(&header.to_bytes());
        std::fs::write(path, &bytes).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(
            result.is_err(),
            "non-empty integrity descriptor must protect a declared fact range"
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn v10_base_migrates_once_to_v11_with_stable_catalog() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = write_single_fact_v11(path);
        downgrade_current_base_to_v10(path);
        let v10 = std::fs::read(path).unwrap();
        let v10_header = FileHeader::from_bytes(&v10[..PAGE_SIZE]).unwrap();
        assert_eq!(v10_header.version, 10);

        {
            let migrated = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
                .expect("complete v10 base must migrate");
            assert_eq!(
                migrated
                    .storage()
                    .get_facts_by_entity(&entity)
                    .unwrap()
                    .len(),
                1
            );
        }

        let first_migration = std::fs::read(path).unwrap();
        let (header, extension) = read_header_and_extension(path);
        let integrity = extension.base_integrity();
        assert_eq!(header.version, crate::storage::FORMAT_VERSION);
        assert_eq!(integrity.base_generation(), 1);
        assert_eq!(integrity.covered_page_start(), 1);
        assert_eq!(integrity.covered_page_count(), v10_header.page_count - 1);
        assert!(first_migration.len() > v10.len());

        {
            let reopened = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
                .expect("migrated v11 graph must reopen");
            assert_eq!(
                reopened
                    .storage()
                    .get_facts_by_entity(&entity)
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(
            std::fs::read(path).unwrap(),
            first_migration,
            "second v11 open must not append or rewrite the catalog"
        );
    }

    #[test]
    fn v10_catalog_sync_failure_keeps_page0_on_v10() {
        let mut storage = PersistentFactStorage::new(MemoryBackend::new(), 256).unwrap();
        storage
            .storage()
            .transact(
                vec![(
                    Uuid::new_v4(),
                    ":integrity/name".to_string(),
                    Value::String("sync fault".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        storage.save().unwrap();

        let mut backend = storage.into_backend().unwrap();
        let current_page0 = backend.read_page(0).unwrap();
        let mut header = FileHeader::from_bytes(&current_page0).unwrap();
        let extension = HeaderExtension::read_from_page0(header.version, &current_page0)
            .unwrap()
            .expect("current memory fixture must contain a v11 extension");
        let base_end = extension.base_integrity().covered_page_end().unwrap();
        header.version = 10;
        header.page_count = base_end;
        header.header_checksum = compute_header_checksum(&header);
        let v10_extension = HeaderExtension::empty()
            .with_base_fact_page_start(extension.base_fact_page_start())
            .unwrap();
        let v10_page0 = build_header_page_with_extension(header, v10_extension).unwrap();
        backend.write_page(0, &v10_page0).unwrap();

        let inspection = backend.clone();
        let (faulting, config) =
            crate::storage::backend::fault_inject::FaultInjectingBackend::with_config(backend);
        config.lock().unwrap().fail_sync_after = Some(0);

        let result = PersistentFactStorage::new(faulting, 256);
        assert!(
            result.is_err(),
            "catalog sync failure must abort v10 migration"
        );
        assert_eq!(
            inspection.read_page(0).unwrap(),
            v10_page0,
            "catalog sync failure must not publish the v11 page-0 descriptor"
        );
        assert_eq!(
            FileHeader::from_bytes(&inspection.read_page(0).unwrap())
                .unwrap()
                .version,
            10,
            "v10 page 0 remains the only published authority after the failed migration"
        );
    }

    #[test]
    fn corrupt_v10_base_refuses_migration_without_rewrite() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        downgrade_current_base_to_v10(path);
        flip_page_byte(path, 1, PAGE_SIZE - 1);
        let corrupted_v10 = std::fs::read(path).unwrap();

        let result = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256);
        assert!(result.is_err(), "corrupt v10 base must not be migrated");
        assert_eq!(
            std::fs::read(path).unwrap(),
            corrupted_v10,
            "failed v10 migration must leave page 0 and all bytes unchanged"
        );
        assert_eq!(
            FileHeader::from_bytes(&corrupted_v10[..PAGE_SIZE])
                .unwrap()
                .version,
            10
        );
    }

    #[test]
    fn v10_late_cow_base_migrates_with_exact_coverage_and_preserved_prefix() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = write_single_fact_v11(path);
        {
            let mut storage =
                PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
            storage
                .storage()
                .transact(
                    vec![(entity, ":integrity/age".to_string(), Value::Integer(7))],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            assert_eq!(storage.save().unwrap(), CheckpointOutcome::DeltaSegment);
            let candidate = storage
                .write_recompact_candidate_from_visible_facts()
                .expect("late COW candidate must build");
            storage
                .publish_recompact_candidate(candidate)
                .expect("late COW candidate must publish");
        }
        let (_, current_extension) = read_header_and_extension(path);
        assert!(current_extension.base_fact_page_start() > 1);
        assert_eq!(
            current_extension.base_integrity().base_generation(),
            2,
            "COW recompact must publish the next base generation"
        );

        downgrade_current_base_to_v10(path);
        let v10 = std::fs::read(path).unwrap();
        let v10_header = FileHeader::from_bytes(&v10[..PAGE_SIZE]).unwrap();
        let v10_extension = HeaderExtension::read_from_page0(10, &v10[..PAGE_SIZE])
            .unwrap()
            .expect("v10 COW extension must decode");
        let base_start = v10_extension.base_fact_page_start();
        assert!(base_start > 1);

        {
            let migrated = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
                .expect("late COW v10 base must migrate");
            assert_eq!(
                migrated
                    .storage()
                    .get_facts_by_entity(&entity)
                    .unwrap()
                    .len(),
                2
            );
        }
        let migrated = std::fs::read(path).unwrap();
        let (_, migrated_extension) = read_header_and_extension(path);
        let descriptor = migrated_extension.base_integrity();
        assert_eq!(descriptor.covered_page_start(), base_start);
        assert_eq!(
            descriptor.covered_page_count(),
            v10_header.page_count - base_start
        );
        assert_eq!(
            &migrated[PAGE_SIZE..v10.len()],
            &v10[PAGE_SIZE..],
            "late COW migration must preserve every pre-existing non-header byte"
        );
    }

    #[test]
    fn selected_multi_delta_v10_migration_preserves_lineage_bytes_and_slots() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let v10 = crate::gate_e_test_support::NATIVE_FIXTURE;
        std::fs::write(path, v10).unwrap();
        let v10_header = FileHeader::from_bytes(&v10[..PAGE_SIZE]).unwrap();
        let v10_extension = HeaderExtension::read_from_page0(10, &v10[..PAGE_SIZE])
            .unwrap()
            .expect("frozen v10 extension must decode");
        assert_eq!(v10_header.version, 10);
        assert_eq!(
            usize::try_from(v10_header.page_count).unwrap() * PAGE_SIZE,
            v10.len(),
            "frozen migration fixture must be a complete published prefix"
        );

        {
            let migrated = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
                .expect("selected multi-delta v10 graph must migrate");
            assert_eq!(migrated.storage().get_all_facts().unwrap().len(), 13);
        }
        let first_migration = std::fs::read(path).unwrap();
        let (header, extension) = read_header_and_extension(path);
        assert_eq!(header.version, crate::storage::FORMAT_VERSION);
        assert_eq!(extension.primary(), v10_extension.primary());
        assert_eq!(extension.secondary(), v10_extension.secondary());
        assert_eq!(
            &first_migration[PAGE_SIZE..v10.len()],
            &v10[PAGE_SIZE..],
            "migration must append the catalog without rewriting base/delta/manifest bytes"
        );

        drop(
            PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
                .expect("migrated multi-delta graph must reopen"),
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            first_migration,
            "second open must not rewrite or grow the migrated lineage"
        );
    }

    #[test]
    fn v11_delta_checkpoint_preserves_base_catalog_identity_and_bytes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = write_single_fact_v11(path);
        let (before_header, before_extension) = read_header_and_extension(path);
        let before_descriptor = before_extension.base_integrity();
        let before_bytes = std::fs::read(path).unwrap();
        let catalog_start =
            usize::try_from(before_descriptor.catalog_page_start()).unwrap() * PAGE_SIZE;
        let catalog_end =
            usize::try_from(before_descriptor.catalog_page_end().unwrap()).unwrap() * PAGE_SIZE;
        let before_catalog = before_bytes[catalog_start..catalog_end].to_vec();

        {
            let mut storage =
                PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
            storage
                .storage()
                .transact(
                    vec![(entity, ":integrity/age".to_string(), Value::Integer(7))],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            assert_eq!(storage.save().unwrap(), CheckpointOutcome::DeltaSegment);
        }

        let (after_header, after_extension) = read_header_and_extension(path);
        let after_bytes = std::fs::read(path).unwrap();
        assert!(after_header.page_count > before_header.page_count);
        assert_eq!(after_extension.base_integrity(), before_descriptor);
        assert_eq!(
            &after_bytes[catalog_start..catalog_end],
            before_catalog.as_slice(),
            "delta publish must preserve the selected base catalog byte-exact"
        );
    }

    #[test]
    fn selected_delta_does_not_mask_corrupt_v11_base_page() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let entity = write_single_fact_v11(path);
        {
            let mut storage =
                PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
            storage
                .storage()
                .transact(
                    vec![(entity, ":integrity/age".to_string(), Value::Integer(7))],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            assert_eq!(storage.save().unwrap(), CheckpointOutcome::DeltaSegment);
        }
        let (_, extension) = read_header_and_extension(path);
        flip_page_byte(path, extension.base_fact_page_start(), PAGE_SIZE - 1);

        let reopened = PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256)
            .expect("selected delta metadata must open without scanning its base");
        let error = reopened.storage().get_facts_by_entity(&entity).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Base page integrity checksum mismatch"),
            "selected delta must not fall back around corrupt base data"
        );
    }

    #[test]
    fn full_save_refuses_to_bless_corrupt_v11_base() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let (_, extension) = read_header_and_extension(path);
        let mut storage =
            PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
        flip_page_byte(path, extension.base_fact_page_start(), PAGE_SIZE - 1);
        let corrupted_page0 = std::fs::read(path).unwrap()[..PAGE_SIZE].to_vec();

        storage.mark_dirty();
        let result = storage.save();
        assert!(result.is_err(), "full save must verify the old generation");
        assert_eq!(
            &std::fs::read(path).unwrap()[..PAGE_SIZE],
            corrupted_page0.as_slice(),
            "failed full save must not publish a new generation"
        );
        storage.dirty = false;
    }

    #[test]
    fn successful_full_save_increments_base_generation() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let (_, before_extension) = read_header_and_extension(path);
        assert_eq!(before_extension.base_integrity().base_generation(), 1);

        {
            let mut storage =
                PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
            storage.mark_dirty();
            assert_eq!(storage.save().unwrap(), CheckpointOutcome::FullRebuild);
        }
        let (_, after_extension) = read_header_and_extension(path);
        assert_eq!(
            after_extension.base_integrity().base_generation(),
            2,
            "full save must bind republished pages to the next generation"
        );
    }

    #[test]
    fn native_backup_refuses_corrupt_v11_base() {
        let source = tempfile::NamedTempFile::new().unwrap();
        let path = source.path();
        write_single_fact_v11(path);
        let (_, extension) = read_header_and_extension(path);
        let mut storage =
            PersistentFactStorage::new(FileBackend::open(path).unwrap(), 256).unwrap();
        flip_page_byte(path, extension.base_fact_page_start(), PAGE_SIZE - 1);
        let mut destination = tempfile::tempfile().unwrap();

        assert!(
            storage.copy_published_image_to(&mut destination).is_err(),
            "backup must not copy a base that fails its generation catalog"
        );
    }

    #[test]
    fn v11_open_reads_catalog_metadata_but_no_base_pages() {
        struct PageIdCountingBackend {
            inner: FileBackend,
            reads: Arc<Mutex<Vec<u64>>>,
        }

        impl StorageBackend for PageIdCountingBackend {
            fn write_page(&mut self, page_id: u64, data: &[u8]) -> Result<()> {
                self.inner.write_page(page_id, data)
            }

            fn read_page(&self, page_id: u64) -> Result<Vec<u8>> {
                self.reads.lock().unwrap().push(page_id);
                self.inner.read_page(page_id)
            }

            fn sync(&mut self) -> Result<()> {
                self.inner.sync()
            }

            fn page_count(&self) -> Result<u64> {
                self.inner.page_count()
            }

            fn close(&mut self) -> Result<()> {
                self.inner.close()
            }

            fn backend_name(&self) -> &'static str {
                "page-id-counting-file"
            }

            fn is_new(&self) -> bool {
                self.inner.is_new()
            }
        }

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        write_single_fact_v11(path);
        let (_, extension) = read_header_and_extension(path);
        let descriptor = extension.base_integrity();
        let reads = Arc::new(Mutex::new(Vec::new()));
        {
            let backend = PageIdCountingBackend {
                inner: FileBackend::open(path).unwrap(),
                reads: reads.clone(),
            };
            let storage = PersistentFactStorage::new(backend, 256).unwrap();
            drop(storage);
        }
        let read_ids = reads.lock().unwrap();
        assert!(
            read_ids.iter().any(|page_id| {
                *page_id >= descriptor.catalog_page_start()
                    && *page_id < descriptor.catalog_page_end().unwrap()
            }),
            "open must load and verify catalog metadata"
        );
        assert!(
            read_ids.iter().all(|page_id| {
                *page_id < descriptor.covered_page_start()
                    || *page_id >= descriptor.covered_page_end().unwrap()
            }),
            "open must not scan fact or index pages"
        );
    }

    #[test]
    fn empty_v10_manifest_canonicalizes_to_empty_v11() {
        use crate::storage::backend::FileBackend;
        use crate::storage::delta_manifest::{DeltaManifest, write_manifest_pages};
        use crate::storage::header_extension::{
            HeaderExtension, HeaderManifestSlot, HeaderManifestSlotSelection,
            build_header_page_with_extension, select_header_manifest_slot_from_page0,
        };
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        {
            let mut backend = FileBackend::open(&path).unwrap();
            let mut header = FileHeader::new();
            header.version = 10;
            let manifest = DeltaManifest::from_parts(
                11,
                DeltaBaseIdentity::from_header(&header),
                0,
                0,
                Vec::new(),
            )
            .expect("manifest should build");
            let descriptor = write_manifest_pages(&mut backend, 1, &manifest)
                .expect("manifest pages should write");
            header.page_count = 1 + descriptor.manifest_page_count();
            header.header_checksum = compute_header_checksum(&header);
            let page0 = build_header_page_with_extension(
                header,
                HeaderExtension::new(descriptor, HeaderManifestSlot::empty()),
            )
            .expect("header page should build");
            backend.write_page(0, &page0).unwrap();
            backend.sync().unwrap();
        }

        drop(PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap());
        let canonical = std::fs::read(&path).unwrap();
        let header = FileHeader::from_bytes(&canonical[..PAGE_SIZE]).unwrap();
        assert_eq!(header.version, crate::storage::FORMAT_VERSION);
        assert_eq!(header.page_count, 1);
        assert!(matches!(
            select_header_manifest_slot_from_page0(header.version, &canonical[..PAGE_SIZE])
                .unwrap(),
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
        drop(PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            canonical,
            "canonical empty v11 reopen must not rewrite the source"
        );
    }

    #[test]
    fn test_v10_header_without_extension_rejected_on_load() {
        let mut backend = MemoryBackend::new();
        let mut header = FileHeader::new();
        header.version = 10;
        header.page_count = 2;
        header.header_checksum = compute_header_checksum(&header);

        let mut header_page = header.to_bytes();
        header_page.resize(PAGE_SIZE, 0);
        backend.write_page(0, &header_page).unwrap();
        backend.write_page(1, &vec![0u8; PAGE_SIZE]).unwrap();

        let result = PersistentFactStorage::new(backend, 256);

        let message = match result {
            Ok(_) => panic!("v10 file without page-0 extension must be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(message.contains("Header extension is missing"));
    }

    #[test]
    fn test_out_of_bounds_delta_manifest_descriptor_rejected_on_load() {
        use crate::storage::delta_manifest::DeltaManifest;
        use crate::storage::header_extension::{
            HeaderExtension, HeaderManifestSlot, build_header_page_with_extension,
        };

        let mut header = FileHeader::new();
        header.version = 10;
        let manifest = DeltaManifest::from_parts(
            12,
            DeltaBaseIdentity::from_header(&header),
            0,
            0,
            Vec::new(),
        )
        .expect("manifest should build");
        let encoded = manifest.encode().expect("manifest should encode");
        let descriptor =
            HeaderManifestSlot::new(12, 99, 1, encoded.len() as u64, crc32fast::hash(&encoded))
                .expect("descriptor should build");
        header.page_count = 2;
        header.header_checksum = compute_header_checksum(&header);
        let page0 = build_header_page_with_extension(
            header,
            HeaderExtension::new(descriptor, HeaderManifestSlot::empty()),
        )
        .expect("header page should build");

        let mut backend = MemoryBackend::new();
        backend.write_page(0, &page0).unwrap();
        backend.write_page(1, &vec![0u8; PAGE_SIZE]).unwrap();

        let result = PersistentFactStorage::new(backend, 256);
        let message = match result {
            Ok(_) => panic!("out-of-bounds delta manifest descriptor must be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(message.contains("Delta manifest recovery required"));
    }

    #[test]
    fn test_both_invalid_delta_manifest_slots_rejected_on_load() {
        use crate::storage::delta_manifest::DeltaManifest;
        use crate::storage::header_extension::{
            HEADER_EXTENSION_OFFSET, HeaderExtension, HeaderManifestSlot,
            build_header_page_with_extension,
        };

        let mut header = FileHeader::new();
        header.version = 10;
        let manifest = DeltaManifest::from_parts(
            13,
            DeltaBaseIdentity::from_header(&header),
            0,
            0,
            Vec::new(),
        )
        .expect("manifest should build");
        let encoded = manifest.encode().expect("manifest should encode");
        let primary =
            HeaderManifestSlot::new(13, 1, 1, encoded.len() as u64, crc32fast::hash(&encoded))
                .expect("primary descriptor should build");
        let secondary =
            HeaderManifestSlot::new(12, 2, 1, encoded.len() as u64, crc32fast::hash(&encoded))
                .expect("secondary descriptor should build");
        header.page_count = 3;
        header.header_checksum = compute_header_checksum(&header);
        let mut page0 =
            build_header_page_with_extension(header, HeaderExtension::new(primary, secondary))
                .expect("header page should build");
        let primary_checksum_offset = HEADER_EXTENSION_OFFSET + HeaderExtension::PREFIX_LEN + 36;
        let secondary_checksum_offset = primary_checksum_offset + HeaderManifestSlot::LEN;
        page0[primary_checksum_offset] ^= 0xAA;
        page0[secondary_checksum_offset] ^= 0x55;

        let mut backend = MemoryBackend::new();
        backend.write_page(0, &page0).unwrap();
        backend.write_page(1, &vec![0u8; PAGE_SIZE]).unwrap();
        backend.write_page(2, &vec![0u8; PAGE_SIZE]).unwrap();

        let result = PersistentFactStorage::new(backend, 256);
        let message = match result {
            Ok(_) => panic!("both invalid delta manifest slots must be rejected"),
            Err(err) => err.to_string(),
        };
        assert!(message.contains("corrupt manifest slot"));
    }

    #[test]
    fn test_delta_checkpoint_fact_log_matches_visible_full_rebuild() {
        let base = Uuid::from_u128(0xabc);
        let target = Uuid::from_u128(0xdef);
        let base_fact = Fact::with_valid_time(
            base,
            ":name".to_string(),
            Value::String("base".to_string()),
            100,
            1,
            100,
            VALID_TIME_FOREVER,
        );
        let edge_fact = Fact::with_valid_time(
            base,
            ":edge/to".to_string(),
            Value::Ref(target),
            200,
            2,
            200,
            VALID_TIME_FOREVER,
        );
        let target_fact = Fact::with_valid_time(
            target,
            ":name".to_string(),
            Value::String("target".to_string()),
            200,
            2,
            200,
            VALID_TIME_FOREVER,
        );

        let mut delta_storage =
            PersistentFactStorage::new(MemoryBackend::new(), 256).expect("storage should create");
        delta_storage
            .storage()
            .load_fact(base_fact.clone())
            .expect("base fact should load");
        delta_storage.storage().restore_tx_counter_from(1);
        delta_storage.mark_dirty();
        delta_storage.save().expect("base checkpoint should save");
        delta_storage
            .storage()
            .load_fact(edge_fact.clone())
            .expect("edge fact should load");
        delta_storage
            .storage()
            .load_fact(target_fact.clone())
            .expect("target fact should load");
        delta_storage.storage().restore_tx_counter_from(2);
        delta_storage.mark_dirty();
        delta_storage.save().expect("delta checkpoint should save");
        assert!(matches!(
            delta_storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));

        let rebuild_storage = delta_storage
            .build_compact_copy(MemoryBackend::new(), 256)
            .expect("compact copy should build");

        assert_eq!(
            fact_projection(delta_storage.storage()).expect("delta facts should load"),
            fact_projection(rebuild_storage.storage()).expect("rebuild facts should load"),
            "delta checkpoint and full rebuild must expose the same fact identities"
        );
    }

    type FactProjection = (Uuid, String, Vec<u8>, i64, i64, u64, u64, bool);

    fn fact_projection(storage: &FactStorage) -> Result<Vec<FactProjection>> {
        let mut facts: Vec<_> = storage
            .get_all_facts()?
            .into_iter()
            .map(|fact| {
                (
                    fact.entity,
                    fact.attribute,
                    encode_value(&fact.value),
                    fact.valid_from,
                    fact.valid_to,
                    fact.tx_count,
                    fact.tx_id,
                    fact.asserted,
                )
            })
            .collect();
        facts.sort();
        Ok(facts)
    }

    fn fact_log_projection(storage: &FactStorage) -> Result<Vec<FactRecord>> {
        let mut records = Vec::new();
        storage.for_each_fact(|fact| {
            records.push(FactRecord::from_fact(fact));
            Ok(())
        })?;
        Ok(records)
    }

    fn test_ref_edge_facts(source: Uuid, target: Uuid) -> (Fact, Fact, Fact, Fact) {
        let base_fact = Fact::with_valid_time(
            source,
            ":name".to_string(),
            Value::String("source".to_string()),
            100,
            1,
            100,
            VALID_TIME_FOREVER,
        );
        let edge_value = Value::Ref(target);
        let edge_assert = Fact::with_valid_time(
            source,
            ":edge/to".to_string(),
            edge_value.clone(),
            200,
            2,
            200,
            VALID_TIME_FOREVER,
        );
        let target_fact = Fact::with_valid_time(
            target,
            ":name".to_string(),
            Value::String("target".to_string()),
            200,
            2,
            200,
            VALID_TIME_FOREVER,
        );
        let edge_retract = Fact::retract_with_valid_time(
            source,
            ":edge/to".to_string(),
            edge_value,
            300,
            3,
            200,
            VALID_TIME_FOREVER,
        );
        (base_fact, edge_assert, target_fact, edge_retract)
    }

    fn storage_with_visible_ref_delta_on<B: StorageBackend + 'static>(
        backend: B,
    ) -> Result<PersistentFactStorage<B>> {
        let source = Uuid::from_u128(0xabc);
        let target = Uuid::from_u128(0xdef);
        let (base_fact, edge_assert, target_fact, edge_retract) =
            test_ref_edge_facts(source, target);

        let mut storage = PersistentFactStorage::new(backend, 256)?;
        storage.storage().load_fact(base_fact)?;
        storage.storage().restore_tx_counter_from(1);
        storage.mark_dirty();
        assert_eq!(storage.save()?, CheckpointOutcome::FullRebuild);

        storage.storage().load_fact(edge_assert)?;
        storage.storage().load_fact(target_fact)?;
        storage.storage().load_fact(edge_retract)?;
        storage.storage().restore_tx_counter_from(3);
        storage.mark_dirty();
        assert_eq!(storage.save()?, CheckpointOutcome::DeltaSegment);
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        Ok(storage)
    }

    fn storage_with_visible_ref_delta() -> Result<PersistentFactStorage<MemoryBackend>> {
        storage_with_visible_ref_delta_on(MemoryBackend::new())
    }

    #[test]
    fn test_compact_copy_is_contiguous_and_preserves_history_and_watermark() -> Result<()> {
        let storage = storage_with_visible_ref_delta()?;
        let before = fact_projection(storage.storage())?;
        storage.storage().restore_tx_counter_from(9);

        let compact = storage.build_compact_copy(MemoryBackend::new(), 256)?;
        assert!(matches!(
            compact.delta_manifest_selection(),
            PersistedManifestSelection::NoDeltaManifest
        ));
        assert_eq!(
            compact.committed_fact_page_start.load(Ordering::SeqCst),
            1,
            "fresh compact images must start at page 1"
        );
        assert_eq!(
            compact.storage().current_tx_count(),
            9,
            "a watermark newer than the last fact must survive compaction"
        );
        assert_eq!(
            before,
            fact_projection(compact.storage())?,
            "compact copy must preserve every full-history identity field"
        );

        let backend = compact.into_backend()?;
        let reopened = PersistentFactStorage::new(backend, 256)?;
        assert_eq!(reopened.storage().current_tx_count(), 9);
        assert_eq!(before, fact_projection(reopened.storage())?);
        Ok(())
    }

    #[test]
    fn test_recompact_visible_delta_preserves_history_and_removes_manifest() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;
        let before = fact_projection(storage.storage())?;
        assert_eq!(before.len(), 4, "full-history rows must include retraction");
        assert!(
            storage.storage().get_all_facts()?.iter().any(|fact| {
                fact.attribute == ":edge/to" && matches!(fact.value, Value::Ref(_)) && fact.asserted
            }),
            "Ref edge assertion must be present before recompact"
        );
        assert!(
            storage.storage().get_all_facts()?.iter().any(|fact| {
                fact.attribute == ":edge/to"
                    && matches!(fact.value, Value::Ref(_))
                    && !fact.asserted
            }),
            "Ref edge retraction must be present before recompact"
        );

        let outcome = storage.recompact_visible_delta()?;
        assert_eq!(outcome, CheckpointOutcome::FullRebuildFromVisibleDelta);
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::NoDeltaManifest
        ));
        assert!(
            storage.committed_fact_page_start.load(Ordering::SeqCst) > 1,
            "recompact must publish a copy-on-write base after the previous image"
        );
        assert_eq!(
            before,
            fact_projection(storage.storage())?,
            "recompact must preserve full-history identity"
        );

        let backend = storage.into_backend()?;
        let reopened = PersistentFactStorage::new(backend, 256)?;
        assert!(matches!(
            reopened.delta_manifest_selection(),
            PersistedManifestSelection::NoDeltaManifest
        ));
        assert_eq!(
            before,
            fact_projection(reopened.storage())?,
            "reopened recompact base must preserve full-history identity"
        );
        Ok(())
    }

    #[test]
    fn test_recompact_visible_delta_preserves_fact_log_order() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;
        let before = fact_log_projection(storage.storage())?;
        assert_eq!(before.len(), 4, "fact log must include full Ref history");
        assert_eq!(
            before
                .iter()
                .map(|record| record.tx_count)
                .collect::<Vec<_>>(),
            vec![1, 2, 2, 3],
            "fixture must exercise deterministic storage replay order"
        );

        assert_eq!(
            storage.recompact_visible_delta()?,
            CheckpointOutcome::FullRebuildFromVisibleDelta
        );
        assert_eq!(
            before,
            fact_log_projection(storage.storage())?,
            "recompact must preserve export/replay fact-log order"
        );

        let backend = storage.into_backend()?;
        let reopened = PersistentFactStorage::new(backend, 256)?;
        assert_eq!(
            before,
            fact_log_projection(reopened.storage())?,
            "reopened recompact base must preserve export/replay fact-log order"
        );
        Ok(())
    }

    #[test]
    fn test_recompact_pre_header_candidate_keeps_previous_manifest_visible() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;
        let before = fact_projection(storage.storage())?;

        let candidate = storage.write_recompact_candidate_from_visible_facts()?;
        assert!(
            candidate.base_fact_page_start > 1,
            "candidate pages must be written after the current published image"
        );
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));

        let backend = storage.into_backend()?;
        let reopened = PersistentFactStorage::new(backend, 256)?;
        assert!(matches!(
            reopened.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        assert_eq!(
            before,
            fact_projection(reopened.storage())?,
            "unpublished recompact pages must not change the visible graph"
        );
        Ok(())
    }

    #[test]
    fn test_recompact_visible_delta_rejects_uncheckpointed_pending_facts() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;
        let pending = Fact::with_valid_time(
            Uuid::from_u128(0x1234),
            ":pending/name".to_string(),
            Value::String("pending".to_string()),
            400,
            4,
            400,
            VALID_TIME_FOREVER,
        );
        storage.storage().load_fact(pending)?;

        let result = storage.recompact_visible_delta();
        assert!(result.is_err(), "pending facts must block recompact");
        let message = match result {
            Ok(_) => String::new(),
            Err(error) => error.to_string(),
        };
        assert!(
            message.contains("pending facts"),
            "error should name the pending-facts guard"
        );
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_delta_maintenance_decision_skips_healthy_delta() -> Result<()> {
        let storage = storage_with_visible_ref_delta()?;

        assert_eq!(
            storage.delta_maintenance_decision(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_delta_maintenance_decision_uses_selected_manifest_growth() -> Result<()> {
        let mut storage = PersistentFactStorage::new(MemoryBackend::new(), 256)?;
        let base_page_count = 1_000_000;
        let mut segments = Vec::new();
        let mut page_start = base_page_count;
        for index in 0..1_024u64 {
            let tx_count = index.saturating_add(1);
            segments.push(DeltaManifestSegment::fixture(
                page_start, 1, tx_count, 1, tx_count, tx_count,
            ));
            page_start = page_start.saturating_add(1);
        }
        let manifest =
            DeltaManifest::new(1, DeltaBaseIdentity::fixture(base_page_count, 0), segments)?;
        storage.delta_manifest_selection = PersistedManifestSelection::Use {
            slot: HeaderManifestSlotName::Primary,
            manifest,
        };

        assert_eq!(
            storage.delta_maintenance_decision(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
        Ok(())
    }

    fn install_threshold_manifest<B: StorageBackend>(
        storage: &mut PersistentFactStorage<B>,
        segment_count: u64,
    ) -> Result<()> {
        let base_page_count = 1_000_000;
        let mut segments = Vec::new();
        let mut page_start = base_page_count;
        for index in 0..segment_count {
            let tx_count = index.saturating_add(1);
            segments.push(DeltaManifestSegment::fixture(
                page_start, 1, tx_count, 1, tx_count, tx_count,
            ));
            page_start = page_start.saturating_add(1);
        }
        let manifest =
            DeltaManifest::new(1, DeltaBaseIdentity::fixture(base_page_count, 0), segments)?;
        storage.delta_manifest_selection = PersistedManifestSelection::Use {
            slot: HeaderManifestSlotName::Primary,
            manifest,
        };
        Ok(())
    }

    #[test]
    fn test_idle_delta_maintenance_noops_for_healthy_delta() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;

        assert_eq!(
            storage.run_idle_delta_maintenance()?,
            CheckpointOutcome::Noop
        );
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_idle_delta_maintenance_recompacts_scheduled_delta() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;
        let before = fact_projection(storage.storage())?;
        install_threshold_manifest(&mut storage, 1_024)?;

        assert_eq!(
            storage.delta_maintenance_decision(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
        assert_eq!(
            storage.run_idle_delta_maintenance()?,
            CheckpointOutcome::FullRebuildFromVisibleDelta
        );
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::NoDeltaManifest
        ));
        assert_eq!(
            before,
            fact_projection(storage.storage())?,
            "idle maintenance must preserve visible fact identity"
        );
        Ok(())
    }

    #[test]
    fn test_v11_open_and_delta_checkpoint_defer_v12_upgrade_until_idle_maintenance() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("v11-idle-upgrade.graph");
        let original_entity = write_single_fact_v11(&path);
        downgrade_current_base_to_v11(&path);
        let v11_bytes = std::fs::read(&path)?;

        let mut storage = PersistentFactStorage::new(FileBackend::open(&path)?, 256)?;
        assert_eq!(
            std::fs::read(&path)?,
            v11_bytes,
            "opening v11 must not rewrite the published image"
        );
        assert_eq!(
            storage.delta_maintenance_decision(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
        assert!(
            storage
                .storage()
                .get_all_facts()?
                .iter()
                .any(|fact| fact.entity == original_entity)
        );

        let delta_entity = Uuid::new_v4();
        storage.storage().transact(
            vec![(
                delta_entity,
                ":integrity/name".to_string(),
                Value::String("Delta B".to_string()),
            )],
            None,
        )?;
        storage.mark_dirty();
        assert_eq!(storage.save()?, CheckpointOutcome::DeltaSegment);
        let (delta_header, _) = read_header_and_extension(&path);
        assert_eq!(
            delta_header.version,
            crate::storage::INTEGRITY_FORMAT_VERSION,
            "foreground delta checkpoint must preserve v11"
        );

        assert_eq!(
            storage.run_idle_delta_maintenance()?,
            CheckpointOutcome::FullRebuildFromVisibleDelta
        );
        let (upgraded_header, _) = read_header_and_extension(&path);
        assert_eq!(upgraded_header.version, crate::storage::FORMAT_VERSION);
        let upgraded_bytes = std::fs::read(&path)?;
        assert_eq!(
            storage.run_idle_delta_maintenance()?,
            CheckpointOutcome::Noop
        );
        assert_eq!(
            std::fs::read(&path)?,
            upgraded_bytes,
            "a healthy v12 base must not be rewritten by later idle maintenance"
        );

        drop(storage);
        let reopened = PersistentFactStorage::new(FileBackend::open(&path)?, 256)?;
        let visible = reopened.storage().get_all_facts()?;
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|fact| fact.entity == original_entity));
        assert!(visible.iter().any(|fact| fact.entity == delta_entity));
        Ok(())
    }

    #[test]
    fn test_idle_delta_maintenance_rejects_pending_facts() -> Result<()> {
        let mut storage = storage_with_visible_ref_delta()?;
        install_threshold_manifest(&mut storage, 1_024)?;
        let pending = Fact::with_valid_time(
            Uuid::from_u128(0x5678),
            ":pending/name".to_string(),
            Value::String("pending".to_string()),
            400,
            4,
            400,
            VALID_TIME_FOREVER,
        );
        storage.storage().load_fact(pending)?;

        let result = storage.run_idle_delta_maintenance();
        assert!(result.is_err(), "pending facts must block idle maintenance");
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_idle_delta_maintenance_failure_keeps_visible_delta() -> Result<()> {
        let (backend, config) =
            crate::storage::backend::fault_inject::FaultInjectingBackend::with_config(
                MemoryBackend::new(),
            );
        let mut storage = storage_with_visible_ref_delta_on(backend)?;
        let before = fact_projection(storage.storage())?;
        install_threshold_manifest(&mut storage, 1_024)?;
        config.lock().unwrap().fail_sync_after = Some(0);

        let result = storage.run_idle_delta_maintenance();

        assert!(
            result.is_err(),
            "maintenance must surface recompact publish failures"
        );
        assert!(matches!(
            storage.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        assert_eq!(
            before,
            fact_projection(storage.storage())?,
            "failed maintenance must keep the previous visible graph"
        );

        config.lock().unwrap().fail_sync_after = None;
        let backend = storage.into_backend()?;
        let reopened = PersistentFactStorage::new(backend, 256)?;
        assert!(matches!(
            reopened.delta_manifest_selection(),
            PersistedManifestSelection::Use { .. }
        ));
        assert_eq!(
            before,
            fact_projection(reopened.storage())?,
            "reopened storage must ignore unpublished recompact pages"
        );
        Ok(())
    }

    #[test]
    #[ignore = "Q2-B 1M recompact measurement; run manually with --ignored --nocapture"]
    fn measure_q2b_recompact_streaming_1m() -> Result<()> {
        let fact_count = std::env::var("MINIGRAF_Q2B_RECOMPACT_FACTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000_000);
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("q2b-recompact-streaming.graph");
        let mut storage = PersistentFactStorage::new(FileBackend::open(&path)?, 256)?;

        for index in 0..fact_count {
            let n = u64::try_from(index)
                .map_err(|_| anyhow::anyhow!("fixture index exceeds u64::MAX"))?
                .saturating_add(1);
            storage.storage().load_fact(Fact::with_valid_time(
                Uuid::from_u128(u128::from(n)),
                ":bench/value".to_string(),
                Value::Integer(i64::try_from(n).unwrap_or(i64::MAX)),
                n,
                n,
                i64::try_from(n).unwrap_or(i64::MAX),
                VALID_TIME_FOREVER,
            ))?;
        }
        let base_tx_count =
            u64::try_from(fact_count).map_err(|_| anyhow::anyhow!("fixture too large"))?;
        storage.storage().restore_tx_counter_from(base_tx_count);
        storage.mark_dirty();
        assert_eq!(storage.save()?, CheckpointOutcome::FullRebuild);

        let base_file_bytes = std::fs::metadata(&path)?.len();
        let base_fact_pages = storage.committed_fact_pages.load(Ordering::SeqCst);
        let delta_tx_count = base_tx_count.saturating_add(1);
        storage.storage().load_fact(Fact::with_valid_time(
            Uuid::from_u128(0xfeed),
            ":bench/ref".to_string(),
            Value::Ref(Uuid::from_u128(0xbeef)),
            delta_tx_count,
            delta_tx_count,
            i64::try_from(delta_tx_count).unwrap_or(i64::MAX),
            VALID_TIME_FOREVER,
        ))?;
        storage.storage().restore_tx_counter_from(delta_tx_count);
        storage.mark_dirty();
        assert_eq!(storage.save()?, CheckpointOutcome::DeltaSegment);

        let recompact_started = Instant::now();
        assert_eq!(
            storage.recompact_visible_delta()?,
            CheckpointOutcome::FullRebuildFromVisibleDelta
        );
        let recompact_elapsed = recompact_started.elapsed();
        let recompact_file_bytes = std::fs::metadata(&path)?.len();
        let recompact_fact_pages = storage.committed_fact_pages.load(Ordering::SeqCst);
        let candidate_fact_page_bytes = recompact_fact_pages.saturating_mul(PAGE_SIZE as u64);

        println!(
            "q2b_recompact_streaming,facts={},elapsed_ms={:.3},base_file_bytes={},recompact_file_bytes={},base_fact_pages={},recompact_fact_pages={},candidate_fact_page_bytes={}",
            fact_count.saturating_add(1),
            recompact_elapsed.as_secs_f64() * 1000.0,
            base_file_bytes,
            recompact_file_bytes,
            base_fact_pages,
            recompact_fact_pages,
            candidate_fact_page_bytes
        );
        Ok(())
    }

    #[test]
    fn test_load_v6_wires_committed_index_reader() {
        let alice = Uuid::new_v4();
        let backend = {
            let backend = MemoryBackend::new();
            let mut s = PersistentFactStorage::new(backend, 256).unwrap();
            s.storage()
                .transact(
                    vec![(
                        alice,
                        ":name".to_string(),
                        Value::String("Alice".to_string()),
                    )],
                    None,
                )
                .unwrap();
            s.mark_dirty();
            s.save().unwrap();
            s.into_backend().unwrap()
        };

        let s2 = PersistentFactStorage::new(backend, 256).unwrap();
        let facts = s2.storage().get_facts_by_entity(&alice).unwrap();
        assert_eq!(
            facts.len(),
            1,
            "committed fact must be visible after reopen"
        );
    }

    #[test]
    fn test_checkpoint_outcome_variants_cover_delta_and_fallback_paths() {
        let backend = MemoryBackend::new();
        let mut storage = PersistentFactStorage::new(backend, 256).unwrap();

        let clean_outcome = storage.save().unwrap();
        assert_eq!(
            clean_outcome,
            CheckpointOutcome::Noop,
            "clean save should publish nothing"
        );
        assert!(
            !clean_outcome.permits_wal_retire(),
            "no-op save must not prove WAL durability"
        );

        storage
            .storage()
            .transact(
                vec![(
                    Uuid::new_v4(),
                    ":base/name".to_string(),
                    Value::String("base".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        let base_outcome = storage.save().unwrap();
        assert_eq!(
            base_outcome,
            CheckpointOutcome::FullRebuild,
            "first checkpoint should build a base view"
        );
        assert!(
            base_outcome.permits_wal_retire(),
            "full rebuild should prove WAL durability"
        );

        storage
            .storage()
            .transact(
                vec![(
                    Uuid::new_v4(),
                    ":delta/name".to_string(),
                    Value::String("delta".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        assert_eq!(
            storage.save().unwrap(),
            CheckpointOutcome::DeltaSegment,
            "small append on a base view should publish a delta segment"
        );

        storage
            .storage()
            .transact(
                vec![(
                    Uuid::new_v4(),
                    ":after-delta/name".to_string(),
                    Value::String("after".to_string()),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        assert_eq!(
            storage.save().unwrap(),
            CheckpointOutcome::DeltaSegment,
            "pending facts on a visible delta should publish through the inactive manifest slot"
        );
    }

    #[test]
    fn repeated_delta_checkpoint_does_not_reread_resident_segments() {
        struct CountingBackend {
            inner: MemoryBackend,
            reads: Arc<AtomicU64>,
        }

        impl StorageBackend for CountingBackend {
            fn write_page(&mut self, page_id: u64, data: &[u8]) -> Result<()> {
                self.inner.write_page(page_id, data)
            }

            fn read_page(&self, page_id: u64) -> Result<Vec<u8>> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.inner.read_page(page_id)
            }

            fn sync(&mut self) -> Result<()> {
                self.inner.sync()
            }

            fn page_count(&self) -> Result<u64> {
                self.inner.page_count()
            }

            fn has_complete_page_prefix(&self, published_page_count: u64) -> Result<bool> {
                self.inner.has_complete_page_prefix(published_page_count)
            }

            fn close(&mut self) -> Result<()> {
                self.inner.close()
            }

            fn backend_name(&self) -> &'static str {
                "counting-memory"
            }

            fn is_new(&self) -> bool {
                self.inner.is_new()
            }
        }

        let reads = Arc::new(AtomicU64::new(0));
        let backend = CountingBackend {
            inner: MemoryBackend::new(),
            reads: reads.clone(),
        };
        let mut storage = PersistentFactStorage::new(backend, 256).unwrap();

        for (index, expected_outcome) in [
            CheckpointOutcome::FullRebuild,
            CheckpointOutcome::DeltaSegment,
        ]
        .into_iter()
        .enumerate()
        {
            storage
                .storage()
                .transact(
                    vec![(
                        Uuid::from_u128(index as u128 + 1),
                        ":resident/name".to_string(),
                        Value::Integer(index as i64),
                    )],
                    None,
                )
                .unwrap();
            storage.mark_dirty();
            assert_eq!(storage.save().unwrap(), expected_outcome);
        }

        let reads_before_second_delta = reads.load(Ordering::SeqCst);
        let newest_entity = Uuid::from_u128(3);
        storage
            .storage()
            .transact(
                vec![(
                    newest_entity,
                    ":resident/name".to_string(),
                    Value::Integer(2),
                )],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        assert_eq!(storage.save().unwrap(), CheckpointOutcome::DeltaSegment);

        assert_eq!(
            reads.load(Ordering::SeqCst) - reads_before_second_delta,
            1,
            "checkpoint should read page 0, not replay older delta pages"
        );
        assert_eq!(storage.resident_delta_segment_count, 2);
        assert_eq!(
            storage
                .storage()
                .get_facts_by_entity(&newest_entity)
                .unwrap()
                .len(),
            1,
            "new segment must be query-visible after the resident append"
        );
    }

    #[test]
    fn test_save_twice_merges_committed_and_pending() {
        let backend = MemoryBackend::new();
        let mut storage = PersistentFactStorage::new(backend, 256).unwrap();
        let e1 = Uuid::new_v4();
        let e2 = Uuid::new_v4();

        // First checkpoint (e1 committed)
        storage
            .storage()
            .transact(
                vec![(e1, ":name".to_string(), Value::String("Alice".to_string()))],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        storage.save().unwrap();

        // Second checkpoint (e2 pending → committed)
        storage
            .storage()
            .transact(
                vec![(e2, ":name".to_string(), Value::String("Bob".to_string()))],
                None,
            )
            .unwrap();
        storage.mark_dirty();
        storage.save().unwrap();

        let backend = storage.into_backend().unwrap();
        let s2 = PersistentFactStorage::new(backend, 256).unwrap();
        let e1_facts = s2.storage().get_facts_by_entity(&e1).unwrap();
        let e2_facts = s2.storage().get_facts_by_entity(&e2).unwrap();
        assert_eq!(
            e1_facts.len(),
            1,
            "e1 from first checkpoint must survive second checkpoint"
        );
        assert_eq!(
            e2_facts.len(),
            1,
            "e2 from second checkpoint must be visible"
        );
    }

    #[test]
    fn test_v6_migration_from_v5_unit() {
        let mut backend = MemoryBackend::new();
        let mut page = vec![0u8; PAGE_SIZE];
        page[0..4].copy_from_slice(b"MGRF");
        page[4..8].copy_from_slice(&5u32.to_le_bytes()); // version = 5
        page[8..16].copy_from_slice(&2u64.to_le_bytes()); // page_count = 2 (header + 1 empty page)
        page[68] = 0x02; // fact_page_format = PACKED
        backend.write_page(0, &page).unwrap();
        // Write a structurally valid empty packed fact page so page_count > 1
        // triggers migration without relying on the old silent wrong-type skip.
        let mut empty_fact_page = vec![0u8; PAGE_SIZE];
        empty_fact_page[0] = crate::storage::packed_pages::PAGE_TYPE_PACKED;
        page[64..68].copy_from_slice(&crc32fast::hash(&empty_fact_page).to_le_bytes());
        backend.write_page(0, &page).unwrap();
        backend.write_page(1, &empty_fact_page).unwrap();

        let s = PersistentFactStorage::new(backend, 256).unwrap();
        let b = s.into_backend().unwrap();
        let header_page = b.read_page(0).unwrap();
        let header = crate::storage::FileHeader::from_bytes(&header_page).unwrap();
        assert_eq!(
            header.version,
            crate::storage::FORMAT_VERSION,
            "migration must upgrade header to current format"
        );
        assert_eq!(header.to_bytes().len(), 84, "header must be 84 bytes");
        // page_count=2 means 1 fact page (page 1), even if empty
        assert_eq!(
            header.fact_page_count, 1,
            "fact_page_count must reflect page layout"
        );
    }

    #[test]
    fn indexed_nonempty_v5_migrates_to_cow_v11() {
        use crate::storage::btree::write_all_indexes;
        use std::collections::BTreeMap;

        let entity = Uuid::from_u128(0x55);
        let fact = Fact::with_valid_time(
            entity,
            ":legacy/name".to_string(),
            Value::String("indexed v5".to_string()),
            9,
            1,
            9,
            VALID_TIME_FOREVER,
        );
        let (fact_pages, fact_refs) = pack_facts(std::slice::from_ref(&fact), 1).unwrap();
        let mut backend = MemoryBackend::new();
        backend
            .write_page(0, &build_header_page(FileHeader::new()).unwrap())
            .unwrap();
        backend.write_page(1, &fact_pages[0]).unwrap();
        let (eavt, aevt, avet, vaet) =
            build_sorted_index_entries(std::slice::from_ref(&fact), &fact_refs);
        let roots = write_all_indexes(
            &eavt.into_iter().collect::<BTreeMap<_, _>>(),
            &aevt.into_iter().collect::<BTreeMap<_, _>>(),
            &avet.into_iter().collect::<BTreeMap<_, _>>(),
            &vaet.into_iter().collect::<BTreeMap<_, _>>(),
            &mut backend,
            2,
        )
        .unwrap();
        let legacy_page_count = backend.page_count().unwrap();
        let mut header = FileHeader::new();
        header.version = 5;
        header.page_count = legacy_page_count;
        header.node_count = 1;
        header.last_checkpointed_tx_count = 1;
        header.eavt_root_page = roots.0;
        header.aevt_root_page = roots.1;
        header.avet_root_page = roots.2;
        header.vaet_root_page = roots.3;
        header.index_checksum = crc32fast::hash(&fact_pages[0]);
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        backend
            .write_page(0, &build_header_page(header).unwrap())
            .unwrap();

        let migrated = PersistentFactStorage::new(backend, 256)
            .expect("indexed non-empty v5 graph must migrate");
        assert_eq!(
            migrated
                .storage()
                .get_facts_by_entity(&entity)
                .unwrap()
                .len(),
            1
        );
        let backend = migrated.into_backend().unwrap();
        let page0 = backend.read_page(0).unwrap();
        let current_header = FileHeader::from_bytes(&page0).unwrap();
        let extension = HeaderExtension::read_from_page0(current_header.version, &page0)
            .unwrap()
            .expect("migrated v11 header must contain its extension");
        assert_eq!(current_header.version, crate::storage::FORMAT_VERSION);
        assert_eq!(current_header.node_count, 1);
        assert_eq!(extension.base_fact_page_start(), legacy_page_count);
    }

    #[test]
    fn nonempty_v5_with_root_on_page_one_rejects_without_publishing() {
        let entity = Uuid::from_u128(0x551);
        let fact = Fact::with_valid_time(
            entity,
            ":legacy/name".to_string(),
            Value::String("must survive".to_string()),
            9,
            1,
            9,
            VALID_TIME_FOREVER,
        );
        let (fact_pages, _) = pack_facts(&[fact], 1).unwrap();
        let mut backend = MemoryBackend::new();
        let mut header = FileHeader::new();
        header.version = 5;
        header.page_count = 2;
        header.node_count = 1;
        header.eavt_root_page = 1;
        header.index_checksum = crc32fast::hash(&fact_pages[0]);
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        let page0 = build_header_page(header).unwrap();
        backend.write_page(0, &page0).unwrap();
        backend.write_page(1, &fact_pages[0]).unwrap();
        let inspection = backend.clone();

        let result = PersistentFactStorage::new(backend, 256);
        assert!(
            result.is_err(),
            "a corrupt v5 root must not shorten the fact range to zero"
        );
        assert_eq!(
            inspection.read_page(0).unwrap(),
            page0,
            "failed v5 migration must keep v5 page 0 authoritative"
        );
        assert_eq!(inspection.read_page(1).unwrap(), fact_pages[0]);
    }

    #[test]
    fn corrupt_v5_packed_base_rejects_migration_without_publishing() {
        let mut backend = MemoryBackend::new();
        let mut page0 = vec![0u8; PAGE_SIZE];
        page0[0..4].copy_from_slice(b"MGRF");
        page0[4..8].copy_from_slice(&5u32.to_le_bytes());
        page0[8..16].copy_from_slice(&2u64.to_le_bytes());
        page0[68] = FACT_PAGE_FORMAT_PACKED;
        let mut fact_page = vec![0u8; PAGE_SIZE];
        fact_page[0] = crate::storage::packed_pages::PAGE_TYPE_PACKED;
        page0[64..68].copy_from_slice(&crc32fast::hash(&fact_page).to_le_bytes());
        fact_page[PAGE_SIZE - 1] ^= 0x01;
        backend.write_page(0, &page0).unwrap();
        backend.write_page(1, &fact_page).unwrap();
        let inspection = backend.clone();

        let result = PersistentFactStorage::new(backend, 256);
        assert!(result.is_err(), "corrupt v5 base must reject migration");
        assert_eq!(
            FileHeader::from_bytes(&inspection.read_page(0).unwrap())
                .unwrap()
                .version,
            5,
            "failed v5 migration must leave page 0 on v5"
        );
    }

    #[test]
    fn test_v6_migration_crash_safe_unit() {
        let mut backend = MemoryBackend::new();
        let mut page = vec![0u8; PAGE_SIZE];
        page[0..4].copy_from_slice(b"MGRF");
        page[4..8].copy_from_slice(&5u32.to_le_bytes());
        page[8..16].copy_from_slice(&1u64.to_le_bytes()); // page_count = 1
        page[68] = 0x02;
        backend.write_page(0, &page).unwrap();
        backend.write_page(1, &vec![0xFF_u8; PAGE_SIZE]).unwrap();
        backend.write_page(2, &vec![0xFF_u8; PAGE_SIZE]).unwrap();

        let s = PersistentFactStorage::new(backend, 256).unwrap();
        let b = s.into_backend().unwrap();
        let header_bytes = b.read_page(0).unwrap();
        let header = crate::storage::FileHeader::from_bytes(&header_bytes).unwrap();
        assert_eq!(
            header.version,
            crate::storage::FORMAT_VERSION,
            "migration must complete despite prior partial run"
        );
    }

    /// Regression test for fuzz-discovered timeout (artifact:
    /// `timeout-23b2b7e0aa43d92c12c49f123a48d6f4ace6ce33`).
    ///
    /// A crafted v5 header with page_count=3_604_123_350 and vaet_root_page=61
    /// caused migrate_v5_to_v6 to use page_count as the B-tree start page.
    /// build_btree then wrote a leaf at offset ~14 TB in a sparse file, and
    /// compute_page_checksum looped over 3.6 billion zero-filled sparse pages
    /// (each read_exact returns zeros, no error), hanging the process.
    ///
    /// The malformed file must now fail closed in constant time without
    /// rewriting page 0 or manufacturing an empty current-format database.
    #[test]
    fn test_v5_migration_large_page_count_does_not_hang() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Write the exact fuzz-artifact header bytes (75 bytes + zero padding to 4096)
        let mut page = vec![0u8; PAGE_SIZE];
        page[0..4].copy_from_slice(b"MGRF");
        page[4..8].copy_from_slice(&5u32.to_le_bytes()); // version = 5
        page[8..16].copy_from_slice(&3_604_123_350u64.to_le_bytes()); // page_count (huge)
        page[56..64].copy_from_slice(&61u64.to_le_bytes()); // vaet_root_page = 61
        page[68] = 0x20; // fact_page_format
        std::fs::write(&path, &page).unwrap();

        let before = std::fs::read(&path).unwrap();
        let started = Instant::now();
        let result = FileBackend::open(&path)
            .and_then(|backend| PersistentFactStorage::new(backend, 256).map(|_| ()));
        assert!(
            result.is_err(),
            "missing v5 fact pages must fail instead of publishing data loss"
        );
        assert!(
            started.elapsed().as_secs() < 2,
            "crafted page count must fail without a multi-billion-page scan"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "failed migration must leave the v5 image byte-exact"
        );
    }

    #[test]
    fn test_header_checksum_computation() {
        use crate::storage::FileHeader;

        let mut header = FileHeader::new();
        header.page_count = 10;
        header.node_count = 5;

        let checksum = compute_header_checksum(&header);
        assert_ne!(checksum, 0, "checksum must be non-zero");

        let mut header2 = FileHeader::new();
        header2.page_count = 10;
        header2.node_count = 5;
        assert_eq!(compute_header_checksum(&header2), checksum);

        let mut header3 = FileHeader::new();
        header3.page_count = 11;
        assert_ne!(compute_header_checksum(&header3), checksum);
    }

    #[test]
    fn test_header_checksum_corruption_detection() {
        use crate::storage::{FORMAT_VERSION, FileHeader};

        let mut header = FileHeader::new();
        header.version = FORMAT_VERSION;
        let valid_checksum = compute_header_checksum(&header);
        header.header_checksum = valid_checksum;

        header.page_count = 999;

        let computed = compute_header_checksum(&header);
        assert_ne!(computed, header.header_checksum);
    }

    #[test]
    fn test_save_with_valid_header_read() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;
        use uuid::Uuid;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();

        {
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            pfs.storage()
                .transact(
                    vec![(
                        alice,
                        ":name".to_string(),
                        Value::String("Alice".to_string()),
                    )],
                    None,
                )
                .unwrap();
            pfs.dirty = true;
            pfs.save().unwrap();
        }

        {
            let pfs = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            let facts = pfs.storage().get_facts_by_entity(&alice).unwrap();
            assert_eq!(facts.len(), 1, "should load facts from existing file");
        }
    }

    #[test]
    fn test_save_fails_on_corrupted_header() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().expect("valid path").to_string();
        drop(tmp);

        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            file.write_all(&vec![0u8; PAGE_SIZE]).unwrap();
            file.write_all(&vec![0u8; PAGE_SIZE]).unwrap();
        }

        let result = FileBackend::open(&path);
        assert!(
            result.is_err(),
            "should fail on corrupted header in existing file"
        );
    }

    #[test]
    fn test_is_new_returns_correct_value() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().expect("valid path").to_string();
        drop(tmp);

        let backend = FileBackend::open(&path).unwrap();
        assert!(backend.is_new(), "newly created file should be new");
        drop(backend);

        let backend = FileBackend::open(&path).unwrap();
        assert!(!backend.is_new(), "reopened file should not be new");
        drop(backend);
    }

    // ══ #214 fault-injection unit tests ══════════════════════════════════

    mod fault_injection_tests {
        use super::*;
        use crate::storage::backend::MemoryBackend;
        use crate::storage::backend::fault_inject::{FaultConfig, FaultInjectingBackend};

        fn make_pfs_with_config() -> (
            PersistentFactStorage<FaultInjectingBackend<MemoryBackend>>,
            std::sync::Arc<std::sync::Mutex<FaultConfig>>,
        ) {
            let (backend, config) = FaultInjectingBackend::with_config(MemoryBackend::new());
            let pfs = PersistentFactStorage::new(backend, 16).unwrap();
            (pfs, config)
        }

        fn stage_fact(pfs: &mut PersistentFactStorage<FaultInjectingBackend<MemoryBackend>>) {
            let entity = Uuid::new_v4();
            pfs.storage()
                .transact(
                    vec![(entity, ":test/attr".to_string(), Value::Boolean(true))],
                    None,
                )
                .unwrap();
            pfs.mark_dirty();
        }

        #[test]
        fn save_returns_error_when_write_fails() {
            let (mut pfs, config) = make_pfs_with_config();
            stage_fact(&mut pfs);
            config.lock().unwrap().fail_write_after = Some(0);
            let result = pfs.save();
            assert!(
                result.is_err(),
                "save must return Err when write_page fails"
            );
        }

        #[test]
        fn save_returns_error_when_sync_fails() {
            let (mut pfs, config) = make_pfs_with_config();
            stage_fact(&mut pfs);
            config.lock().unwrap().fail_sync_after = Some(0);
            let result = pfs.save();
            assert!(result.is_err(), "save must return Err when sync fails");
        }

        #[test]
        fn save_error_message_is_non_empty() {
            let (mut pfs, config) = make_pfs_with_config();
            stage_fact(&mut pfs);
            config.lock().unwrap().fail_sync_after = Some(0);
            let err = pfs.save().unwrap_err();
            assert!(
                !err.to_string().is_empty(),
                "error message must not be empty"
            );
        }
    }

    // ── A2: for_each_fact_since — tail reads without a committed full scan ──
    mod for_each_fact_since {
        use super::*;
        use crate::graph::types::Value;
        use crate::storage::StorageBackend;
        use crate::storage::backend::FileBackend;
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        use tempfile::NamedTempFile;
        use uuid::Uuid;

        /// Delegating backend that counts `read_page` calls, so tests can
        /// prove the since path touches only a bounded page set.
        struct CountingBackend {
            inner: FileBackend,
            reads: Arc<AtomicU64>,
        }

        impl StorageBackend for CountingBackend {
            fn write_page(&mut self, page_id: u64, data: &[u8]) -> anyhow::Result<()> {
                self.inner.write_page(page_id, data)
            }
            fn read_page(&self, page_id: u64) -> anyhow::Result<Vec<u8>> {
                self.reads.fetch_add(1, AtomicOrdering::SeqCst);
                self.inner.read_page(page_id)
            }
            fn sync(&mut self) -> anyhow::Result<()> {
                self.inner.sync()
            }
            fn page_count(&self) -> anyhow::Result<u64> {
                self.inner.page_count()
            }
            fn close(&mut self) -> anyhow::Result<()> {
                self.inner.close()
            }
            fn backend_name(&self) -> &'static str {
                "counting-file"
            }
            fn is_new(&self) -> bool {
                self.inner.is_new()
            }
        }

        /// Write `n_tx` single-fact transactions and checkpoint them into the
        /// committed base, returning the temp file handle and head tx_count.
        fn build_committed_base(n_tx: u64) -> (NamedTempFile, u64) {
            let tmp = NamedTempFile::new().unwrap();
            let path = tmp.path().to_str().unwrap().to_string();
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            let entity = Uuid::new_v4();
            for i in 0..n_tx {
                pfs.storage()
                    .transact(
                        vec![(
                            entity,
                            ":a2/seq".to_string(),
                            Value::Integer(i64::try_from(i).unwrap()),
                        )],
                        None,
                    )
                    .unwrap();
            }
            pfs.mark_dirty();
            pfs.save().unwrap();
            let head = pfs.storage().current_tx_count();
            (tmp, head)
        }

        fn open_counting(path: &str) -> (PersistentFactStorage<CountingBackend>, Arc<AtomicU64>) {
            let reads = Arc::new(AtomicU64::new(0));
            let backend = CountingBackend {
                inner: FileBackend::open(path).unwrap(),
                reads: reads.clone(),
            };
            let pfs = PersistentFactStorage::new(backend, 256).unwrap();
            (pfs, reads)
        }

        #[test]
        fn since_tail_reads_far_fewer_pages_than_full_scan() {
            // ~3000 facts ≈ 120 packed pages in the committed base.
            let (tmp, head) = build_committed_base(3000);
            let path = tmp.path().to_str().unwrap().to_string();

            // Cold open #1 — full scan page reads as the baseline.
            let (full_pfs, full_reads) = open_counting(&path);
            let before_full = full_reads.load(AtomicOrdering::SeqCst);
            let mut full_count = 0u64;
            full_pfs
                .storage()
                .for_each_fact(|_| {
                    full_count = full_count.saturating_add(1);
                    Ok(())
                })
                .unwrap();
            let full_scan_reads = full_reads
                .load(AtomicOrdering::SeqCst)
                .saturating_sub(before_full);
            assert_eq!(full_count, 3000, "baseline full scan must see every fact");
            drop(full_pfs);

            // Cold open #2 — a 2-transaction tail must not replay the base.
            let (tail_pfs, tail_reads) = open_counting(&path);
            let before_tail = tail_reads.load(AtomicOrdering::SeqCst);
            let mut tail = Vec::new();
            tail_pfs
                .storage()
                .for_each_fact_since(head.saturating_sub(2), |fact| {
                    tail.push(fact);
                    Ok(())
                })
                .unwrap();
            let tail_scan_reads = tail_reads
                .load(AtomicOrdering::SeqCst)
                .saturating_sub(before_tail);

            assert_eq!(tail.len(), 2, "tail must contain exactly the last 2 txs");
            assert!(
                tail.iter().all(|f| f.tx_count > head - 2),
                "tail must only contain records past the cursor"
            );
            assert!(
                full_scan_reads >= 20,
                "baseline full scan should touch the whole base, got {full_scan_reads} reads"
            );
            // Probe cost is O(log pages) + the tail pages; anything close to
            // the full-scan read count means the base was replayed.
            assert!(
                tail_scan_reads <= full_scan_reads / 2,
                "since-tail must not replay the base: {tail_scan_reads} reads vs full scan {full_scan_reads}"
            );
            assert!(
                tail_scan_reads <= 15,
                "since-tail must read only probe + tail pages, got {tail_scan_reads} (full scan: {full_scan_reads})"
            );
        }

        #[test]
        fn since_matches_filtered_full_export_across_layers() {
            // Base (first save) + delta segment (second save) + pending.
            let tmp = NamedTempFile::new().unwrap();
            let path = tmp.path().to_str().unwrap().to_string();
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            let entity = Uuid::new_v4();
            let tx = |pfs: &mut PersistentFactStorage<FileBackend>, i: i64| {
                pfs.storage()
                    .transact(
                        vec![(entity, ":a2/layer".to_string(), Value::Integer(i))],
                        None,
                    )
                    .unwrap();
            };
            for i in 0..40 {
                tx(&mut pfs, i);
            }
            pfs.mark_dirty();
            assert!(matches!(
                pfs.save().unwrap(),
                CheckpointOutcome::FullRebuild
            ));
            for i in 40..60 {
                tx(&mut pfs, i);
            }
            pfs.mark_dirty();
            assert!(matches!(
                pfs.save().unwrap(),
                CheckpointOutcome::DeltaSegment
            ));
            for i in 60..70 {
                tx(&mut pfs, i);
            }

            let mut full = Vec::new();
            pfs.storage()
                .for_each_fact(|fact| {
                    full.push(fact);
                    Ok(())
                })
                .unwrap();
            assert_eq!(full.len(), 70, "all three layers must be visible");

            let head = pfs.storage().current_tx_count();
            for since in [0, 1, 39, 40, 41, 59, 60, 65, head, head + 5] {
                let mut got = Vec::new();
                pfs.storage()
                    .for_each_fact_since(since, |fact| {
                        got.push(fact);
                        Ok(())
                    })
                    .unwrap();
                let expected: Vec<_> = full
                    .iter()
                    .filter(|f| f.tx_count > since)
                    .cloned()
                    .collect();
                assert_eq!(
                    got.len(),
                    expected.len(),
                    "since={since} tail length must match filtered full scan"
                );
                let matches = got
                    .iter()
                    .zip(expected.iter())
                    .filter(|(a, b)| a == b)
                    .count();
                assert_eq!(
                    matches,
                    expected.len(),
                    "since={since} tail must be the exact ordered subsequence"
                );
            }
        }

        #[test]
        fn committed_base_pages_are_tx_nondecreasing() {
            // The page probe in for_each_fact_since relies on packed fact
            // pages holding facts in nondecreasing tx_count order. Exercise
            // base build, delta append, and recompact, then verify the
            // storage-order stream never goes backwards.
            let tmp = NamedTempFile::new().unwrap();
            let path = tmp.path().to_str().unwrap().to_string();
            let mut pfs =
                PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            let entity = Uuid::new_v4();
            for round in 0..4 {
                for i in 0..30 {
                    pfs.storage()
                        .transact(
                            vec![(
                                entity,
                                ":a2/mono".to_string(),
                                Value::Integer(round * 100 + i),
                            )],
                            None,
                        )
                        .unwrap();
                }
                pfs.mark_dirty();
                pfs.save().unwrap();
            }
            pfs.recompact_visible_delta().unwrap();

            let mut last_tx = 0u64;
            let mut seen = 0u64;
            pfs.storage()
                .for_each_fact(|fact| {
                    assert!(
                        fact.tx_count >= last_tx,
                        "storage order must be tx-nondecreasing"
                    );
                    last_tx = fact.tx_count;
                    seen = seen.saturating_add(1);
                    Ok(())
                })
                .unwrap();
            assert_eq!(seen, 120, "recompacted stream must keep every fact");
        }
    }
}
