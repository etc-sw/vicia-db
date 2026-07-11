use crate::storage::{FORMAT_VERSION, FileHeader, PAGE_SIZE, StorageBackend};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

/// A page inside a sparse backend's declared image has not been staged yet.
///
/// Browser callers can downcast this error, fetch the named page from
/// IndexedDB, stage it as clean, and retry the synchronous storage operation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct PageNotResident(u64);

impl PageNotResident {
    /// Return the logical page ID that must be fetched.
    pub(crate) fn page_id(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PageNotResident {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Page {} is not resident", self.0)
    }
}

impl Error for PageNotResident {}

/// Return the missing page ID when `error` came from a sparse buffer read.
pub(crate) fn page_not_resident_id(error: &anyhow::Error) -> Option<u64> {
    error
        .downcast_ref::<PageNotResident>()
        .map(|missing| missing.page_id())
}

/// Synchronous in-memory page buffer with dirty-page tracking.
///
/// Implements `StorageBackend` so it can be used with `PersistentFactStorage`.
/// After `PersistentFactStorage::save()` writes updated pages here, call
/// `take_dirty()` to retrieve the page IDs that must be flushed to IndexedDB.
pub struct BrowserBufferBackend {
    pages: HashMap<u64, Vec<u8>>,
    dirty: HashSet<u64>,
    /// Pages that stay resident after they become clean. Dirty pages are
    /// always non-evictable independently of this set.
    pinned: HashSet<u64>,
    /// Logical size of the backing page image. In sparse mode this is larger
    /// than `pages.len()` while demand pages are not resident.
    logical_page_count: u64,
    sparse: bool,
}

impl BrowserBufferBackend {
    /// Create an empty buffer (new database).
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
            dirty: HashSet::new(),
            pinned: HashSet::new(),
            logical_page_count: 0,
            sparse: false,
        }
    }

    /// Load pages from an existing snapshot. Dirty set starts empty.
    /// Used during `BrowserDb::open()` after fetching all pages from IndexedDB.
    pub fn load_pages(pages: HashMap<u64, Vec<u8>>) -> Self {
        let logical_page_count = pages.len() as u64;
        Self {
            pages,
            dirty: HashSet::new(),
            pinned: HashSet::new(),
            logical_page_count,
            sparse: false,
        }
    }

    /// Load pages and mark every page dirty.
    /// Used during `BrowserDb::import_graph()` so all pages are flushed to IDB.
    pub fn load_pages_all_dirty(pages: HashMap<u64, Vec<u8>>) -> Self {
        let dirty: HashSet<u64> = pages.keys().copied().collect();
        let logical_page_count = pages.len() as u64;
        Self {
            pages,
            dirty,
            pinned: HashSet::new(),
            logical_page_count,
            sparse: false,
        }
    }

    /// Create a sparse current-format buffer from validated resident pages.
    ///
    /// Page 0 must be present and publish `declared_page_count`. All resident
    /// and pinned IDs must lie inside that published prefix, and pinned pages
    /// must already be resident. The supplied pages start clean.
    pub(crate) fn load_sparse_pages(
        pages: HashMap<u64, Vec<u8>>,
        declared_page_count: u64,
        pinned_page_ids: HashSet<u64>,
    ) -> Result<Self> {
        if declared_page_count == 0 {
            anyhow::bail!("Sparse browser image must declare at least page 0");
        }
        for (page_id, page) in &pages {
            validate_complete_page(*page_id, page)?;
            if *page_id >= declared_page_count {
                anyhow::bail!(
                    "Resident page {} is outside declared page count {}",
                    page_id,
                    declared_page_count
                );
            }
        }
        for page_id in &pinned_page_ids {
            if !pages.contains_key(page_id) {
                anyhow::bail!("Pinned page {} is not resident", page_id);
            }
        }

        let page_zero = pages
            .get(&0)
            .ok_or_else(|| anyhow::anyhow!("Sparse browser image is missing page 0"))?;
        let header = FileHeader::from_bytes(page_zero)?;
        header.validate()?;
        if header.version != FORMAT_VERSION {
            anyhow::bail!(
                "Sparse browser paging requires format v{}, found v{}",
                FORMAT_VERSION,
                header.version
            );
        }
        if header.page_count != declared_page_count {
            anyhow::bail!(
                "Sparse browser page count mismatch: page 0 publishes {}, loader declared {}",
                header.page_count,
                declared_page_count
            );
        }

        Ok(Self {
            pages,
            dirty: HashSet::new(),
            pinned: pinned_page_ids,
            logical_page_count: declared_page_count,
            sparse: true,
        })
    }

    /// Bootstrap a sparse current-format buffer with page 0 pinned.
    pub(crate) fn load_sparse_page_zero(page_zero: Vec<u8>) -> Result<Self> {
        validate_complete_page(0, &page_zero)?;
        let header = FileHeader::from_bytes(&page_zero)?;
        let declared_page_count = header.page_count;
        Self::load_sparse_pages(
            HashMap::from([(0, page_zero)]),
            declared_page_count,
            HashSet::from([0]),
        )
    }

    /// Stage one complete page fetched from the durable backing store.
    ///
    /// Staging never marks a page dirty and refuses to overwrite a local dirty
    /// page with older durable bytes.
    pub(crate) fn stage_clean_page(&mut self, page_id: u64, page: Vec<u8>) -> Result<()> {
        self.stage_clean_pages([(page_id, page)])
    }

    /// Atomically stage a batch of complete pages as clean residents.
    pub(crate) fn stage_clean_pages(
        &mut self,
        pages: impl IntoIterator<Item = (u64, Vec<u8>)>,
    ) -> Result<()> {
        let pages: Vec<(u64, Vec<u8>)> = pages.into_iter().collect();
        let mut page_ids = HashSet::with_capacity(pages.len());
        for (page_id, page) in &pages {
            validate_complete_page(*page_id, page)?;
            if *page_id >= self.logical_page_count {
                anyhow::bail!(
                    "Staged page {} is outside logical page count {}",
                    page_id,
                    self.logical_page_count
                );
            }
            if self.dirty.contains(page_id) {
                anyhow::bail!("Cannot stage clean bytes over dirty page {}", page_id);
            }
            if !page_ids.insert(*page_id) {
                anyhow::bail!("Sparse page batch contains duplicate page {}", page_id);
            }
        }
        self.pages.extend(pages);
        Ok(())
    }

    /// Replace the exact set of pages retained as clean browser authority.
    pub(crate) fn replace_pinned_pages(
        &mut self,
        page_ids: impl IntoIterator<Item = u64>,
    ) -> Result<()> {
        let page_ids: HashSet<u64> = page_ids.into_iter().collect();
        for page_id in &page_ids {
            if *page_id >= self.logical_page_count {
                anyhow::bail!(
                    "Cannot pin page {} outside logical page count {}",
                    page_id,
                    self.logical_page_count
                );
            }
            if !self.pages.contains_key(page_id) {
                anyhow::bail!("Cannot pin non-resident page {}", page_id);
            }
        }
        self.pinned = page_ids;
        Ok(())
    }

    /// Convert a complete, durable current-format candidate to sparse mode.
    ///
    /// This is the post-commit transition for import and maintenance. It
    /// refuses incomplete candidates and unflushed dirty pages, then replaces
    /// the authority pin set without changing any page bytes.
    pub(crate) fn configure_sparse_residency(
        &mut self,
        pinned_page_ids: impl IntoIterator<Item = u64>,
    ) -> Result<u64> {
        let pinned_page_ids: HashSet<u64> = pinned_page_ids.into_iter().collect();
        let declared_page_count = self.validate_sparse_residency_candidate(&pinned_page_ids)?;

        self.pinned = pinned_page_ids;
        self.logical_page_count = declared_page_count;
        self.sparse = true;
        Ok(declared_page_count)
    }

    /// Validate that a complete resident image can become a bounded sparse
    /// authority without changing its current residency mode.
    ///
    /// Strict browser import uses this before publishing candidate pages so a
    /// later `openPaged()` cannot discover that the committed image is only a
    /// legacy recovery state or is physically incomplete.
    pub(crate) fn validate_sparse_residency(
        &self,
        pinned_page_ids: impl IntoIterator<Item = u64>,
    ) -> Result<u64> {
        let pinned_page_ids: HashSet<u64> = pinned_page_ids.into_iter().collect();
        self.validate_sparse_residency_candidate(&pinned_page_ids)
    }

    fn validate_sparse_residency_candidate(&self, pinned_page_ids: &HashSet<u64>) -> Result<u64> {
        if !self.dirty.is_empty() {
            anyhow::bail!("Cannot configure sparse residency with dirty pages");
        }
        let declared_page_count = self.exportable_page_count()?;
        if self.logical_page_count != declared_page_count {
            anyhow::bail!(
                "Sparse candidate page count mismatch: backend has {}, page 0 publishes {}",
                self.logical_page_count,
                declared_page_count
            );
        }
        let page_zero = self
            .pages
            .get(&0)
            .ok_or_else(|| anyhow::anyhow!("Sparse browser image is missing page 0"))?;
        let header = FileHeader::from_bytes(page_zero)?;
        if header.version != FORMAT_VERSION {
            anyhow::bail!(
                "Sparse browser paging requires format v{}, found v{}",
                FORMAT_VERSION,
                header.version
            );
        }
        for page_id in pinned_page_ids {
            if *page_id >= declared_page_count {
                anyhow::bail!(
                    "Cannot pin page {} outside declared page count {}",
                    page_id,
                    declared_page_count
                );
            }
            if !self.pages.contains_key(page_id) {
                anyhow::bail!("Cannot pin non-resident page {}", page_id);
            }
        }
        Ok(declared_page_count)
    }

    /// Evict clean, unpinned pages until the resident set reaches `limit`.
    ///
    /// If dirty and pinned pages alone exceed the limit, all eligible pages
    /// are removed and the unavoidable residents remain. The lowest eligible
    /// page IDs are removed first to keep behavior deterministic in tests.
    pub(crate) fn evict_clean_unpinned_to(&mut self, limit: usize) -> usize {
        if !self.sparse || self.pages.len() <= limit {
            return 0;
        }
        let mut candidates: Vec<u64> = self
            .pages
            .keys()
            .filter(|page_id| !self.dirty.contains(page_id) && !self.pinned.contains(page_id))
            .copied()
            .collect();
        candidates.sort_unstable();

        let mut removed = 0usize;
        for page_id in candidates {
            if self.pages.len() <= limit {
                break;
            }
            if self.pages.remove(&page_id).is_some() {
                removed = removed.saturating_add(1);
            }
        }
        removed
    }

    /// Evict every clean page that is not pinned.
    pub(crate) fn evict_all_clean_unpinned(&mut self) -> usize {
        self.evict_clean_unpinned_to(0)
    }

    /// Return the number of currently resident pages.
    pub(crate) fn resident_page_count(&self) -> usize {
        self.pages.len()
    }

    /// Return the number of dirty resident pages.
    pub(crate) fn dirty_page_count(&self) -> usize {
        self.dirty.len()
    }

    /// Return the number of pinned resident pages.
    pub(crate) fn pinned_page_count(&self) -> usize {
        self.pinned.len()
    }

    /// Return whether a page is currently resident.
    pub(crate) fn is_page_resident(&self, page_id: u64) -> bool {
        self.pages.contains_key(&page_id)
    }

    /// Return whether this backend uses a sparse logical image.
    pub(crate) fn is_sparse(&self) -> bool {
        self.sparse
    }

    /// Drain and return the set of page IDs written since the last call.
    /// Clears the dirty set. Call after `pfs.save()` to get pages to flush.
    pub fn take_dirty(&mut self) -> HashSet<u64> {
        std::mem::take(&mut self.dirty)
    }

    /// Clone every page as `(page_id, bytes)` pairs, sorted by page id.
    /// Used by `BrowserDb::import_graph()` to flush the complete
    /// post-construction page set (including any pages written during format
    /// migration) to IndexedDB in one atomic replace.
    pub fn all_pages(&self) -> Vec<(u64, Vec<u8>)> {
        let mut pages: Vec<(u64, Vec<u8>)> = self
            .pages
            .iter()
            .map(|(id, data)| (*id, data.clone()))
            .collect();
        pages.sort_unstable_by_key(|(id, _)| *id);
        pages
    }

    /// Return the page count published by the validated page-0 header.
    ///
    /// Physical tail pages can remain after an interrupted copy-on-write
    /// candidate. They are not part of the portable graph image and must not
    /// leak through browser export or atomic replacement.
    pub fn declared_page_count(&self) -> Result<u64> {
        if self.pages.is_empty() {
            return Ok(0);
        }
        let page = self
            .pages
            .get(&0)
            .ok_or_else(|| anyhow::anyhow!("Page 0 not found"))?;
        let header = FileHeader::from_bytes(page)?;
        header.validate()?;
        if header.page_count == 0 {
            anyhow::bail!("Invalid header: published page_count is zero");
        }
        Ok(header.page_count)
    }

    /// Return the page count when every page in the published prefix exists.
    pub fn exportable_page_count(&self) -> Result<u64> {
        let page_count = self.declared_page_count()?;
        let mut resident_prefix_pages = 0u64;
        for (page_id, page) in &self.pages {
            if *page_id < page_count {
                validate_complete_page(*page_id, page)?;
                resident_prefix_pages = resident_prefix_pages
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("Resident browser page count overflow"))?;
            }
        }
        if resident_prefix_pages != page_count {
            anyhow::bail!(
                "Published database is truncated: expected {} prefix pages, found {}",
                page_count,
                resident_prefix_pages
            );
        }
        Ok(page_count)
    }

    /// Discard physical pages beyond page 0's published prefix.
    ///
    /// Missing pages inside the prefix are intentionally not rejected here:
    /// `PersistentFactStorage` may have recovered through the previous valid
    /// manifest. Export performs the stronger contiguity check.
    pub fn retain_declared_prefix(&mut self) -> Result<u64> {
        let page_count = self.declared_page_count()?;
        self.pages.retain(|page_id, _| *page_id < page_count);
        self.dirty.retain(|page_id| *page_id < page_count);
        self.pinned.retain(|page_id| *page_id < page_count);
        self.logical_page_count = page_count;
        Ok(page_count)
    }
}

