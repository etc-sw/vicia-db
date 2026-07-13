//! On-disk B+tree for covering index persistence (v6 base layout, v12 prefix leaves).
//!
//! Each node maps to exactly one 4KB page. The `PageCache` serves all reads.
//! `build_btree` does a bulk-build (write-all-leaves, then internal levels
//! bottom-up). Range scans traverse the tree through the cache.

use crate::storage::cache::PageCache;
use crate::storage::index::{
    AevtEntryWire, AevtKey, CurrentAevtEntryRef, CurrentEavtEntryRef, CurrentVaetEntryRef,
    EavtEntryWire, EavtKey, FactRef, VaetEntryWire, VaetKey,
};
use crate::storage::page_integrity::BasePageIntegrityCatalog;
use crate::storage::{PAGE_SIZE, StorageBackend};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::Mutex;

#[cfg(feature = "bench-internals")]
use std::cell::{Cell, RefCell};

/// Repository-only counters for the page-backed leaf read path.
#[cfg(feature = "bench-internals")]
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct LeafReadDiagnostics {
    pub internal_pages_visited: u64,
    pub leaf_pages_visited: u64,
    pub raw_leaf_pages_visited: u64,
    pub prefix_leaf_pages_visited: u64,
    pub leaf_entries_available: u64,
    pub leaf_entries_decoded: u64,
    pub leaf_entries_emitted: u64,
    pub leaf_entries_skipped: u64,
    pub lower_bound_key_comparisons: u64,
    pub prefix_restart_blocks_reconstructed: u64,
    pub prefix_entries_reconstructed: u64,
    pub reconstructed_key_bytes: u64,
    pub full_leaf_vec_peak_entries: u64,
    pub full_leaf_vec_peak_struct_bytes: u64,
    pub full_leaf_vec_peak_decoded_payload_bytes: u64,
    pub raw_decode_elapsed_ns: u64,
    pub prefix_decode_elapsed_ns: u64,
    pub early_stop_count: u64,
    pub leaf_boundary_transitions: u64,
    pub aevt_projection_decodes: u64,
    pub projected_aevt_emitted: u64,
    pub eavt_projection_decodes: u64,
    pub projected_eavt_emitted: u64,
    pub vaet_projection_decodes: u64,
    pub projected_vaet_emitted: u64,
    pub borrowed_attribute_bytes: u64,
    pub borrowed_value_bytes: u64,
    pub projected_owned_aevt_decodes: u64,
    pub projected_owned_eavt_decodes: u64,
    pub projected_owned_vaet_decodes: u64,
    pub resume_key_materializations: u64,
    pub resume_key_buffer_reuses: u64,
}

#[cfg(feature = "bench-internals")]
thread_local! {
    static PEAK_SERIALIZED_ENTRIES: Cell<u64> = const { Cell::new(0) };
    static PEAK_SERIALIZED_BYTES: Cell<u64> = const { Cell::new(0) };
    static LEAF_READ_DIAGNOSTICS_ENABLED: Cell<bool> = const { Cell::new(false) };
    static LEAF_READ_DIAGNOSTICS: RefCell<LeafReadDiagnostics> = const {
        RefCell::new(LeafReadDiagnostics {
            internal_pages_visited: 0,
            leaf_pages_visited: 0,
            raw_leaf_pages_visited: 0,
            prefix_leaf_pages_visited: 0,
            leaf_entries_available: 0,
            leaf_entries_decoded: 0,
            leaf_entries_emitted: 0,
            leaf_entries_skipped: 0,
            lower_bound_key_comparisons: 0,
            prefix_restart_blocks_reconstructed: 0,
            prefix_entries_reconstructed: 0,
            reconstructed_key_bytes: 0,
            full_leaf_vec_peak_entries: 0,
            full_leaf_vec_peak_struct_bytes: 0,
            full_leaf_vec_peak_decoded_payload_bytes: 0,
            raw_decode_elapsed_ns: 0,
            prefix_decode_elapsed_ns: 0,
            early_stop_count: 0,
            leaf_boundary_transitions: 0,
            aevt_projection_decodes: 0,
            projected_aevt_emitted: 0,
            eavt_projection_decodes: 0,
            projected_eavt_emitted: 0,
            vaet_projection_decodes: 0,
            projected_vaet_emitted: 0,
            borrowed_attribute_bytes: 0,
            borrowed_value_bytes: 0,
            projected_owned_aevt_decodes: 0,
            projected_owned_eavt_decodes: 0,
            projected_owned_vaet_decodes: 0,
            resume_key_materializations: 0,
            resume_key_buffer_reuses: 0,
        })
    };
}

#[cfg(feature = "bench-internals")]
pub(crate) fn reset_leaf_read_diagnostics() {
    LEAF_READ_DIAGNOSTICS.with(|slot| *slot.borrow_mut() = LeafReadDiagnostics::default());
}

#[cfg(feature = "bench-internals")]
pub(crate) fn leaf_read_diagnostics() -> LeafReadDiagnostics {
    LEAF_READ_DIAGNOSTICS.with(|slot| slot.borrow().clone())
}

#[cfg(feature = "bench-internals")]
pub(crate) fn note_resume_key(reused: bool) {
    update_leaf_read_diagnostics(|diagnostics| {
        if reused {
            diagnostics.resume_key_buffer_reuses =
                diagnostics.resume_key_buffer_reuses.saturating_add(1);
        } else {
            diagnostics.resume_key_materializations =
                diagnostics.resume_key_materializations.saturating_add(1);
        }
    });
}

#[cfg(not(feature = "bench-internals"))]
pub(crate) fn note_resume_key(_reused: bool) {}

#[cfg(feature = "bench-internals")]
pub(crate) fn set_leaf_read_diagnostics_enabled(enabled: bool) {
    LEAF_READ_DIAGNOSTICS_ENABLED.set(enabled);
}

#[cfg(feature = "bench-internals")]
#[inline(always)]
fn leaf_read_diagnostics_enabled() -> bool {
    LEAF_READ_DIAGNOSTICS_ENABLED.get()
}

#[cfg(feature = "bench-internals")]
#[inline(always)]
fn update_leaf_read_diagnostics(update: impl FnOnce(&mut LeafReadDiagnostics)) {
    if leaf_read_diagnostics_enabled() {
        LEAF_READ_DIAGNOSTICS.with(|slot| update(&mut slot.borrow_mut()));
    }
}

#[cfg(feature = "bench-internals")]
pub(crate) fn reset_build_diagnostics() {
    PEAK_SERIALIZED_ENTRIES.set(0);
    PEAK_SERIALIZED_BYTES.set(0);
}

#[cfg(feature = "bench-internals")]
pub(crate) fn build_diagnostics() -> (u64, u64) {
    (PEAK_SERIALIZED_ENTRIES.get(), PEAK_SERIALIZED_BYTES.get())
}

#[cfg(feature = "bench-internals")]
fn observe_serialized_frontier(entries: usize, bytes: usize) {
    PEAK_SERIALIZED_ENTRIES.set(
        PEAK_SERIALIZED_ENTRIES
            .get()
            .max(u64::try_from(entries).unwrap_or(u64::MAX)),
    );
    PEAK_SERIALIZED_BYTES.set(
        PEAK_SERIALIZED_BYTES
            .get()
            .max(u64::try_from(bytes).unwrap_or(u64::MAX)),
    );
}

#[cfg(not(feature = "bench-internals"))]
fn observe_serialized_frontier(_entries: usize, _bytes: usize) {}

// ─── Page type constants ───────────────────────────────────────────────────────

/// Leaf node page type (v6).
pub const PAGE_TYPE_LEAF: u8 = 0x21;
/// Internal node page type (v6).
pub const PAGE_TYPE_INTERNAL: u8 = 0x22;
/// Prefix-compressed leaf node page type (v12).
pub const PAGE_TYPE_PREFIX_LEAF: u8 = 0x23;

// ─── Fixed sizes ──────────────────────────────────────────────────────────────

/// Leaf page fixed header: type(1) + reserved(1) + entry_count(2) + next_leaf(8) = 12 bytes.
const LEAF_HEADER_SIZE: usize = 12;
/// Internal page fixed header: type(1) + reserved(1) + key_count(2) + rightmost_child(8) = 12 bytes.
const INTERNAL_HEADER_SIZE: usize = 12;
/// Slot directory entry: offset(u16) + length(u16) = 4 bytes.
const SLOT_SIZE: usize = 4;
/// Full serialized entries restart at this interval so corruption and future
/// point decoding stay bounded to a small page-local run.
const PREFIX_RESTART_INTERVAL: usize = 16;
/// shared-prefix length (u16) + decoded length (u16).
const PREFIX_RECORD_HEADER_SIZE: usize = 4;
/// Production bulk-build fill percentage. This changes packing policy only;
/// leaf encoding remains selected independently per page.
pub(crate) const DEFAULT_BTREE_FILL_PERCENT: u8 = 90;

#[derive(Clone, Copy)]
pub(crate) struct BtreeBuildOptions {
    fill_percent: u8,
}

impl BtreeBuildOptions {
    #[cfg_attr(not(any(test, feature = "bench-internals")), allow(dead_code))]
    pub(crate) fn new(fill_percent: u8) -> Result<Self> {
        if !(50..=100).contains(&fill_percent) {
            anyhow::bail!("B-tree fill percent must be between 50 and 100")
        }
        Ok(Self { fill_percent })
    }

    fn fill_bytes(self) -> usize {
        PAGE_SIZE.saturating_mul(usize::from(self.fill_percent)) / 100
    }
}

impl Default for BtreeBuildOptions {
    fn default() -> Self {
        Self {
            fill_percent: DEFAULT_BTREE_FILL_PERCENT,
        }
    }
}

// ─── Safe slice access helpers ───────────────────────────────────────────────

/// Read a u16 from 2 bytes at the given offset, returning an error if out of bounds.
fn read_u16_at(page: &[u8], offset: usize) -> Result<u16> {
    let bytes = page
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| anyhow!("out of bounds: read_u16 at {offset} (len {})", page.len()))?;
    Ok(u16::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| anyhow!("slice at {offset} not 2 bytes"))?,
    ))
}

/// Read a u64 from 8 bytes at the given offset, returning an error if out of bounds.
fn read_u64_at(page: &[u8], offset: usize) -> Result<u64> {
    let bytes = page
        .get(offset..offset.saturating_add(8))
        .ok_or_else(|| anyhow!("out of bounds: read_u64 at {offset} (len {})", page.len()))?;
    Ok(u64::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| anyhow!("slice at {offset} not 8 bytes"))?,
    ))
}

fn is_leaf_page_type(page_type: u8) -> bool {
    page_type == PAGE_TYPE_LEAF || page_type == PAGE_TYPE_PREFIX_LEAF
}

fn prefix_record_layout(
    previous: Option<&[u8]>,
    index: usize,
    entry: &[u8],
) -> Result<(u16, u16, usize)> {
    let shared = if index.is_multiple_of(PREFIX_RESTART_INTERVAL) {
        0
    } else {
        previous
            .unwrap_or_default()
            .iter()
            .zip(entry)
            .take_while(|(left, right)| left == right)
            .count()
    };
    let shared = u16::try_from(shared).map_err(|_| anyhow!("leaf prefix exceeds u16"))?;
    let decoded_len =
        u16::try_from(entry.len()).map_err(|_| anyhow!("leaf entry length exceeds u16"))?;
    let stored_len = PREFIX_RECORD_HEADER_SIZE
        .checked_add(entry.len().saturating_sub(usize::from(shared)))
        .ok_or_else(|| anyhow!("compressed leaf record length overflow"))?;
    Ok((shared, decoded_len, stored_len))
}

// ─── Low-level page writers ───────────────────────────────────────────────────

/// Write a single leaf page and insert it into the cache.
///
/// `entries`: each element is the postcard-serialised `(K, FactRef)` bytes for
/// one index entry, in sort order. Written end-to-start in the page.
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
fn write_leaf_page(
    backend: &mut dyn StorageBackend,
    cache: &PageCache,
    page_id: u64,
    entries: &[Vec<u8>],
    next_leaf: u64,
) -> Result<()> {
    let raw_bytes = entries.iter().map(Vec::len).sum::<usize>();
    let mut compressed_bytes = 0usize;
    let mut previous: Option<&[u8]> = None;
    for (index, entry) in entries.iter().enumerate() {
        let (_, _, stored_len) = prefix_record_layout(previous, index, entry)?;
        compressed_bytes = compressed_bytes
            .checked_add(stored_len)
            .ok_or_else(|| anyhow!("compressed leaf page length overflow"))?;
        previous = Some(entry);
    }
    let page_type = if compressed_bytes < raw_bytes {
        PAGE_TYPE_PREFIX_LEAF
    } else {
        PAGE_TYPE_LEAF
    };
    let entry_count =
        u16::try_from(entries.len()).map_err(|_| anyhow!("too many entries: {}", entries.len()))?;
    let mut page = vec![0u8; PAGE_SIZE];

    // Fixed header
    page[0] = page_type;
    page[1] = if page_type == PAGE_TYPE_PREFIX_LEAF {
        u8::try_from(PREFIX_RESTART_INTERVAL)?
    } else {
        0
    };
    page[2..4].copy_from_slice(&entry_count.to_le_bytes());
    page[4..12].copy_from_slice(&next_leaf.to_le_bytes());

    // Slot directory starts at byte 12; data written end-to-start
    let mut write_pos = PAGE_SIZE;
    let mut previous: Option<&[u8]> = None;
    for (i, entry) in entries.iter().enumerate() {
        let stored_len = if page_type == PAGE_TYPE_PREFIX_LEAF {
            let (shared, decoded_len, stored_len) = prefix_record_layout(previous, i, entry)?;
            write_pos -= stored_len;
            page[write_pos..write_pos + 2].copy_from_slice(&shared.to_le_bytes());
            page[write_pos + 2..write_pos + 4].copy_from_slice(&decoded_len.to_le_bytes());
            let suffix = entry
                .get(usize::from(shared)..)
                .ok_or_else(|| anyhow!("leaf prefix exceeds entry length"))?;
            page[write_pos + PREFIX_RECORD_HEADER_SIZE..write_pos + stored_len]
                .copy_from_slice(suffix);
            stored_len
        } else {
            write_pos -= entry.len();
            page[write_pos..write_pos + entry.len()].copy_from_slice(entry);
            entry.len()
        };
        previous = Some(entry);
        let slot_off = LEAF_HEADER_SIZE + i * SLOT_SIZE;
        let write_pos_u16 =
            u16::try_from(write_pos).map_err(|_| anyhow!("write_pos {write_pos} exceeds u16"))?;
        let entry_len_u16 =
            u16::try_from(stored_len).map_err(|_| anyhow!("entry len {stored_len} exceeds u16"))?;
        page[slot_off..slot_off + 2].copy_from_slice(&write_pos_u16.to_le_bytes());
        page[slot_off + 2..slot_off + 4].copy_from_slice(&entry_len_u16.to_le_bytes());
    }

    backend.write_page(page_id, &page)?;
    cache.put_dirty(page_id, page);
    Ok(())
}

