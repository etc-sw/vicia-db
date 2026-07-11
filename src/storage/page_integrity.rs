use crate::storage::PAGE_SIZE;
use anyhow::{Result, bail};
use crc32fast::Hasher;

pub(crate) const PAGE_INTEGRITY_CATALOG_MAGIC: [u8; 8] = *b"MGPGC001";
pub(crate) const PAGE_INTEGRITY_CATALOG_CODEC_VERSION: u32 = 1;

const PAGE_INTEGRITY_CATALOG_RESERVED: u32 = 0;
const PAGE_CHECKSUM_DOMAIN: &[u8] = b"MGPGC001:base-page:v1\0";
const CATALOG_HEADER_LEN: usize = 8 + 4 + 4 + 8 + 8 + 8;
const CHECKSUM_LEN: usize = 4;
/// Runtime bound for eagerly loaded integrity metadata.
///
/// 64 MiB covers 16,777,206 graph pages, or just under 64 GiB of base data.
/// The current 1M-fact acceptance graph uses roughly 100K pages.
pub(crate) const MAX_BASE_INTEGRITY_CATALOG_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_BASE_INTEGRITY_COVERED_PAGES: u64 = 16_777_206;

/// Lossless checksum catalog for the immutable pages of one published base.
///
/// The catalog deliberately binds every checksum to both the base generation
/// and the absolute page id. A valid page therefore cannot be transplanted
/// between bases or positions without detection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BasePageIntegrityCatalog {
    base_generation: u64,
    covered_page_start: u64,
    covered_page_count: u64,
    page_checksums: Vec<u32>,
}

impl BasePageIntegrityCatalog {
    /// Build a catalog from already-computed, position-ordered page checksums.
    pub(crate) fn build(
        base_generation: u64,
        covered_page_start: u64,
        page_checksums: Vec<u32>,
    ) -> Result<Self> {
        let covered_page_count = u64::try_from(page_checksums.len())
            .map_err(|_| anyhow::anyhow!("Base page integrity catalog page count overflow"))?;
        validate_layout(base_generation, covered_page_start, covered_page_count)?;

        Ok(Self {
            base_generation,
            covered_page_start,
            covered_page_count,
            page_checksums,
        })
    }