impl Default for BrowserBufferBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserBufferBackend {
    /// Read a page by ID (delegates to `StorageBackend::read_page`, usable without the trait).
    pub fn read_page_raw(&self, page_id: u64) -> anyhow::Result<Vec<u8>> {
        self.read_page(page_id)
    }

    /// Return the number of pages stored (delegates to `StorageBackend::page_count`, usable without the trait).
    pub fn page_count_raw(&self) -> anyhow::Result<u64> {
        self.page_count()
    }
}

impl StorageBackend for BrowserBufferBackend {
    fn write_page(&mut self, page_id: u64, data: &[u8]) -> Result<()> {
        if data.len() != PAGE_SIZE {
            anyhow::bail!(
                "Invalid page size: {} bytes (expected {})",
                data.len(),
                PAGE_SIZE
            );
        }
        let next_page_count = page_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Page id overflow"))?;
        self.pages.insert(page_id, data.to_vec());
        self.dirty.insert(page_id);
        self.logical_page_count = self.logical_page_count.max(next_page_count);
        Ok(())
    }

    fn read_page(&self, page_id: u64) -> Result<Vec<u8>> {
        if let Some(page) = self.pages.get(&page_id) {
            return Ok(page.clone());
        }
        if self.sparse && page_id < self.logical_page_count {
            return Err(PageNotResident(page_id).into());
        }
        anyhow::bail!("Page {} not found", page_id)
    }