/// Write a single internal node page and insert it into the cache.
///
/// `child_ids`: all child page IDs in order; the last one is `rightmost_child`.
/// `sep_bytes`: postcard-serialised Key bytes for each separator key.
///   `sep_bytes[j]` = first key of `child_ids[j+1]`'s subtree.
///   `sep_bytes.len()` == `child_ids.len() - 1`.
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
fn write_internal_page(
    backend: &mut dyn StorageBackend,
    cache: &PageCache,
    page_id: u64,
    child_ids: &[u64],
    sep_bytes: &[Vec<u8>],
) -> Result<()> {
    debug_assert_eq!(child_ids.len(), sep_bytes.len() + 1);
    // Defensive check: empty child_ids would cause panic on .last()
    if child_ids.is_empty() {
        anyhow::bail!("internal page has no children");
    }
    let key_count = u16::try_from(sep_bytes.len())
        .map_err(|_| anyhow!("too many sep keys: {}", sep_bytes.len()))?;
    let rightmost_child = *child_ids
        .last()
        .ok_or_else(|| anyhow!("child_ids is empty"))?;

    let mut page = vec![0u8; PAGE_SIZE];

    // Fixed header
    page[0] = PAGE_TYPE_INTERNAL;
    page[1] = 0; // reserved
    page[2..4].copy_from_slice(&key_count.to_le_bytes());
    page[4..12].copy_from_slice(&rightmost_child.to_le_bytes());

    // Child array: key_count entries starting at byte 12
    let child_arr_start = INTERNAL_HEADER_SIZE;
    for (i, &cid) in child_ids[..child_ids.len() - 1].iter().enumerate() {
        let off = child_arr_start + i * 8;
        page[off..off + 8].copy_from_slice(&cid.to_le_bytes());
    }

    // Slot directory for separator keys: after child array
    let slot_dir_start = INTERNAL_HEADER_SIZE + (key_count as usize) * 8;

    // Separator key data written end-to-start
    let mut write_pos = PAGE_SIZE;
    for (i, sep) in sep_bytes.iter().enumerate() {
        write_pos -= sep.len();
        page[write_pos..write_pos + sep.len()].copy_from_slice(sep);
        let slot_off = slot_dir_start + i * SLOT_SIZE;
        let write_pos_u16 =
            u16::try_from(write_pos).map_err(|_| anyhow!("write_pos {write_pos} exceeds u16"))?;
        let sep_len_u16 =
            u16::try_from(sep.len()).map_err(|_| anyhow!("sep len {} exceeds u16", sep.len()))?;
        page[slot_off..slot_off + 2].copy_from_slice(&write_pos_u16.to_le_bytes());
        page[slot_off + 2..slot_off + 4].copy_from_slice(&sep_len_u16.to_le_bytes());
    }

    backend.write_page(page_id, &page)?;
    cache.put_dirty(page_id, page);
    Ok(())
}

// ─── build_btree ──────────────────────────────────────────────────────────────

/// Serialize `(key, fact_ref)` pairs into the byte format expected by [`build_btree`].
///
/// Each item produces `(entry_bytes, key_bytes)` where:
/// - `entry_bytes` = postcard encoding of `(&key, &fact_ref)` — stored in leaf nodes
/// - `key_bytes`   = postcard encoding of `&key` alone — used as separator in internal nodes
///
/// Callers **must sort** entries before calling; this function preserves order.
/// Keeping serialisation in this small generic helper means `build_btree` itself
/// is monomorphised only once.
pub fn btree_entries<K: Serialize>(
    iter: impl Iterator<Item = (K, FactRef)>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    iter.map(|(key, fact_ref)| {
        let entry_bytes = postcard::to_allocvec(&(&key, &fact_ref))?;
        let key_bytes = postcard::to_allocvec(&key)?;
        Ok((entry_bytes, key_bytes))
    })
    .collect()
}

/// Build a B+tree from pre-serialised sorted entries and write it to the backend.
///
/// Each item in `sorted_entries` is `(entry_bytes, key_bytes)` as produced by
/// [`btree_entries`]. Entries **must already be sorted** by key.
///
/// Returns `(root_page_id, next_free_page_id)`. Chain multiple calls:
/// pass the returned `next_free_page_id` as `start_page_id` for the next index.
///
/// All written pages are inserted into `cache` via `put_dirty`.
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_btree(
    sorted_entries: impl Iterator<Item = (Vec<u8>, Vec<u8>)>,
    backend: &mut dyn StorageBackend,
    cache: &PageCache,
    start_page_id: u64,
) -> Result<(u64, u64)> {
    build_btree_serialized_results(
        sorted_entries.map(Ok),
        Ok,
        backend,
        cache,
        start_page_id,
        BtreeBuildOptions::default(),
    )
}

pub(crate) fn build_btree_with_options(
    sorted_entries: impl Iterator<Item = (Vec<u8>, Vec<u8>)>,
    backend: &mut dyn StorageBackend,
    cache: &PageCache,
    start_page_id: u64,
    options: BtreeBuildOptions,
) -> Result<(u64, u64)> {
    build_btree_serialized_results(
        sorted_entries.map(Ok),
        Ok,
        backend,
        cache,
        start_page_id,
        options,
    )
}

pub(crate) fn build_btree_from_key_entries<K: Serialize>(
    sorted_entries: impl Iterator<Item = (K, FactRef)>,
    backend: &mut dyn StorageBackend,
    cache: &PageCache,
    start_page_id: u64,
    options: BtreeBuildOptions,
) -> Result<(u64, u64)> {
    build_btree_serialized_results(
        sorted_entries.map(|(key, fact_ref)| {
            let entry_bytes = postcard::to_allocvec(&(&key, &fact_ref))?;
            Ok((entry_bytes, key))
        }),
        |key| postcard::to_allocvec(&key).map_err(Into::into),
        backend,
        cache,
        start_page_id,
        options,
    )
}

fn build_btree_serialized_results<K>(
    sorted_entries: impl Iterator<Item = Result<(Vec<u8>, K)>>,
    mut serialize_first_key: impl FnMut(K) -> Result<Vec<u8>>,
    backend: &mut dyn StorageBackend,
    cache: &PageCache,
    start_page_id: u64,
    options: BtreeBuildOptions,
) -> Result<(u64, u64)> {
    let page_fill_bytes = options.fill_bytes();
    // ── Phase 1: pack entries into leaf pages ─────────────────────────────────
    let mut leaf_infos: Vec<(u64, Vec<u8>)> = Vec::new();

    let mut cur_entries: Vec<Vec<u8>> = Vec::new();
    let mut cur_raw_bytes: usize = 0;
    let mut cur_prefix_bytes: usize = 0;
    let mut cur_first_key: Option<Vec<u8>> = None;
    let mut next_page = start_page_id;

    for entry in sorted_entries {
        let (entry_bytes, key) = entry?;
        let (_, _, prefix_entry_len) = prefix_record_layout(
            cur_entries.last().map(Vec::as_slice),
            cur_entries.len(),
            &entry_bytes,
        )?;
        let projected = LEAF_HEADER_SIZE
            + (cur_entries.len() + 1) * SLOT_SIZE
            + (cur_raw_bytes + entry_bytes.len()).min(cur_prefix_bytes + prefix_entry_len);

        if projected > PAGE_SIZE && cur_entries.is_empty() {
            anyhow::bail!("B-tree leaf entry does not fit in one page")
        }

        if projected > page_fill_bytes && !cur_entries.is_empty() {
            write_leaf_page(backend, cache, next_page, &cur_entries, 0)?;
            let first_key = cur_first_key.take().ok_or_else(|| {
                anyhow::anyhow!("BUG: cur_first_key empty when writing leaf page")
            })?;
            leaf_infos.push((next_page, first_key));
            next_page += 1;
            cur_entries.clear();
            cur_raw_bytes = 0;
            cur_prefix_bytes = 0;
            cur_first_key = None;
        }

        if cur_first_key.is_none() {
            cur_first_key = Some(serialize_first_key(key)?);
        }
        let (_, _, prefix_entry_len) = prefix_record_layout(
            cur_entries.last().map(Vec::as_slice),
            cur_entries.len(),
            &entry_bytes,
        )?;
        cur_raw_bytes += entry_bytes.len();
        cur_prefix_bytes += prefix_entry_len;
        cur_entries.push(entry_bytes);
        observe_serialized_frontier(cur_entries.len(), cur_raw_bytes.min(cur_prefix_bytes));
    }

    // Flush the last (or only) batch
    if cur_entries.is_empty() && leaf_infos.is_empty() {
        // Empty tree: single empty leaf
        write_leaf_page(backend, cache, next_page, &[], 0)?;
        return Ok((next_page, next_page + 1));
    }
    if !cur_entries.is_empty() {
        write_leaf_page(backend, cache, next_page, &cur_entries, 0)?;
        let first_key = cur_first_key.take().ok_or_else(|| {
            anyhow::anyhow!("BUG: cur_first_key empty when flushing last leaf page")
        })?;
        leaf_infos.push((next_page, first_key));
        next_page += 1;
    }

    // Patch next_leaf pointers: leaf[i].next_leaf = leaf[i+1].page_id
    for i in 0..leaf_infos.len() - 1 {
        let (pid, _) = leaf_infos
            .get(i)
            .ok_or_else(|| anyhow!("leaf_infos[{i}] out of bounds"))?
            .clone();
        let (next_lid, _) = leaf_infos
            .get(i + 1)
            .ok_or_else(|| anyhow!("leaf_infos[{}] out of bounds", i + 1))?
            .clone();
        let cached = cache.get_or_load(pid, backend)?;
        let mut page = (*cached).clone();
        // Safety: page is PAGE_SIZE bytes; offset 4..12 is always valid
        page.get_mut(4..12)
            .ok_or_else(|| anyhow!("page too small to write next_leaf"))?
            .copy_from_slice(&next_lid.to_le_bytes());
        backend.write_page(pid, &page)?;
        cache.put_dirty(pid, page);
    }

    // Single leaf: it is the root
    if leaf_infos.len() == 1 {
        return Ok((
            leaf_infos
                .first()
                .ok_or_else(|| anyhow!("leaf_infos unexpectedly empty"))?
                .0,
            next_page,
        ));
    }

    // ── Phase 2: build internal levels bottom-up ──────────────────────────────
    let mut current_level = leaf_infos;

    loop {
        if current_level.len() == 1 {
            return Ok((
                current_level
                    .first()
                    .ok_or_else(|| anyhow!("current_level unexpectedly empty"))?
                    .0,
                next_page,
            ));
        }

        let mut next_level: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut i = 0;

        while i < current_level.len() {
            let i_start = i;
            let first_entry = current_level
                .get(i)
                .ok_or_else(|| anyhow!("current_level[{i}] out of bounds"))?;
            let mut child_ids: Vec<u64> = vec![first_entry.0];
            let mut sep_bytes: Vec<Vec<u8>> = Vec::new();
            let mut sep_data_bytes: usize = 0;
            i += 1;

            while i < current_level.len() {
                let entry = current_level
                    .get(i)
                    .ok_or_else(|| anyhow!("current_level[{i}] out of bounds"))?;
                let sep = entry.1.clone();
                let projected = INTERNAL_HEADER_SIZE
                    + (child_ids.len() - 1) * 8
                    + (sep_bytes.len() + 1) * SLOT_SIZE
                    + sep_data_bytes
                    + sep.len();

                if projected > PAGE_SIZE && sep_bytes.is_empty() {
                    anyhow::bail!("B-tree separator does not fit in one page")
                }

                if projected > page_fill_bytes && !sep_bytes.is_empty() {
                    break;
                }

                sep_data_bytes += sep.len();
                sep_bytes.push(sep);
                observe_serialized_frontier(sep_bytes.len(), sep_data_bytes);
                child_ids.push(
                    current_level
                        .get(i)
                        .ok_or_else(|| anyhow!("current_level[{i}] out of bounds"))?
                        .0,
                );
                i += 1;
            }

            let node_page_id = next_page;
            write_internal_page(backend, cache, node_page_id, &child_ids, &sep_bytes)?;
            next_page += 1;

            let first_key = current_level
                .get(i_start)
                .ok_or_else(|| anyhow!("current_level[{i_start}] out of bounds"))?
                .1
                .clone();
            next_level.push((node_page_id, first_key));
        }

        current_level = next_level;
    }
}

/// Merge two already-sorted iterators without materializing either input.
pub fn merge_sorted_iters<T: Ord>(
    a: impl Iterator<Item = T>,
    b: impl Iterator<Item = T>,
) -> impl Iterator<Item = T> {
    let mut ai = a.peekable();
    let mut bi = b.peekable();
    std::iter::from_fn(move || match (ai.peek(), bi.peek()) {
        (Some(_), Some(_)) => {
            if ai.peek() <= bi.peek() {
                ai.next()
            } else {
                bi.next()
            }
        }
        (Some(_), None) => ai.next(),
        (None, Some(_)) => bi.next(),
        (None, None) => None,
    })
}

// ─── Leaf traversal helpers ───────────────────────────────────────────────────

/// Traverse internal nodes from `root` to find the leftmost (first) leaf page.
#[allow(clippy::arithmetic_side_effects)]
fn find_leftmost_leaf(root: u64, backend: &dyn StorageBackend, cache: &PageCache) -> Result<u64> {
    let mut page_id = root;
    loop {
        let page = cache.get_or_load(page_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty page at page_id={page_id}"))?;
        match page_type {
            page_type if is_leaf_page_type(page_type) => return Ok(page_id),
            PAGE_TYPE_INTERNAL => {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.internal_pages_visited =
                        diagnostics.internal_pages_visited.saturating_add(1);
                });
                let key_count = read_u16_at(&page[..], 2)? as usize;
                if key_count == 0 {
                    page_id = read_u64_at(&page[..], 4)?;
                } else {
                    page_id = read_u64_at(&page[..], INTERNAL_HEADER_SIZE)?;
                }
            }
            t => anyhow::bail!(
                "find_leftmost_leaf: unexpected page type 0x{:02x} at page_id={}",
                t,
                page_id
            ),
        }
    }
}

