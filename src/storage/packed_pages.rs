//! Packed fact page format (page_type = 0x02).
//!
//! Layout of a packed page:
//! ```text
//! [12-byte header]
//!   byte 0:    page_type  (0x02 = packed fact data)
//!   byte 1:    _reserved  (0x00)
//!   bytes 2-3: record_count  (u16 LE)
//!   bytes 4-11: next_page   (u64 LE, 0 = no overflow)
//!
//! [record directory: record_count × 4 bytes each]
//!   per entry: offset u16 LE | length u16 LE
//!   (offset measured from page start)
//!
//! [record data: variable-length postcard-serialised Facts]
//!   written from end of page backwards
//! ```
//!
//! Overflow pages (`page_type = 0x03`) are reserved for future use.
//! The `next_page` field is always written as 0 in Phase 6.2.

use crate::graph::types::Fact;
use crate::storage::index::FactRef;
use crate::storage::{PAGE_SIZE, StorageBackend};
use anyhow::Result;

/// Page type byte for packed fact pages.
pub const PAGE_TYPE_PACKED: u8 = 0x02;
/// Page type byte for overflow pages (reserved, not yet used).
#[allow(dead_code)]
pub const PAGE_TYPE_OVERFLOW: u8 = 0x03;

/// Packed page header size in bytes.
pub const PACKED_HEADER_SIZE: usize = 12;

/// Maximum serialised size (postcard bytes) for a single fact in a packed page.
///
/// Derived from the page layout: `PAGE_SIZE (4096) - PACKED_HEADER_SIZE (12) - 4`
/// (4 bytes for one record-directory entry).
///
/// In practice the usable space for a `Value::String` is roughly 3 900–4 000 bytes
/// after accounting for the fixed overhead of the other `Fact` fields (two UUIDs,
/// attribute string, counters, timestamps, boolean flag).
///
/// File-backed databases reject facts that exceed this limit at insertion time.
/// In-memory databases (`Minigraf::in_memory()`) have no size constraint.
pub const MAX_FACT_BYTES: usize = PAGE_SIZE - PACKED_HEADER_SIZE - 4;

/// Pack a slice of facts into packed pages.
///
/// Returns `(pages, fact_refs)` where:
/// - `pages[i]` is exactly `PAGE_SIZE` bytes of packed page data
/// - `fact_refs[j]` is the `FactRef { page_id, slot_index }` for `facts[j]`
///
/// `start_page_id` is assigned to `pages[0]`; subsequent pages get
/// `start_page_id + 1`, `start_page_id + 2`, etc.
pub fn pack_facts(facts: &[Fact], start_page_id: u64) -> Result<(Vec<Vec<u8>>, Vec<FactRef>)> {
    let mut packer = PackedFactPacker::new(start_page_id);
    let mut fact_refs: Vec<FactRef> = Vec::with_capacity(facts.len());

    for fact in facts {
        fact_refs.push(packer.push(fact)?);
    }

    Ok((packer.finish(), fact_refs))
}

/// Incrementally pack facts into packed pages.
///
/// This preserves the exact page layout produced by [`pack_facts`] while
/// allowing callers to stream facts instead of materializing a full fact slice.
pub struct PackedFactPacker {
    start_page_id: u64,
    pages: Vec<Vec<u8>>,
    current_page: Vec<u8>,
    current_record_count: u16,
    dir_offset: usize,
    data_offset: usize,
}

impl PackedFactPacker {
    pub fn new(start_page_id: u64) -> Self {
        Self {
            start_page_id,
            pages: Vec::new(),
            current_page: new_packed_page(),
            current_record_count: 0,
            dir_offset: PACKED_HEADER_SIZE,
            data_offset: PAGE_SIZE,
        }
    }

