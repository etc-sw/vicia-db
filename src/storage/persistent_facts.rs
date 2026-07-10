use crate::graph::FactStorage;
/// Persistent fact storage that integrates StorageBackend with Datalog facts.
///
/// This module bridges the gap between high-level fact operations and
/// low-level page-based storage backends.
use crate::graph::types::{Fact, RETRACT_ALL_VALID_FROM, VALID_TIME_FOREVER, Value};
use crate::storage::FACT_PAGE_FORMAT_PACKED;
use crate::storage::btree_v6::{
    MutexStorageBackend, OnDiskIndexReader, btree_entries, build_btree, merge_sorted_vecs,
    stream_all_entries,
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
    HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName, HeaderManifestSlotRecoveryReason,
    HeaderManifestSlotSelection, build_header_page, build_header_page_with_extension,
    select_header_manifest_slot_from_page0,
};
use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, VaetKey, encode_value};
use crate::storage::packed_pages::{PackedFactPacker, pack_facts};
use crate::storage::{
    CommittedFactReader, CommittedIndexReader, FileHeader, PAGE_SIZE, StorageBackend,
};
use anyhow::Result;
use crc32fast::Hasher;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn normalize_legacy_retractions(facts: &mut [Fact]) {
    for fact in facts {
        if !fact.asserted {
            fact.valid_from = RETRACT_ALL_VALID_FROM;
            fact.valid_to = VALID_TIME_FOREVER;
        }
    }
}

/// Compute the CRC32 sync checksum over all facts (used in tests only).
///
/// Sorts facts by `(tx_count, entity_bytes, attribute)` before hashing to
/// produce a stable total order independent of Vec insertion order.
#[cfg(all(test, not(target_arch = "wasm32")))]
fn compute_index_checksum(facts: &[Fact]) -> u32 {
    let mut sorted: Vec<&Fact> = facts.iter().collect();
    sorted.sort_by(|a, b| {
        a.tx_count
            .cmp(&b.tx_count)
            .then_with(|| a.entity.as_bytes().cmp(b.entity.as_bytes()))
            .then_with(|| a.attribute.as_str().cmp(b.attribute.as_str()))
    });
    let mut hasher = Hasher::new();
    for fact in sorted {
        let bytes = postcard::to_allocvec(fact)
            .expect("BUG: failed to serialize Fact for index checksum; this should never happen");
        hasher.update(&bytes);
    }
    hasher.finalize()
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
    backend: Arc<Mutex<B>>,
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
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        crate::storage::packed_pages::read_all_from_pages(&*backend, first_fact_page, n)
    }

    fn for_each_fact(
        &self,
        visit: &mut dyn FnMut(crate::graph::types::Fact) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let n = self.committed_fact_pages.load(Ordering::SeqCst);
        let first_fact_page = self.committed_fact_page_start.load(Ordering::SeqCst);
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        crate::storage::packed_pages::for_each_from_pages(&*backend, first_fact_page, n, visit)
    }

    fn committed_page_count(&self) -> u64 {
        self.committed_fact_pages.load(Ordering::SeqCst)
    }
}

struct LayeredFactLoaderImpl {
    base: Arc<dyn CommittedFactReader>,
    delta_facts: BTreeMap<FactRef, Fact>,
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
}

impl LayeredFactLoaderImpl {
    fn new(base: Arc<dyn CommittedFactReader>, segments: &[DeltaSegment]) -> Self {
        let mut delta_facts = BTreeMap::new();
        for segment in segments {
            for (fact_ref, fact) in segment.payload().facts() {
                delta_facts.insert(*fact_ref, fact.clone());
            }
        }
        Self { base, delta_facts }
    }
}

impl CommittedFactReader for LayeredFactLoaderImpl {
    fn resolve(&self, fact_ref: FactRef) -> anyhow::Result<Fact> {
        if let Some(fact) = self.delta_facts.get(&fact_ref) {
            return Ok(fact.clone());
        }
        self.base.resolve(fact_ref)
    }

    fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
        let mut facts = self.base.stream_all()?;
        facts.extend(self.delta_facts.values().cloned());
        Ok(facts)
    }

    fn for_each_fact(
        &self,
        visit: &mut dyn FnMut(Fact) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.base.for_each_fact(visit)?;
        for fact in self.delta_facts.values().cloned() {
            visit(fact)?;
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
/// - Page 0: File header (metadata)
/// - Page 1+: Serialized facts (one fact per page, for simplicity)
///
/// # Storage Strategy (Phase 3-5)
///
/// Current implementation uses a simple "load all, save all" approach:
/// - On open: Deserialize all facts into memory (FactStorage)
/// - All operations: Work on in-memory `Vec<Fact>`
/// - On save: Serialize all facts back to disk
///
/// **Trade-offs:**
/// - ✅ Simple, correct, easy to reason about
/// - ✅ Fast queries (no disk I/O)
/// - ✅ Good for embedded use cases with small-medium datasets
/// - ❌ Memory usage = entire database size
/// - ❌ Not scalable to very large datasets
///
/// **Scalability:**
/// - Works well for <100K facts (typical use case)
/// - Memory footprint: ~100-200 bytes per fact
/// - Example: 100K facts ≈ 10-20MB memory (acceptable for embedded)
///
/// # Future: Phase 6 (Performance)
///
/// Phase 6 will introduce page-based access with indexes:
/// - EAVT, AEVT, AVET, VAET indexes (in-memory B-trees)
/// - On-demand fact loading from disk
/// - LRU cache for hot pages
/// - Memory-mapped file access (optional)
/// - Target: Scale to millions of facts with bounded memory
///
/// The page-based backend (StorageBackend) is designed to support this
/// future architecture without breaking changes.
pub struct PersistentFactStorage<B: StorageBackend + 'static> {
    backend: Arc<Mutex<B>>,
    page_cache: Arc<PageCache>,
    storage: FactStorage,
    dirty: bool,
    last_checkpointed_tx_count: u64,
    header_manifest_selection: HeaderManifestSlotSelection,
    delta_manifest_selection: PersistedManifestSelection,
    committed_fact_pages: Arc<AtomicU64>,
    committed_fact_page_start: Arc<AtomicU64>,
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
            committed_fact_pages,
            committed_fact_page_start,
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

    fn base_fact_loader(&self) -> Arc<dyn CommittedFactReader> {
        Arc::new(CommittedFactLoaderImpl {
            backend: self.backend.clone(),
            backend_adapter: MutexStorageBackend(self.backend.clone()),
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
        let base_loader = self.base_fact_loader();
        let fact_reader: Arc<dyn CommittedFactReader> = if delta_segments.is_empty() {
            base_loader
        } else {
            Arc::new(LayeredFactLoaderImpl::new(
                base_loader,
                delta_segments.as_slice(),
            ))
        };
        self.storage.set_committed_reader(fact_reader);

        if header.eavt_root_page == 0 {
            return Ok(());
        }

        let base_index_reader = Arc::new(OnDiskIndexReader::new(
            self.backend.clone(),
            self.page_cache.clone(),
            header.eavt_root_page,
            header.aevt_root_page,
            header.avet_root_page,
            header.vaet_root_page,
        ));
        let index_reader: Arc<dyn CommittedIndexReader> = if delta_segments.is_empty() {
            base_index_reader
        } else {
            let mut eavt = Vec::new();
            let mut aevt = Vec::new();
            let mut avet = Vec::new();
            let mut vaet = Vec::new();
            for segment in &delta_segments {
                let payload = segment.payload();
                eavt.extend(payload.eavt.iter().cloned());
                aevt.extend(payload.aevt.iter().cloned());
                avet.extend(payload.avet.iter().cloned());
                vaet.extend(payload.vaet.iter().cloned());
            }
            let base_keyed_reader: Arc<dyn KeyedIndexReader> = base_index_reader;
            Arc::new(LayeredIndexReader::new(
                base_keyed_reader,
                DeltaIndexEntries::from_entries(eavt, aevt, avet, vaet),
            ))
        };
        self.storage.set_committed_index_reader(index_reader);
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
                    .with_base_fact_page_start(extension.base_fact_page_start())?,
                HeaderManifestSlotName::Secondary,
            )),
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Secondary,
                ..
            } => Ok((
                HeaderExtension::new(descriptor, extension.secondary())
                    .with_base_fact_page_start(extension.base_fact_page_start())?,
                HeaderManifestSlotName::Primary,
            )),
            HeaderManifestSlotSelection::NoDeltaManifest
            | HeaderManifestSlotSelection::RecoveryRequired { .. } => Ok((
                HeaderExtension::new(descriptor, HeaderManifestSlot::empty())
                    .with_base_fact_page_start(extension.base_fact_page_start())?,
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

    #[allow(dead_code)]
    fn save_full_rebuild_from_visible_facts(&mut self) -> Result<CheckpointOutcome> {
        let all_facts = self.storage.get_all_facts()?;
        let (fact_pages, fact_refs) = pack_facts(&all_facts, 1)?;
        let num_fact_pages = u64::try_from(fact_pages.len())
            .map_err(|_| anyhow::anyhow!("fact page count exceeds u64::MAX"))?;
        let (eavt_entries, aevt_entries, avet_entries, vaet_entries) =
            build_sorted_index_entries(&all_facts, &fact_refs);
        let node_count = u64::try_from(all_facts.len())
            .map_err(|_| anyhow::anyhow!("fact count exceeds u64::MAX"))?;
        let checkpoint_tx_count = self.storage.current_tx_count();

        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        self.page_cache.invalidate_from(1);
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
            .ok_or_else(|| anyhow::anyhow!("page count overflow computing index_start"))?;
        let (eavt_root, next1) = build_btree(
            btree_entries(eavt_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            index_start,
        )?;
        let (aevt_root, next2) = build_btree(
            btree_entries(aevt_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next1,
        )?;
        let (avet_root, next3) = build_btree(
            btree_entries(avet_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next2,
        )?;
        let (vaet_root, next4) = build_btree(
            btree_entries(vaet_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next3,
        )?;
        backend.sync()?;

        let total_data_pages = next4.saturating_sub(1);
        let checksum = compute_page_checksum(&*backend, 1, total_data_pages)?;
        let mut header = FileHeader::new();
        header.page_count = next4;
        header.node_count = node_count;
        header.last_checkpointed_tx_count = checkpoint_tx_count;
        header.eavt_root_page = eavt_root;
        header.aevt_root_page = aevt_root;
        header.avet_root_page = avet_root;
        header.vaet_root_page = vaet_root;
        header.index_checksum = checksum;
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        header.fact_page_count = num_fact_pages;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page(header)?;
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
        self.last_checkpointed_tx_count = checkpoint_tx_count;
        self.dirty = false;
        self.wire_committed_readers(&header, Vec::new())?;
        self.storage.post_checkpoint_clear();
        Ok(CheckpointOutcome::FullRebuildFromVisibleDelta)
    }

    fn write_recompact_candidate_from_visible_facts(&mut self) -> Result<RecompactCandidate> {
        let checkpoint_tx_count = self.storage.current_tx_count();

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
        if base_fact_page_start == 0 {
            anyhow::bail!("Recompact base candidate cannot start on page 0");
        }

        let mut packer = PackedFactPacker::new(base_fact_page_start);
        let mut index_entries = new_index_entries_with_capacity(0);
        let mut node_count = 0u64;
        self.storage.for_each_fact(|fact| {
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

        self.page_cache.invalidate_from(base_fact_page_start);
        for (i, page_data) in fact_pages.iter().enumerate() {
            let page_offset =
                u64::try_from(i).map_err(|_| anyhow::anyhow!("page index {i} exceeds u64::MAX"))?;
            let page_id = base_fact_page_start
                .checked_add(page_offset)
                .ok_or_else(|| anyhow::anyhow!("page id overflow writing recompact facts"))?;
            backend.write_page(page_id, page_data)?;
        }

        let index_start = base_fact_page_start
            .checked_add(num_fact_pages)
            .ok_or_else(|| {
                anyhow::anyhow!("page count overflow computing recompact index_start")
            })?;
        let (eavt_root, next1) = build_btree(
            btree_entries(eavt_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            index_start,
        )?;
        let (aevt_root, next2) = build_btree(
            btree_entries(aevt_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next1,
        )?;
        let (avet_root, next3) = build_btree(
            btree_entries(avet_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next2,
        )?;
        let (vaet_root, next4) = build_btree(
            btree_entries(vaet_entries.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next3,
        )?;

        backend.sync()?;

        let total_data_pages = next4.saturating_sub(base_fact_page_start);
        let checksum = compute_page_checksum(&*backend, base_fact_page_start, total_data_pages)?;
        let mut header = FileHeader::new();
        header.page_count = next4;
        header.node_count = node_count;
        header.last_checkpointed_tx_count = checkpoint_tx_count;
        header.eavt_root_page = eavt_root;
        header.aevt_root_page = aevt_root;
        header.avet_root_page = avet_root;
        header.vaet_root_page = vaet_root;
        header.index_checksum = checksum;
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        header.fact_page_count = num_fact_pages;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page_with_base_start(header, base_fact_page_start)?;
        Ok(RecompactCandidate {
            header,
            header_page,
            base_fact_page_start,
            fact_page_count: num_fact_pages,
            checkpoint_tx_count,
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
        backend.write_page(0, &candidate.header_page)?;
        backend.sync()?;
        drop(backend);

        self.header_manifest_selection = manifest_selection;
        self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
        self.committed_fact_pages
            .store(candidate.fact_page_count, Ordering::SeqCst);
        self.committed_fact_page_start
            .store(candidate.base_fact_page_start, Ordering::SeqCst);
        self.last_checkpointed_tx_count = candidate.checkpoint_tx_count;
        self.dirty = false;
        self.wire_committed_readers(&candidate.header, Vec::new())?;
        self.storage.post_checkpoint_clear();
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

        let base_fact_page_start =
            HeaderExtension::read_from_page0(header.version, &raw_header_bytes)?
                .map(|extension| extension.base_fact_page_start())
                .unwrap_or(1);
        if (base_fact_page_start == 0 || base_fact_page_start >= header.page_count.max(1))
            && header.fact_page_count > 0
        {
            anyhow::bail!("Header base fact page start is out of bounds");
        }
        self.committed_fact_page_start
            .store(base_fact_page_start, Ordering::SeqCst);

        let (header_manifest_selection, delta_manifest_selection, selected_delta_segments) = {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            Self::load_usable_delta_selection(&*backend, &header, &raw_header_bytes)?
        };
        self.header_manifest_selection = header_manifest_selection;
        if let PersistedManifestSelection::RecoveryRequired { reason } = delta_manifest_selection {
            let reason = match reason {
                PersistedManifestRecoveryReason::CorruptManifestSlot => "corrupt manifest slot",
                PersistedManifestRecoveryReason::NoValidManifest => "no valid manifest",
            };
            anyhow::bail!("Delta manifest recovery required: {reason}");
        }
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
            // Legacy one-per-page format (v4 or earlier): load all facts, then migrate to v5.
            self.load_one_per_page_legacy(&header)?;
            self.storage.restore_tx_counter()?;
            self.dirty = true;
            self.save()?;
            return Ok(());
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

        // Compute page-based checksum to verify data integrity.
        // New files (post-fix): checksum covers ALL pages (facts + indexes).
        // Old files (pre-fix): checksum covers only fact pages.
        // Try full checksum first; fall back to fact-only for backwards compat.
        let needs_format_upgrade = header.version < crate::storage::FORMAT_VERSION;
        let needs_rebuild = if selected_delta_has_segments {
            if needs_format_upgrade {
                anyhow::bail!("Delta manifest requires current v10 file format");
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
            let (eavt_root, next1) = build_btree(
                btree_entries(eavt_entries.into_iter())?.into_iter(),
                &mut *backend,
                &self.page_cache,
                index_start,
            )?;
            let (aevt_root, next2) = build_btree(
                btree_entries(aevt_entries.into_iter())?.into_iter(),
                &mut *backend,
                &self.page_cache,
                next1,
            )?;
            let (avet_root, next3) = build_btree(
                btree_entries(avet_entries.into_iter())?.into_iter(),
                &mut *backend,
                &self.page_cache,
                next2,
            )?;
            let (vaet_root, next4) = build_btree(
                btree_entries(vaet_entries.into_iter())?.into_iter(),
                &mut *backend,
                &self.page_cache,
                next3,
            )?;

            // Write header with full-coverage checksum (facts + indexes)
            let total_data_pages = next4.saturating_sub(1);
            let full_checksum = compute_page_checksum(&*backend, 1, total_data_pages)?;

            let mut new_header = FileHeader::new();
            new_header.page_count = next4;
            new_header.node_count = all_facts.len() as u64;
            new_header.last_checkpointed_tx_count = max_tx;
            new_header.eavt_root_page = eavt_root;
            new_header.aevt_root_page = aevt_root;
            new_header.avet_root_page = avet_root;
            new_header.vaet_root_page = vaet_root;
            new_header.index_checksum = full_checksum;
            new_header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
            new_header.fact_page_count = num_fact_pages;

            let write_checksum = compute_header_checksum(&new_header);
            new_header.header_checksum = write_checksum;

            let header_page = build_header_page(new_header)?;
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

            // Wire OnDiskIndexReader
            let index_reader: std::sync::Arc<dyn crate::storage::CommittedIndexReader> =
                std::sync::Arc::new(OnDiskIndexReader::new(
                    self.backend.clone(),
                    self.page_cache.clone(),
                    eavt_root,
                    aevt_root,
                    avet_root,
                    vaet_root,
                ));
            self.storage.set_committed_index_reader(index_reader);
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

    /// Load facts from legacy one-per-page format (v4 and earlier).
    fn load_one_per_page_legacy(&mut self, header: &FileHeader) -> Result<usize> {
        let page_count = header.page_count;
        let backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        let mut loaded: usize = 0;
        let mut skipped: usize = 0;
        for page_id in 1..page_count {
            let page = backend.read_page(page_id)?;
            // Try to deserialize a fact from this page (legacy format: raw postcard bytes)
            match postcard::from_bytes::<Fact>(&page) {
                Ok(mut fact) => {
                    if header.version < 9 && !fact.asserted {
                        fact.valid_from = RETRACT_ALL_VALID_FROM;
                        fact.valid_to = VALID_TIME_FOREVER;
                    }
                    self.storage.load_fact(fact)?;
                    loaded = loaded.saturating_add(1);
                }
                Err(e) => {
                    skipped = skipped.saturating_add(1);
                    eprintln!(
                        "Warning: failed to deserialize fact at page {}: {}. Skipping.",
                        page_id, e
                    );
                }
            }
        }
        if skipped > 0 {
            eprintln!(
                "Warning: {} facts failed to deserialize during legacy load (version {})",
                skipped, header.version
            );
        }
        Ok(loaded)
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
        let page_count = header.page_count;

        // Read all v1 facts (track deserialization failures)
        let mut v1_facts: Vec<FactV1> = Vec::new();
        let mut skipped: usize = 0;
        for page_id in 1..page_count {
            let page = backend.read_page(page_id)?;
            match postcard::from_bytes::<FactV1>(&page) {
                Ok(fact) => v1_facts.push(fact),
                Err(e) => {
                    skipped = skipped.saturating_add(1);
                    eprintln!(
                        "Warning: failed to deserialize v1 fact at page {}: {}. Skipping.",
                        page_id, e
                    );
                }
            }
        }
        if skipped > 0 {
            eprintln!(
                "Warning: {} v1 facts failed to deserialize during migration",
                skipped
            );
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

        self.storage.clear()?;
        for fact in migrated {
            self.storage.load_fact(fact)?;
        }
        self.storage.restore_tx_counter()?;

        // Persist in v2 format immediately
        self.dirty = true;
        self.save()?;
        Ok(())
    }

    /// Migrate a v5 file (paged-blob indexes) to v6 (on-disk B+tree indexes).
    fn migrate_v5_to_v6(&mut self, header: &FileHeader) -> Result<()> {
        let num_fact_pages = {
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

        // Validate the calculated range contains valid pages.
        // If the file was in an inconsistent state (partial checkpoint),
        // the calculated range might be incorrect. Do a quick validation
        // by checking that the first fact page can be read (doesn't need to
        // be a packed page - the index checksum will catch any real corruption).
        let validated_num_fact_pages = if num_fact_pages > 0 {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            // Just verify we can read the page - actual validation happens via checksum
            if backend.read_page(1).is_ok() {
                num_fact_pages
            } else {
                eprintln!(
                    "Warning: cannot read first fact page (page 1). Header claims {}. Using 0.",
                    num_fact_pages
                );
                0
            }
        } else {
            num_fact_pages
        };

        let mut all_facts = if validated_num_fact_pages > 0 {
            let backend = self
                .backend
                .lock()
                .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
            crate::storage::packed_pages::read_all_from_pages(
                &*backend,
                1,
                validated_num_fact_pages,
            )?
        } else {
            Vec::new()
        };
        normalize_legacy_retractions(&mut all_facts);

        let (fact_pages, fact_refs) = pack_facts(&all_facts, 1)?;
        let new_fact_page_count = u64::try_from(fact_pages.len())
            .map_err(|_| anyhow::anyhow!("fact page count exceeds u64::MAX"))?;
        self.committed_fact_pages
            .store(new_fact_page_count, Ordering::SeqCst);
        self.committed_fact_page_start.store(1, Ordering::SeqCst);
        let (eavt, aevt, avet, vaet) = build_sorted_index_entries(&all_facts, &fact_refs);
        let node_count = u64::try_from(all_facts.len())
            .map_err(|_| anyhow::anyhow!("fact count exceeds u64::MAX"))?;
        let max_tx = all_facts
            .iter()
            .map(|fact| fact.tx_count)
            .max()
            .unwrap_or(header.last_checkpointed_tx_count);

        let mut backend = self
            .backend
            .lock()
            .map_err(|_| anyhow::anyhow!("backend mutex poisoned"))?;
        self.page_cache.invalidate_from(1);
        for (i, page_data) in fact_pages.iter().enumerate() {
            let page_offset =
                u64::try_from(i).map_err(|_| anyhow::anyhow!("page index {i} exceeds u64::MAX"))?;
            let page_id = 1u64
                .checked_add(page_offset)
                .ok_or_else(|| anyhow::anyhow!("page id overflow writing fact pages"))?;
            backend.write_page(page_id, page_data)?;
        }
        // Use the actual end of fact pages as the start for new index pages, NOT
        // header.page_count — that field comes from the (possibly untrusted) file
        // on disk and may be a huge fuzz-crafted value that causes build_btree to
        // write leaf pages at a ~TB offset, then compute_page_checksum to loop
        // over billions of pages.
        let next_free = 1u64
            .checked_add(new_fact_page_count)
            .ok_or_else(|| anyhow::anyhow!("page count overflow computing next_free"))?;

        let (eavt_root, next_free2) = build_btree(
            btree_entries(eavt.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next_free,
        )?;
        let (aevt_root, next_free3) = build_btree(
            btree_entries(aevt.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next_free2,
        )?;
        let (avet_root, next_free4) = build_btree(
            btree_entries(avet.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next_free3,
        )?;
        let (vaet_root, final_next_free) = build_btree(
            btree_entries(vaet.into_iter())?.into_iter(),
            &mut *backend,
            &self.page_cache,
            next_free4,
        )?;

        let mut new_header = FileHeader::new(); // current format
        new_header.page_count = final_next_free;
        new_header.node_count = node_count;
        new_header.last_checkpointed_tx_count = max_tx;
        new_header.eavt_root_page = eavt_root;
        new_header.aevt_root_page = aevt_root;
        new_header.avet_root_page = avet_root;
        new_header.vaet_root_page = vaet_root;
        // Checksum over all data pages (facts + indexes)
        let total_data_pages = final_next_free.saturating_sub(1);
        let computed_checksum = compute_page_checksum(&*backend, 1, total_data_pages)?;
        new_header.index_checksum = computed_checksum;
        new_header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        new_header.fact_page_count = new_fact_page_count;
        new_header.header_checksum = compute_header_checksum(&new_header);

        let header_page = build_header_page(new_header)?;
        let manifest_selection =
            select_header_manifest_slot_from_page0(new_header.version, &header_page)?;
        backend.write_page(0, &header_page)?;
        backend.sync()?;
        drop(backend);

        self.header_manifest_selection = manifest_selection;
        self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
        self.last_checkpointed_tx_count = max_tx;

        let loader: Arc<dyn crate::storage::CommittedFactReader> =
            Arc::new(CommittedFactLoaderImpl {
                backend: self.backend.clone(),
                backend_adapter: MutexStorageBackend(self.backend.clone()),
                page_cache: self.page_cache.clone(),
                committed_fact_pages: self.committed_fact_pages.clone(),
                committed_fact_page_start: self.committed_fact_page_start.clone(),
            });
        self.storage.set_committed_reader(loader);

        let index_reader: Arc<dyn crate::storage::CommittedIndexReader> =
            Arc::new(OnDiskIndexReader::new(
                self.backend.clone(),
                self.page_cache.clone(),
                eavt_root,
                aevt_root,
                avet_root,
                vaet_root,
            ));
        self.storage.set_committed_index_reader(index_reader);

        self.storage.restore_tx_counter_from(max_tx);
        self.dirty = false;
        Ok(())
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
        if curr_header.version != crate::storage::FORMAT_VERSION
            || curr_header.fact_page_format != FACT_PAGE_FORMAT_PACKED
            || curr_header.eavt_root_page == 0
            || curr_header.aevt_root_page == 0
            || curr_header.avet_root_page == 0
            || curr_header.vaet_root_page == 0
        {
            return Ok(None);
        }

        let (base_identity, mut manifest_segments, mut visible_delta_segments) =
            if let Some(manifest) = selected_delta_manifest.as_ref() {
                (
                    manifest.base_identity(),
                    manifest.segments().to_vec(),
                    Self::load_delta_segments_from_manifest(&*backend, &curr_header, manifest)?,
                )
            } else {
                (
                    DeltaBaseIdentity::from_header(&curr_header),
                    Vec::new(),
                    Vec::new(),
                )
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
        visible_delta_segments.push(segment);
        self.wire_committed_readers(&header, visible_delta_segments)?;
        self.storage.post_checkpoint_clear();
        Ok(Some(CheckpointOutcome::DeltaSegment))
    }

    /// Save all facts from memory to the backend using packed pages and v6 on-disk B+tree indexes.
    pub fn save(&mut self) -> Result<CheckpointOutcome> {
        if !self.dirty {
            return Ok(CheckpointOutcome::Noop);
        }

        // ── Step A: read current header + stream old B+tree entries BEFORE overwriting ──
        let pending_facts = self.storage.get_pending_facts();
        if let Ok(Some(outcome)) = self.try_save_delta_segment(&pending_facts) {
            return Ok(outcome);
        }
        if matches!(
            self.delta_manifest_selection,
            PersistedManifestSelection::Use { .. }
        ) {
            let candidate = self.write_recompact_candidate_from_visible_facts()?;
            return self.publish_recompact_candidate(candidate);
        }

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

        // Stream committed B+tree entries BEFORE writing new pages that may overlap
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

        // Invalidate cached pages that will be overwritten (old index pages)
        self.page_cache.invalidate_from(new_fact_start);

        // ── Step B: pack pending facts as new appended pages ────────────────────
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

        // Sync fact pages to disk before building indexes on top of them.
        // Without this, a crash during index build could leave partially-flushed
        // fact pages that the old header's index roots would try to traverse.
        backend.sync()?;

        // ── Step C: build sorted index entries for pending facts ────────────────
        let (pending_eavt, pending_aevt, pending_avet, pending_vaet) =
            build_sorted_index_entries(&pending_facts, &new_fact_refs);

        // ── Step D: merge committed + pending entries, build new B+trees ─────────
        let index_start = base_fact_page_start
            .checked_add(new_total_fact_pages)
            .ok_or_else(|| anyhow::anyhow!("page count overflow computing index_start"))?;

        let eavt_ser = if !committed_eavt.is_empty() {
            btree_entries(merge_sorted_vecs(committed_eavt, pending_eavt))?
        } else {
            btree_entries(pending_eavt.into_iter())?
        };
        let (eavt_root, next1) = build_btree(
            eavt_ser.into_iter(),
            &mut *backend,
            &self.page_cache,
            index_start,
        )?;

        let aevt_ser = if !committed_aevt.is_empty() {
            btree_entries(merge_sorted_vecs(committed_aevt, pending_aevt))?
        } else {
            btree_entries(pending_aevt.into_iter())?
        };
        let (aevt_root, next2) =
            build_btree(aevt_ser.into_iter(), &mut *backend, &self.page_cache, next1)?;

        let avet_ser = if !committed_avet.is_empty() {
            btree_entries(merge_sorted_vecs(committed_avet, pending_avet))?
        } else {
            btree_entries(pending_avet.into_iter())?
        };
        let (avet_root, next3) =
            build_btree(avet_ser.into_iter(), &mut *backend, &self.page_cache, next2)?;

        let vaet_ser = if !committed_vaet.is_empty() {
            btree_entries(merge_sorted_vecs(committed_vaet, pending_vaet))?
        } else {
            btree_entries(pending_vaet.into_iter())?
        };
        let (vaet_root, next4) =
            build_btree(vaet_ser.into_iter(), &mut *backend, &self.page_cache, next3)?;

        // Sync index pages to disk before writing the header.
        // The header update is the atomic commit point: once it's durable,
        // recovery uses the new root pages. All data those roots reference
        // must already be on stable storage.
        backend.sync()?;

        // CRC32 over ALL data pages (facts + indexes), excluding page 0 (header).
        // This detects corruption in both fact pages and B+tree index pages.
        let total_data_pages = next4.saturating_sub(base_fact_page_start);
        let checksum = compute_page_checksum(&*backend, base_fact_page_start, total_data_pages)?;

        // ── Step E: write header (last write = crash-safe boundary) ─────────────
        let mut header = FileHeader::new(); // current format
        header.page_count = next4;
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
        header.index_checksum = checksum;
        header.fact_page_format = FACT_PAGE_FORMAT_PACKED;
        header.fact_page_count = new_total_fact_pages;
        header.header_checksum = compute_header_checksum(&header);

        let header_page = build_header_page_with_base_start(header, base_fact_page_start)?;
        let manifest_selection =
            select_header_manifest_slot_from_page0(header.version, &header_page)?;
        backend.write_page(0, &header_page)?;
        backend.sync()?;
        drop(backend);

        self.header_manifest_selection = manifest_selection;
        self.delta_manifest_selection = PersistedManifestSelection::NoDeltaManifest;
        self.committed_fact_pages
            .store(new_total_fact_pages, Ordering::SeqCst);
        self.committed_fact_page_start
            .store(base_fact_page_start, Ordering::SeqCst);
        self.last_checkpointed_tx_count = self.storage.current_tx_count();
        self.dirty = false;

        // ── Step F: wire CommittedFactReader and CommittedIndexReader ────────────
        let loader: Arc<dyn crate::storage::CommittedFactReader> =
            Arc::new(CommittedFactLoaderImpl {
                backend: self.backend.clone(),
                backend_adapter: MutexStorageBackend(self.backend.clone()),
                page_cache: self.page_cache.clone(),
                committed_fact_pages: self.committed_fact_pages.clone(),
                committed_fact_page_start: self.committed_fact_page_start.clone(),
            });
        self.storage.set_committed_reader(loader);

        let index_reader: Arc<dyn crate::storage::CommittedIndexReader> =
            Arc::new(OnDiskIndexReader::new(
                self.backend.clone(),
                self.page_cache.clone(),
                eavt_root,
                aevt_root,
                avet_root,
                vaet_root,
            ));
        self.storage.set_committed_index_reader(index_reader);

        // Clear pending — all data now on disk
        self.storage.post_checkpoint_clear();

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

    /// Run a closure with read access to the underlying storage backend.
    ///
    /// Used by the browser WASM layer to read pages after `save()` without
    /// exposing the `Arc<Mutex<B>>` directly.
    #[cfg(all(target_arch = "wasm32", feature = "browser"))]
    pub(crate) fn with_backend<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&B) -> R,
    {
        let guard = self.backend.lock().unwrap();
        f(&*guard)
    }

    /// Run a closure with mutable access to the underlying storage backend.
    ///
    /// Used by the browser WASM layer to drain dirty pages after `save()`.
    #[cfg(all(target_arch = "wasm32", feature = "browser"))]
    pub(crate) fn with_backend_mut<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut B) -> R,
    {
        let mut guard = self.backend.lock().unwrap();
        f(&mut *guard)
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

fn build_header_page_with_base_start(
    header: FileHeader,
    base_fact_page_start: u64,
) -> Result<Vec<u8>> {
    if header.version == crate::storage::FORMAT_VERSION {
        let extension = HeaderExtension::empty().with_base_fact_page_start(base_fact_page_start)?;
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
    fn test_save_writes_v4_header() {
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
    fn test_sync_check_detects_mismatch_and_rebuilds() {
        use crate::graph::types::Value;
        use crate::storage::StorageBackend;
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;
        use uuid::Uuid;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();

        // Write a database with 1 fact
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

        // Corrupt the index_checksum (bytes 64..68 of page 0), then recompute header_checksum
        {
            let mut backend = FileBackend::open(&path).unwrap();
            let mut page = backend.read_page(0).unwrap();
            page[64] ^= 0xFF;
            let new_header_checksum = compute_header_checksum_from_bytes(&page);
            page[80] = (new_header_checksum & 0xFF) as u8;
            page[81] = ((new_header_checksum >> 8) & 0xFF) as u8;
            page[82] = ((new_header_checksum >> 16) & 0xFF) as u8;
            page[83] = ((new_header_checksum >> 24) & 0xFF) as u8;
            backend.write_page(0, &page).unwrap();
            backend.sync().unwrap();
        }

        // Re-open — new() should detect mismatch, rebuild, and succeed
        {
            let pfs = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
            // v6: after rebuild, indexes are on disk; verify fact accessibility
            let alice_facts = pfs.storage().get_facts_by_entity(&alice).unwrap();
            assert_eq!(
                alice_facts.len(),
                1,
                "After rebuild, fact must be accessible via index"
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
        let c1 = compute_index_checksum(&facts);
        // Reversed order — same checksum (deterministic sort applied inside)
        let facts_reversed = vec![facts[1].clone(), facts[0].clone()];
        let c2 = compute_index_checksum(&facts_reversed);
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
    fn test_save_v5_checksum_stored() {
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
    fn test_v4_database_migrates_to_v5_on_open() {
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
            header.page_count = 2;
            header.node_count = 1;
            let mut hbytes = header.to_bytes();
            // Force version to 4
            hbytes[4..8].copy_from_slice(&4u32.to_le_bytes());
            // Force fact_page_format byte (offset 68) to 0
            hbytes[68] = 0;
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
    fn test_v5_load_fast_path_indexes_loaded() {
        use crate::storage::backend::FileBackend;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let alice = Uuid::new_v4();

        // Save in v5 format
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
    fn test_save_writes_v10_empty_header_extension() {
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
            .expect("v10 empty extension should decode");

        assert_eq!(header.version, crate::storage::FORMAT_VERSION);
        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
    }

    #[test]
    fn test_reopen_reads_v10_empty_extension_as_no_delta_manifest() {
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
    fn test_reopen_loads_selected_delta_manifest_from_v10_slot() {
        use crate::storage::backend::FileBackend;
        use crate::storage::delta_manifest::{
            DeltaManifest, PersistedManifestSelection, write_manifest_pages,
        };
        use crate::storage::header_extension::{
            HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName,
            build_header_page_with_extension,
        };
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        {
            let mut backend = FileBackend::open(&path).unwrap();
            let mut header = FileHeader::new();
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

        let reopened = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256).unwrap();
        assert!(matches!(
            reopened.delta_manifest_selection(),
            PersistedManifestSelection::Use {
                slot: HeaderManifestSlotName::Primary,
                manifest
            } if manifest.generation() == 11
        ));
    }

    #[test]
    fn test_v10_header_without_extension_rejected_on_load() {
        let mut backend = MemoryBackend::new();
        let mut header = FileHeader::new();
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

        let header = FileHeader::new();
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
        let mut header = header;
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

        let header = FileHeader::new();
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
        let mut header = header;
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

        let mut rebuild_storage =
            PersistentFactStorage::new(MemoryBackend::new(), 256).expect("storage should create");
        rebuild_storage
            .storage()
            .load_fact(base_fact)
            .expect("base fact should load");
        rebuild_storage.storage().restore_tx_counter_from(1);
        rebuild_storage.mark_dirty();
        rebuild_storage.save().expect("base checkpoint should save");
        rebuild_storage
            .storage()
            .load_fact(edge_fact)
            .expect("edge fact should load");
        rebuild_storage
            .storage()
            .load_fact(target_fact)
            .expect("target fact should load");
        rebuild_storage.storage().restore_tx_counter_from(2);
        rebuild_storage.mark_dirty();
        rebuild_storage
            .save_full_rebuild_from_visible_facts()
            .expect("full rebuild should save");

        assert_eq!(
            fact_projection(delta_storage.storage()).expect("delta facts should load"),
            fact_projection(rebuild_storage.storage()).expect("rebuild facts should load"),
            "delta checkpoint and full rebuild must expose the same fact identities"
        );
    }

    fn fact_projection(
        storage: &FactStorage,
    ) -> Result<Vec<(Uuid, String, Vec<u8>, i64, i64, u64, u64, bool)>> {
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
        // Write an empty fact page so page_count > 1 triggers load()
        backend.write_page(1, &vec![0u8; PAGE_SIZE]).unwrap();

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
    /// After the fix, next_free = 1 + validated_num_fact_pages (= 1 here),
    /// so migration completes in constant time and produces a sane current header.
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

        // Must complete without hanging; next_free must be computed from actual
        // fact pages (= 1 + 0 = 1), not from the crafted header.page_count.
        let s = PersistentFactStorage::new(FileBackend::open(&path).unwrap(), 256)
            .expect("migration must complete");
        let b = s.into_backend().unwrap();
        let header_bytes = b.read_page(0).unwrap();
        let header = crate::storage::FileHeader::from_bytes(&header_bytes).unwrap();
        assert_eq!(
            header.version,
            crate::storage::FORMAT_VERSION,
            "migration must upgrade to current format"
        );
        // Resulting page_count must be small (0 fact pages + 4 index pages + header),
        // NOT the crafted 3.6-billion-page value.
        assert!(
            header.page_count < 100,
            "page_count must be sane after migration"
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
}