/// Traverse from `root` to the leaf that would contain `key`.
// Called by range_scan which is called by OnDiskIndexReader::range_scan_*.
#[allow(dead_code)]
#[allow(clippy::arithmetic_side_effects)]
fn find_leaf_for_key<K>(
    root: u64,
    key: &K,
    backend: &dyn StorageBackend,
    cache: &PageCache,
) -> Result<u64>
where
    K: for<'de> Deserialize<'de> + Ord,
{
    let mut page_id = root;
    loop {
        let page = cache.get_or_load(page_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty page at page_id={page_id}"))?;
        match page_type {
            page_type if is_leaf_page_type(page_type) => return Ok(page_id),
            PAGE_TYPE_INTERNAL => {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.internal_pages_visited =
                        diagnostics.internal_pages_visited.saturating_add(1);
                });
                let key_count = read_u16_at(&page[..], 2)? as usize;
                let rightmost_child = read_u64_at(&page[..], 4)?;
                let child_arr_start = INTERNAL_HEADER_SIZE;
                let slot_dir_start = INTERNAL_HEADER_SIZE + key_count * 8;

                let mut descended = false;
                for i in 0..key_count {
                    let slot_off = slot_dir_start + i * SLOT_SIZE;
                    let sep_offset = read_u16_at(&page[..], slot_off)? as usize;
                    let sep_length = read_u16_at(&page[..], slot_off + 2)? as usize;
                    let sep_slice = page
                        .get(sep_offset..sep_offset.saturating_add(sep_length))
                        .ok_or_else(|| {
                            anyhow!(
                                "sep slice out of bounds: offset={sep_offset} len={sep_length} page_len={}",
                                page.len()
                            )
                        })?;
                    let sep_key: K = postcard::from_bytes(sep_slice)?;

                    if *key < sep_key {
                        let child_off = child_arr_start + i * 8;
                        page_id = read_u64_at(&page[..], child_off)?;
                        descended = true;
                        break;
                    }
                }
                if !descended {
                    page_id = rightmost_child;
                }
            }
            t => anyhow::bail!(
                "find_leaf_for_key: unexpected page type 0x{:02x} at page_id={}",
                t,
                page_id
            ),
        }
    }
}

/// Read all `(K, FactRef)` entries from a leaf page's slot directory.
#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
fn read_leaf_entries<K>(page: &[u8]) -> Result<Vec<(K, FactRef)>>
where
    K: for<'de> Deserialize<'de>,
{
    #[cfg(feature = "bench-internals")]
    let decode_started = std::time::Instant::now();
    let page_type = page
        .first()
        .copied()
        .ok_or_else(|| anyhow!("empty leaf page"))?;
    if !is_leaf_page_type(page_type) {
        anyhow::bail!("expected leaf page, found type 0x{page_type:02x}")
    }
    if page_type == PAGE_TYPE_PREFIX_LEAF
        && page.get(1).copied() != Some(u8::try_from(PREFIX_RESTART_INTERVAL)?)
    {
        anyhow::bail!("unsupported prefix leaf restart interval")
    }
    let entry_count = read_u16_at(page, 2)? as usize;
    #[cfg(feature = "bench-internals")]
    update_leaf_read_diagnostics(|diagnostics| {
        diagnostics.leaf_pages_visited = diagnostics.leaf_pages_visited.saturating_add(1);
        diagnostics.leaf_entries_available = diagnostics
            .leaf_entries_available
            .saturating_add(u64::try_from(entry_count).unwrap_or(u64::MAX));
        if page_type == PAGE_TYPE_PREFIX_LEAF {
            diagnostics.prefix_leaf_pages_visited =
                diagnostics.prefix_leaf_pages_visited.saturating_add(1);
        } else {
            diagnostics.raw_leaf_pages_visited =
                diagnostics.raw_leaf_pages_visited.saturating_add(1);
        }
    });
    let mut entries = Vec::with_capacity(entry_count);
    let mut previous = Vec::new();
    #[cfg(feature = "bench-internals")]
    let mut decoded_payload_bytes = 0usize;
    for i in 0..entry_count {
        let slot_off = LEAF_HEADER_SIZE + i * SLOT_SIZE;
        let offset = read_u16_at(page, slot_off)? as usize;
        let length = read_u16_at(page, slot_off + 2)? as usize;
        let stored = page
            .get(offset..offset.saturating_add(length))
            .ok_or_else(|| {
                anyhow!(
                    "entry slice out of bounds: offset={offset} len={length} page_len={}",
                    page.len()
                )
            })?;
        let decoded = if page_type == PAGE_TYPE_PREFIX_LEAF {
            if stored.len() < PREFIX_RECORD_HEADER_SIZE {
                anyhow::bail!("compressed leaf record is truncated")
            }
            let shared = usize::from(read_u16_at(stored, 0)?);
            let decoded_len = usize::from(read_u16_at(stored, 2)?);
            if i.is_multiple_of(PREFIX_RESTART_INTERVAL) && shared != 0 {
                anyhow::bail!("compressed leaf restart record has a prefix")
            }
            if shared > previous.len() || shared > decoded_len {
                anyhow::bail!("compressed leaf prefix is out of bounds")
            }
            let suffix = stored
                .get(PREFIX_RECORD_HEADER_SIZE..)
                .ok_or_else(|| anyhow!("compressed leaf record is truncated"))?;
            if shared.saturating_add(suffix.len()) != decoded_len {
                anyhow::bail!("compressed leaf decoded length mismatch")
            }
            previous.truncate(shared);
            previous.extend_from_slice(suffix);
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                if i.is_multiple_of(PREFIX_RESTART_INTERVAL) {
                    diagnostics.prefix_restart_blocks_reconstructed = diagnostics
                        .prefix_restart_blocks_reconstructed
                        .saturating_add(1);
                }
                diagnostics.prefix_entries_reconstructed =
                    diagnostics.prefix_entries_reconstructed.saturating_add(1);
                diagnostics.reconstructed_key_bytes = diagnostics
                    .reconstructed_key_bytes
                    .saturating_add(u64::try_from(previous.len()).unwrap_or(u64::MAX));
            });
            previous.as_slice()
        } else {
            stored
        };
        let (k, fr): (K, FactRef) = postcard::from_bytes(decoded)?;
        #[cfg(feature = "bench-internals")]
        {
            decoded_payload_bytes = decoded_payload_bytes.saturating_add(decoded.len());
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_decoded =
                    diagnostics.leaf_entries_decoded.saturating_add(1);
            });
        }
        entries.push((k, fr));
    }
    #[cfg(feature = "bench-internals")]
    update_leaf_read_diagnostics(|diagnostics| {
        diagnostics.full_leaf_vec_peak_entries = diagnostics
            .full_leaf_vec_peak_entries
            .max(u64::try_from(entries.capacity()).unwrap_or(u64::MAX));
        diagnostics.full_leaf_vec_peak_struct_bytes =
            diagnostics.full_leaf_vec_peak_struct_bytes.max(
                u64::try_from(
                    entries
                        .capacity()
                        .saturating_mul(std::mem::size_of::<(K, FactRef)>()),
                )
                .unwrap_or(u64::MAX),
            );
        diagnostics.full_leaf_vec_peak_decoded_payload_bytes = diagnostics
            .full_leaf_vec_peak_decoded_payload_bytes
            .max(u64::try_from(decoded_payload_bytes).unwrap_or(u64::MAX));
        let elapsed = u64::try_from(decode_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if page_type == PAGE_TYPE_PREFIX_LEAF {
            diagnostics.prefix_decode_elapsed_ns =
                diagnostics.prefix_decode_elapsed_ns.saturating_add(elapsed);
        } else {
            diagnostics.raw_decode_elapsed_ns =
                diagnostics.raw_decode_elapsed_ns.saturating_add(elapsed);
        }
    });
    Ok(entries)
}

/// Page-backed cursor over one raw or restart-compressed leaf.
///
/// The cursor owns the cached page and at most one decoded entry. Prefix leaves
/// additionally retain only the previous serialized entry needed to reconstruct
/// the next record.
struct LeafEntryCursor<K> {
    page: Arc<Vec<u8>>,
    page_type: u8,
    entry_count: usize,
    slot: usize,
    previous: Vec<u8>,
    buffered: Option<(K, FactRef)>,
    marker: PhantomData<K>,
}

impl<K> LeafEntryCursor<K>
where
    K: for<'de> Deserialize<'de> + Ord,
{
    fn new(page: Arc<Vec<u8>>) -> Result<Self> {
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty leaf page"))?;
        if !is_leaf_page_type(page_type) {
            anyhow::bail!("expected leaf page, found type 0x{page_type:02x}")
        }
        if page_type == PAGE_TYPE_PREFIX_LEAF
            && page.get(1).copied() != Some(u8::try_from(PREFIX_RESTART_INTERVAL)?)
        {
            anyhow::bail!("unsupported prefix leaf restart interval")
        }
        let entry_count = usize::from(read_u16_at(&page, 2)?);
        let slot_directory_end = LEAF_HEADER_SIZE
            .checked_add(entry_count.saturating_mul(SLOT_SIZE))
            .ok_or_else(|| anyhow!("leaf slot directory length overflow"))?;
        if slot_directory_end > page.len() {
            anyhow::bail!("leaf slot directory is truncated")
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_pages_visited = diagnostics.leaf_pages_visited.saturating_add(1);
            diagnostics.leaf_entries_available = diagnostics
                .leaf_entries_available
                .saturating_add(u64::try_from(entry_count).unwrap_or(u64::MAX));
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                diagnostics.prefix_leaf_pages_visited =
                    diagnostics.prefix_leaf_pages_visited.saturating_add(1);
            } else {
                diagnostics.raw_leaf_pages_visited =
                    diagnostics.raw_leaf_pages_visited.saturating_add(1);
            }
        });
        Ok(Self {
            page,
            page_type,
            entry_count,
            slot: 0,
            previous: Vec::new(),
            buffered: None,
            marker: PhantomData,
        })
    }

    fn seek_lower_bound(&mut self, start: &K) -> Result<()> {
        if self.page_type == PAGE_TYPE_PREFIX_LEAF {
            self.seek_prefix_lower_bound(start)
        } else {
            self.seek_raw_lower_bound(start)
        }
    }

    fn seek_raw_lower_bound(&mut self, start: &K) -> Result<()> {
        let mut low = 0usize;
        let mut high = self.entry_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let (key, _) = self.decode_raw(middle)?;
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.lower_bound_key_comparisons =
                    diagnostics.lower_bound_key_comparisons.saturating_add(1);
            });
            if key < *start {
                low = middle.saturating_add(1);
            } else {
                high = middle;
            }
        }
        self.slot = low;
        if low < self.entry_count {
            self.buffered = Some(self.decode_raw(low)?);
            self.slot = low.saturating_add(1);
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_entries_skipped = diagnostics
                .leaf_entries_skipped
                .saturating_add(u64::try_from(low).unwrap_or(u64::MAX));
        });
        Ok(())
    }

    fn seek_prefix_lower_bound(&mut self, start: &K) -> Result<()> {
        if self.entry_count == 0 {
            return Ok(());
        }
        let restart_count = self.entry_count.div_ceil(PREFIX_RESTART_INTERVAL);
        let mut low = 0usize;
        let mut high = restart_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let slot = middle.saturating_mul(PREFIX_RESTART_INTERVAL);
            let (key, _) = self.decode_prefix_restart(slot)?;
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.lower_bound_key_comparisons =
                    diagnostics.lower_bound_key_comparisons.saturating_add(1);
            });
            if key <= *start {
                low = middle.saturating_add(1);
            } else {
                high = middle;
            }
        }
        let block = low.saturating_sub(1);
        let block_start = block.saturating_mul(PREFIX_RESTART_INTERVAL);
        self.slot = block_start;
        self.previous.clear();
        while self.slot < self.entry_count {
            let entry_slot = self.slot;
            let entry = self.decode_prefix_next()?;
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.lower_bound_key_comparisons =
                    diagnostics.lower_bound_key_comparisons.saturating_add(1);
            });
            if entry.0 >= *start {
                self.buffered = Some(entry);
                break;
            }
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_skipped =
                    diagnostics.leaf_entries_skipped.saturating_add(1);
            });
            if entry_slot
                .saturating_add(1)
                .is_multiple_of(PREFIX_RESTART_INTERVAL)
            {
                break;
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn next_entry(&mut self) -> Result<Option<(K, FactRef)>> {
        if let Some(entry) = self.buffered.take() {
            return Ok(Some(entry));
        }
        if self.slot >= self.entry_count {
            return Ok(None);
        }
        if self.page_type == PAGE_TYPE_PREFIX_LEAF {
            self.decode_prefix_next().map(Some)
        } else {
            let slot = self.slot;
            self.slot = self.slot.saturating_add(1);
            self.decode_raw(slot).map(Some)
        }
    }

    #[inline(always)]
    fn stored_entry(&self, slot: usize) -> Result<&[u8]> {
        if slot >= self.entry_count {
            anyhow::bail!("leaf slot {slot} exceeds entry count {}", self.entry_count)
        }
        let slot_off = LEAF_HEADER_SIZE
            .checked_add(slot.saturating_mul(SLOT_SIZE))
            .ok_or_else(|| anyhow!("leaf slot offset overflow"))?;
        let offset = usize::from(read_u16_at(&self.page, slot_off)?);
        let length = usize::from(read_u16_at(&self.page, slot_off.saturating_add(2))?);
        self.page
            .get(offset..offset.saturating_add(length))
            .ok_or_else(|| {
                anyhow!(
                    "entry slice out of bounds: offset={offset} len={length} page_len={}",
                    self.page.len()
                )
            })
    }

    #[inline(always)]
    fn decode_raw(&self, slot: usize) -> Result<(K, FactRef)> {
        #[cfg(feature = "bench-internals")]
        let started = leaf_read_diagnostics_enabled().then(std::time::Instant::now);
        let entry = postcard::from_bytes(self.stored_entry(slot)?)?;
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_entries_decoded = diagnostics.leaf_entries_decoded.saturating_add(1);
            if let Some(started) = started {
                diagnostics.raw_decode_elapsed_ns =
                    diagnostics.raw_decode_elapsed_ns.saturating_add(
                        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
            }
        });
        Ok(entry)
    }

    #[inline(always)]
    fn decode_prefix_restart(&mut self, slot: usize) -> Result<(K, FactRef)> {
        self.previous.clear();
        self.decode_prefix_at(slot)
    }

    #[inline(always)]
    fn decode_prefix_next(&mut self) -> Result<(K, FactRef)> {
        let slot = self.slot;
        let entry = self.decode_prefix_at(slot)?;
        self.slot = self.slot.saturating_add(1);
        Ok(entry)
    }

    #[inline(always)]
    fn decode_prefix_at(&mut self, slot: usize) -> Result<(K, FactRef)> {
        #[cfg(feature = "bench-internals")]
        let started = leaf_read_diagnostics_enabled().then(std::time::Instant::now);
        let page = &self.page;
        let previous = &mut self.previous;
        if slot >= self.entry_count {
            anyhow::bail!("leaf slot {slot} exceeds entry count {}", self.entry_count)
        }
        let slot_off = LEAF_HEADER_SIZE
            .checked_add(slot.saturating_mul(SLOT_SIZE))
            .ok_or_else(|| anyhow!("leaf slot offset overflow"))?;
        let offset = usize::from(read_u16_at(page, slot_off)?);
        let length = usize::from(read_u16_at(page, slot_off.saturating_add(2))?);
        let stored = page
            .get(offset..offset.saturating_add(length))
            .ok_or_else(|| {
                anyhow!(
                    "entry slice out of bounds: offset={offset} len={length} page_len={}",
                    page.len()
                )
            })?;
        if stored.len() < PREFIX_RECORD_HEADER_SIZE {
            anyhow::bail!("compressed leaf record is truncated")
        }
        let shared = usize::from(read_u16_at(stored, 0)?);
        let decoded_len = usize::from(read_u16_at(stored, 2)?);
        if slot.is_multiple_of(PREFIX_RESTART_INTERVAL) && shared != 0 {
            anyhow::bail!("compressed leaf restart record has a prefix")
        }
        if shared > previous.len() || shared > decoded_len {
            anyhow::bail!("compressed leaf prefix is out of bounds")
        }
        let suffix = stored
            .get(PREFIX_RECORD_HEADER_SIZE..)
            .ok_or_else(|| anyhow!("compressed leaf record is truncated"))?;
        if shared.saturating_add(suffix.len()) != decoded_len {
            anyhow::bail!("compressed leaf decoded length mismatch")
        }
        previous.truncate(shared);
        previous.extend_from_slice(suffix);
        let entry = postcard::from_bytes(previous)?;
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_entries_decoded = diagnostics.leaf_entries_decoded.saturating_add(1);
            if slot.is_multiple_of(PREFIX_RESTART_INTERVAL) {
                diagnostics.prefix_restart_blocks_reconstructed = diagnostics
                    .prefix_restart_blocks_reconstructed
                    .saturating_add(1);
            }
            diagnostics.prefix_entries_reconstructed =
                diagnostics.prefix_entries_reconstructed.saturating_add(1);
            diagnostics.reconstructed_key_bytes = diagnostics
                .reconstructed_key_bytes
                .saturating_add(u64::try_from(previous.len()).unwrap_or(u64::MAX));
            if let Some(started) = started {
                diagnostics.prefix_decode_elapsed_ns =
                    diagnostics.prefix_decode_elapsed_ns.saturating_add(
                        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
            }
        });
        Ok(entry)
    }
}