    pub fn push(&mut self, fact: &Fact) -> Result<FactRef> {
        let serialised = postcard::to_allocvec(fact)?;
        let len = serialised.len();
        let dir_entry_size = 4usize;

        // Check if this fact exceeds the maximum slot size.
        if len > MAX_FACT_BYTES {
            anyhow::bail!(
                "Fact serialised size {} bytes exceeds maximum slot size {} bytes",
                len,
                MAX_FACT_BYTES
            );
        }

        // Check if this fact fits on the current page.
        // Free space = data_offset - dir_offset - dir_entry_size (for the new dir entry).
        // saturating_sub is safe: dir_offset + dir_entry_size is bounded by PAGE_SIZE.
        let free = self
            .data_offset
            .saturating_sub(self.dir_offset.saturating_add(dir_entry_size));
        if len > free || self.current_record_count == u16::MAX {
            // Flush current page and start a new one.
            write_record_count(&mut self.current_page, self.current_record_count);
            let flushed_page = std::mem::replace(&mut self.current_page, new_packed_page());
            self.pages.push(flushed_page);
            self.current_record_count = 0;
            self.dir_offset = PACKED_HEADER_SIZE;
            self.data_offset = PAGE_SIZE;
        }

        // Write data from end of page backwards.
        // len <= MAX_FACT_BYTES <= PAGE_SIZE, and we checked len <= free <= data_offset,
        // so this subtraction cannot underflow.
        self.data_offset = self.data_offset.wrapping_sub(len);
        self.current_page
            .get_mut(self.data_offset..self.data_offset.saturating_add(len))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "packed page too short: data region {}..{} out of bounds",
                    self.data_offset,
                    self.data_offset.saturating_add(len)
                )
            })?
            .copy_from_slice(&serialised);

        // Write directory entry: offset (u16 LE) | length (u16 LE).
        // data_offset <= PAGE_SIZE (4096) which fits in u16; len <= MAX_FACT_BYTES < u16::MAX.
        let offset_u16 = u16::try_from(self.data_offset)
            .map_err(|_| anyhow::anyhow!("data_offset {} overflows u16", self.data_offset))?;
        let len_u16 = u16::try_from(len)
            .map_err(|_| anyhow::anyhow!("serialised fact too large: {} bytes", len))?;
        self.current_page
            .get_mut(self.dir_offset..self.dir_offset.saturating_add(2))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "packed page dir out of bounds at {}..{}",
                    self.dir_offset,
                    self.dir_offset.saturating_add(2)
                )
            })?
            .copy_from_slice(&offset_u16.to_le_bytes());
        self.current_page
            .get_mut(self.dir_offset.saturating_add(2)..self.dir_offset.saturating_add(4))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "packed page dir out of bounds at {}..{}",
                    self.dir_offset.saturating_add(2),
                    self.dir_offset.saturating_add(4)
                )
            })?
            .copy_from_slice(&len_u16.to_le_bytes());
        self.dir_offset = self.dir_offset.saturating_add(4);

        let page_id = self.start_page_id.saturating_add(
            u64::try_from(self.pages.len())
                .map_err(|_| anyhow::anyhow!("too many pages: overflows u64"))?,
        );
        let fact_ref = FactRef {
            page_id,
            slot_index: self.current_record_count,
        };
        self.current_record_count = self.current_record_count.saturating_add(1);
        Ok(fact_ref)
    }

    pub fn finish(mut self) -> Vec<Vec<u8>> {
        // Always flush the last page (even if no facts were packed).
        write_record_count(&mut self.current_page, self.current_record_count);
        self.pages.push(self.current_page);
        self.pages
    }
}

/// Read a single fact from a packed page at the given slot index.
pub fn read_slot(page: &[u8], slot: u16) -> Result<Fact> {
    if page.len() < PAGE_SIZE {
        anyhow::bail!(
            "Page too short: {} bytes (expected {})",
            page.len(),
            PAGE_SIZE
        );
    }
    let page_type = *page
        .first()
        .ok_or_else(|| anyhow::anyhow!("packed page empty"))?;
    if page_type != PAGE_TYPE_PACKED {
        anyhow::bail!("Expected packed page (0x02), got 0x{:02x}", page_type);
    }
    let b2 = *page
        .get(2)
        .ok_or_else(|| anyhow::anyhow!("packed page too short for record_count byte 2"))?;
    let b3 = *page
        .get(3)
        .ok_or_else(|| anyhow::anyhow!("packed page too short for record_count byte 3"))?;
    let record_count = u16::from_le_bytes([b2, b3]);
    if slot >= record_count {
        anyhow::bail!(
            "Slot {} out of bounds (page has {} records)",
            slot,
            record_count
        );
    }
    // dir_base = PACKED_HEADER_SIZE + slot * 4; slot < record_count <= u16::MAX,
    // so slot as usize * 4 <= (65534 * 4) which fits in usize.
    let slot_usize = usize::from(slot);
    let dir_base = PACKED_HEADER_SIZE.saturating_add(slot_usize.saturating_mul(4));
    let db0 = *page
        .get(dir_base)
        .ok_or_else(|| anyhow::anyhow!("packed page dir entry {} out of bounds", slot))?;
    let db1 = *page
        .get(dir_base.saturating_add(1))
        .ok_or_else(|| anyhow::anyhow!("packed page dir entry {} byte 1 out of bounds", slot))?;
    let db2 = *page
        .get(dir_base.saturating_add(2))
        .ok_or_else(|| anyhow::anyhow!("packed page dir entry {} byte 2 out of bounds", slot))?;
    let db3 = *page
        .get(dir_base.saturating_add(3))
        .ok_or_else(|| anyhow::anyhow!("packed page dir entry {} byte 3 out of bounds", slot))?;
    let offset = usize::from(u16::from_le_bytes([db0, db1]));
    let length = usize::from(u16::from_le_bytes([db2, db3]));
    if offset.saturating_add(length) > PAGE_SIZE {
        anyhow::bail!("Record at slot {} extends beyond page boundary", slot);
    }
    let fact: Fact = postcard::from_bytes(
        page.get(offset..offset.saturating_add(length))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "packed page record {}..{} out of bounds",
                    offset,
                    offset.saturating_add(length)
                )
            })?,
    )?;
    Ok(fact)
}