    /// Build a catalog and compute one checksum for each full graph page.
    ///
    /// Pages are assigned consecutive ids beginning at `covered_page_start`.
    #[cfg(test)]
    pub(crate) fn from_pages<I, P>(
        base_generation: u64,
        covered_page_start: u64,
        pages: I,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<[u8]>,
    {
        let mut page_checksums = Vec::new();
        let mut page_id = covered_page_start;

        for page in pages {
            page_checksums.push(compute_page_checksum(
                base_generation,
                page_id,
                page.as_ref(),
            )?);
            page_id = page_id.checked_add(1).ok_or_else(|| {
                anyhow::anyhow!("Base page integrity catalog page range overflow")
            })?;
        }

        Self::build(base_generation, covered_page_start, page_checksums)
    }

    pub(crate) fn base_generation(&self) -> u64 {
        self.base_generation
    }

    pub(crate) fn covered_page_start(&self) -> u64 {
        self.covered_page_start
    }

    pub(crate) fn covered_page_count(&self) -> u64 {
        self.covered_page_count
    }

    pub(crate) fn checksum_for_page(&self, page_id: u64) -> Result<u32> {
        let offset = page_id
            .checked_sub(self.covered_page_start)
            .ok_or_else(|| {
                anyhow::anyhow!("Base page integrity catalog does not cover page {page_id}")
            })?;
        let offset = usize::try_from(offset).map_err(|_| {
            anyhow::anyhow!("Base page integrity catalog page offset does not fit in memory")
        })?;
        self.page_checksums.get(offset).copied().ok_or_else(|| {
            anyhow::anyhow!("Base page integrity catalog does not cover page {page_id}")
        })
    }

    /// Verify one full graph page against its generation-bound catalog entry.
    pub(crate) fn verify_page(&self, page_id: u64, page: &[u8]) -> Result<()> {
        let expected = self.checksum_for_page(page_id)?;
        let actual = compute_page_checksum(self.base_generation, page_id, page)?;
        if actual != expected {
            bail!("Base page integrity checksum mismatch for page {page_id}");
        }
        Ok(())
    }

    pub(crate) fn encoded_len(&self) -> Result<usize> {
        encoded_len_for_count(self.covered_page_count())
    }

    pub(crate) fn encoded_len_for_page_count(covered_page_count: u64) -> Result<usize> {
        encoded_len_for_count(covered_page_count)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let covered_page_count = self.covered_page_count();
        validate_layout(
            self.base_generation,
            self.covered_page_start,
            covered_page_count,
        )?;

        let mut encoded = Vec::with_capacity(self.encoded_len()?);
        encoded.extend_from_slice(&PAGE_INTEGRITY_CATALOG_MAGIC);
        encoded.extend_from_slice(&PAGE_INTEGRITY_CATALOG_CODEC_VERSION.to_le_bytes());
        encoded.extend_from_slice(&PAGE_INTEGRITY_CATALOG_RESERVED.to_le_bytes());
        encoded.extend_from_slice(&self.base_generation.to_le_bytes());
        encoded.extend_from_slice(&self.covered_page_start.to_le_bytes());
        encoded.extend_from_slice(&covered_page_count.to_le_bytes());
        for checksum in &self.page_checksums {
            encoded.extend_from_slice(&checksum.to_le_bytes());
        }
        debug_assert_eq!(encoded.len(), self.encoded_len()?);
        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &[u8]) -> Result<Self> {
        if encoded.len() < CATALOG_HEADER_LEN {
            bail!(
                "Base page integrity catalog is truncated: expected at least {CATALOG_HEADER_LEN} bytes, got {}",
                encoded.len()
            );
        }
        if encoded.get(0..8) != Some(PAGE_INTEGRITY_CATALOG_MAGIC.as_slice()) {
            bail!("Base page integrity catalog magic mismatch");
        }

        let codec_version = read_u32_le(encoded, 8, "catalog codec version")?;
        if codec_version != PAGE_INTEGRITY_CATALOG_CODEC_VERSION {
            bail!("Unsupported base page integrity catalog codec version {codec_version}");
        }
        let reserved = read_u32_le(encoded, 12, "catalog reserved field")?;
        if reserved != PAGE_INTEGRITY_CATALOG_RESERVED {
            bail!("Base page integrity catalog reserved field must be zero");
        }

        let base_generation = read_u64_le(encoded, 16, "catalog base generation")?;
        let covered_page_start = read_u64_le(encoded, 24, "catalog covered page start")?;
        let covered_page_count = read_u64_le(encoded, 32, "catalog covered page count")?;
        validate_layout(base_generation, covered_page_start, covered_page_count)?;

        let expected_len = encoded_len_for_count(covered_page_count)?;
        if encoded.len() != expected_len {
            bail!(
                "Base page integrity catalog length mismatch: expected {expected_len} bytes, got {}",
                encoded.len()
            );
        }

        let checksum_count = usize::try_from(covered_page_count).map_err(|_| {
            anyhow::anyhow!("Base page integrity catalog page count does not fit in memory")
        })?;
        let mut page_checksums = Vec::new();
        page_checksums
            .try_reserve_exact(checksum_count)
            .map_err(|_| {
                anyhow::anyhow!("Base page integrity catalog allocation exceeds memory limits")
            })?;
        for index in 0..checksum_count {
            let offset = CATALOG_HEADER_LEN
                .checked_add(index.checked_mul(CHECKSUM_LEN).ok_or_else(|| {
                    anyhow::anyhow!("Base page integrity catalog checksum offset overflow")
                })?)
                .ok_or_else(|| {
                    anyhow::anyhow!("Base page integrity catalog checksum offset overflow")
                })?;
            page_checksums.push(read_u32_le(encoded, offset, "catalog page checksum")?);
        }

        Self::build(base_generation, covered_page_start, page_checksums)
    }

    /// CRC stored by the owning file-format descriptor for this exact encoding.
    #[cfg(test)]
    pub(crate) fn encoded_crc32(&self) -> Result<u32> {
        Ok(catalog_crc32(&self.encode()?))
    }
}

/// Compute the generation- and position-bound checksum of exactly one graph page.
pub(crate) fn compute_page_checksum(
    base_generation: u64,
    page_id: u64,
    page: &[u8],
) -> Result<u32> {
    if base_generation == 0 {
        bail!("Base page integrity checksum requires a non-zero generation");
    }
    if page_id == 0 {
        bail!("Base page integrity checksum does not cover header page 0");
    }
    if page.len() != PAGE_SIZE {
        bail!(
            "Base page integrity checksum requires exactly {PAGE_SIZE} page bytes, got {}",
            page.len()
        );
    }

    let mut hasher = Hasher::new();
    hasher.update(PAGE_CHECKSUM_DOMAIN);
    hasher.update(&base_generation.to_le_bytes());
    hasher.update(&page_id.to_le_bytes());
    hasher.update(page);
    Ok(hasher.finalize())
}

/// Compute the CRC for an encoded catalog. The encoding starts with a unique
/// magic value, so hashing the exact bytes also domain-separates this CRC from
/// other file-format payloads.
pub(crate) fn catalog_crc32(encoded_catalog: &[u8]) -> u32 {
    crc32fast::hash(encoded_catalog)
}

fn validate_layout(
    base_generation: u64,
    covered_page_start: u64,
    covered_page_count: u64,
) -> Result<()> {
    if covered_page_count == 0 {
        if base_generation != 0 || covered_page_start != 1 {
            bail!(
                "Empty base page integrity catalog requires generation 0, page start 1, and page count 0"
            );
        }
        return Ok(());
    }

    if base_generation == 0 {
        bail!("Non-empty base page integrity catalog requires a non-zero generation");
    }
    if covered_page_start == 0 {
        bail!("Base page integrity catalog must not cover header page 0");
    }
    covered_page_start
        .checked_add(covered_page_count)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity catalog page range overflow"))?;
    Ok(())
}

fn encoded_len_for_count(covered_page_count: u64) -> Result<usize> {
    if covered_page_count > MAX_BASE_INTEGRITY_COVERED_PAGES {
        bail!(
            "Base page integrity catalog exceeds the supported {}-byte metadata limit",
            MAX_BASE_INTEGRITY_CATALOG_BYTES
        );
    }
    let checksum_bytes = covered_page_count
        .checked_mul(CHECKSUM_LEN as u64)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity catalog encoded length overflow"))?;
    let encoded_len = (CATALOG_HEADER_LEN as u64)
        .checked_add(checksum_bytes)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity catalog encoded length overflow"))?;
    let encoded_len = usize::try_from(encoded_len).map_err(|_| {
        anyhow::anyhow!("Base page integrity catalog encoded length does not fit in memory")
    })?;
    if encoded_len > MAX_BASE_INTEGRITY_CATALOG_BYTES {
        bail!(
            "Base page integrity catalog exceeds the supported {}-byte metadata limit",
            MAX_BASE_INTEGRITY_CATALOG_BYTES
        );
    }
    Ok(encoded_len)
}

fn read_u32_le(bytes: &[u8], offset: usize, label: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity {label} offset overflow"))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity {label} is truncated"))?;
    let mut value = [0u8; 4];
    value.copy_from_slice(slice);
    Ok(u32::from_le_bytes(value))
}