// ─── stream_all_entries ───────────────────────────────────────────────────────

/// Stream all `(K, FactRef)` entries from a B+tree in sorted order.
pub fn stream_all_entries<K>(
    root_page_id: u64,
    backend: &dyn StorageBackend,
    cache: &PageCache,
) -> Result<Vec<(K, FactRef)>>
where
    K: for<'de> Deserialize<'de> + Ord,
{
    let first_leaf = find_leftmost_leaf(root_page_id, backend, cache)?;
    let mut result = Vec::new();
    let mut leaf_id = first_leaf;

    loop {
        let page = cache.get_or_load(leaf_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty page at page_id={leaf_id}"))?;
        if !is_leaf_page_type(page_type) {
            anyhow::bail!(
                "stream_all_entries: expected leaf page at page_id={}",
                leaf_id
            );
        }
        let next_leaf = read_u64_at(&page[..], 4)?;
        let mut cursor = LeafEntryCursor::<K>::new(page)?;
        while let Some(entry) = cursor.next_entry()? {
            result.push(entry);
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_emitted =
                    diagnostics.leaf_entries_emitted.saturating_add(1);
            });
        }

        if next_leaf == 0 {
            break;
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_boundary_transitions =
                diagnostics.leaf_boundary_transitions.saturating_add(1);
        });
        leaf_id = next_leaf;
    }

    Ok(result)
}

// ─── range_scan ───────────────────────────────────────────────────────────────

/// Scan the B+tree for all `FactRef`s whose key is in `[start, end)`.
///
/// `end: None` means unbounded (scan to last leaf).
// Called by OnDiskIndexReader::range_scan_* (via trait object dispatch).
#[allow(dead_code)]
pub fn range_scan<K>(
    root_page_id: u64,
    start: &K,
    end: Option<&K>,
    backend: &dyn StorageBackend,
    cache: &PageCache,
) -> Result<Vec<FactRef>>
where
    K: Serialize + for<'de> Deserialize<'de> + Ord,
{
    Ok(
        range_scan_entries(root_page_id, start, end, backend, cache)?
            .into_iter()
            .map(|(_, fact_ref)| fact_ref)
            .collect(),
    )
}

/// Scan the B+tree for all keyed entries whose key is in `[start, end)`.
///
/// `end: None` means unbounded (scan to last leaf).
pub(crate) fn range_scan_entries<K>(
    root_page_id: u64,
    start: &K,
    end: Option<&K>,
    backend: &dyn StorageBackend,
    cache: &PageCache,
) -> Result<Vec<(K, FactRef)>>
where
    K: Serialize + for<'de> Deserialize<'de> + Ord,
{
    let start_leaf = find_leaf_for_key(root_page_id, start, backend, cache)?;
    let mut result = Vec::new();
    let mut leaf_id = start_leaf;

    'outer: loop {
        let page = cache.get_or_load(leaf_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty page at page_id={leaf_id}"))?;
        if !is_leaf_page_type(page_type) {
            anyhow::bail!("range_scan: expected leaf at page_id={}", leaf_id);
        }
        let next_leaf = read_u64_at(&page[..], 4)?;
        let mut cursor = LeafEntryCursor::<K>::new(page)?;
        if leaf_id == start_leaf {
            cursor.seek_lower_bound(start)?;
        }
        while let Some((k, fr)) = cursor.next_entry()? {
            if let Some(e) = end
                && k >= *e
            {
                break 'outer;
            }
            result.push((k, fr));
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_emitted =
                    diagnostics.leaf_entries_emitted.saturating_add(1);
            });
        }

        if next_leaf == 0 {
            break;
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_boundary_transitions =
                diagnostics.leaf_boundary_transitions.saturating_add(1);
        });
        leaf_id = next_leaf;
    }

    Ok(result)
}

fn visit_range_entries<K>(
    root_page_id: u64,
    start: &K,
    end: Option<&K>,
    backend: &dyn StorageBackend,
    cache: &PageCache,
    visit: &mut dyn FnMut(&K, FactRef) -> Result<bool>,
) -> Result<bool>
where
    K: Serialize + for<'de> Deserialize<'de> + Ord,
{
    let start_leaf = find_leaf_for_key(root_page_id, start, backend, cache)?;
    let mut leaf_id = start_leaf;
    'outer: loop {
        let page = cache.get_or_load(leaf_id, backend)?;
        if !page.first().copied().is_some_and(is_leaf_page_type) {
            anyhow::bail!("range_scan: expected leaf at page_id={}", leaf_id);
        }
        let next_leaf = read_u64_at(&page[..], 4)?;
        let mut cursor = LeafEntryCursor::<K>::new(page)?;
        if leaf_id == start_leaf {
            cursor.seek_lower_bound(start)?;
        }
        macro_rules! visit_entry {
            ($entry:expr) => {{
                let (key, fact_ref) = $entry;
                if end.is_some_and(|end| key >= *end) {
                    break 'outer;
                }
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.leaf_entries_emitted =
                        diagnostics.leaf_entries_emitted.saturating_add(1);
                });
                if !visit(&key, fact_ref)? {
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.early_stop_count =
                            diagnostics.early_stop_count.saturating_add(1);
                    });
                    return Ok(false);
                }
            }};
        }
        if let Some(entry) = cursor.buffered.take() {
            visit_entry!(entry);
        }
        if cursor.page_type == PAGE_TYPE_PREFIX_LEAF {
            while cursor.slot < cursor.entry_count {
                visit_entry!(cursor.decode_prefix_next()?);
            }
        } else {
            while cursor.slot < cursor.entry_count {
                let slot = cursor.slot;
                cursor.slot = cursor.slot.saturating_add(1);
                visit_entry!(cursor.decode_raw(slot)?);
            }
        }
        if next_leaf == 0 {
            break;
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_boundary_transitions =
                diagnostics.leaf_boundary_transitions.saturating_add(1);
        });
        leaf_id = next_leaf;
    }
    Ok(true)
}

fn leaf_entry_slice(page: &[u8], entry_count: usize, slot: usize) -> Result<&[u8]> {
    if slot >= entry_count {
        anyhow::bail!("leaf slot {slot} exceeds entry count {entry_count}")
    }
    let slot_offset = LEAF_HEADER_SIZE
        .checked_add(slot.saturating_mul(SLOT_SIZE))
        .ok_or_else(|| anyhow!("leaf slot offset overflow"))?;
    let offset = usize::from(read_u16_at(page, slot_offset)?);
    let length = usize::from(read_u16_at(page, slot_offset.saturating_add(2))?);
    page.get(offset..offset.saturating_add(length))
        .ok_or_else(|| anyhow!("entry slice out of bounds"))
}

fn reconstruct_prefix_entry(
    page: &[u8],
    entry_count: usize,
    slot: usize,
    previous: &mut Vec<u8>,
) -> Result<()> {
    let stored = leaf_entry_slice(page, entry_count, slot)?;
    if stored.len() < PREFIX_RECORD_HEADER_SIZE {
        anyhow::bail!("compressed leaf record is truncated")
    }
    let shared = usize::from(read_u16_at(stored, 0)?);
    let decoded_len = usize::from(read_u16_at(stored, 2)?);
    if slot.is_multiple_of(PREFIX_RESTART_INTERVAL) && shared != 0 {
        anyhow::bail!("compressed leaf restart record has a prefix")
    }
    if shared > previous.len() || shared > decoded_len {
        anyhow::bail!("compressed leaf prefix is out of bounds")
    }
    let suffix = stored
        .get(PREFIX_RECORD_HEADER_SIZE..)
        .ok_or_else(|| anyhow!("compressed leaf record is truncated"))?;
    if shared.saturating_add(suffix.len()) != decoded_len {
        anyhow::bail!("compressed leaf decoded length mismatch")
    }
    previous.truncate(shared);
    previous.extend_from_slice(suffix);
    #[cfg(feature = "bench-internals")]
    update_leaf_read_diagnostics(|diagnostics| {
        if slot.is_multiple_of(PREFIX_RESTART_INTERVAL) {
            diagnostics.prefix_restart_blocks_reconstructed = diagnostics
                .prefix_restart_blocks_reconstructed
                .saturating_add(1);
        }
        diagnostics.prefix_entries_reconstructed =
            diagnostics.prefix_entries_reconstructed.saturating_add(1);
        diagnostics.reconstructed_key_bytes = diagnostics
            .reconstructed_key_bytes
            .saturating_add(u64::try_from(previous.len()).unwrap_or(u64::MAX));
    });
    Ok(())
}

#[inline(always)]
fn decode_projected_aevt(bytes: &[u8], _prefix: bool) -> Result<(AevtEntryWire<'_>, FactRef)> {
    #[cfg(feature = "bench-internals")]
    let started = leaf_read_diagnostics_enabled().then(std::time::Instant::now);
    let decoded = AevtEntryWire::decode_entry(bytes)?;
    #[cfg(feature = "bench-internals")]
    update_leaf_read_diagnostics(|diagnostics| {
        diagnostics.leaf_entries_decoded = diagnostics.leaf_entries_decoded.saturating_add(1);
        diagnostics.aevt_projection_decodes = diagnostics.aevt_projection_decodes.saturating_add(1);
        let (attribute_bytes, value_bytes) = decoded.0.borrowed_lengths();
        diagnostics.borrowed_attribute_bytes = diagnostics
            .borrowed_attribute_bytes
            .saturating_add(u64::try_from(attribute_bytes).unwrap_or(u64::MAX));
        diagnostics.borrowed_value_bytes = diagnostics
            .borrowed_value_bytes
            .saturating_add(u64::try_from(value_bytes).unwrap_or(u64::MAX));
        if let Some(started) = started {
            let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if _prefix {
                diagnostics.prefix_decode_elapsed_ns =
                    diagnostics.prefix_decode_elapsed_ns.saturating_add(elapsed);
            } else {
                diagnostics.raw_decode_elapsed_ns =
                    diagnostics.raw_decode_elapsed_ns.saturating_add(elapsed);
            }
        }
    });
    Ok(decoded)
}

#[inline(always)]
fn decode_projected_eavt(bytes: &[u8], _prefix: bool) -> Result<(EavtEntryWire<'_>, FactRef)> {
    #[cfg(feature = "bench-internals")]
    let started = leaf_read_diagnostics_enabled().then(std::time::Instant::now);
    let decoded = EavtEntryWire::decode_entry(bytes)?;
    #[cfg(feature = "bench-internals")]
    update_leaf_read_diagnostics(|diagnostics| {
        diagnostics.leaf_entries_decoded = diagnostics.leaf_entries_decoded.saturating_add(1);
        diagnostics.eavt_projection_decodes = diagnostics.eavt_projection_decodes.saturating_add(1);
        let (attribute_bytes, value_bytes) = decoded.0.borrowed_lengths();
        diagnostics.borrowed_attribute_bytes = diagnostics
            .borrowed_attribute_bytes
            .saturating_add(u64::try_from(attribute_bytes).unwrap_or(u64::MAX));
        diagnostics.borrowed_value_bytes = diagnostics
            .borrowed_value_bytes
            .saturating_add(u64::try_from(value_bytes).unwrap_or(u64::MAX));
        if let Some(started) = started {
            let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if _prefix {
                diagnostics.prefix_decode_elapsed_ns =
                    diagnostics.prefix_decode_elapsed_ns.saturating_add(elapsed);
            } else {
                diagnostics.raw_decode_elapsed_ns =
                    diagnostics.raw_decode_elapsed_ns.saturating_add(elapsed);
            }
        }
    });
    Ok(decoded)
}

