//! Generation-bound catalog for persisted current-projection page images.
//!
//! A catalog is optional acceleration metadata. Page 0 publishes it through
//! its own double-buffered slots; the ledger roots and delta manifest remain
//! the only authority for facts.

// Encoder offsets are derived from a freshly allocated, fully sized buffer.
// Decoder reads use checked helpers before any fixed-range access.
#![allow(clippy::indexing_slicing)]

use crate::storage::{PAGE_SIZE, StorageBackend};
use anyhow::{Result, anyhow, bail};

const MAGIC: [u8; 8] = *b"MGPRC001";
const COMMIT_MARKER: [u8; 8] = *b"MGPRDONE";
const CODEC_VERSION: u32 = 1;
const HEADER_LEN: usize = 80;
const HEADER_CHECKSUM_OFFSET: usize = 60;
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const MAX_CATALOG_ENTRIES: usize = 4096;
const MAX_ATTRIBUTE_BYTES: usize = 64 * 1024;

/// Persistent ledger state to which a projection catalog and its images belong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionLedgerIdentity {
    base_generation: u64,
    manifest_generation: u64,
    tx_count: u64,
}

impl ProjectionLedgerIdentity {
    pub(crate) fn new(base_generation: u64, manifest_generation: u64, tx_count: u64) -> Self {
        Self {
            base_generation,
            manifest_generation,
            tx_count,
        }
    }

    /// Immutable base generation selected by the ledger.
    #[must_use]
    #[cfg(any(test, feature = "bench-internals"))]
    pub fn base_generation(self) -> u64 {
        self.base_generation
    }

    /// Selected delta-manifest generation, or zero when no manifest exists.
    #[must_use]
    #[cfg(any(test, feature = "bench-internals"))]
    pub fn manifest_generation(self) -> u64 {
        self.manifest_generation
    }