fn read_u64_le(bytes: &[u8], offset: usize, label: &str) -> Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity {label} offset overflow"))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow::anyhow!("Base page integrity {label} is truncated"))?;
    let mut value = [0u8; 8];
    value.copy_from_slice(slice);
    Ok(u64::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(fill: u8) -> Vec<u8> {
        vec![fill; PAGE_SIZE]
    }

    fn encoded_header(generation: u64, start: u64, count: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&PAGE_INTEGRITY_CATALOG_MAGIC);
        bytes.extend_from_slice(&PAGE_INTEGRITY_CATALOG_CODEC_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&generation.to_le_bytes());
        bytes.extend_from_slice(&start.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes
    }

    #[test]
    fn non_empty_catalog_round_trips_with_stable_layout() {
        let catalog = BasePageIntegrityCatalog::build(7, 11, vec![0x0102_0304, 0xA0B0_C0D0])
            .expect("catalog should build");

        let encoded = catalog.encode().expect("catalog should encode");
        assert_eq!(encoded.len(), CATALOG_HEADER_LEN + 8);
        assert_eq!(&encoded[0..8], b"MGPGC001");
        assert_eq!(&encoded[8..12], &1u32.to_le_bytes());
        assert_eq!(&encoded[12..16], &0u32.to_le_bytes());
        assert_eq!(&encoded[16..24], &7u64.to_le_bytes());
        assert_eq!(&encoded[24..32], &11u64.to_le_bytes());
        assert_eq!(&encoded[32..40], &2u64.to_le_bytes());
        assert_eq!(&encoded[40..44], &0x0102_0304u32.to_le_bytes());
        assert_eq!(&encoded[44..48], &0xA0B0_C0D0u32.to_le_bytes());

        let decoded = BasePageIntegrityCatalog::decode(&encoded).expect("catalog should decode");
        assert_eq!(decoded, catalog);
        assert_eq!(decoded.base_generation(), 7);
        assert_eq!(decoded.covered_page_start(), 11);
        assert_eq!(decoded.covered_page_count(), 2);
        assert_eq!(
            decoded
                .covered_page_start()
                .checked_add(decoded.covered_page_count()),
            Some(13)
        );
    }

    #[test]
    fn empty_catalog_has_one_canonical_encoding() {
        let catalog =
            BasePageIntegrityCatalog::build(0, 1, Vec::new()).expect("empty catalog should build");
        let encoded = catalog.encode().expect("empty catalog should encode");

        assert_eq!(encoded, encoded_header(0, 1, 0));
        assert_eq!(catalog.covered_page_count(), 0);
        assert_eq!(
            BasePageIntegrityCatalog::decode(&encoded).expect("empty catalog should decode"),
            catalog
        );
    }

    #[test]
    fn from_pages_computes_ordered_checksums_and_verifies_each_page() {
        let pages = vec![page(0x11), page(0x22), page(0x33)];
        let catalog = BasePageIntegrityCatalog::from_pages(9, 4, &pages)
            .expect("catalog should build from pages");

        assert_eq!(catalog.covered_page_count(), 3);
        for (offset, page) in pages.iter().enumerate() {
            let page_id = 4 + offset as u64;
            assert_eq!(
                catalog
                    .checksum_for_page(page_id)
                    .expect("page should be covered"),
                compute_page_checksum(9, page_id, page).expect("checksum should compute")
            );
            catalog
                .verify_page(page_id, page)
                .expect("page should verify");
        }
    }

    #[test]
    fn page_checksum_binds_generation_page_id_and_all_page_bytes() {
        let original = page(0x5A);
        let checksum = compute_page_checksum(41, 23, &original).expect("checksum should compute");
        assert_eq!(checksum, 0x7D01_805F, "checksum codec must remain stable");
        assert_ne!(
            checksum,
            compute_page_checksum(42, 23, &original).expect("checksum should compute")
        );
        assert_ne!(
            checksum,
            compute_page_checksum(41, 24, &original).expect("checksum should compute")
        );

        let mut changed_first = original.clone();
        changed_first[0] ^= 1;
        assert_ne!(
            checksum,
            compute_page_checksum(41, 23, &changed_first).expect("checksum should compute")
        );
        let mut changed_last = original;
        changed_last[PAGE_SIZE - 1] ^= 1;
        assert_ne!(
            checksum,
            compute_page_checksum(41, 23, &changed_last).expect("checksum should compute")
        );
    }

    #[test]
    fn page_checksum_rejects_invalid_identity_and_non_page_inputs() {
        assert!(compute_page_checksum(0, 1, &page(0)).is_err());
        assert!(compute_page_checksum(1, 0, &page(0)).is_err());
        assert!(compute_page_checksum(1, 1, &vec![0; PAGE_SIZE - 1]).is_err());
        assert!(compute_page_checksum(1, 1, &vec![0; PAGE_SIZE + 1]).is_err());
    }

    #[test]
    fn verify_page_rejects_mutation_wrong_length_and_uncovered_ids() {
        let original = page(0x77);
        let catalog =
            BasePageIntegrityCatalog::from_pages(3, 8, [&original]).expect("catalog should build");

        let mut changed = original.clone();
        changed[123] ^= 0x80;
        assert!(catalog.verify_page(8, &changed).is_err());
        assert!(catalog.verify_page(8, &original[..PAGE_SIZE - 1]).is_err());
        assert!(catalog.verify_page(7, &original).is_err());
        assert!(catalog.verify_page(9, &original).is_err());
    }

    #[test]
    fn build_enforces_canonical_empty_and_non_empty_metadata() {
        assert!(BasePageIntegrityCatalog::build(0, 1, Vec::new()).is_ok());
        assert!(BasePageIntegrityCatalog::build(1, 1, Vec::new()).is_err());
        assert!(BasePageIntegrityCatalog::build(0, 0, Vec::new()).is_err());
        assert!(BasePageIntegrityCatalog::build(0, 2, Vec::new()).is_err());
        assert!(BasePageIntegrityCatalog::build(0, 1, vec![1]).is_err());
        assert!(BasePageIntegrityCatalog::build(1, 0, vec![1]).is_err());
        assert!(BasePageIntegrityCatalog::build(1, 1, vec![1]).is_ok());
    }

    #[test]
    fn build_and_from_pages_reject_page_range_overflow() {
        assert!(BasePageIntegrityCatalog::build(1, u64::MAX, vec![1]).is_err());
        assert!(BasePageIntegrityCatalog::from_pages(1, u64::MAX, [page(1)]).is_err());
    }

    #[test]
    fn decode_rejects_truncation_and_trailing_bytes() {
        assert!(BasePageIntegrityCatalog::decode(&[]).is_err());
        assert!(BasePageIntegrityCatalog::decode(&[0; CATALOG_HEADER_LEN - 1]).is_err());

        let mut encoded = BasePageIntegrityCatalog::build(1, 1, vec![7])
            .expect("catalog should build")
            .encode()
            .expect("catalog should encode");
        encoded.pop();
        assert!(BasePageIntegrityCatalog::decode(&encoded).is_err());

        let mut encoded = BasePageIntegrityCatalog::build(1, 1, vec![7])
            .expect("catalog should build")
            .encode()
            .expect("catalog should encode");
        encoded.push(0);
        assert!(BasePageIntegrityCatalog::decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_magic_codec_and_reserved_field_changes() {
        let encoded = BasePageIntegrityCatalog::build(1, 1, vec![7])
            .expect("catalog should build")
            .encode()
            .expect("catalog should encode");

        let mut bad_magic = encoded.clone();
        bad_magic[0] ^= 1;
        assert!(BasePageIntegrityCatalog::decode(&bad_magic).is_err());

        let mut bad_codec = encoded.clone();
        bad_codec[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert!(BasePageIntegrityCatalog::decode(&bad_codec).is_err());

        let mut bad_reserved = encoded;
        bad_reserved[12..16].copy_from_slice(&1u32.to_le_bytes());
        assert!(BasePageIntegrityCatalog::decode(&bad_reserved).is_err());
    }

    #[test]
    fn decode_enforces_layout_invariants_from_untrusted_bytes() {
        assert!(BasePageIntegrityCatalog::decode(&encoded_header(1, 1, 0)).is_err());
        assert!(BasePageIntegrityCatalog::decode(&encoded_header(0, 1, 1)).is_err());
        assert!(BasePageIntegrityCatalog::decode(&encoded_header(1, 0, 1)).is_err());
        assert!(BasePageIntegrityCatalog::decode(&encoded_header(1, u64::MAX, 1)).is_err());

        let huge_count = u64::MAX / 4 + 1;
        assert!(BasePageIntegrityCatalog::decode(&encoded_header(1, 1, huge_count)).is_err());
    }

    #[test]
    fn decoded_checksum_order_matches_covered_page_ids() {
        let catalog = BasePageIntegrityCatalog::build(55, 100, vec![10, 20, 30])
            .expect("catalog should build");
        let decoded =
            BasePageIntegrityCatalog::decode(&catalog.encode().expect("catalog should encode"))
                .expect("catalog should decode");

        assert_eq!(decoded.checksum_for_page(100).expect("covered page"), 10);
        assert_eq!(decoded.checksum_for_page(101).expect("covered page"), 20);
        assert_eq!(decoded.checksum_for_page(102).expect("covered page"), 30);
        assert!(decoded.checksum_for_page(99).is_err());
        assert!(decoded.checksum_for_page(103).is_err());
    }

    #[test]
    fn catalog_crc_covers_exact_encoded_bytes() {
        let first =
            BasePageIntegrityCatalog::build(5, 2, vec![1, 2]).expect("catalog should build");
        let first_bytes = first.encode().expect("catalog should encode");
        assert_eq!(
            first.encoded_crc32().expect("catalog CRC should compute"),
            catalog_crc32(&first_bytes)
        );
        assert_eq!(
            catalog_crc32(&first_bytes),
            0x5470_5FB0,
            "catalog CRC codec must remain stable"
        );

        let second_bytes = BasePageIntegrityCatalog::build(5, 2, vec![1, 3])
            .expect("catalog should build")
            .encode()
            .expect("catalog should encode");
        assert_ne!(catalog_crc32(&first_bytes), catalog_crc32(&second_bytes));
    }
}