#[inline(always)]
fn decode_projected_vaet(bytes: &[u8], _prefix: bool) -> Result<(VaetEntryWire<'_>, FactRef)> {
    #[cfg(feature = "bench-internals")]
    let started = leaf_read_diagnostics_enabled().then(std::time::Instant::now);
    let decoded = VaetEntryWire::decode_entry(bytes)?;
    #[cfg(feature = "bench-internals")]
    update_leaf_read_diagnostics(|diagnostics| {
        diagnostics.leaf_entries_decoded = diagnostics.leaf_entries_decoded.saturating_add(1);
        diagnostics.vaet_projection_decodes = diagnostics.vaet_projection_decodes.saturating_add(1);
        diagnostics.borrowed_attribute_bytes = diagnostics
            .borrowed_attribute_bytes
            .saturating_add(u64::try_from(decoded.0.borrowed_attribute_len()).unwrap_or(u64::MAX));
        if let Some(started) = started {
            let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if _prefix {
                diagnostics.prefix_decode_elapsed_ns =
                    diagnostics.prefix_decode_elapsed_ns.saturating_add(elapsed);
            } else {
                diagnostics.raw_decode_elapsed_ns =
                    diagnostics.raw_decode_elapsed_ns.saturating_add(elapsed);
            }
        }
    });
    Ok(decoded)
}

/// Exact-entity/attribute EAVT scan. Raw entries borrow the cached page;
/// prefix entries borrow the single restart reconstruction buffer.
fn visit_current_eavt_range(
    root_page_id: u64,
    start: &EavtKey,
    end: Option<&EavtKey>,
    backend: &dyn StorageBackend,
    cache: &PageCache,
    visit: &mut dyn for<'a> FnMut(CurrentEavtEntryRef<'a>, FactRef) -> Result<bool>,
) -> Result<bool> {
    let start_leaf = find_leaf_for_key(root_page_id, start, backend, cache)?;
    let mut leaf_id = start_leaf;
    'leaves: loop {
        let page = cache.get_or_load(leaf_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty leaf page"))?;
        if !is_leaf_page_type(page_type) {
            anyhow::bail!("range_scan: expected leaf at page_id={leaf_id}")
        }
        if page_type == PAGE_TYPE_PREFIX_LEAF
            && page.get(1).copied() != Some(u8::try_from(PREFIX_RESTART_INTERVAL)?)
        {
            anyhow::bail!("unsupported prefix leaf restart interval")
        }
        let entry_count = usize::from(read_u16_at(&page, 2)?);
        let directory_end = LEAF_HEADER_SIZE
            .checked_add(entry_count.saturating_mul(SLOT_SIZE))
            .ok_or_else(|| anyhow!("leaf slot directory length overflow"))?;
        if directory_end > page.len() {
            anyhow::bail!("leaf slot directory is truncated")
        }
        let next_leaf = read_u64_at(&page, 4)?;
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_pages_visited = diagnostics.leaf_pages_visited.saturating_add(1);
            diagnostics.leaf_entries_available = diagnostics
                .leaf_entries_available
                .saturating_add(u64::try_from(entry_count).unwrap_or(u64::MAX));
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                diagnostics.prefix_leaf_pages_visited =
                    diagnostics.prefix_leaf_pages_visited.saturating_add(1);
            } else {
                diagnostics.raw_leaf_pages_visited =
                    diagnostics.raw_leaf_pages_visited.saturating_add(1);
            }
        });

        let mut slot = 0usize;
        let mut previous = Vec::new();
        if leaf_id == start_leaf && entry_count != 0 {
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                let restart_count = entry_count.div_ceil(PREFIX_RESTART_INTERVAL);
                let mut low = 0usize;
                let mut high = restart_count;
                while low < high {
                    let middle = low + (high - low) / 2;
                    let restart_slot = middle.saturating_mul(PREFIX_RESTART_INTERVAL);
                    previous.clear();
                    reconstruct_prefix_entry(&page, entry_count, restart_slot, &mut previous)?;
                    let (wire, _) = decode_projected_eavt(&previous, true)?;
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.lower_bound_key_comparisons =
                            diagnostics.lower_bound_key_comparisons.saturating_add(1);
                    });
                    if wire.cmp_owned(start).is_lt() {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                let restart = low
                    .saturating_sub(1)
                    .saturating_mul(PREFIX_RESTART_INTERVAL);
                previous.clear();
                slot = restart;
            } else {
                let mut low = 0usize;
                let mut high = entry_count;
                while low < high {
                    let middle = low + (high - low) / 2;
                    let (wire, _) = decode_projected_eavt(
                        leaf_entry_slice(&page, entry_count, middle)?,
                        false,
                    )?;
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.lower_bound_key_comparisons =
                            diagnostics.lower_bound_key_comparisons.saturating_add(1);
                    });
                    if wire.cmp_owned(start).is_lt() {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                slot = low;
            }
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_entries_skipped = diagnostics
                .leaf_entries_skipped
                .saturating_add(u64::try_from(slot).unwrap_or(u64::MAX));
        });

        while slot < entry_count {
            let bytes = if page_type == PAGE_TYPE_PREFIX_LEAF {
                reconstruct_prefix_entry(&page, entry_count, slot, &mut previous)?;
                previous.as_slice()
            } else {
                leaf_entry_slice(&page, entry_count, slot)?
            };
            slot = slot.saturating_add(1);
            let (wire, fact_ref) =
                decode_projected_eavt(bytes, page_type == PAGE_TYPE_PREFIX_LEAF)?;
            if leaf_id == start_leaf && wire.cmp_owned(start).is_lt() {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.leaf_entries_skipped =
                        diagnostics.leaf_entries_skipped.saturating_add(1);
                });
                continue;
            }
            if end.is_some_and(|bound| !wire.cmp_owned(bound).is_lt()) {
                break 'leaves;
            }
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_emitted =
                    diagnostics.leaf_entries_emitted.saturating_add(1);
                diagnostics.projected_eavt_emitted =
                    diagnostics.projected_eavt_emitted.saturating_add(1);
            });
            if !visit(wire.project(), fact_ref)? {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.early_stop_count = diagnostics.early_stop_count.saturating_add(1);
                });
                return Ok(false);
            }
        }
        if next_leaf == 0 {
            break;
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_boundary_transitions =
                diagnostics.leaf_boundary_transitions.saturating_add(1);
        });
        leaf_id = next_leaf;
    }
    Ok(true)
}

/// Exact reverse-reference VAET scan. Raw entries borrow the cached page;
/// prefix entries borrow the single restart reconstruction buffer.
fn visit_current_vaet_range(
    root_page_id: u64,
    start: &VaetKey,
    end: Option<&VaetKey>,
    backend: &dyn StorageBackend,
    cache: &PageCache,
    visit: &mut dyn for<'a> FnMut(CurrentVaetEntryRef<'a>, FactRef) -> Result<bool>,
) -> Result<bool> {
    let start_leaf = find_leaf_for_key(root_page_id, start, backend, cache)?;
    let mut leaf_id = start_leaf;
    'leaves: loop {
        let page = cache.get_or_load(leaf_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty leaf page"))?;
        if !is_leaf_page_type(page_type) {
            anyhow::bail!("range_scan: expected leaf at page_id={leaf_id}")
        }
        if page_type == PAGE_TYPE_PREFIX_LEAF
            && page.get(1).copied() != Some(u8::try_from(PREFIX_RESTART_INTERVAL)?)
        {
            anyhow::bail!("unsupported prefix leaf restart interval")
        }
        let entry_count = usize::from(read_u16_at(&page, 2)?);
        let directory_end = LEAF_HEADER_SIZE
            .checked_add(entry_count.saturating_mul(SLOT_SIZE))
            .ok_or_else(|| anyhow!("leaf slot directory length overflow"))?;
        if directory_end > page.len() {
            anyhow::bail!("leaf slot directory is truncated")
        }
        let next_leaf = read_u64_at(&page, 4)?;
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_pages_visited = diagnostics.leaf_pages_visited.saturating_add(1);
            diagnostics.leaf_entries_available = diagnostics
                .leaf_entries_available
                .saturating_add(u64::try_from(entry_count).unwrap_or(u64::MAX));
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                diagnostics.prefix_leaf_pages_visited =
                    diagnostics.prefix_leaf_pages_visited.saturating_add(1);
            } else {
                diagnostics.raw_leaf_pages_visited =
                    diagnostics.raw_leaf_pages_visited.saturating_add(1);
            }
        });

        let mut slot = 0usize;
        let mut previous = Vec::new();
        if leaf_id == start_leaf && entry_count != 0 {
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                let restart_count = entry_count.div_ceil(PREFIX_RESTART_INTERVAL);
                let mut low = 0usize;
                let mut high = restart_count;
                while low < high {
                    let middle = low + (high - low) / 2;
                    let restart_slot = middle.saturating_mul(PREFIX_RESTART_INTERVAL);
                    previous.clear();
                    reconstruct_prefix_entry(&page, entry_count, restart_slot, &mut previous)?;
                    let (wire, _) = decode_projected_vaet(&previous, true)?;
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.lower_bound_key_comparisons =
                            diagnostics.lower_bound_key_comparisons.saturating_add(1);
                    });
                    if wire.cmp_owned(start).is_lt() {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                let restart = low
                    .saturating_sub(1)
                    .saturating_mul(PREFIX_RESTART_INTERVAL);
                previous.clear();
                slot = restart;
            } else {
                let mut low = 0usize;
                let mut high = entry_count;
                while low < high {
                    let middle = low + (high - low) / 2;
                    let (wire, _) = decode_projected_vaet(
                        leaf_entry_slice(&page, entry_count, middle)?,
                        false,
                    )?;
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.lower_bound_key_comparisons =
                            diagnostics.lower_bound_key_comparisons.saturating_add(1);
                    });
                    if wire.cmp_owned(start).is_lt() {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                slot = low;
            }
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_entries_skipped = diagnostics
                .leaf_entries_skipped
                .saturating_add(u64::try_from(slot).unwrap_or(u64::MAX));
        });

        while slot < entry_count {
            let bytes = if page_type == PAGE_TYPE_PREFIX_LEAF {
                reconstruct_prefix_entry(&page, entry_count, slot, &mut previous)?;
                previous.as_slice()
            } else {
                leaf_entry_slice(&page, entry_count, slot)?
            };
            slot = slot.saturating_add(1);
            let (wire, fact_ref) =
                decode_projected_vaet(bytes, page_type == PAGE_TYPE_PREFIX_LEAF)?;
            if leaf_id == start_leaf && wire.cmp_owned(start).is_lt() {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.leaf_entries_skipped =
                        diagnostics.leaf_entries_skipped.saturating_add(1);
                });
                continue;
            }
            if end.is_some_and(|bound| !wire.cmp_owned(bound).is_lt()) {
                break 'leaves;
            }
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_emitted =
                    diagnostics.leaf_entries_emitted.saturating_add(1);
                diagnostics.projected_vaet_emitted =
                    diagnostics.projected_vaet_emitted.saturating_add(1);
            });
            if !visit(wire.project(), fact_ref)? {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.early_stop_count = diagnostics.early_stop_count.saturating_add(1);
                });
                return Ok(false);
            }
        }
        if next_leaf == 0 {
            break;
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_boundary_transitions =
                diagnostics.leaf_boundary_transitions.saturating_add(1);
        });
        leaf_id = next_leaf;
    }
    Ok(true)
}

/// Current-attribute-only AEVT scan. Raw entries borrow the cached page;
/// prefix entries borrow the single restart reconstruction buffer.
fn visit_current_aevt_range(
    root_page_id: u64,
    start: &AevtKey,
    end: Option<&AevtKey>,
    backend: &dyn StorageBackend,
    cache: &PageCache,
    visit: &mut dyn for<'a> FnMut(CurrentAevtEntryRef<'a>, FactRef) -> Result<bool>,
) -> Result<bool> {
    let start_leaf = find_leaf_for_key(root_page_id, start, backend, cache)?;
    let mut leaf_id = start_leaf;
    'leaves: loop {
        let page = cache.get_or_load(leaf_id, backend)?;
        let page_type = page
            .first()
            .copied()
            .ok_or_else(|| anyhow!("empty leaf page"))?;
        if !is_leaf_page_type(page_type) {
            anyhow::bail!("range_scan: expected leaf at page_id={leaf_id}")
        }
        if page_type == PAGE_TYPE_PREFIX_LEAF
            && page.get(1).copied() != Some(u8::try_from(PREFIX_RESTART_INTERVAL)?)
        {
            anyhow::bail!("unsupported prefix leaf restart interval")
        }
        let entry_count = usize::from(read_u16_at(&page, 2)?);
        let directory_end = LEAF_HEADER_SIZE
            .checked_add(entry_count.saturating_mul(SLOT_SIZE))
            .ok_or_else(|| anyhow!("leaf slot directory length overflow"))?;
        if directory_end > page.len() {
            anyhow::bail!("leaf slot directory is truncated")
        }
        let next_leaf = read_u64_at(&page, 4)?;
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_pages_visited = diagnostics.leaf_pages_visited.saturating_add(1);
            diagnostics.leaf_entries_available = diagnostics
                .leaf_entries_available
                .saturating_add(u64::try_from(entry_count).unwrap_or(u64::MAX));
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                diagnostics.prefix_leaf_pages_visited =
                    diagnostics.prefix_leaf_pages_visited.saturating_add(1);
            } else {
                diagnostics.raw_leaf_pages_visited =
                    diagnostics.raw_leaf_pages_visited.saturating_add(1);
            }
        });

        let mut slot = 0usize;
        let mut previous = Vec::new();
        if leaf_id == start_leaf && entry_count != 0 {
            if page_type == PAGE_TYPE_PREFIX_LEAF {
                let restart_count = entry_count.div_ceil(PREFIX_RESTART_INTERVAL);
                let mut low = 0usize;
                let mut high = restart_count;
                while low < high {
                    let middle = low + (high - low) / 2;
                    let restart_slot = middle.saturating_mul(PREFIX_RESTART_INTERVAL);
                    previous.clear();
                    reconstruct_prefix_entry(&page, entry_count, restart_slot, &mut previous)?;
                    let (wire, _) = decode_projected_aevt(&previous, true)?;
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.lower_bound_key_comparisons =
                            diagnostics.lower_bound_key_comparisons.saturating_add(1);
                    });
                    if wire.cmp_owned(start).is_lt() {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                let restart = low
                    .saturating_sub(1)
                    .saturating_mul(PREFIX_RESTART_INTERVAL);
                previous.clear();
                slot = restart;
            } else {
                let mut low = 0usize;
                let mut high = entry_count;
                while low < high {
                    let middle = low + (high - low) / 2;
                    let (wire, _) = decode_projected_aevt(
                        leaf_entry_slice(&page, entry_count, middle)?,
                        false,
                    )?;
                    #[cfg(feature = "bench-internals")]
                    update_leaf_read_diagnostics(|diagnostics| {
                        diagnostics.lower_bound_key_comparisons =
                            diagnostics.lower_bound_key_comparisons.saturating_add(1);
                    });
                    if wire.cmp_owned(start).is_lt() {
                        low = middle.saturating_add(1);
                    } else {
                        high = middle;
                    }
                }
                slot = low;
            }
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_entries_skipped = diagnostics
                .leaf_entries_skipped
                .saturating_add(u64::try_from(slot).unwrap_or(u64::MAX));
        });

        while slot < entry_count {
            let bytes = if page_type == PAGE_TYPE_PREFIX_LEAF {
                reconstruct_prefix_entry(&page, entry_count, slot, &mut previous)?;
                previous.as_slice()
            } else {
                leaf_entry_slice(&page, entry_count, slot)?
            };
            slot = slot.saturating_add(1);
            let (wire, fact_ref) =
                decode_projected_aevt(bytes, page_type == PAGE_TYPE_PREFIX_LEAF)?;
            if leaf_id == start_leaf && wire.cmp_owned(start).is_lt() {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.leaf_entries_skipped =
                        diagnostics.leaf_entries_skipped.saturating_add(1);
                });
                continue;
            }
            if end.is_some_and(|bound| !wire.cmp_owned(bound).is_lt()) {
                break 'leaves;
            }
            #[cfg(feature = "bench-internals")]
            update_leaf_read_diagnostics(|diagnostics| {
                diagnostics.leaf_entries_emitted =
                    diagnostics.leaf_entries_emitted.saturating_add(1);
                diagnostics.projected_aevt_emitted =
                    diagnostics.projected_aevt_emitted.saturating_add(1);
            });
            if !visit(wire.project(), fact_ref)? {
                #[cfg(feature = "bench-internals")]
                update_leaf_read_diagnostics(|diagnostics| {
                    diagnostics.early_stop_count = diagnostics.early_stop_count.saturating_add(1);
                });
                return Ok(false);
            }
        }
        if next_leaf == 0 {
            break;
        }
        #[cfg(feature = "bench-internals")]
        update_leaf_read_diagnostics(|diagnostics| {
            diagnostics.leaf_boundary_transitions =
                diagnostics.leaf_boundary_transitions.saturating_add(1);
        });
        leaf_id = next_leaf;
    }
    Ok(true)
}