/// Decode the highest `tx_count` present on a packed page.
///
/// Returns `Ok(None)` when the page is not a packed fact page or holds no
/// records. Packed fact pages hold facts in nondecreasing `tx_count` order
/// (checkpoints append pending facts in commit order and recompact preserves
/// `for_each_fact` order), so the last slot carries the page maximum.
pub fn last_tx_count(page: &[u8]) -> Result<Option<u64>> {
    let page_type = page.first().copied().unwrap_or(0);
    if page.len() < PAGE_SIZE || page_type != PAGE_TYPE_PACKED {
        return Ok(None);
    }
    let b2 = page.get(2).copied().unwrap_or(0);
    let b3 = page.get(3).copied().unwrap_or(0);
    let record_count = u16::from_le_bytes([b2, b3]);
    if record_count == 0 {
        return Ok(None);
    }
    Ok(Some(read_slot(page, record_count.saturating_sub(1))?.tx_count))
}

/// Read all facts from a contiguous range of packed fact pages.
///
/// `first_page_id` is the backend page ID of the first packed fact page.
/// `num_pages` is the number of pages to read.
/// Non-packed pages (e.g., index pages) are silently skipped.
pub fn read_all_from_pages(
    backend: &dyn StorageBackend,
    first_page_id: u64,
    num_pages: u64,
) -> Result<Vec<Fact>> {
    let mut facts = Vec::new();
    for_each_from_pages(backend, first_page_id, num_pages, &mut |fact| {
        facts.push(fact);
        Ok(())
    })?;
    Ok(facts)
}