    fn sync(&mut self) -> Result<()> {
        Ok(()) // no-op: durability handled by IndexedDbBackend
    }

    fn page_count(&self) -> Result<u64> {
        Ok(self.logical_page_count)
    }

    fn has_complete_page_prefix(&self, published_page_count: u64) -> Result<bool> {
        Ok((0..published_page_count).all(|page_id| {
            self.pages
                .get(&page_id)
                .is_some_and(|page| page.len() == PAGE_SIZE)
        }))
    }

    fn close(&mut self) -> Result<()> {
        Ok(()) // no-op
    }

    fn backend_name(&self) -> &'static str {
        "browser-buffer"
    }

    fn is_new(&self) -> bool {
        self.logical_page_count == 0
    }
}

fn validate_complete_page(page_id: u64, page: &[u8]) -> Result<()> {
    if page.len() != PAGE_SIZE {
        anyhow::bail!(
            "Page {} has invalid length {} (expected {})",
            page_id,
            page.len(),
            PAGE_SIZE
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(byte: u8) -> Vec<u8> {
        vec![byte; PAGE_SIZE]
    }

    fn current_page_zero(page_count: u64) -> Vec<u8> {
        let mut header = FileHeader::new();
        header.page_count = page_count;
        crate::storage::header_extension::build_header_page(header).expect("header page")
    }

    #[test]
    fn write_marks_dirty() {
        let mut buf = BrowserBufferBackend::new();
        buf.write_page(0, &page(1)).unwrap();
        let dirty = buf.take_dirty();
        assert!(dirty.contains(&0));
    }

    #[test]
    fn take_dirty_clears_set() {
        let mut buf = BrowserBufferBackend::new();
        buf.write_page(0, &page(1)).unwrap();
        let _ = buf.take_dirty();
        assert!(buf.take_dirty().is_empty());
    }

    #[test]
    fn read_after_write_returns_same_bytes() {
        let mut buf = BrowserBufferBackend::new();
        let p = page(42);
        buf.write_page(3, &p).unwrap();
        assert_eq!(buf.read_page(3).unwrap(), p);
    }

    #[test]
    fn page_count_reflects_distinct_ids() {
        let mut buf = BrowserBufferBackend::new();
        buf.write_page(0, &page(0)).unwrap();
        buf.write_page(1, &page(1)).unwrap();
        buf.write_page(0, &page(2)).unwrap(); // overwrite
        assert_eq!(buf.page_count().unwrap(), 2);
    }

    #[test]
    fn load_pages_starts_with_no_dirty() {
        let pages = HashMap::from([(0u64, page(0)), (1u64, page(1))]);
        let mut buf = BrowserBufferBackend::load_pages(pages);
        assert!(buf.take_dirty().is_empty());
    }

    #[test]
    fn load_pages_all_dirty_marks_all() {
        let pages = HashMap::from([(0u64, page(0)), (1u64, page(1))]);
        let mut buf = BrowserBufferBackend::load_pages_all_dirty(pages);
        let dirty = buf.take_dirty();
        assert!(dirty.contains(&0));
        assert!(dirty.contains(&1));
    }

    // `#[wasm_bindgen_test]` (not plain `#[test]`): this module only compiles
    // on wasm32, where the wasm-bindgen harness silently skips libtest tests.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn all_pages_returns_every_page() {
        let pages = HashMap::from([(2u64, page(2)), (0u64, page(0)), (1u64, page(1))]);
        let buf = BrowserBufferBackend::load_pages(pages);
        let all = buf.all_pages();
        assert_eq!(all.len(), 3);
        let ids: Vec<u64> = all.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 1, 2], "pages must be sorted by id");
        assert_eq!(all[2].1, page(2));
    }

    #[test]
    fn is_new_true_when_empty() {
        assert!(BrowserBufferBackend::new().is_new());
    }

    #[test]
    fn is_new_false_after_write() {
        let mut buf = BrowserBufferBackend::new();
        buf.write_page(0, &page(0)).unwrap();
        assert!(!buf.is_new());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn retain_declared_prefix_drops_unpublished_tail() {
        let header = crate::storage::header_extension::build_header_page(FileHeader::new())
            .expect("header page");
        let pages = HashMap::from([(0u64, header), (1u64, page(99))]);
        let mut buf = BrowserBufferBackend::load_pages_all_dirty(pages);

        assert_eq!(buf.retain_declared_prefix().unwrap(), 1);
        assert_eq!(buf.page_count().unwrap(), 1);
        assert_eq!(buf.all_pages().len(), 1);
        assert!(buf.take_dirty().contains(&0));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn empty_buffer_has_zero_declared_and_exportable_pages() {
        let mut buf = BrowserBufferBackend::new();
        assert_eq!(buf.retain_declared_prefix().unwrap(), 0);
        assert_eq!(buf.exportable_page_count().unwrap(), 0);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn short_published_page_is_neither_complete_nor_exportable() {
        let mut header = FileHeader::new();
        header.page_count = 2;
        let mut page0 = header.to_bytes();
        page0.resize(PAGE_SIZE, 0);
        let pages = HashMap::from([(0u64, page0), (1u64, vec![0; PAGE_SIZE - 1])]);
        let buf = BrowserBufferBackend::load_pages(pages);

        assert!(!buf.has_complete_page_prefix(2).unwrap());
        assert!(
            buf.exportable_page_count().is_err(),
            "short published pages must not form a portable graph image"
        );
    }

    #[test]
    fn wrong_page_size_errors() {
        let mut buf = BrowserBufferBackend::new();
        assert!(buf.write_page(0, &[0u8; 100]).is_err());
    }

    #[test]
    fn read_missing_page_errors() {
        let buf = BrowserBufferBackend::new();
        assert!(buf.read_page(99).is_err());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn sparse_page_zero_keeps_logical_size_and_reports_demand_page() {
        let buf = BrowserBufferBackend::load_sparse_page_zero(current_page_zero(4))
            .expect("sparse page-zero bootstrap");

        assert!(buf.is_sparse());
        assert_eq!(buf.page_count().unwrap(), 4);
        assert_eq!(buf.resident_page_count(), 1);
        assert_eq!(buf.pinned_page_count(), 1);
        assert!(buf.is_page_resident(0));

        let missing = buf.read_page(2).expect_err("page 2 must require staging");
        assert_eq!(page_not_resident_id(&missing), Some(2));
        let outside = buf
            .read_page(4)
            .expect_err("page outside the logical image must fail");
        assert_eq!(page_not_resident_id(&outside), None);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn clean_staging_is_atomic_and_does_not_mark_pages_dirty() {
        let mut buf = BrowserBufferBackend::load_sparse_page_zero(current_page_zero(4))
            .expect("sparse page-zero bootstrap");
        buf.stage_clean_pages([(1, page(1)), (2, page(2))])
            .expect("stage clean range");

        assert_eq!(buf.resident_page_count(), 3);
        assert_eq!(buf.dirty_page_count(), 0);
        assert_eq!(buf.read_page(2).unwrap(), page(2));

        assert!(
            buf.stage_clean_pages([(3, page(3)), (4, page(4))]).is_err(),
            "an out-of-range page must reject the whole staged batch"
        );
        assert!(!buf.is_page_resident(3));

        buf.write_page(1, &page(9)).unwrap();
        assert!(
            buf.stage_clean_page(1, page(1)).is_err(),
            "durable bytes must not overwrite an unflushed dirty page"
        );
        assert_eq!(buf.read_page(1).unwrap(), page(9));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn bounded_eviction_keeps_pinned_and_dirty_pages() {
        let mut buf = BrowserBufferBackend::load_sparse_page_zero(current_page_zero(5))
            .expect("sparse page-zero bootstrap");
        buf.stage_clean_pages([(1, page(1)), (2, page(2)), (3, page(3))])
            .expect("stage clean pages");
        buf.replace_pinned_pages([0, 2])
            .expect("pin exact metadata pages");
        buf.write_page(3, &page(9)).unwrap();

        assert_eq!(buf.evict_clean_unpinned_to(1), 1);
        assert!(!buf.is_page_resident(1));
        assert!(buf.is_page_resident(0));
        assert!(buf.is_page_resident(2));
        assert!(buf.is_page_resident(3));
        assert_eq!(buf.resident_page_count(), 3);
        assert_eq!(buf.page_count().unwrap(), 5);

        let dirty = buf.take_dirty();
        assert!(dirty.contains(&3));
        assert_eq!(buf.evict_clean_unpinned_to(2), 1);
        assert!(!buf.is_page_resident(3));
        assert_eq!(buf.resident_page_count(), 2);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn retain_declared_prefix_resets_sparse_logical_count() {
        let mut buf = BrowserBufferBackend::load_sparse_page_zero(current_page_zero(4))
            .expect("sparse page-zero bootstrap");
        buf.stage_clean_pages([(1, page(1)), (3, page(3))])
            .expect("stage sparse residents");
        buf.replace_pinned_pages([0, 3])
            .expect("pin page zero and tail page");

        buf.write_page(0, &current_page_zero(2)).unwrap();
        assert_eq!(buf.retain_declared_prefix().unwrap(), 2);
        assert_eq!(buf.page_count().unwrap(), 2);
        assert_eq!(buf.resident_page_count(), 2);
        assert_eq!(buf.pinned_page_count(), 1);
        assert!(!buf.is_page_resident(3));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn sparse_constructor_rejects_invalid_pins_counts_and_legacy() {
        let pages = HashMap::from([(0, current_page_zero(3))]);
        assert!(
            BrowserBufferBackend::load_sparse_pages(pages.clone(), 3, HashSet::from([1])).is_err(),
            "a pinned page must already be resident"
        );
        assert!(
            BrowserBufferBackend::load_sparse_pages(pages, 2, HashSet::from([0])).is_err(),
            "loader and page-zero counts must agree"
        );

        let mut legacy_header = FileHeader::new();
        legacy_header.version = FORMAT_VERSION - 1;
        let legacy_page_zero = crate::storage::header_extension::build_header_page(legacy_header)
            .expect("legacy header page");
        assert!(
            BrowserBufferBackend::load_sparse_page_zero(legacy_page_zero).is_err(),
            "legacy images must take the eager migration path"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn complete_candidate_can_switch_to_sparse_and_replace_authority_pins() {
        let pages = HashMap::from([(0, current_page_zero(3)), (1, page(1)), (2, page(2))]);
        let mut buf = BrowserBufferBackend::load_pages(pages);

        assert_eq!(
            buf.configure_sparse_residency([0, 2])
                .expect("configure complete candidate"),
            3
        );
        assert!(buf.is_sparse());
        assert_eq!(buf.pinned_page_count(), 2);
        assert_eq!(buf.evict_all_clean_unpinned(), 1);
        assert!(!buf.is_page_resident(1));

        buf.replace_pinned_pages([0])
            .expect("replace exact authority pins");
        assert_eq!(buf.pinned_page_count(), 1);
        assert_eq!(buf.evict_all_clean_unpinned(), 1);
        assert_eq!(buf.resident_page_count(), 1);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn eager_buffer_never_evicts_its_only_page_image() {
        let pages = HashMap::from([(0, current_page_zero(2)), (1, page(1))]);
        let mut buf = BrowserBufferBackend::load_pages(pages);

        assert_eq!(buf.evict_all_clean_unpinned(), 0);
        assert_eq!(buf.resident_page_count(), 2);
        assert_eq!(buf.read_page(1).unwrap(), page(1));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn dirty_candidate_cannot_switch_to_sparse_residency() {
        let pages = HashMap::from([(0, current_page_zero(1))]);
        let mut buf = BrowserBufferBackend::load_pages_all_dirty(pages);
        assert!(
            buf.configure_sparse_residency([0]).is_err(),
            "candidate must be durable before sparse conversion"
        );
        assert!(!buf.is_sparse());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn unpublished_tail_must_be_trimmed_before_sparse_conversion() {
        let pages = HashMap::from([(0, current_page_zero(2)), (1, page(1)), (2, page(2))]);
        let mut buf = BrowserBufferBackend::load_pages(pages);

        assert!(
            buf.configure_sparse_residency([0]).is_err(),
            "logical and page-zero counts must agree before sparse conversion"
        );
        assert!(!buf.is_sparse());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn sparse_write_extends_logical_page_count() {
        let mut buf = BrowserBufferBackend::new();
        buf.write_page(7, &page(7)).unwrap();
        assert_eq!(buf.page_count().unwrap(), 8);
        assert_eq!(buf.resident_page_count(), 1);
    }
}