// ─── MutexStorageBackend ──────────────────────────────────────────────────────

/// Read-only [`StorageBackend`] adapter that locks `Arc<Mutex<B>>` only for the
/// duration of a single [`StorageBackend::read_page`] call.
///
/// Used by [`OnDiskIndexReader::range_scan_*`] and [`crate::storage::persistent_facts`]
/// so that the backend mutex is held only while reading one cold page from disk,
/// rather than for the entire operation. On a cache hit [`PageCache::get_or_load`]
/// never calls `read_page`, so no lock is acquired at all. All methods other than
/// `read_page` are unimplemented and will panic if called.
pub(crate) struct MutexStorageBackend<B> {
    backend: Arc<Mutex<B>>,
    base_integrity: Option<Arc<BasePageIntegrityCatalog>>,
}

impl<B: StorageBackend> MutexStorageBackend<B> {
    pub(crate) fn new(backend: Arc<Mutex<B>>) -> Self {
        Self {
            backend,
            base_integrity: None,
        }
    }

    pub(crate) fn verified(
        backend: Arc<Mutex<B>>,
        base_integrity: Arc<BasePageIntegrityCatalog>,
    ) -> Self {
        Self {
            backend,
            base_integrity: Some(base_integrity),
        }
    }
}

impl<B: StorageBackend> StorageBackend for MutexStorageBackend<B> {
    fn read_page(&self, page_id: u64) -> anyhow::Result<Vec<u8>> {
        let page = self
            .backend
            .lock()
            .map_err(|e| anyhow!("MutexStorageBackend lock poisoned: {e}"))?
            .read_page(page_id)?;
        if let Some(base_integrity) = &self.base_integrity {
            base_integrity.verify_page(page_id, &page)?;
        }
        Ok(page)
    }

    #[allow(clippy::unimplemented)]
    fn write_page(&mut self, _page_id: u64, _data: &[u8]) -> anyhow::Result<()> {
        unimplemented!("MutexStorageBackend is read-only; write_page must not be called")
    }

    #[allow(clippy::unimplemented)]
    fn sync(&mut self) -> anyhow::Result<()> {
        unimplemented!("MutexStorageBackend is read-only; sync must not be called")
    }

    #[allow(clippy::unimplemented)]
    fn page_count(&self) -> anyhow::Result<u64> {
        unimplemented!("MutexStorageBackend is read-only; page_count must not be called")
    }

    #[allow(clippy::unimplemented)]
    fn close(&mut self) -> anyhow::Result<()> {
        unimplemented!("MutexStorageBackend is read-only; close must not be called")
    }

    #[allow(clippy::unimplemented)]
    fn backend_name(&self) -> &'static str {
        unimplemented!("MutexStorageBackend is read-only; backend_name must not be called")
    }

    fn is_new(&self) -> bool {
        self.backend.lock().map(|g| g.is_new()).unwrap_or(false)
    }
}

// ─── OnDiskIndexReader ────────────────────────────────────────────────────────

/// Implements `CommittedIndexReader` by delegating to `range_scan` on
/// on-disk B+tree pages via the page cache.
// Fields are read by range_scan_* methods of the CommittedIndexReader impl.
#[allow(dead_code)]
pub struct OnDiskIndexReader<B: StorageBackend + 'static> {
    backend_adapter: MutexStorageBackend<B>,
    cache: Arc<PageCache>,
    pub(crate) eavt_root: u64,
    pub(crate) aevt_root: u64,
    pub(crate) avet_root: u64,
    pub(crate) vaet_root: u64,
}

impl<B: StorageBackend + 'static> OnDiskIndexReader<B> {
    pub fn new(
        backend: Arc<Mutex<B>>,
        cache: Arc<PageCache>,
        eavt_root: u64,
        aevt_root: u64,
        avet_root: u64,
        vaet_root: u64,
    ) -> Self {
        OnDiskIndexReader {
            backend_adapter: MutexStorageBackend::new(backend),
            cache,
            eavt_root,
            aevt_root,
            avet_root,
            vaet_root,
        }
    }

    pub(crate) fn new_verified(
        backend: Arc<Mutex<B>>,
        cache: Arc<PageCache>,
        base_integrity: Arc<BasePageIntegrityCatalog>,
        eavt_root: u64,
        aevt_root: u64,
        avet_root: u64,
        vaet_root: u64,
    ) -> Self {
        OnDiskIndexReader {
            backend_adapter: MutexStorageBackend::verified(backend, base_integrity),
            cache,
            eavt_root,
            aevt_root,
            avet_root,
            vaet_root,
        }
    }
}