/// Visit all facts from a contiguous range of packed fact pages without
/// materializing a complete `Vec<Fact>` first.
///
/// `first_page_id` is the backend page ID of the first packed fact page.
/// `num_pages` is the number of pages to read.
/// Non-packed pages (e.g., index pages) are silently skipped.
pub fn for_each_from_pages(
    backend: &dyn StorageBackend,
    first_page_id: u64,
    num_pages: u64,
    visit: &mut dyn FnMut(Fact) -> Result<()>,
) -> Result<()> {
    for i in 0..num_pages {
        let page = backend.read_page(first_page_id.saturating_add(i))?;
        let page_type = page.first().copied().unwrap_or(0);
        if page.len() < PAGE_SIZE || page_type != PAGE_TYPE_PACKED {
            continue;
        }
        let b2 = page.get(2).copied().unwrap_or(0);
        let b3 = page.get(3).copied().unwrap_or(0);
        let record_count = u16::from_le_bytes([b2, b3]);
        for slot in 0..record_count {
            visit(read_slot(&page, slot)?)?;
        }
    }
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn new_packed_page() -> Vec<u8> {
    let mut page = vec![0u8; PAGE_SIZE];
    // Safety: page is PAGE_SIZE bytes; index 0 is always valid.
    if let Some(b) = page.get_mut(0) {
        *b = PAGE_TYPE_PACKED;
    }
    // byte 1: reserved = 0x00 (already zero from vec initialisation)
    // bytes 2-3: record_count = 0 (written later via write_record_count)
    // bytes 4-11: next_page = 0 (already zero)
    page
}

fn write_record_count(page: &mut [u8], count: u16) {
    if let Some(slot) = page.get_mut(2..4) {
        slot.copy_from_slice(&count.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{Fact, VALID_TIME_FOREVER, Value};
    use uuid::Uuid;

    fn make_fact(n: u64) -> Fact {
        Fact::with_valid_time(
            Uuid::from_u128(n as u128),
            ":attr".to_string(),
            Value::Integer(n as i64),
            n,
            n,
            0,
            VALID_TIME_FOREVER,
        )
    }

    fn make_rich_fact_set() -> Vec<Fact> {
        let source = Uuid::from_u128(0xabc);
        let target = Uuid::from_u128(0xdef);
        let ref_value = Value::Ref(target);
        vec![
            Fact::with_valid_time(
                source,
                ":name".to_string(),
                Value::String("source".to_string()),
                100,
                1,
                100,
                VALID_TIME_FOREVER,
            ),
            Fact::with_valid_time(
                source,
                ":edge/to".to_string(),
                ref_value.clone(),
                200,
                2,
                120,
                220,
            ),
            Fact::retract_with_valid_time(
                source,
                ":edge/to".to_string(),
                ref_value.clone(),
                300,
                3,
                120,
                220,
            ),
            Fact::with_valid_time(
                source,
                ":edge/to".to_string(),
                ref_value,
                300,
                3,
                220,
                VALID_TIME_FOREVER,
            ),
        ]
    }

    #[test]
    fn test_single_fact_roundtrip() {
        let facts = vec![make_fact(1)];
        let (pages, refs) = pack_facts(&facts, 1).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].page_id, 1);
        assert_eq!(refs[0].slot_index, 0);
        let recovered = read_slot(&pages[0], 0).unwrap();
        assert_eq!(recovered.entity, facts[0].entity);
        assert_eq!(recovered.tx_count, facts[0].tx_count);
    }

    #[test]
    fn test_multiple_facts_pack_fewer_pages() {
        let facts: Vec<Fact> = (0..50).map(make_fact).collect();
        let (pages, refs) = pack_facts(&facts, 1).unwrap();
        assert!(
            pages.len() < 50,
            "packed pages ({}) should be < 50",
            pages.len()
        );
        assert_eq!(refs.len(), 50);
    }

    #[test]
    fn test_slot_index_roundtrip() {
        let facts: Vec<Fact> = (0..30).map(make_fact).collect();
        let (pages, refs) = pack_facts(&facts, 1).unwrap();
        for (i, fact) in facts.iter().enumerate() {
            let r = &refs[i];
            let page = &pages[(r.page_id - 1) as usize]; // page_id is 1-based, pages vec is 0-based
            let recovered = read_slot(page, r.slot_index).unwrap();
            assert_eq!(recovered.entity, fact.entity, "fact {} mismatched", i);
        }
    }

    #[test]
    fn test_streaming_packer_matches_pack_facts_layout() {
        let mut facts = make_rich_fact_set();
        facts.extend((0..80).map(make_fact));
        let (expected_pages, expected_refs) = pack_facts(&facts, 42).unwrap();

        let mut packer = PackedFactPacker::new(42);
        let mut refs = Vec::new();
        for fact in &facts {
            refs.push(packer.push(fact).unwrap());
        }

        assert_eq!(packer.finish(), expected_pages);
        assert_eq!(refs, expected_refs);
    }

    #[test]
    fn test_page_type_byte_is_0x02() {
        let facts = vec![make_fact(1)];
        let (pages, _) = pack_facts(&facts, 1).unwrap();
        assert_eq!(pages[0][0], PAGE_TYPE_PACKED);
    }

    /// Pin every field in the 12-byte page header to its canonical byte offset.
    ///
    /// Like the FileHeader layout test, a roundtrip would not catch accidental
    /// field swaps or `to_ne_bytes()` use on big-endian platforms.  We assert
    /// the raw bytes directly against the spec in the module doc comment.
    #[test]
    fn test_packed_page_header_byte_layout() {
        let facts: Vec<Fact> = (0..3).map(make_fact).collect();
        let (pages, _) = pack_facts(&facts, 1).unwrap();
        let page = &pages[0];

        // byte 0: page_type = 0x02
        assert_eq!(page[0], 0x02, "byte 0 must be PAGE_TYPE_PACKED (0x02)");

        // byte 1: _reserved = 0x00
        assert_eq!(page[1], 0x00, "byte 1 must be reserved zero");

        // bytes 2..4: record_count (u16 LE) — 3 facts, all fit in one page
        let record_count = u16::from_le_bytes([page[2], page[3]]);
        assert_eq!(record_count, 3, "record_count at bytes 2-3 must be 3");
        // Also verify raw LE encoding: low byte first
        assert_eq!(page[2], 3, "record_count low byte at offset 2");
        assert_eq!(page[3], 0, "record_count high byte at offset 3");

        // bytes 4..12: next_page (u64 LE) = 0 (no overflow in Phase 6.2)
        let next_page = u64::from_le_bytes(page[4..12].try_into().unwrap());
        assert_eq!(next_page, 0, "next_page at bytes 4-11 must be 0");
        assert_eq!(&page[4..12], &0u64.to_le_bytes(), "next_page raw LE bytes");
    }

    /// Verify the record directory entry layout: each entry is 4 bytes,
    /// (offset: u16 LE, length: u16 LE), starting at byte 12.
    #[test]
    fn test_packed_page_record_directory_layout() {
        let facts = vec![make_fact(1)];
        let (pages, _) = pack_facts(&facts, 1).unwrap();
        let page = &pages[0];

        // With 1 fact, record_count = 1; directory entry at bytes 12..16.
        let record_count = u16::from_le_bytes([page[2], page[3]]);
        assert_eq!(record_count, 1);

        // Directory entry 0: offset (u16 LE) at bytes 12-13, length (u16 LE) at 14-15.
        let offset = u16::from_le_bytes([page[12], page[13]]) as usize;
        let length = u16::from_le_bytes([page[14], page[15]]) as usize;

        // Offset must be within the page and after the header + directory.
        assert!(
            offset >= PACKED_HEADER_SIZE + 4,
            "data offset must be past header+directory"
        );
        assert!(offset < PAGE_SIZE, "data offset must be within page");

        // Length must be nonzero and the data must fit within the page.
        assert!(length > 0, "record length must be nonzero");
        assert!(offset + length <= PAGE_SIZE, "record must fit within page");

        // Verify the bytes at the directory offset are the LE encoding of those values.
        assert_eq!(
            &page[12..14],
            &(offset as u16).to_le_bytes(),
            "directory offset LE"
        );
        assert_eq!(
            &page[14..16],
            &(length as u16).to_le_bytes(),
            "directory length LE"
        );
    }

    #[test]
    fn test_read_all_from_pages_roundtrip() {
        use crate::storage::backend::MemoryBackend;
        let facts: Vec<Fact> = (0..60).map(make_fact).collect();
        let (pages, _refs) = pack_facts(&facts, 1).unwrap();
        let mut backend = MemoryBackend::new();
        for (i, page) in pages.iter().enumerate() {
            backend.write_page((i + 1) as u64, page).unwrap();
        }
        let recovered = read_all_from_pages(&backend, 1, pages.len() as u64).unwrap();
        assert_eq!(recovered.len(), 60);
        for (orig, rec) in facts.iter().zip(recovered.iter()) {
            assert_eq!(orig.entity, rec.entity);
        }
    }

    #[test]
    fn test_oversized_fact_returns_error() {
        // Create a fact with a very large string value (>4080 bytes)
        let big_string = "x".repeat(5000);
        let fact = Fact::with_valid_time(
            Uuid::from_u128(999),
            ":big".to_string(),
            Value::String(big_string),
            1,
            1,
            0,
            VALID_TIME_FOREVER,
        );
        let result = pack_facts(&[fact], 1);
        assert!(result.is_err(), "oversized fact must return Err, not panic");
    }

    #[test]
    fn test_last_tx_count_reads_page_maximum() {
        let facts: Vec<Fact> = (1..=5).map(make_fact).collect();
        let (pages, _) = pack_facts(&facts, 1).unwrap();
        assert_eq!(pages.len(), 1, "five small facts should fit on one page");
        let last = last_tx_count(&pages[0]).unwrap();
        assert_eq!(last, Some(5), "last slot must carry the page max tx_count");
    }

    #[test]
    fn test_last_tx_count_none_for_empty_and_non_packed_pages() {
        let (pages, _) = pack_facts(&[], 1).unwrap();
        assert_eq!(pages.len(), 1, "empty pack still flushes one page");
        assert_eq!(
            last_tx_count(&pages[0]).unwrap(),
            None,
            "zero-record page has no tx_count"
        );

        let non_packed = vec![0u8; PAGE_SIZE];
        assert_eq!(
            last_tx_count(&non_packed).unwrap(),
            None,
            "non-packed page type must be rejected as None"
        );

        let short_page = vec![PAGE_TYPE_PACKED; 8];
        assert_eq!(
            last_tx_count(&short_page).unwrap(),
            None,
            "short page must be rejected as None"
        );
    }
}