    /// Exact durable transaction watermark represented by the catalog.
    #[must_use]
    #[cfg(any(test, feature = "bench-internals"))]
    pub fn tx_count(self) -> u64 {
        self.tx_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionCatalogEntry {
    attribute: String,
    valid_time_floor: i64,
    image_page_start: u64,
    image_page_count: u64,
    image_logical_bytes: u64,
    row_count: u64,
    fingerprint: u64,
}

impl ProjectionCatalogEntry {
    pub(crate) fn new(
        attribute: String,
        valid_time_floor: i64,
        image_page_start: u64,
        image_page_count: u64,
        image_logical_bytes: u64,
        row_count: u64,
        fingerprint: u64,
    ) -> Result<Self> {
        if attribute.is_empty() || attribute.len() > MAX_ATTRIBUTE_BYTES {
            bail!("Projection catalog attribute length is invalid");
        }
        if image_page_start == 0 || image_page_count == 0 {
            bail!("Projection image range must contain non-header pages");
        }
        let capacity = image_page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| anyhow!("Projection image capacity overflow"))?;
        if image_logical_bytes < PAGE_SIZE as u64 || image_logical_bytes > capacity {
            bail!("Projection image logical length exceeds its page range");
        }
        image_page_start
            .checked_add(image_page_count)
            .ok_or_else(|| anyhow!("Projection image page range overflow"))?;
        Ok(Self {
            attribute,
            valid_time_floor,
            image_page_start,
            image_page_count,
            image_logical_bytes,
            row_count,
            fingerprint,
        })
    }

    pub(crate) fn image_page_start(&self) -> u64 {
        self.image_page_start
    }

    pub(crate) fn image_page_count(&self) -> u64 {
        self.image_page_count
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn image_logical_bytes(&self) -> u64 {
        self.image_logical_bytes
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn row_count(&self) -> u64 {
        self.row_count
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    fn key(&self) -> (&str, i64) {
        (&self.attribute, self.valid_time_floor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionCatalog {
    generation: u64,
    identity: ProjectionLedgerIdentity,
    entries: Vec<ProjectionCatalogEntry>,
}

impl ProjectionCatalog {
    pub(crate) fn new(
        generation: u64,
        identity: ProjectionLedgerIdentity,
        mut entries: Vec<ProjectionCatalogEntry>,
    ) -> Result<Self> {
        if generation == 0 {
            bail!("Projection catalog generation must be non-zero");
        }
        if entries.is_empty() || entries.len() > MAX_CATALOG_ENTRIES {
            bail!("Projection catalog entry count is invalid");
        }
        entries.sort_by(|left, right| left.key().cmp(&right.key()));
        if entries
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.key() == right.key()))
        {
            bail!("Projection catalog contains duplicate selector keys");
        }
        Ok(Self {
            generation,
            identity,
            entries,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn identity(&self) -> ProjectionLedgerIdentity {
        self.identity
    }

    pub(crate) fn entries(&self) -> &[ProjectionCatalogEntry] {
        &self.entries
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn entry(
        &self,
        attribute: &str,
        valid_time_floor: i64,
    ) -> Option<&ProjectionCatalogEntry> {
        self.entries
            .binary_search_by(|entry| entry.key().cmp(&(attribute, valid_time_floor)))
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn encode_pages(&self) -> Result<(Vec<u8>, u64, u32)> {
        let mut payload = Vec::new();
        for entry in &self.entries {
            let attribute_len = u32::try_from(entry.attribute.len())
                .map_err(|_| anyhow!("Projection catalog attribute length exceeds u32"))?;
            payload.extend_from_slice(&attribute_len.to_le_bytes());
            payload.extend_from_slice(entry.attribute.as_bytes());
            payload.extend_from_slice(&entry.valid_time_floor.to_le_bytes());
            payload.extend_from_slice(&entry.image_page_start.to_le_bytes());
            payload.extend_from_slice(&entry.image_page_count.to_le_bytes());
            payload.extend_from_slice(&entry.image_logical_bytes.to_le_bytes());
            payload.extend_from_slice(&entry.row_count.to_le_bytes());
            payload.extend_from_slice(&entry.fingerprint.to_le_bytes());
        }
        let logical_len = HEADER_LEN
            .checked_add(payload.len())
            .ok_or_else(|| anyhow!("Projection catalog length overflow"))?;
        if logical_len > MAX_CATALOG_BYTES {
            bail!("Projection catalog exceeds the bounded codec limit");
        }
        let padded_len = logical_len
            .checked_add(PAGE_SIZE - 1)
            .map(|length| length / PAGE_SIZE * PAGE_SIZE)
            .ok_or_else(|| anyhow!("Projection catalog alignment overflow"))?;
        let mut bytes = vec![0u8; padded_len];
        bytes[0..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&CODEC_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(HEADER_LEN)?.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.generation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.identity.base_generation().to_le_bytes());
        bytes[32..40].copy_from_slice(&self.identity.manifest_generation().to_le_bytes());
        bytes[40..48].copy_from_slice(&self.identity.tx_count().to_le_bytes());
        bytes[48..52].copy_from_slice(
            &u32::try_from(self.entries.len())
                .map_err(|_| anyhow!("Projection catalog entry count exceeds u32"))?
                .to_le_bytes(),
        );
        bytes[52..56].copy_from_slice(
            &u32::try_from(payload.len())
                .map_err(|_| anyhow!("Projection catalog payload length exceeds u32"))?
                .to_le_bytes(),
        );
        bytes[56..60].copy_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        bytes[64..72].copy_from_slice(&COMMIT_MARKER);
        bytes[HEADER_LEN..logical_len].copy_from_slice(&payload);
        let header_checksum = header_checksum(&bytes[..HEADER_LEN]);
        bytes[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&header_checksum.to_le_bytes());
        let logical_len_u64 = u64::try_from(logical_len)?;
        let catalog_checksum = crc32fast::hash(&bytes[..logical_len]);
        Ok((bytes, logical_len_u64, catalog_checksum))
    }

    pub(crate) fn decode_pages(bytes: &[u8], logical_len: u64) -> Result<Self> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(PAGE_SIZE) {
            bail!("Projection catalog is not a complete page range");
        }
        let logical_len = usize::try_from(logical_len)
            .map_err(|_| anyhow!("Projection catalog length exceeds usize"))?;
        if logical_len < HEADER_LEN || logical_len > bytes.len() || logical_len > MAX_CATALOG_BYTES
        {
            bail!("Projection catalog logical length is invalid");
        }
        if logical_len <= bytes.len().saturating_sub(PAGE_SIZE) {
            bail!("Projection catalog page count is not canonical for its length");
        }
        if bytes[logical_len..].iter().any(|byte| *byte != 0) {
            bail!("Projection catalog padding is non-zero");
        }
        if bytes.get(0..8) != Some(MAGIC.as_slice())
            || read_u32(bytes, 8, "codec version")? != CODEC_VERSION
            || usize::try_from(read_u32(bytes, 12, "header length")?)? != HEADER_LEN
        {
            bail!("Projection catalog header is unsupported");
        }
        if bytes.get(64..72) != Some(COMMIT_MARKER.as_slice()) {
            bail!("Projection catalog commit marker is missing");
        }
        if bytes[72..HEADER_LEN].iter().any(|byte| *byte != 0) {
            bail!("Projection catalog reserved header bytes are non-zero");
        }
        let stored_header_checksum = read_u32(bytes, HEADER_CHECKSUM_OFFSET, "header checksum")?;
        if stored_header_checksum != header_checksum(&bytes[..HEADER_LEN]) {
            bail!("Projection catalog header checksum mismatch");
        }
        let entry_count = usize::try_from(read_u32(bytes, 48, "entry count")?)?;
        if entry_count == 0 || entry_count > MAX_CATALOG_ENTRIES {
            bail!("Projection catalog entry count is invalid");
        }
        let payload_len = usize::try_from(read_u32(bytes, 52, "payload length")?)?;
        if HEADER_LEN.checked_add(payload_len) != Some(logical_len) {
            bail!("Projection catalog payload length mismatch");
        }
        let payload = bytes
            .get(HEADER_LEN..logical_len)
            .ok_or_else(|| anyhow!("Projection catalog payload is truncated"))?;
        if read_u32(bytes, 56, "payload checksum")? != crc32fast::hash(payload) {
            bail!("Projection catalog payload checksum mismatch");
        }
        let mut cursor = 0usize;
        let mut entries = Vec::new();
        entries.try_reserve_exact(entry_count)?;
        for _ in 0..entry_count {
            let attribute_len = usize::try_from(read_u32(payload, cursor, "attribute length")?)?;
            cursor = cursor
                .checked_add(4)
                .ok_or_else(|| anyhow!("Projection catalog cursor overflow"))?;
            if attribute_len == 0 || attribute_len > MAX_ATTRIBUTE_BYTES {
                bail!("Projection catalog attribute length is invalid");
            }
            let attribute_end = cursor
                .checked_add(attribute_len)
                .ok_or_else(|| anyhow!("Projection catalog attribute range overflow"))?;
            let attribute = std::str::from_utf8(
                payload
                    .get(cursor..attribute_end)
                    .ok_or_else(|| anyhow!("Projection catalog attribute is truncated"))?,
            )?
            .to_owned();
            cursor = attribute_end;
            let valid_time_floor = read_i64(payload, cursor, "valid-time floor")?;
            cursor += 8;
            let image_page_start = read_u64(payload, cursor, "image page start")?;
            cursor += 8;
            let image_page_count = read_u64(payload, cursor, "image page count")?;
            cursor += 8;
            let image_logical_bytes = read_u64(payload, cursor, "image logical bytes")?;
            cursor += 8;
            let row_count = read_u64(payload, cursor, "row count")?;
            cursor += 8;
            let fingerprint = read_u64(payload, cursor, "fingerprint")?;
            cursor += 8;
            entries.push(ProjectionCatalogEntry::new(
                attribute,
                valid_time_floor,
                image_page_start,
                image_page_count,
                image_logical_bytes,
                row_count,
                fingerprint,
            )?);
        }
        if cursor != payload.len() {
            bail!("Projection catalog contains trailing payload bytes");
        }
        if entries
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.key() >= right.key()))
        {
            bail!("Projection catalog entries are not in canonical order");
        }
        let generation = read_u64(bytes, 16, "generation")?;
        let identity = ProjectionLedgerIdentity::new(
            read_u64(bytes, 24, "base generation")?,
            read_u64(bytes, 32, "manifest generation")?,
            read_u64(bytes, 40, "transaction watermark")?,
        );
        Self::new(generation, identity, entries)
    }
}

pub(crate) fn read_catalog<B: StorageBackend>(
    backend: &B,
    page_start: u64,
    page_count: u64,
    logical_len: u64,
    expected_checksum: u32,
) -> Result<ProjectionCatalog> {
    let page_count_usize = usize::try_from(page_count)
        .map_err(|_| anyhow!("Projection catalog page count exceeds usize"))?;
    let capacity = page_count_usize
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| anyhow!("Projection catalog capacity overflow"))?;
    if capacity > MAX_CATALOG_BYTES {
        bail!("Projection catalog exceeds the bounded codec limit");
    }
    let mut bytes = Vec::with_capacity(capacity);
    for offset in 0..page_count {
        let page_id = page_start
            .checked_add(offset)
            .ok_or_else(|| anyhow!("Projection catalog page id overflow"))?;
        let page = backend.read_page(page_id)?;
        if page.len() != PAGE_SIZE {
            bail!("Projection catalog page has invalid size");
        }
        bytes.extend_from_slice(&page);
    }
    let logical_len_usize = usize::try_from(logical_len)?;
    let logical = bytes
        .get(..logical_len_usize)
        .ok_or_else(|| anyhow!("Projection catalog payload is truncated"))?;
    if crc32fast::hash(logical) != expected_checksum {
        bail!("Projection catalog descriptor checksum mismatch");
    }
    ProjectionCatalog::decode_pages(&bytes, logical_len)
}

fn header_checksum(header: &[u8]) -> u32 {
    let mut bytes = header.to_vec();
    if let Some(field) = bytes.get_mut(HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4) {
        field.fill(0);
    }
    crc32fast::hash(&bytes)
}

fn read_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| anyhow!("Projection catalog {label} offset overflow"))?;
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or_else(|| anyhow!("Projection catalog {label} is truncated"))?
            .try_into()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize, label: &str) -> Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| anyhow!("Projection catalog {label} offset overflow"))?;
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or_else(|| anyhow!("Projection catalog {label} is truncated"))?
            .try_into()?,
    ))
}