impl<B: StorageBackend + 'static> crate::storage::CommittedIndexReader for OnDiskIndexReader<B> {
    fn range_scan_eavt(
        &self,
        start: &crate::storage::index::EavtKey,
        end: Option<&crate::storage::index::EavtKey>,
    ) -> anyhow::Result<Vec<crate::storage::index::FactRef>> {
        if self.eavt_root == 0 {
            return Ok(vec![]);
        }
        range_scan(
            self.eavt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn visit_current_eavt_entries(
        &self,
        start: &EavtKey,
        end: Option<&EavtKey>,
        visit: &mut dyn for<'a> FnMut(CurrentEavtEntryRef<'a>, FactRef) -> Result<bool>,
    ) -> Result<bool> {
        if self.eavt_root == 0 {
            return Ok(true);
        }
        visit_current_eavt_range(
            self.eavt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }

    fn range_scan_aevt(
        &self,
        start: &crate::storage::index::AevtKey,
        end: Option<&crate::storage::index::AevtKey>,
    ) -> anyhow::Result<Vec<crate::storage::index::FactRef>> {
        if self.aevt_root == 0 {
            return Ok(vec![]);
        }
        range_scan(
            self.aevt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn visit_aevt_entries(
        &self,
        start: &crate::storage::index::AevtKey,
        end: Option<&crate::storage::index::AevtKey>,
        visit: &mut dyn FnMut(
            &crate::storage::index::AevtKey,
            crate::storage::index::FactRef,
        ) -> anyhow::Result<bool>,
    ) -> anyhow::Result<bool> {
        if self.aevt_root == 0 {
            return Ok(true);
        }
        visit_range_entries(
            self.aevt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }

    fn visit_current_aevt_entries(
        &self,
        start: &AevtKey,
        end: Option<&AevtKey>,
        visit: &mut dyn for<'a> FnMut(CurrentAevtEntryRef<'a>, FactRef) -> Result<bool>,
    ) -> Result<bool> {
        if self.aevt_root == 0 {
            return Ok(true);
        }
        visit_current_aevt_range(
            self.aevt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }

    fn range_scan_avet(
        &self,
        start: &crate::storage::index::AvetKey,
        end: Option<&crate::storage::index::AvetKey>,
    ) -> anyhow::Result<Vec<crate::storage::index::FactRef>> {
        if self.avet_root == 0 {
            return Ok(vec![]);
        }
        range_scan(
            self.avet_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn range_scan_vaet(
        &self,
        start: &crate::storage::index::VaetKey,
        end: Option<&crate::storage::index::VaetKey>,
    ) -> anyhow::Result<Vec<crate::storage::index::FactRef>> {
        if self.vaet_root == 0 {
            return Ok(vec![]);
        }
        range_scan(
            self.vaet_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn visit_current_vaet_entries(
        &self,
        start: &VaetKey,
        end: Option<&VaetKey>,
        visit: &mut dyn for<'a> FnMut(CurrentVaetEntryRef<'a>, FactRef) -> Result<bool>,
    ) -> Result<bool> {
        if self.vaet_root == 0 {
            return Ok(true);
        }
        visit_current_vaet_range(
            self.vaet_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }
}

impl<B: StorageBackend + 'static> crate::storage::delta_index::KeyedIndexReader
    for OnDiskIndexReader<B>
{
    fn range_scan_eavt_entries(
        &self,
        start: &crate::storage::index::EavtKey,
        end: Option<&crate::storage::index::EavtKey>,
    ) -> anyhow::Result<
        Vec<(
            crate::storage::index::EavtKey,
            crate::storage::index::FactRef,
        )>,
    > {
        if self.eavt_root == 0 {
            return Ok(Vec::new());
        }
        range_scan_entries(
            self.eavt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn visit_current_eavt_entries(
        &self,
        start: &EavtKey,
        end: Option<&EavtKey>,
        visit: &mut dyn for<'a> FnMut(CurrentEavtEntryRef<'a>, FactRef) -> Result<bool>,
    ) -> Result<bool> {
        if self.eavt_root == 0 {
            return Ok(true);
        }
        visit_current_eavt_range(
            self.eavt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }

    fn range_scan_aevt_entries(
        &self,
        start: &crate::storage::index::AevtKey,
        end: Option<&crate::storage::index::AevtKey>,
    ) -> anyhow::Result<
        Vec<(
            crate::storage::index::AevtKey,
            crate::storage::index::FactRef,
        )>,
    > {
        if self.aevt_root == 0 {
            return Ok(Vec::new());
        }
        range_scan_entries(
            self.aevt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn visit_aevt_entries(
        &self,
        start: &crate::storage::index::AevtKey,
        end: Option<&crate::storage::index::AevtKey>,
        visit: &mut dyn FnMut(
            &crate::storage::index::AevtKey,
            crate::storage::index::FactRef,
        ) -> anyhow::Result<bool>,
    ) -> anyhow::Result<bool> {
        if self.aevt_root == 0 {
            return Ok(true);
        }
        visit_range_entries(
            self.aevt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }

    fn visit_current_aevt_entries(
        &self,
        start: &AevtKey,
        end: Option<&AevtKey>,
        visit: &mut dyn for<'a> FnMut(CurrentAevtEntryRef<'a>, FactRef) -> Result<bool>,
    ) -> Result<bool> {
        if self.aevt_root == 0 {
            return Ok(true);
        }
        visit_current_aevt_range(
            self.aevt_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }

    fn range_scan_avet_entries(
        &self,
        start: &crate::storage::index::AvetKey,
        end: Option<&crate::storage::index::AvetKey>,
    ) -> anyhow::Result<
        Vec<(
            crate::storage::index::AvetKey,
            crate::storage::index::FactRef,
        )>,
    > {
        if self.avet_root == 0 {
            return Ok(Vec::new());
        }
        range_scan_entries(
            self.avet_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn range_scan_vaet_entries(
        &self,
        start: &crate::storage::index::VaetKey,
        end: Option<&crate::storage::index::VaetKey>,
    ) -> anyhow::Result<
        Vec<(
            crate::storage::index::VaetKey,
            crate::storage::index::FactRef,
        )>,
    > {
        if self.vaet_root == 0 {
            return Ok(Vec::new());
        }
        range_scan_entries(
            self.vaet_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
        )
    }

    fn visit_current_vaet_entries(
        &self,
        start: &VaetKey,
        end: Option<&VaetKey>,
        visit: &mut dyn for<'a> FnMut(CurrentVaetEntryRef<'a>, FactRef) -> Result<bool>,
    ) -> Result<bool> {
        if self.vaet_root == 0 {
            return Ok(true);
        }
        visit_current_vaet_range(
            self.vaet_root,
            start,
            end,
            &self.backend_adapter,
            &self.cache,
            visit,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::Value;
    use crate::storage::backend::MemoryBackend;
    use crate::storage::index::{AevtKey, EavtKey, FactRef, VaetKey, encode_value};
    use uuid::Uuid;

    #[test]
    fn btree_fill_percent_is_bounded() {
        assert!(BtreeBuildOptions::new(49).is_err());
        assert!(BtreeBuildOptions::new(50).is_ok());
        assert!(BtreeBuildOptions::new(100).is_ok());
        assert!(BtreeBuildOptions::new(101).is_err());
    }

    #[test]
    fn lazy_key_serialization_matches_eager_tree_bytes() {
        let entries = (0u128..200)
            .map(|n| make_eavt(n, ":lazy", n as u64 + 1))
            .collect::<Vec<_>>();
        let mut eager = MemoryBackend::new();
        let mut lazy = MemoryBackend::new();
        let eager_cache = PageCache::new(256);
        let lazy_cache = PageCache::new(256);
        let serialized = btree_entries(entries.iter().cloned()).unwrap();
        let eager_result = build_btree_with_options(
            serialized.into_iter(),
            &mut eager,
            &eager_cache,
            1,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        let lazy_result = build_btree_from_key_entries(
            entries.into_iter(),
            &mut lazy,
            &lazy_cache,
            1,
            BtreeBuildOptions::default(),
        )
        .unwrap();
        assert_eq!(lazy_result, eager_result);
        for page_id in 1..eager_result.1 {
            assert_eq!(
                lazy.read_page(page_id).unwrap(),
                eager.read_page(page_id).unwrap()
            );
        }
    }

    fn make_eavt(n: u128, attr: &str, tx: u64) -> (EavtKey, FactRef) {
        (
            EavtKey {
                entity: Uuid::from_u128(n),
                attribute: attr.to_string(),
                valid_from: 0,
                valid_to: i64::MAX,
                tx_count: tx,
                value_bytes: encode_value(&Value::Integer(tx.cast_signed())),
                tx_id: tx,
                asserted: true,
            },
            FactRef {
                page_id: tx + 1,
                slot_index: 0,
            },
        )
    }

    fn make_aevt(n: u128, attr: &str, tx: u64) -> (AevtKey, FactRef) {
        (
            AevtKey {
                attribute: attr.to_string(),
                entity: Uuid::from_u128(n),
                valid_from: 0,
                valid_to: i64::MAX,
                tx_count: tx,
                value_bytes: encode_value(&Value::Integer(tx.cast_signed())),
                tx_id: tx,
                asserted: true,
            },
            FactRef {
                page_id: tx + 1,
                slot_index: 0,
            },
        )
    }

    fn make_vaet(n: u128, attr: &str, tx: u64) -> (VaetKey, FactRef) {
        (
            VaetKey {
                ref_target: Uuid::from_u128(500),
                attribute: attr.to_owned(),
                valid_from: 0,
                valid_to: i64::MAX,
                source_entity: Uuid::from_u128(n),
                tx_count: tx,
                tx_id: tx,
                asserted: true,
            },
            FactRef {
                page_id: tx + 1,
                slot_index: 0,
            },
        )
    }

    #[allow(clippy::indexing_slicing)]
    fn raw_leaf_page(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut page = vec![0u8; PAGE_SIZE];
        page[0] = PAGE_TYPE_LEAF;
        page[2..4].copy_from_slice(&u16::try_from(entries.len()).unwrap().to_le_bytes());
        let mut data_end = PAGE_SIZE;
        for (index, entry) in entries.iter().enumerate() {
            data_end -= entry.len();
            page[data_end..data_end + entry.len()].copy_from_slice(entry);
            let slot = LEAF_HEADER_SIZE + index * SLOT_SIZE;
            page[slot..slot + 2].copy_from_slice(&u16::try_from(data_end).unwrap().to_le_bytes());
            page[slot + 2..slot + 4]
                .copy_from_slice(&u16::try_from(entry.len()).unwrap().to_le_bytes());
        }
        page
    }

    #[test]
    fn test_read_u16_at_oob_rejected() {
        let page = vec![0u8; 4];
        assert!(read_u16_at(&page, 3).is_err());
        assert!(read_u16_at(&page, 4).is_err());
    }

    #[test]
    fn test_read_u64_at_oob_rejected() {
        let page = vec![0u8; 4];
        assert!(read_u64_at(&page, 0).is_err());
        assert!(read_u64_at(&page, 1).is_err());
    }

    #[test]
    fn test_build_btree_empty_returns_single_leaf() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(64);
        let entries: Vec<(EavtKey, FactRef)> = vec![];
        let ser = btree_entries(entries.into_iter()).unwrap();
        let (root, next_free) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();
        assert_eq!(root, 1, "root must be at start_page_id");
        assert_eq!(next_free, 2, "single empty leaf = 1 page");
        // Verify it is a leaf page
        let page = cache.get_or_load(1, &backend).unwrap();
        assert!(is_leaf_page_type(page[0]));
        let entry_count = read_u16_at(&page[..], 2).unwrap();
        assert_eq!(entry_count, 0);
    }

    #[test]
    fn test_build_btree_single_entry() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(64);
        let entries = vec![make_eavt(1, ":name", 1)];
        let ser = btree_entries(entries.into_iter()).unwrap();
        let (root, next_free) = build_btree(ser.into_iter(), &mut backend, &cache, 5).unwrap();
        assert_eq!(root, 5);
        assert_eq!(next_free, 6);
        let page = cache.get_or_load(5, &backend).unwrap();
        assert!(is_leaf_page_type(page[0]));
        assert_eq!(read_u16_at(&page[..], 2).unwrap(), 1);
    }

    #[test]
    fn prefix_leaf_roundtrips_and_rejects_corrupt_prefixes() {
        let expected = (0u128..32)
            .map(|n| make_eavt(n, ":shared/attribute", n as u64 + 1))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(8);
        write_leaf_page(&mut backend, &cache, 1, &serialized, 0).unwrap();

        let page = backend.read_page(1).unwrap();
        assert_eq!(page[0], PAGE_TYPE_PREFIX_LEAF);
        assert_eq!(
            read_leaf_entries::<EavtKey>(&page).unwrap(),
            expected,
            "prefix-compressed leaves must preserve exact sorted entries"
        );

        let mut corrupt_continuation = page.clone();
        let continuation_slot = LEAF_HEADER_SIZE + SLOT_SIZE;
        let continuation_offset =
            usize::from(read_u16_at(&corrupt_continuation, continuation_slot).unwrap());
        corrupt_continuation[continuation_offset..continuation_offset + 2]
            .copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(
            read_leaf_entries::<EavtKey>(&corrupt_continuation).is_err(),
            "a prefix beyond the previous decoded entry must be rejected"
        );

        let mut corrupt_restart = page;
        let restart_slot = LEAF_HEADER_SIZE + PREFIX_RESTART_INTERVAL * SLOT_SIZE;
        let restart_offset = usize::from(read_u16_at(&corrupt_restart, restart_slot).unwrap());
        corrupt_restart[restart_offset..restart_offset + 2].copy_from_slice(&1u16.to_le_bytes());
        assert!(
            read_leaf_entries::<EavtKey>(&corrupt_restart).is_err(),
            "restart records must never depend on an earlier entry"
        );
        let mut cursor = LeafEntryCursor::<EavtKey>::new(Arc::new(corrupt_restart)).unwrap();
        assert!(
            cursor
                .seek_lower_bound(&expected[PREFIX_RESTART_INTERVAL].0)
                .is_err(),
            "the page-backed cursor must reject a corrupt restart"
        );
    }

    #[test]
    fn raw_leaf_cursor_seeks_before_exact_between_and_after() {
        let expected = (1u128..=32)
            .map(|n| make_eavt(n, ":raw", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let page = Arc::new(raw_leaf_page(&serialized));
        assert_eq!(page[0], PAGE_TYPE_LEAF);

        let starts = [
            (make_eavt(0, ":raw", 0).0, Some(0usize)),
            (expected[10].0.clone(), Some(10)),
            (
                EavtKey {
                    attribute: ":zz".to_string(),
                    ..expected[10].0.clone()
                },
                Some(11),
            ),
            (make_eavt(33, ":raw", 33).0, None),
        ];
        for (start, expected_index) in starts {
            let mut cursor = LeafEntryCursor::<EavtKey>::new(page.clone()).unwrap();
            cursor.seek_lower_bound(&start).unwrap();
            let actual = cursor.next_entry().unwrap();
            assert_eq!(actual, expected_index.map(|index| expected[index].clone()));
        }
    }

    #[test]
    fn prefix_leaf_cursor_seeks_from_one_restart_block() {
        let expected = (1u128..=48)
            .map(|n| make_aevt(n, ":shared/attribute", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(8);
        write_leaf_page(&mut backend, &cache, 1, &serialized, 0).unwrap();
        let page = Arc::new(backend.read_page(1).unwrap());
        assert_eq!(page[0], PAGE_TYPE_PREFIX_LEAF);

        let starts = [
            (make_aevt(0, ":shared/attribute", 0).0, Some(0usize)),
            (expected[25].0.clone(), Some(25)),
            (
                AevtKey {
                    valid_from: 1,
                    ..expected[25].0.clone()
                },
                Some(26),
            ),
            (make_aevt(49, ":shared/attribute", 49).0, None),
        ];
        for (start, expected_index) in starts {
            let mut cursor = LeafEntryCursor::<AevtKey>::new(page.clone()).unwrap();
            cursor.seek_lower_bound(&start).unwrap();
            let actual = cursor.next_entry().unwrap();
            assert_eq!(actual, expected_index.map(|index| expected[index].clone()));
        }

        #[cfg(feature = "bench-internals")]
        {
            reset_leaf_read_diagnostics();
            set_leaf_read_diagnostics_enabled(true);
            let mut cursor = LeafEntryCursor::<AevtKey>::new(page).unwrap();
            cursor.seek_lower_bound(&expected[25].0).unwrap();
            let diagnostics = leaf_read_diagnostics();
            set_leaf_read_diagnostics_enabled(false);
            assert!(diagnostics.prefix_entries_reconstructed > 0);
            assert!(
                diagnostics
                    .prefix_entries_reconstructed
                    .saturating_sub(diagnostics.prefix_restart_blocks_reconstructed)
                    < u64::try_from(PREFIX_RESTART_INTERVAL).unwrap()
            );
        }
    }

    #[test]
    fn projected_aevt_visitor_obeys_raw_bounds_and_early_stop() {
        let expected = (1u128..=32)
            .map(|n| make_aevt(n, ":raw/aevt", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        backend.write_page(1, &raw_leaf_page(&serialized)).unwrap();
        let cache = PageCache::new(8);
        let mut seen = Vec::new();
        let complete = visit_current_aevt_range(
            1,
            &expected[9].0,
            Some(&expected[20].0),
            &backend,
            &cache,
            &mut |entry, fact_ref| {
                seen.push((entry.entity, entry.tx_count, fact_ref));
                Ok(seen.len() < 3)
            },
        )
        .unwrap();
        assert!(!complete);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, expected[9].0.entity);
        assert_eq!(seen[2].2, expected[11].1);
    }

    #[test]
    fn projected_eavt_visitor_obeys_raw_bounds_and_early_stop() {
        let expected = (1u128..=32)
            .map(|n| make_eavt(n, ":raw/eavt", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        backend.write_page(1, &raw_leaf_page(&serialized)).unwrap();
        let cache = PageCache::new(8);
        let mut seen = Vec::new();
        let complete = visit_current_eavt_range(
            1,
            &expected[9].0,
            Some(&expected[20].0),
            &backend,
            &cache,
            &mut |entry, fact_ref| {
                seen.push((entry.entity, entry.attribute.to_owned(), fact_ref));
                Ok(seen.len() < 3)
            },
        )
        .unwrap();
        assert!(!complete);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, expected[9].0.entity);
        assert_eq!(seen[0].1, ":raw/eavt");
        assert_eq!(seen[2].2, expected[11].1);
    }

    #[test]
    fn projected_vaet_visitor_obeys_raw_bounds_and_early_stop() {
        let expected = (1u128..=32)
            .map(|n| make_vaet(n, ":edge/to", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        backend.write_page(1, &raw_leaf_page(&serialized)).unwrap();
        let cache = PageCache::new(8);
        let mut seen = Vec::new();
        let complete = visit_current_vaet_range(
            1,
            &expected[9].0,
            Some(&expected[20].0),
            &backend,
            &cache,
            &mut |entry, fact_ref| {
                seen.push((entry.source_entity, entry.attribute.to_owned(), fact_ref));
                Ok(seen.len() < 3)
            },
        )
        .unwrap();
        assert!(!complete);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, expected[9].0.source_entity);
        assert_eq!(seen[2].2, expected[11].1);
    }

    #[test]
    fn projected_aevt_visitor_reconstructs_prefix_restart_range() {
        let expected = (1u128..=48)
            .map(|n| make_aevt(n, ":shared/projected", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(8);
        write_leaf_page(&mut backend, &cache, 1, &serialized, 0).unwrap();
        assert_eq!(backend.read_page(1).unwrap()[0], PAGE_TYPE_PREFIX_LEAF);

        let mut seen = Vec::new();
        let complete = visit_current_aevt_range(
            1,
            &expected[17].0,
            Some(&expected[35].0),
            &backend,
            &cache,
            &mut |entry, fact_ref| {
                seen.push((entry.entity, entry.value_bytes.to_vec(), fact_ref));
                Ok(true)
            },
        )
        .unwrap();
        assert!(complete);
        assert_eq!(seen.len(), 18);
        assert_eq!(
            seen.first().map(|entry| entry.0),
            Some(expected[17].0.entity)
        );
        assert_eq!(seen.last().map(|entry| entry.2), Some(expected[34].1));
    }

    #[test]
    fn projected_eavt_visitor_reconstructs_prefix_restart_range() {
        let expected = (1u128..=48)
            .map(|n| make_eavt(n, ":shared/projected", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(8);
        write_leaf_page(&mut backend, &cache, 1, &serialized, 0).unwrap();
        assert_eq!(backend.read_page(1).unwrap()[0], PAGE_TYPE_PREFIX_LEAF);

        let mut seen = Vec::new();
        let complete = visit_current_eavt_range(
            1,
            &expected[17].0,
            Some(&expected[35].0),
            &backend,
            &cache,
            &mut |entry, fact_ref| {
                seen.push((entry.entity, entry.value_bytes.to_vec(), fact_ref));
                Ok(true)
            },
        )
        .unwrap();
        assert!(complete);
        assert_eq!(seen.len(), 18);
        assert_eq!(
            seen.first().map(|entry| entry.0),
            Some(expected[17].0.entity)
        );
        assert_eq!(seen.last().map(|entry| entry.2), Some(expected[34].1));
    }

    #[test]
    fn projected_vaet_visitor_reconstructs_prefix_restart_range() {
        let expected = (1u128..=48)
            .map(|n| make_vaet(n, ":shared/reference", n as u64))
            .collect::<Vec<_>>();
        let serialized = btree_entries(expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(8);
        write_leaf_page(&mut backend, &cache, 1, &serialized, 0).unwrap();
        assert_eq!(backend.read_page(1).unwrap()[0], PAGE_TYPE_PREFIX_LEAF);

        let mut seen = Vec::new();
        let complete = visit_current_vaet_range(
            1,
            &expected[17].0,
            Some(&expected[35].0),
            &backend,
            &cache,
            &mut |entry, fact_ref| {
                seen.push((entry.source_entity, fact_ref));
                Ok(true)
            },
        )
        .unwrap();
        assert!(complete);
        assert_eq!(seen.len(), 18);
        assert_eq!(
            seen.first().map(|entry| entry.0),
            Some(expected[17].0.source_entity)
        );
        assert_eq!(seen.last().map(|entry| entry.1), Some(expected[34].1));
    }

    #[test]
    fn leaf_cursor_rejects_malformed_slot_restart_and_serialized_entry() {
        let raw_expected = (1u128..=8)
            .map(|n| make_eavt(n, ":raw", n as u64))
            .collect::<Vec<_>>();
        let raw_serialized = btree_entries(raw_expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(8);
        let raw = raw_leaf_page(&raw_serialized);
        assert_eq!(raw[0], PAGE_TYPE_LEAF);

        let mut malformed_slot = raw.clone();
        malformed_slot[LEAF_HEADER_SIZE..LEAF_HEADER_SIZE + 2]
            .copy_from_slice(&u16::MAX.to_le_bytes());
        let mut cursor = LeafEntryCursor::<EavtKey>::new(Arc::new(malformed_slot)).unwrap();
        assert!(cursor.next_entry().is_err());

        let mut malformed_entry = raw;
        malformed_entry[LEAF_HEADER_SIZE + 2..LEAF_HEADER_SIZE + 4]
            .copy_from_slice(&1u16.to_le_bytes());
        let mut cursor = LeafEntryCursor::<EavtKey>::new(Arc::new(malformed_entry)).unwrap();
        assert!(cursor.next_entry().is_err());

        let prefix_expected = (1u128..=20)
            .map(|n| make_aevt(n, ":prefix", n as u64))
            .collect::<Vec<_>>();
        let prefix_serialized = btree_entries(prefix_expected.iter().cloned())
            .unwrap()
            .into_iter()
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        write_leaf_page(&mut backend, &cache, 2, &prefix_serialized, 0).unwrap();
        let mut prefix = backend.read_page(2).unwrap();
        assert_eq!(prefix[0], PAGE_TYPE_PREFIX_LEAF);
        prefix[1] = 8;
        assert!(LeafEntryCursor::<AevtKey>::new(Arc::new(prefix)).is_err());
    }

    #[test]
    fn test_build_btree_chained_next_free() {
        // Two sequential build_btree calls: second must start where first ended.
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(128);
        let entries1 = btree_entries((0u128..5).map(|n| make_eavt(n, ":a", n as u64 + 1))).unwrap();
        let (_, next1) = build_btree(entries1.into_iter(), &mut backend, &cache, 1).unwrap();

        let entries2 =
            btree_entries((5u128..10).map(|n| make_eavt(n, ":b", n as u64 + 1))).unwrap();
        let (root2, next2) =
            build_btree(entries2.into_iter(), &mut backend, &cache, next1).unwrap();

        assert!(root2 >= next1, "second tree must not overlap with first");
        assert!(next2 > root2);
    }

    #[test]
    fn test_build_btree_pages_in_cache_after_build() {
        // All written pages must be retrievable from cache without backend read
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        let entries =
            btree_entries((0u128..100).map(|n| make_eavt(n, ":x", n as u64 + 1))).unwrap();
        let (root, next_free) = build_btree(entries.into_iter(), &mut backend, &cache, 1).unwrap();

        let empty_backend = MemoryBackend::new();
        for page_id in root..next_free {
            let result = cache.get_or_load(page_id, &empty_backend);
            assert!(result.is_ok(), "page {} missing from cache", page_id);
        }
    }

    #[test]
    fn test_build_btree_fill_factor_no_overflow() {
        // With many entries, leaf pages must not exceed PAGE_SIZE
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        let entries = btree_entries(
            (0u128..200).map(|n| make_eavt(n, ":verylongattributename", n as u64 + 1)),
        )
        .unwrap();
        let (root, next_free) = build_btree(entries.into_iter(), &mut backend, &cache, 1).unwrap();

        for page_id in root..next_free {
            let page = cache.get_or_load(page_id, &backend).unwrap();
            assert_eq!(
                page.len(),
                PAGE_SIZE,
                "every page must be exactly PAGE_SIZE"
            );
        }
    }

    #[test]
    fn test_build_btree_internal_node_created_for_many_entries() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(512);
        // ~300 entries should force at least 2 leaf pages and 1 internal node
        let entries = (0u128..300).map(|n| make_eavt(n, ":attr", n as u64 + 1));
        let ser = btree_entries(entries).unwrap();
        let (root, next_free) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let root_page = cache.get_or_load(root, &backend).unwrap();
        let pages_written = next_free - 1;
        assert!(
            pages_written >= 2,
            "300 entries must need multiple pages; got {}",
            pages_written
        );
        // With 300 entries at the production fill factor, we always get multiple leaf
        // pages, so the root MUST be an internal node.
        assert_eq!(
            root_page[0], PAGE_TYPE_INTERNAL,
            "300 entries should produce an internal node root, got page type 0x{:02x}",
            root_page[0]
        );
    }

    #[test]
    fn test_merge_sorted_iters() {
        let a = vec![1u32, 3, 5, 7];
        let b = vec![2u32, 4, 6, 8];
        let merged: Vec<u32> = merge_sorted_iters(a.into_iter(), b.into_iter()).collect();
        assert_eq!(merged, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn test_merge_sorted_iters_empty_left() {
        let merged: Vec<u32> =
            merge_sorted_iters(Vec::<u32>::new().into_iter(), vec![1u32, 2, 3].into_iter())
                .collect();
        assert_eq!(merged, vec![1, 2, 3]);
    }

    #[test]
    fn test_build_btree_leaf_next_pointers_form_chain() {
        // Build a tree with enough entries to require multiple leaf pages,
        // then verify leaf[i].next_leaf == leaf[i+1].page_id
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        // ~100 entries with long keys should span 4-6 leaf pages
        let entries = (0u128..100).map(|n| make_eavt(n, ":verylongattributename", n as u64 + 1));
        let ser = btree_entries(entries).unwrap();
        let (root, next_free) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        // Collect leaf page IDs by following the chain from the leftmost leaf
        // The root may be an internal node; find the leftmost leaf first
        let root_page = cache.get_or_load(root, &backend).unwrap();
        let mut leaf_pid = if is_leaf_page_type(root_page[0]) {
            root
        } else {
            // leftmost leaf: follow first child of each internal node down
            let mut pid = root;
            loop {
                let p = cache.get_or_load(pid, &backend).unwrap();
                if is_leaf_page_type(p[0]) {
                    break pid;
                }
                // first child is at child_array[0] = bytes 12..20
                pid = read_u64_at(&p[..], 12).unwrap();
            }
        };

        // Walk the chain and verify it's contiguous and terminates
        let mut chain: Vec<u64> = vec![leaf_pid];
        loop {
            let p = cache.get_or_load(leaf_pid, &backend).unwrap();
            assert!(is_leaf_page_type(p[0]), "page {} should be leaf", leaf_pid);
            let next = read_u64_at(&p[..], 4).unwrap();
            if next == 0 {
                break;
            }
            chain.push(next);
            leaf_pid = next;
        }

        assert!(
            chain.len() >= 2,
            "100 long-key entries should span multiple leaves; got {} leaves",
            chain.len()
        );
        // Total entries across all leaves must equal 100
        let total_entries: u64 = chain
            .iter()
            .map(|&pid| {
                let p = cache.get_or_load(pid, &backend).unwrap();
                read_u16_at(&p[..], 2).unwrap() as u64
            })
            .sum();
        assert_eq!(total_entries, 100);
        // next_free must be > all leaf page IDs
        for &pid in &chain {
            assert!(
                pid < next_free,
                "leaf {} must be < next_free {}",
                pid,
                next_free
            );
        }
    }

    #[test]
    fn test_merge_sorted_iters_duplicates() {
        let a = vec![1u32, 3, 3, 5];
        let b = vec![2u32, 3, 4];
        let merged: Vec<u32> = merge_sorted_iters(a.into_iter(), b.into_iter()).collect();
        assert_eq!(merged, vec![1, 2, 3, 3, 3, 4, 5]);
    }

    #[test]
    fn test_stream_all_entries_roundtrip() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        let input: Vec<(EavtKey, FactRef)> = (0u128..50)
            .map(|n| make_eavt(n, ":name", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let output: Vec<(EavtKey, FactRef)> = stream_all_entries(root, &backend, &cache).unwrap();

        assert_eq!(output.len(), 50);
        for w in output.windows(2) {
            assert!(w[0].0 <= w[1].0, "entries must be in sorted order");
        }
        for (original, recovered) in input.iter().zip(output.iter()) {
            assert_eq!(original.1, recovered.1);
        }
    }

    #[test]
    fn test_stream_all_entries_empty_tree() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(16);
        let entries: Vec<(EavtKey, FactRef)> = vec![];
        let ser = btree_entries(entries.into_iter()).unwrap();
        let (root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();
        let out: Vec<(EavtKey, FactRef)> = stream_all_entries(root, &backend, &cache).unwrap();
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn test_range_scan_exact_match() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        let input: Vec<(EavtKey, FactRef)> = (0u128..100)
            .map(|n| make_eavt(n, ":v", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let target_entity = Uuid::from_u128(42);
        let start = EavtKey {
            entity: target_entity,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let next_entity = Uuid::from_u128(43);
        let end = EavtKey {
            entity: next_entity,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };

        let refs = range_scan(root, &start, Some(&end), &backend, &cache).unwrap();
        assert_eq!(refs.len(), 1, "exactly one entry for entity 42");
        // make_eavt(42, ":v", 43) → FactRef { page_id: 43+1=44, slot_index: 0 }
        assert_eq!(
            refs[0],
            FactRef {
                page_id: 44,
                slot_index: 0
            }
        );
    }

    #[test]
    fn test_range_scan_empty_range() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        let input: Vec<(EavtKey, FactRef)> = (0u128..50)
            .map(|n| make_eavt(n, ":v", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let start = EavtKey {
            entity: Uuid::from_u128(999),
            attribute: String::new(),
            valid_from: 0,
            valid_to: 0,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let refs = range_scan::<EavtKey>(root, &start, None, &backend, &cache).unwrap();
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_range_scan_unbounded_end() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(256);
        let input: Vec<(EavtKey, FactRef)> = (0u128..10)
            .map(|n| make_eavt(n, ":v", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let start = EavtKey {
            entity: Uuid::from_u128(5),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let refs = range_scan::<EavtKey>(root, &start, None, &backend, &cache).unwrap();
        assert_eq!(refs.len(), 5, "entities 5..9 = 5 entries");
    }

    #[test]
    fn test_range_scan_multi_leaf_span() {
        let mut backend = MemoryBackend::new();
        let cache = PageCache::new(512);
        let input: Vec<(EavtKey, FactRef)> = (0u128..500)
            .map(|n| make_eavt(n, ":a", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let start = EavtKey {
            entity: Uuid::from_u128(100),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end = EavtKey {
            entity: Uuid::from_u128(200),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let refs = range_scan(root, &start, Some(&end), &backend, &cache).unwrap();
        // NOTE: The end key has attribute="" which sorts BEFORE ":a". So entity 200's
        // actual entry {200, ":a", ...} sorts AFTER the end key and is EXCLUDED.
        // Result: entities 100..199 = 100 entries.
        assert_eq!(
            refs.len(),
            100,
            "entities 100..199 (end key excludes entity 200's entry since its attr ':a' > '')"
        );
    }

    #[test]
    fn test_on_disk_index_reader_range_scan_eavt() {
        use crate::storage::CommittedIndexReader;
        use std::sync::Arc;

        let mut backend = MemoryBackend::new();
        let cache = Arc::new(PageCache::new(256));
        let input: Vec<(EavtKey, FactRef)> = (0u128..20)
            .map(|n| make_eavt(n, ":x", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (eavt_root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        let reader =
            OnDiskIndexReader::new(Arc::new(Mutex::new(backend)), cache, eavt_root, 0, 0, 0);

        let start = EavtKey {
            entity: Uuid::from_u128(5),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end = EavtKey {
            entity: Uuid::from_u128(10),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let refs = reader.range_scan_eavt(&start, Some(&end)).unwrap();
        // Same exclusion logic: entity 10's entry {10, ":x", ...} > end {10, "", ...}
        // So entities 5..9 = 5 entries
        assert_eq!(refs.len(), 5, "entities 5..9 (end excludes entity 10)");
    }

    #[test]
    #[cfg(not(target_os = "wasi"))]
    fn test_concurrent_range_scans_correctness() {
        use crate::storage::CommittedIndexReader;
        use std::sync::{Arc, Barrier};
        use std::thread;

        let mut backend = MemoryBackend::new();
        // build_btree takes &PageCache (not Arc), so construct without Arc first
        let cache = PageCache::new(256);
        // 50 entries — enough to span multiple leaf pages
        let input: Vec<(EavtKey, FactRef)> = (0u128..50)
            .map(|n| make_eavt(n, ":x", n as u64 + 1))
            .collect();
        let ser = btree_entries(input.iter().cloned()).unwrap();
        let (eavt_root, _) = build_btree(ser.into_iter(), &mut backend, &cache, 1).unwrap();

        // Wrap in Arc after build_btree is done — OnDiskIndexReader requires Arc<PageCache>
        let reader = Arc::new(OnDiskIndexReader::new(
            Arc::new(Mutex::new(backend)),
            Arc::new(cache),
            eavt_root,
            0,
            0,
            0,
        ));

        // Scan entities 10..19 (10 entries expected)
        let start = EavtKey {
            entity: Uuid::from_u128(10),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end = EavtKey {
            entity: Uuid::from_u128(20),
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };

        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let r = Arc::clone(&reader);
                let b = Arc::clone(&barrier);
                let s = start.clone();
                let e = end.clone();
                thread::spawn(move || {
                    b.wait(); // all 8 threads start simultaneously
                    r.range_scan_eavt(&s, Some(&e)).unwrap()
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let expected_len = results[0].len();
        assert_eq!(expected_len, 10, "expected 10 entries for entities 10..19");
        for (i, res) in results.iter().enumerate() {
            assert_eq!(
                res.len(),
                expected_len,
                "thread {} returned {} refs, expected {}",
                i,
                res.len(),
                expected_len
            );
        }
    }
}