fn read_i64(bytes: &[u8], offset: usize, label: &str) -> Result<i64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| anyhow!("Projection catalog {label} offset overflow"))?;
    Ok(i64::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or_else(|| anyhow!("Projection catalog {label} is truncated"))?
            .try_into()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::ProjectionLedgerIdentity;
    use super::{ProjectionCatalog, ProjectionCatalogEntry};

    fn catalog() -> ProjectionCatalog {
        ProjectionCatalog::new(
            7,
            ProjectionLedgerIdentity::new(3, 5, 11),
            vec![
                ProjectionCatalogEntry::new(":z".into(), 10, 12, 2, 5000, 2, 22).unwrap(),
                ProjectionCatalogEntry::new(":a".into(), -10, 2, 3, 9000, 4, 44).unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn roundtrip_is_deterministic_and_sorted() {
        let catalog = catalog();
        let (first, logical_len, checksum) = catalog.encode_pages().unwrap();
        let (second, _, _) = catalog.encode_pages().unwrap();
        assert_eq!(first, second);
        assert_eq!(
            crc32fast::hash(&first[..usize::try_from(logical_len).unwrap()]),
            checksum
        );
        let decoded = ProjectionCatalog::decode_pages(&first, logical_len).unwrap();
        assert_eq!(decoded, catalog);
        assert_eq!(decoded.entries()[0].attribute, ":a");
    }

    #[test]
    fn corruption_and_noncanonical_bytes_are_rejected() {
        let (bytes, logical_len, _) = catalog().encode_pages().unwrap();
        for index in [0usize, 56, 60, 64, 80] {
            let mut corrupt = bytes.clone();
            corrupt[index] ^= 1;
            assert!(ProjectionCatalog::decode_pages(&corrupt, logical_len).is_err());
        }
        let mut padding = bytes;
        let last = padding.len() - 1;
        padding[last] = 1;
        assert!(ProjectionCatalog::decode_pages(&padding, logical_len).is_err());
    }

    #[test]
    fn duplicate_selector_is_rejected() {
        let entry = ProjectionCatalogEntry::new(":a".into(), 1, 2, 1, 4096, 0, 0).unwrap();
        assert!(
            ProjectionCatalog::new(
                1,
                ProjectionLedgerIdentity::new(1, 0, 1),
                vec![entry.clone(), entry],
            )
            .is_err()
        );
    }
}
