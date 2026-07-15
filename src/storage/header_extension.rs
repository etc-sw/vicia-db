use crate::storage::{FileHeader, PAGE_SIZE};
use anyhow::{Result, bail};

pub(crate) const HEADER_EXTENSION_OFFSET: usize = 84;
const LEGACY_HEADER_EXTENSION_MAGIC: [u8; 8] = *b"MGHEX001";
const HEADER_EXTENSION_MAGIC: [u8; 8] = *b"MGHEX002";
const PROJECTION_HEADER_EXTENSION_MAGIC: [u8; 8] = *b"MGHEX003";
pub(crate) const LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION: u32 = 10;
pub(crate) const HEADER_EXTENSION_FILE_FORMAT_VERSION: u32 = 11;
pub(crate) const PREFIX_LEAF_FILE_FORMAT_VERSION: u32 = 12;
pub(crate) const PROJECTION_CATALOG_FILE_FORMAT_VERSION: u32 = 13;
pub(crate) const LEGACY_HEADER_EXTENSION_LEN: usize =
    HeaderExtension::PREFIX_LEN + (HeaderManifestSlot::LEN * 2) + HeaderExtension::BASE_LAYOUT_LEN;
pub(crate) const HEADER_EXTENSION_LEN: usize =
    LEGACY_HEADER_EXTENSION_LEN + BasePageIntegrityDescriptor::LEN;
pub(crate) const PROJECTION_HEADER_EXTENSION_LEN: usize =
    HEADER_EXTENSION_LEN + (ProjectionCatalogSlot::LEN * 2);
const _: () = assert!(HEADER_EXTENSION_OFFSET + PROJECTION_HEADER_EXTENSION_LEN <= PAGE_SIZE);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeaderManifestSlot {
    generation: u64,
    manifest_page_start: u64,
    manifest_page_count: u64,
    manifest_len: u64,
    manifest_checksum: u32,
    slot_checksum: u32,
}

impl HeaderManifestSlot {
    pub(crate) const LEN: usize = 40;

    pub(crate) fn new(
        generation: u64,
        manifest_page_start: u64,
        manifest_page_count: u64,
        manifest_len: u64,
        manifest_checksum: u32,
    ) -> Result<Self> {
        if generation == 0 {
            bail!("Header manifest slot generation must be non-zero");
        }
        if manifest_page_start == 0 {
            bail!("Header manifest payload must not start on page 0");
        }
        if manifest_page_count == 0 {
            bail!("Header manifest payload page count must be non-zero");
        }
        if manifest_len == 0 {
            bail!("Header manifest payload length must be non-zero");
        }
        manifest_page_start
            .checked_add(manifest_page_count)
            .ok_or_else(|| anyhow::anyhow!("Header manifest payload page range overflow"))?;

        let mut slot = Self {
            generation,
            manifest_page_start,
            manifest_page_count,
            manifest_len,
            manifest_checksum,
            slot_checksum: 0,
        };
        slot.slot_checksum = slot.compute_checksum();
        Ok(slot)
    }

    pub(crate) fn empty() -> Self {
        Self {
            generation: 0,
            manifest_page_start: 0,
            manifest_page_count: 0,
            manifest_len: 0,
            manifest_checksum: 0,
            slot_checksum: 0,
        }
    }

    pub(crate) fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) fn manifest_page_start(self) -> u64 {
        self.manifest_page_start
    }

    pub(crate) fn manifest_page_count(self) -> u64 {
        self.manifest_page_count
    }

    pub(crate) fn manifest_len(self) -> u64 {
        self.manifest_len
    }

    pub(crate) fn manifest_checksum(self) -> u32 {
        self.manifest_checksum
    }

    pub(crate) fn checksum_valid(self) -> bool {
        self.is_empty() || self.slot_checksum == self.compute_checksum()
    }

    fn state(self) -> HeaderManifestSlotState {
        if self.is_empty() {
            HeaderManifestSlotState::Empty
        } else if self.is_selectable() {
            HeaderManifestSlotState::Valid(self)
        } else {
            HeaderManifestSlotState::Invalid
        }
    }

    pub(crate) fn is_selectable(self) -> bool {
        !self.is_empty()
            && self.checksum_valid()
            && self.generation > 0
            && self.manifest_page_start > 0
            && self.manifest_page_count > 0
            && self.manifest_len > 0
            && self
                .manifest_page_start
                .checked_add(self.manifest_page_count)
                .is_some()
    }

    pub(crate) fn is_empty(self) -> bool {
        self.generation == 0
            && self.manifest_page_start == 0
            && self.manifest_page_count == 0
            && self.manifest_len == 0
            && self.manifest_checksum == 0
            && self.slot_checksum == 0
    }

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..8].copy_from_slice(&self.generation.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.manifest_page_start.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.manifest_page_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.manifest_len.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.manifest_checksum.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.slot_checksum.to_le_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::LEN {
            bail!("Header manifest slot length is invalid");
        }
        Ok(Self {
            generation: read_u64_le(bytes, 0, "header manifest slot generation")?,
            manifest_page_start: read_u64_le(bytes, 8, "header manifest payload page start")?,
            manifest_page_count: read_u64_le(bytes, 16, "header manifest payload page count")?,
            manifest_len: read_u64_le(bytes, 24, "header manifest payload length")?,
            manifest_checksum: read_u32_le(bytes, 32, "header manifest payload checksum")?,
            slot_checksum: read_u32_le(bytes, 36, "header manifest slot checksum")?,
        })
    }

    fn checksum_payload(self) -> [u8; Self::LEN - 4] {
        let mut bytes = [0u8; Self::LEN - 4];
        bytes[0..8].copy_from_slice(&self.generation.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.manifest_page_start.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.manifest_page_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.manifest_len.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.manifest_checksum.to_le_bytes());
        bytes
    }

    fn compute_checksum(self) -> u32 {
        crc32fast::hash(&self.checksum_payload())
    }
}

/// Page-0 pointer to one committed projection catalog generation.
///
/// This intentionally mirrors the delta-manifest slot wire size while keeping
/// the two authorities semantically distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionCatalogSlot {
    generation: u64,
    catalog_page_start: u64,
    catalog_page_count: u64,
    catalog_len: u64,
    catalog_checksum: u32,
    slot_checksum: u32,
}

impl ProjectionCatalogSlot {
    pub(crate) const LEN: usize = 40;

    pub(crate) fn new(
        generation: u64,
        catalog_page_start: u64,
        catalog_page_count: u64,
        catalog_len: u64,
        catalog_checksum: u32,
    ) -> Result<Self> {
        if generation == 0 {
            bail!("Projection catalog slot generation must be non-zero");
        }
        if catalog_page_start == 0 || catalog_page_count == 0 || catalog_len == 0 {
            bail!("Projection catalog slot range must be non-empty");
        }
        let capacity = catalog_page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| anyhow::anyhow!("Projection catalog slot capacity overflow"))?;
        let minimum_len = catalog_page_count
            .saturating_sub(1)
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| anyhow::anyhow!("Projection catalog slot minimum length overflow"))?;
        if catalog_len > capacity || catalog_len <= minimum_len {
            bail!("Projection catalog slot length is not canonical for its page count");
        }
        catalog_page_start
            .checked_add(catalog_page_count)
            .ok_or_else(|| anyhow::anyhow!("Projection catalog slot page range overflow"))?;
        let mut slot = Self {
            generation,
            catalog_page_start,
            catalog_page_count,
            catalog_len,
            catalog_checksum,
            slot_checksum: 0,
        };
        slot.slot_checksum = slot.compute_checksum();
        Ok(slot)
    }

    pub(crate) fn empty() -> Self {
        Self {
            generation: 0,
            catalog_page_start: 0,
            catalog_page_count: 0,
            catalog_len: 0,
            catalog_checksum: 0,
            slot_checksum: 0,
        }
    }

    pub(crate) fn generation(self) -> u64 {
        self.generation
    }
    pub(crate) fn catalog_page_start(self) -> u64 {
        self.catalog_page_start
    }
    pub(crate) fn catalog_page_count(self) -> u64 {
        self.catalog_page_count
    }
    pub(crate) fn catalog_len(self) -> u64 {
        self.catalog_len
    }
    pub(crate) fn catalog_checksum(self) -> u32 {
        self.catalog_checksum
    }

    pub(crate) fn is_empty(self) -> bool {
        self == Self::empty()
    }

    pub(crate) fn is_selectable(self) -> bool {
        !self.is_empty()
            && self.slot_checksum == self.compute_checksum()
            && self.catalog_page_start > 0
            && self.catalog_page_count > 0
            && self.catalog_len > 0
            && self
                .catalog_page_start
                .checked_add(self.catalog_page_count)
                .is_some()
    }

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..8].copy_from_slice(&self.generation.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.catalog_page_start.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.catalog_page_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.catalog_len.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.catalog_checksum.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.slot_checksum.to_le_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::LEN {
            bail!("Projection catalog slot length is invalid");
        }
        Ok(Self {
            generation: read_u64_le(bytes, 0, "projection catalog slot generation")?,
            catalog_page_start: read_u64_le(bytes, 8, "projection catalog page start")?,
            catalog_page_count: read_u64_le(bytes, 16, "projection catalog page count")?,
            catalog_len: read_u64_le(bytes, 24, "projection catalog length")?,
            catalog_checksum: read_u32_le(bytes, 32, "projection catalog checksum")?,
            slot_checksum: read_u32_le(bytes, 36, "projection catalog slot checksum")?,
        })
    }

    fn compute_checksum(self) -> u32 {
        crc32fast::hash(&self.to_bytes()[..Self::LEN - 4])
    }
}

/// Page-0 authority for one generation-bound base-page checksum catalog.
///
/// The catalog is published after the covered immutable base pages. Fresh
/// bases place it immediately after the coverage; v10 migrations may append it
/// after an already-published delta lineage. Its exact byte length and CRC let
/// readers checksum the catalog without scanning the base itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BasePageIntegrityDescriptor {
    base_generation: u64,
    covered_page_start: u64,
    covered_page_count: u64,
    catalog_page_start: u64,
    catalog_page_count: u64,
    catalog_len: u64,
    catalog_checksum: u32,
    descriptor_checksum: u32,
}

impl BasePageIntegrityDescriptor {
    pub(crate) const LEN: usize = (8 * 6) + (4 * 2);

    pub(crate) fn new(
        base_generation: u64,
        covered_page_start: u64,
        covered_page_count: u64,
        catalog_page_start: u64,
        catalog_page_count: u64,
        catalog_len: u64,
        catalog_checksum: u32,
    ) -> Result<Self> {
        let mut descriptor = Self {
            base_generation,
            covered_page_start,
            covered_page_count,
            catalog_page_start,
            catalog_page_count,
            catalog_len,
            catalog_checksum,
            descriptor_checksum: 0,
        };
        descriptor.validate_layout()?;
        descriptor.descriptor_checksum = descriptor.compute_checksum();
        Ok(descriptor)
    }

    pub(crate) fn empty() -> Self {
        Self {
            base_generation: 0,
            covered_page_start: 0,
            covered_page_count: 0,
            catalog_page_start: 0,
            catalog_page_count: 0,
            catalog_len: 0,
            catalog_checksum: 0,
            descriptor_checksum: 0,
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self == Self::empty()
    }

    pub(crate) fn base_generation(self) -> u64 {
        self.base_generation
    }

    pub(crate) fn covered_page_start(self) -> u64 {
        self.covered_page_start
    }

    pub(crate) fn covered_page_count(self) -> u64 {
        self.covered_page_count
    }

    pub(crate) fn covered_page_end(self) -> Result<u64> {
        self.covered_page_start
            .checked_add(self.covered_page_count)
            .ok_or_else(|| anyhow::anyhow!("Base integrity covered page range overflow"))
    }

    pub(crate) fn catalog_page_start(self) -> u64 {
        self.catalog_page_start
    }

    pub(crate) fn catalog_page_count(self) -> u64 {
        self.catalog_page_count
    }

    pub(crate) fn catalog_page_end(self) -> Result<u64> {
        self.catalog_page_start
            .checked_add(self.catalog_page_count)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog page range overflow"))
    }

    pub(crate) fn catalog_len(self) -> u64 {
        self.catalog_len
    }

    pub(crate) fn catalog_checksum(self) -> u32 {
        self.catalog_checksum
    }

    pub(crate) fn checksum_valid(self) -> bool {
        self.is_empty() || self.descriptor_checksum == self.compute_checksum()
    }

    fn validate_layout(self) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        if self.base_generation == 0 {
            bail!("Base integrity descriptor generation must be non-zero");
        }
        if self.covered_page_start == 0 || self.covered_page_count == 0 {
            bail!("Base integrity descriptor must cover non-header pages");
        }
        if self.covered_page_end()? > self.catalog_page_start {
            bail!("Base integrity catalog must not overlap covered pages");
        }
        if self.catalog_page_count == 0 || self.catalog_len == 0 {
            bail!("Base integrity catalog page count and length must be non-zero");
        }
        let catalog_capacity = self
            .catalog_page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog capacity overflow"))?;
        if self.catalog_len > catalog_capacity {
            bail!("Base integrity catalog length exceeds its page capacity");
        }
        let minimum_len = self
            .catalog_page_count
            .saturating_sub(1)
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| anyhow::anyhow!("Base integrity catalog minimum length overflow"))?;
        if self.catalog_len <= minimum_len {
            bail!("Base integrity catalog page count is not canonical for its length");
        }
        self.catalog_page_end()?;
        Ok(())
    }

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..8].copy_from_slice(&self.base_generation.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.covered_page_start.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.covered_page_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.catalog_page_start.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.catalog_page_count.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.catalog_len.to_le_bytes());
        bytes[48..52].copy_from_slice(&self.catalog_checksum.to_le_bytes());
        bytes[52..56].copy_from_slice(&self.descriptor_checksum.to_le_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::LEN {
            bail!("Base integrity descriptor length is invalid");
        }
        let descriptor = Self {
            base_generation: read_u64_le(bytes, 0, "base integrity generation")?,
            covered_page_start: read_u64_le(bytes, 8, "base integrity covered page start")?,
            covered_page_count: read_u64_le(bytes, 16, "base integrity covered page count")?,
            catalog_page_start: read_u64_le(bytes, 24, "base integrity catalog page start")?,
            catalog_page_count: read_u64_le(bytes, 32, "base integrity catalog page count")?,
            catalog_len: read_u64_le(bytes, 40, "base integrity catalog length")?,
            catalog_checksum: read_u32_le(bytes, 48, "base integrity catalog checksum")?,
            descriptor_checksum: read_u32_le(bytes, 52, "base integrity descriptor checksum")?,
        };
        if descriptor.is_empty() {
            return Ok(descriptor);
        }
        descriptor.validate_layout()?;
        if !descriptor.checksum_valid() {
            bail!("Base integrity descriptor checksum mismatch");
        }
        Ok(descriptor)
    }

    fn compute_checksum(self) -> u32 {
        crc32fast::hash(&self.to_bytes()[..Self::LEN - 4])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderExtension {
    primary: HeaderManifestSlot,
    secondary: HeaderManifestSlot,
    base_fact_page_start: u64,
    base_layout_checksum: u32,
    base_integrity: BasePageIntegrityDescriptor,
    projection_primary: ProjectionCatalogSlot,
    projection_secondary: ProjectionCatalogSlot,
}

impl HeaderExtension {
    pub(crate) const PREFIX_LEN: usize = 12;
    pub(crate) const BASE_LAYOUT_LEN: usize = 12;

    pub(crate) fn new(primary: HeaderManifestSlot, secondary: HeaderManifestSlot) -> Self {
        let base_fact_page_start = 1;
        Self {
            primary,
            secondary,
            base_fact_page_start,
            base_layout_checksum: Self::compute_base_layout_checksum(base_fact_page_start),
            base_integrity: BasePageIntegrityDescriptor::empty(),
            projection_primary: ProjectionCatalogSlot::empty(),
            projection_secondary: ProjectionCatalogSlot::empty(),
        }
    }

    pub(crate) fn empty() -> Self {
        Self::new(HeaderManifestSlot::empty(), HeaderManifestSlot::empty())
    }

    pub(crate) fn primary(&self) -> HeaderManifestSlot {
        self.primary
    }

    pub(crate) fn secondary(&self) -> HeaderManifestSlot {
        self.secondary
    }

    pub(crate) fn base_fact_page_start(&self) -> u64 {
        self.base_fact_page_start
    }

    pub(crate) fn base_integrity(&self) -> BasePageIntegrityDescriptor {
        self.base_integrity
    }

    pub(crate) fn projection_primary(&self) -> ProjectionCatalogSlot {
        self.projection_primary
    }

    pub(crate) fn projection_secondary(&self) -> ProjectionCatalogSlot {
        self.projection_secondary
    }

    pub(crate) fn with_projection_catalog_slots(
        mut self,
        primary: ProjectionCatalogSlot,
        secondary: ProjectionCatalogSlot,
    ) -> Result<Self> {
        if (!primary.is_empty() && !primary.is_selectable())
            || (!secondary.is_empty() && !secondary.is_selectable())
        {
            bail!("Projection catalog slot is invalid");
        }
        self.projection_primary = primary;
        self.projection_secondary = secondary;
        Ok(self)
    }

    pub(crate) fn with_base_fact_page_start(mut self, base_fact_page_start: u64) -> Result<Self> {
        if base_fact_page_start == 0 {
            bail!("Base fact pages must not start on page 0");
        }
        self.base_fact_page_start = base_fact_page_start;
        self.base_layout_checksum = Self::compute_base_layout_checksum(base_fact_page_start);
        if !self.base_integrity.is_empty()
            && self.base_integrity.covered_page_start() != base_fact_page_start
        {
            bail!("Base integrity coverage must start at the base fact page");
        }
        Ok(self)
    }

    pub(crate) fn with_base_integrity(
        mut self,
        base_integrity: BasePageIntegrityDescriptor,
    ) -> Result<Self> {
        if !base_integrity.is_empty() {
            base_integrity.validate_layout()?;
            if !base_integrity.checksum_valid() {
                bail!("Base integrity descriptor checksum mismatch");
            }
            if base_integrity.covered_page_start() != self.base_fact_page_start {
                bail!("Base integrity coverage must start at the base fact page");
            }
        }
        self.base_integrity = base_integrity;
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>> {
        self.to_bytes_for_version(HEADER_EXTENSION_FILE_FORMAT_VERSION)
    }

    fn to_bytes_for_version(&self, file_format_version: u32) -> Result<Vec<u8>> {
        let (magic, capacity) = match file_format_version {
            LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION => {
                (LEGACY_HEADER_EXTENSION_MAGIC, LEGACY_HEADER_EXTENSION_LEN)
            }
            HEADER_EXTENSION_FILE_FORMAT_VERSION | PREFIX_LEAF_FILE_FORMAT_VERSION => {
                (HEADER_EXTENSION_MAGIC, HEADER_EXTENSION_LEN)
            }
            PROJECTION_CATALOG_FILE_FORMAT_VERSION => (
                PROJECTION_HEADER_EXTENSION_MAGIC,
                PROJECTION_HEADER_EXTENSION_LEN,
            ),
            _ => bail!("Unsupported header extension file format version"),
        };
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(&magic);
        bytes.extend_from_slice(&file_format_version.to_le_bytes());
        bytes.extend_from_slice(&self.primary.to_bytes());
        bytes.extend_from_slice(&self.secondary.to_bytes());
        bytes.extend_from_slice(&self.base_fact_page_start.to_le_bytes());
        bytes.extend_from_slice(&self.base_layout_checksum.to_le_bytes());
        if file_format_version >= HEADER_EXTENSION_FILE_FORMAT_VERSION {
            bytes.extend_from_slice(&self.base_integrity.to_bytes());
        } else if !self.base_integrity.is_empty() {
            bail!("v10 header extension cannot encode v11 base integrity metadata");
        }
        if file_format_version >= PROJECTION_CATALOG_FILE_FORMAT_VERSION {
            bytes.extend_from_slice(&self.projection_primary.to_bytes());
            bytes.extend_from_slice(&self.projection_secondary.to_bytes());
        } else if !self.projection_primary.is_empty() || !self.projection_secondary.is_empty() {
            bail!("v12 header extension cannot encode projection catalog slots");
        }
        debug_assert_eq!(bytes.len(), capacity);
        Ok(bytes)
    }

    fn write_to_page0_for_version(&self, file_format_version: u32, page: &mut [u8]) -> Result<()> {
        if page.len() != PAGE_SIZE {
            bail!("Header extension write requires a full page 0");
        }
        let bytes = self.to_bytes_for_version(file_format_version)?;
        let extension_end = HEADER_EXTENSION_OFFSET
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow::anyhow!("Header extension page range overflow"))?;
        page.get_mut(HEADER_EXTENSION_OFFSET..extension_end)
            .ok_or_else(|| anyhow::anyhow!("Header extension does not fit in page 0"))?
            .copy_from_slice(&bytes);
        Ok(())
    }

    pub(crate) fn read_from_page0(file_format_version: u32, page: &[u8]) -> Result<Option<Self>> {
        if page.len() < HEADER_EXTENSION_OFFSET {
            bail!("Page 0 is too short for legacy header");
        }
        if file_format_version < LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION {
            return Ok(None);
        }
        if file_format_version != LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION
            && file_format_version != HEADER_EXTENSION_FILE_FORMAT_VERSION
            && file_format_version != PREFIX_LEAF_FILE_FORMAT_VERSION
            && file_format_version != PROJECTION_CATALOG_FILE_FORMAT_VERSION
        {
            bail!("Unsupported header extension file format version");
        }

        let extension_len = match file_format_version {
            LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION => LEGACY_HEADER_EXTENSION_LEN,
            HEADER_EXTENSION_FILE_FORMAT_VERSION | PREFIX_LEAF_FILE_FORMAT_VERSION => {
                HEADER_EXTENSION_LEN
            }
            PROJECTION_CATALOG_FILE_FORMAT_VERSION => PROJECTION_HEADER_EXTENSION_LEN,
            _ => unreachable!("file format version was validated above"),
        };
        let Some(extension_bytes) =
            page.get(HEADER_EXTENSION_OFFSET..HEADER_EXTENSION_OFFSET + extension_len)
        else {
            bail!("Header extension is truncated");
        };
        if extension_bytes.iter().all(|byte| *byte == 0) {
            bail!("Header extension is missing for v{file_format_version} header");
        }

        let magic = extension_bytes
            .get(0..HEADER_EXTENSION_MAGIC.len())
            .ok_or_else(|| anyhow::anyhow!("Header extension missing magic"))?;
        let expected_magic = match file_format_version {
            LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION => LEGACY_HEADER_EXTENSION_MAGIC,
            HEADER_EXTENSION_FILE_FORMAT_VERSION | PREFIX_LEAF_FILE_FORMAT_VERSION => {
                HEADER_EXTENSION_MAGIC
            }
            PROJECTION_CATALOG_FILE_FORMAT_VERSION => PROJECTION_HEADER_EXTENSION_MAGIC,
            _ => unreachable!("file format version was validated above"),
        };
        if magic != expected_magic {
            bail!("Header extension magic mismatch");
        }

        let extension_file_format_version = read_u32_le(
            extension_bytes,
            HEADER_EXTENSION_MAGIC.len(),
            "header extension file format version",
        )?;
        if extension_file_format_version != file_format_version {
            bail!("Unsupported header extension file format version");
        }

        let primary_start = Self::PREFIX_LEN;
        let secondary_start = primary_start
            .checked_add(HeaderManifestSlot::LEN)
            .ok_or_else(|| anyhow::anyhow!("Header extension secondary slot offset overflow"))?;
        let secondary_end = secondary_start
            .checked_add(HeaderManifestSlot::LEN)
            .ok_or_else(|| anyhow::anyhow!("Header extension end offset overflow"))?;
        let base_fact_page_start_offset = secondary_end;
        let base_layout_checksum_offset = base_fact_page_start_offset
            .checked_add(8)
            .ok_or_else(|| anyhow::anyhow!("Header extension base layout checksum overflow"))?;

        let primary = HeaderManifestSlot::from_bytes(
            extension_bytes
                .get(primary_start..secondary_start)
                .ok_or_else(|| anyhow::anyhow!("Header extension primary slot out of bounds"))?,
        )?;
        let secondary = HeaderManifestSlot::from_bytes(
            extension_bytes
                .get(secondary_start..secondary_end)
                .ok_or_else(|| anyhow::anyhow!("Header extension secondary slot out of bounds"))?,
        )?;
        let raw_base_fact_page_start = read_u64_le(
            extension_bytes,
            base_fact_page_start_offset,
            "header extension base fact page start",
        )?;
        let raw_base_layout_checksum = read_u32_le(
            extension_bytes,
            base_layout_checksum_offset,
            "header extension base layout checksum",
        )?;
        let base_fact_page_start = if raw_base_fact_page_start == 0 && raw_base_layout_checksum == 0
        {
            1
        } else {
            if raw_base_fact_page_start == 0 {
                bail!("Header extension base fact page start must be non-zero");
            }
            let expected = Self::compute_base_layout_checksum(raw_base_fact_page_start);
            if raw_base_layout_checksum != expected {
                bail!("Header extension base layout checksum mismatch");
            }
            raw_base_fact_page_start
        };

        let base_integrity = if file_format_version >= HEADER_EXTENSION_FILE_FORMAT_VERSION {
            BasePageIntegrityDescriptor::from_bytes(
                extension_bytes
                    .get(LEGACY_HEADER_EXTENSION_LEN..HEADER_EXTENSION_LEN)
                    .ok_or_else(|| anyhow::anyhow!("Base integrity descriptor is truncated"))?,
            )?
        } else {
            BasePageIntegrityDescriptor::empty()
        };
        if !base_integrity.is_empty() && base_integrity.covered_page_start() != base_fact_page_start
        {
            bail!("Base integrity coverage does not match the base fact page start");
        }

        let (projection_primary, projection_secondary) =
            if file_format_version >= PROJECTION_CATALOG_FILE_FORMAT_VERSION {
                let primary_start = HEADER_EXTENSION_LEN;
                let secondary_start = primary_start + ProjectionCatalogSlot::LEN;
                let secondary_end = secondary_start + ProjectionCatalogSlot::LEN;
                (
                    ProjectionCatalogSlot::from_bytes(
                        extension_bytes
                            .get(primary_start..secondary_start)
                            .ok_or_else(|| {
                                anyhow::anyhow!("Projection primary catalog slot is truncated")
                            })?,
                    )?,
                    ProjectionCatalogSlot::from_bytes(
                        extension_bytes
                            .get(secondary_start..secondary_end)
                            .ok_or_else(|| {
                                anyhow::anyhow!("Projection secondary catalog slot is truncated")
                            })?,
                    )?,
                )
            } else {
                (
                    ProjectionCatalogSlot::empty(),
                    ProjectionCatalogSlot::empty(),
                )
            };

        Ok(Some(Self {
            primary,
            secondary,
            base_fact_page_start,
            base_layout_checksum: Self::compute_base_layout_checksum(base_fact_page_start),
            base_integrity,
            projection_primary,
            projection_secondary,
        }))
    }

    fn compute_base_layout_checksum(base_fact_page_start: u64) -> u32 {
        crc32fast::hash(&base_fact_page_start.to_le_bytes())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderManifestSlotState {
    Empty,
    Valid(HeaderManifestSlot),
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderManifestSlotName {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderManifestSlotRecoveryReason {
    CorruptManifestSlot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderManifestSlotSelection {
    NoDeltaManifest,
    Use {
        slot: HeaderManifestSlotName,
        descriptor: HeaderManifestSlot,
    },
    RecoveryRequired {
        reason: HeaderManifestSlotRecoveryReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionCatalogSlotName {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionCatalogSlotSelection {
    NoProjectionCatalog,
    Candidates {
        newest: (ProjectionCatalogSlotName, ProjectionCatalogSlot),
        previous: Option<(ProjectionCatalogSlotName, ProjectionCatalogSlot)>,
    },
    RecoveryRequired,
}

pub(crate) fn select_projection_catalog_slots(
    extension: &HeaderExtension,
) -> ProjectionCatalogSlotSelection {
    let slots = [
        (
            ProjectionCatalogSlotName::Primary,
            extension.projection_primary(),
        ),
        (
            ProjectionCatalogSlotName::Secondary,
            extension.projection_secondary(),
        ),
    ];
    let has_invalid = slots
        .iter()
        .any(|(_, slot)| !slot.is_empty() && !slot.is_selectable());
    let mut valid = slots
        .into_iter()
        .filter(|(_, slot)| slot.is_selectable())
        .collect::<Vec<_>>();
    valid.sort_by_key(|(_, slot)| std::cmp::Reverse(slot.generation()));
    match valid.as_slice() {
        [] if has_invalid => ProjectionCatalogSlotSelection::RecoveryRequired,
        [] => ProjectionCatalogSlotSelection::NoProjectionCatalog,
        [newest] => ProjectionCatalogSlotSelection::Candidates {
            newest: *newest,
            previous: None,
        },
        [newest, previous] => ProjectionCatalogSlotSelection::Candidates {
            newest: *newest,
            previous: Some(*previous),
        },
        _ => unreachable!("page 0 contains exactly two projection catalog slots"),
    }
}

pub(crate) fn select_header_manifest_slot(
    extension: &HeaderExtension,
) -> HeaderManifestSlotSelection {
    let primary = extension.primary.state();
    let secondary = extension.secondary.state();

    match (primary, secondary) {
        (HeaderManifestSlotState::Valid(primary), HeaderManifestSlotState::Valid(secondary)) => {
            if primary.generation() >= secondary.generation() {
                HeaderManifestSlotSelection::Use {
                    slot: HeaderManifestSlotName::Primary,
                    descriptor: primary,
                }
            } else {
                HeaderManifestSlotSelection::Use {
                    slot: HeaderManifestSlotName::Secondary,
                    descriptor: secondary,
                }
            }
        }
        (HeaderManifestSlotState::Valid(primary), _) => HeaderManifestSlotSelection::Use {
            slot: HeaderManifestSlotName::Primary,
            descriptor: primary,
        },
        (_, HeaderManifestSlotState::Valid(secondary)) => HeaderManifestSlotSelection::Use {
            slot: HeaderManifestSlotName::Secondary,
            descriptor: secondary,
        },
        (HeaderManifestSlotState::Empty, HeaderManifestSlotState::Empty) => {
            HeaderManifestSlotSelection::NoDeltaManifest
        }
        _ => HeaderManifestSlotSelection::RecoveryRequired {
            reason: HeaderManifestSlotRecoveryReason::CorruptManifestSlot,
        },
    }
}

pub(crate) fn select_header_manifest_slot_from_page0(
    file_format_version: u32,
    page: &[u8],
) -> Result<HeaderManifestSlotSelection> {
    let Some(extension) = HeaderExtension::read_from_page0(file_format_version, page)? else {
        return Ok(HeaderManifestSlotSelection::NoDeltaManifest);
    };
    Ok(select_header_manifest_slot(&extension))
}

pub(crate) fn build_header_page(header: FileHeader) -> Result<Vec<u8>> {
    header.validate()?;
    let mut page = header.to_bytes();
    if page.len() > PAGE_SIZE {
        bail!("Header bytes exceed page size");
    }
    page.resize(PAGE_SIZE, 0);

    if header.version == LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION
        || header.version == HEADER_EXTENSION_FILE_FORMAT_VERSION
        || header.version == PREFIX_LEAF_FILE_FORMAT_VERSION
        || header.version == PROJECTION_CATALOG_FILE_FORMAT_VERSION
    {
        HeaderExtension::empty().write_to_page0_for_version(header.version, &mut page)?;
    } else if header.version > PROJECTION_CATALOG_FILE_FORMAT_VERSION {
        bail!("Unsupported header extension file format version");
    }

    Ok(page)
}

pub(crate) fn build_header_page_with_extension(
    header: FileHeader,
    extension: HeaderExtension,
) -> Result<Vec<u8>> {
    header.validate()?;
    if header.version != LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION
        && header.version != HEADER_EXTENSION_FILE_FORMAT_VERSION
        && header.version != PREFIX_LEAF_FILE_FORMAT_VERSION
        && header.version != PROJECTION_CATALOG_FILE_FORMAT_VERSION
    {
        bail!("Header extension requires v10 through v13 file format");
    }
    let mut page = header.to_bytes();
    if page.len() > PAGE_SIZE {
        bail!("Header bytes exceed page size");
    }
    page.resize(PAGE_SIZE, 0);
    extension.write_to_page0_for_version(header.version, &mut page)?;
    Ok(page)
}

fn read_u32_le(bytes: &[u8], offset: usize, label: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| anyhow::anyhow!("{label} offset overflow"))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow::anyhow!("{label} out of bounds"))?;
    let mut buf = [0u8; 4];
    buf.copy_from_slice(slice);
    Ok(u32::from_le_bytes(buf))
}

fn read_u64_le(bytes: &[u8], offset: usize, label: &str) -> Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| anyhow::anyhow!("{label} offset overflow"))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow::anyhow!("{label} out of bounds"))?;
    let mut buf = [0u8; 8];
    buf.copy_from_slice(slice);
    Ok(u64::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::{
        BasePageIntegrityDescriptor, HEADER_EXTENSION_FILE_FORMAT_VERSION, HEADER_EXTENSION_LEN,
        HEADER_EXTENSION_OFFSET, HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName,
        HeaderManifestSlotRecoveryReason, HeaderManifestSlotSelection,
        LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION, LEGACY_HEADER_EXTENSION_LEN,
        PREFIX_LEAF_FILE_FORMAT_VERSION, PROJECTION_CATALOG_FILE_FORMAT_VERSION,
        PROJECTION_HEADER_EXTENSION_LEN, ProjectionCatalogSlot, ProjectionCatalogSlotName,
        ProjectionCatalogSlotSelection, build_header_page, build_header_page_with_extension,
        select_header_manifest_slot, select_header_manifest_slot_from_page0,
        select_projection_catalog_slots,
    };
    use crate::storage::{FORMAT_VERSION, FileHeader, PAGE_SIZE};

    fn slot(generation: u64, page_start: u64) -> HeaderManifestSlot {
        HeaderManifestSlot::new(generation, page_start, 2, 128, 0xCAFE_BABE)
            .expect("slot descriptor should be valid")
    }

    fn projection_slot(generation: u64, page_start: u64) -> ProjectionCatalogSlot {
        ProjectionCatalogSlot::new(generation, page_start, 1, 128, 0x1234_5678)
            .expect("projection slot should be valid")
    }

    fn page_with_extension(extension: &HeaderExtension) -> Vec<u8> {
        page_with_header_version_and_extension(HEADER_EXTENSION_FILE_FORMAT_VERSION, extension)
    }

    fn page_with_header_version_and_extension(
        file_format_version: u32,
        extension: &HeaderExtension,
    ) -> Vec<u8> {
        let mut header = FileHeader::new();
        header.version = file_format_version;
        let mut page = header.to_bytes();
        page.resize(PAGE_SIZE, 0);
        extension
            .write_to_page0_for_version(file_format_version, &mut page)
            .expect("extension should fit in page 0");
        page
    }

    #[test]
    fn current_format_publishes_v12_header_extension_gate() {
        assert_eq!(FORMAT_VERSION, 12);
        assert_eq!(FileHeader::new().version, 12);
        assert_eq!(PREFIX_LEAF_FILE_FORMAT_VERSION, 12);
        assert_eq!(HEADER_EXTENSION_FILE_FORMAT_VERSION, 11);
        assert_eq!(LEGACY_HEADER_EXTENSION_FILE_FORMAT_VERSION, 10);
    }

    #[test]
    fn v13_projection_catalog_slots_round_trip_and_select_newest() {
        let extension = HeaderExtension::empty()
            .with_projection_catalog_slots(projection_slot(4, 100), projection_slot(5, 200))
            .unwrap();
        let page = page_with_header_version_and_extension(
            PROJECTION_CATALOG_FILE_FORMAT_VERSION,
            &extension,
        );
        let decoded =
            HeaderExtension::read_from_page0(PROJECTION_CATALOG_FILE_FORMAT_VERSION, &page)
                .unwrap()
                .unwrap();
        assert_eq!(
            decoded
                .to_bytes_for_version(PROJECTION_CATALOG_FILE_FORMAT_VERSION)
                .unwrap()
                .len(),
            PROJECTION_HEADER_EXTENSION_LEN
        );
        assert!(matches!(
            select_projection_catalog_slots(&decoded),
            ProjectionCatalogSlotSelection::Candidates {
                newest: (ProjectionCatalogSlotName::Secondary, descriptor),
                previous: Some((ProjectionCatalogSlotName::Primary, _)),
            } if descriptor.generation() == 5
        ));
    }

    #[test]
    fn corrupt_newer_projection_slot_keeps_verified_predecessor() {
        let extension = HeaderExtension::empty()
            .with_projection_catalog_slots(projection_slot(4, 100), projection_slot(5, 200))
            .unwrap();
        let mut page = page_with_header_version_and_extension(
            PROJECTION_CATALOG_FILE_FORMAT_VERSION,
            &extension,
        );
        let secondary_checksum = HEADER_EXTENSION_OFFSET
            + HEADER_EXTENSION_LEN
            + ProjectionCatalogSlot::LEN
            + ProjectionCatalogSlot::LEN
            - 1;
        page[secondary_checksum] ^= 1;
        let decoded =
            HeaderExtension::read_from_page0(PROJECTION_CATALOG_FILE_FORMAT_VERSION, &page)
                .unwrap()
                .unwrap();
        assert!(matches!(
            select_projection_catalog_slots(&decoded),
            ProjectionCatalogSlotSelection::Candidates {
                newest: (ProjectionCatalogSlotName::Primary, descriptor),
                previous: None,
            } if descriptor.generation() == 4
        ));
    }

    #[test]
    fn v9_header_still_reads_without_extension() {
        let mut header = FileHeader::new();
        header.version = 9;
        let mut page = header.to_bytes();
        page.resize(PAGE_SIZE, 0);

        let header = FileHeader::from_bytes(&page).expect("legacy header should parse");
        header.validate().expect("legacy header should validate");
        let extension = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension read should work");

        assert_eq!(header.version, 9);
        assert!(
            extension.is_none(),
            "v9 page must not imply a manifest extension"
        );
    }

    #[test]
    fn current_header_page_builder_writes_empty_extension() {
        let header = FileHeader::new();
        let page = build_header_page(header).expect("current header page should build");
        let parsed = FileHeader::from_bytes(&page).expect("current header should parse");
        let selection = select_header_manifest_slot_from_page0(parsed.version, &page)
            .expect("current header extension should decode");

        assert_eq!(page.len(), PAGE_SIZE);
        assert_eq!(parsed.version, FORMAT_VERSION);
        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
    }

    #[test]
    fn v9_header_page_builder_leaves_extension_inactive() {
        let mut header = FileHeader::new();
        header.version = 9;
        let page = build_header_page(header).expect("legacy header page should build");
        let parsed = FileHeader::from_bytes(&page).expect("legacy header should parse");
        let selection = select_header_manifest_slot_from_page0(parsed.version, &page)
            .expect("legacy header extension read should work");

        assert_eq!(parsed.version, 9);
        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
    }

    #[test]
    fn v10_header_without_extension_is_rejected() {
        let mut header = FileHeader::new();
        header.version = 10;
        let mut page = header.to_bytes();
        page.resize(PAGE_SIZE, 0);
        let result = select_header_manifest_slot_from_page0(header.version, &page);

        assert!(
            result.is_err(),
            "v10 page must include a non-empty header extension"
        );
    }

    #[test]
    fn explicit_header_page_builder_writes_provided_extension() {
        let header = FileHeader::new();
        let descriptor = slot(12, 345);
        let page = build_header_page_with_extension(
            header,
            HeaderExtension::new(descriptor, HeaderManifestSlot::empty()),
        )
        .expect("explicit extension page should build");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode")
            .expect("extension should be present");

        assert_eq!(decoded.primary(), descriptor);
        assert!(decoded.secondary().is_empty());
        assert_eq!(decoded.base_fact_page_start(), 1);
    }

    #[test]
    fn v10_header_extension_records_base_fact_page_start() {
        let mut header = FileHeader::new();
        header.version = 10;
        let extension = HeaderExtension::new(slot(12, 345), HeaderManifestSlot::empty())
            .with_base_fact_page_start(900)
            .expect("base start should be valid");
        let page = build_header_page_with_extension(header, extension.clone())
            .expect("explicit extension page should build");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode")
            .expect("extension should be present");

        assert_eq!(decoded, extension);
        assert_eq!(decoded.base_fact_page_start(), 900);
    }

    #[test]
    fn v11_header_extension_round_trips_base_integrity_descriptor() {
        let header = FileHeader::new();
        let integrity = BasePageIntegrityDescriptor::new(7, 900, 20, 1_000, 2, 5_000, 0xDEAD_BEEF)
            .expect("base integrity descriptor should build");
        let extension = HeaderExtension::new(slot(12, 1_100), HeaderManifestSlot::empty())
            .with_base_fact_page_start(900)
            .expect("base start should be valid")
            .with_base_integrity(integrity)
            .expect("integrity should match the base start");

        let page = build_header_page_with_extension(header, extension.clone())
            .expect("v11 extension page should build");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("v11 extension should decode")
            .expect("v11 extension should be present");

        assert_eq!(decoded, extension);
        assert_eq!(decoded.base_integrity(), integrity);
        assert_eq!(integrity.base_generation(), 7);
        assert_eq!(integrity.covered_page_end().unwrap(), 920);
        assert_eq!(integrity.catalog_page_end().unwrap(), 1_002);
        assert_eq!(HEADER_EXTENSION_LEN, LEGACY_HEADER_EXTENSION_LEN + 56);
    }

    #[test]
    fn v11_base_integrity_descriptor_checksum_mismatch_is_rejected() {
        let integrity = BasePageIntegrityDescriptor::new(3, 1, 10, 11, 1, 1_000, 0xCAFE_BABE)
            .expect("base integrity descriptor should build");
        let extension = HeaderExtension::empty()
            .with_base_integrity(integrity)
            .expect("integrity should attach");
        let mut page = page_with_extension(&extension);
        let catalog_checksum_offset = HEADER_EXTENSION_OFFSET + LEGACY_HEADER_EXTENSION_LEN + 48;
        page[catalog_checksum_offset] ^= 0x55;

        let result = HeaderExtension::read_from_page0(11, &page);
        assert!(
            result.is_err(),
            "base integrity descriptor corruption must reject page 0"
        );
    }

    #[test]
    fn v10_extension_cannot_encode_v11_integrity_metadata() {
        let integrity = BasePageIntegrityDescriptor::new(3, 1, 10, 11, 1, 1_000, 0xCAFE_BABE)
            .expect("base integrity descriptor should build");
        let extension = HeaderExtension::empty()
            .with_base_integrity(integrity)
            .expect("integrity should attach");

        assert!(
            extension.to_bytes_for_version(10).is_err(),
            "v10 must not silently drop v11 integrity metadata"
        );
    }

    #[test]
    fn legacy_v10_extension_tail_defaults_base_fact_page_start_to_one() {
        let extension = HeaderExtension::new(slot(7, 100), HeaderManifestSlot::empty());
        let mut page = page_with_header_version_and_extension(10, &extension);
        let base_layout_start =
            HEADER_EXTENSION_OFFSET + HeaderExtension::PREFIX_LEN + HeaderManifestSlot::LEN * 2;
        page[base_layout_start..base_layout_start + HeaderExtension::BASE_LAYOUT_LEN].fill(0);
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("legacy extension should decode")
            .expect("extension should be present");

        assert_eq!(decoded.base_fact_page_start(), 1);
    }

    #[test]
    fn base_fact_page_start_checksum_mismatch_is_rejected() {
        let extension = HeaderExtension::new(slot(7, 100), HeaderManifestSlot::empty())
            .with_base_fact_page_start(900)
            .expect("base start should be valid");
        let mut page = page_with_extension(&extension);
        let base_layout_start =
            HEADER_EXTENSION_OFFSET + HeaderExtension::PREFIX_LEN + HeaderManifestSlot::LEN * 2;
        page[base_layout_start] ^= 0x55;
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let result = HeaderExtension::read_from_page0(header.version, &page);

        assert!(
            result.is_err(),
            "base fact page start checksum mismatch must reject"
        );
    }

    #[test]
    fn v9_header_with_extension_like_tail_is_not_selected() {
        let extension = HeaderExtension::new(slot(7, 100), slot(6, 200));
        let mut header = FileHeader::new();
        header.version = 9;
        let mut page = header.to_bytes();
        page.resize(PAGE_SIZE, 0);
        let extension_bytes = extension.to_bytes().expect("extension should encode");
        page[HEADER_EXTENSION_OFFSET..HEADER_EXTENSION_OFFSET + extension_bytes.len()]
            .copy_from_slice(&extension_bytes);
        let header = FileHeader::from_bytes(&page).expect("legacy header should parse");
        let extension = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension read should work");

        assert_eq!(header.version, 9);
        assert!(extension.is_none(), "v9 tail bytes must be inactive");
    }

    #[test]
    fn v10_header_extension_round_trips_after_legacy_header() {
        let extension = HeaderExtension::new(slot(7, 100), slot(6, 200));
        let page = page_with_header_version_and_extension(10, &extension);
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode")
            .expect("extension should be present");

        assert_eq!(decoded, extension);
        assert_eq!(HEADER_EXTENSION_OFFSET, FileHeader::new().to_bytes().len());
        assert_eq!(
            extension
                .to_bytes_for_version(10)
                .expect("v10 extension should encode")
                .len(),
            LEGACY_HEADER_EXTENSION_LEN
        );
    }

    #[test]
    fn v10_header_with_empty_slots_is_no_delta_manifest() {
        let extension =
            HeaderExtension::new(HeaderManifestSlot::empty(), HeaderManifestSlot::empty());
        let page = page_with_header_version_and_extension(10, &extension);
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("empty extension should decode")
            .expect("extension should be present");
        let selection = select_header_manifest_slot(&decoded);

        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::NoDeltaManifest
        ));
    }

    #[test]
    fn v10_header_selects_newest_valid_slot() {
        let extension = HeaderExtension::new(slot(7, 100), slot(8, 200));
        let page = page_with_header_version_and_extension(10, &extension);
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode")
            .expect("extension should be present");
        let selection = select_header_manifest_slot(&decoded);

        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Secondary,
                descriptor
            } if descriptor.generation() == 8
        ));
    }

    #[test]
    fn v10_header_rejects_wrong_extension_version() {
        let extension = HeaderExtension::new(slot(7, 100), slot(6, 200));
        let mut page = page_with_header_version_and_extension(10, &extension);
        let wrong_version_offset = HEADER_EXTENSION_OFFSET + 8;
        page[wrong_version_offset..wrong_version_offset + 4].copy_from_slice(&9u32.to_le_bytes());
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let result = HeaderExtension::read_from_page0(header.version, &page);

        assert!(
            result.is_err(),
            "v10 extension version mismatch must reject"
        );
    }

    #[test]
    fn v10_header_rejects_wrong_extension_magic() {
        let extension = HeaderExtension::new(slot(7, 100), slot(6, 200));
        let mut page = page_with_header_version_and_extension(10, &extension);
        page[HEADER_EXTENSION_OFFSET..HEADER_EXTENSION_OFFSET + 8].copy_from_slice(b"BADMAGIC");
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let result = HeaderExtension::read_from_page0(header.version, &page);

        assert!(result.is_err(), "v10 extension magic mismatch must reject");
    }

    #[test]
    fn primary_slot_checksum_mismatch_rejects_only_primary() {
        let extension = HeaderExtension::new(slot(2, 100), slot(1, 200));
        let mut page = page_with_extension(&extension);
        page[HEADER_EXTENSION_OFFSET + 24] ^= 0x55;

        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode with one corrupt slot")
            .expect("extension should be present");
        let selection = select_header_manifest_slot(&decoded);

        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Secondary,
                ..
            }
        ));
        assert!(!decoded.primary().checksum_valid());
        assert!(decoded.secondary().checksum_valid());
    }

    #[test]
    fn secondary_slot_checksum_mismatch_rejects_only_secondary() {
        let extension = HeaderExtension::new(slot(2, 100), slot(3, 200));
        let mut page = page_with_extension(&extension);
        page[HEADER_EXTENSION_OFFSET
            + HeaderExtension::PREFIX_LEN
            + HeaderManifestSlot::LEN
            + 8] ^= 0x55;

        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode with one corrupt slot")
            .expect("extension should be present");
        let selection = select_header_manifest_slot(&decoded);

        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Primary,
                ..
            }
        ));
        assert!(decoded.primary().checksum_valid());
        assert!(!decoded.secondary().checksum_valid());
    }

    #[test]
    fn corrupt_newer_slot_falls_back_to_older_valid_slot() {
        let extension = HeaderExtension::new(slot(10, 100), slot(9, 200));
        let mut page = page_with_extension(&extension);
        page[HEADER_EXTENSION_OFFSET + 16] ^= 0xAA;

        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode with newer slot corrupt")
            .expect("extension should be present");
        let selection = select_header_manifest_slot(&decoded);

        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::Use {
                slot: HeaderManifestSlotName::Secondary,
                descriptor
            } if descriptor.generation() == 9
        ));
    }

    #[test]
    fn both_invalid_slots_return_recovery_required() {
        let extension = HeaderExtension::new(slot(10, 100), slot(9, 200));
        let mut page = page_with_extension(&extension);
        page[HEADER_EXTENSION_OFFSET + 16] ^= 0xAA;
        page[HEADER_EXTENSION_OFFSET
            + HeaderExtension::PREFIX_LEN
            + HeaderManifestSlot::LEN
            + 16] ^= 0xAA;

        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode with corrupt slots")
            .expect("extension should be present");
        let selection = select_header_manifest_slot(&decoded);

        assert!(matches!(
            selection,
            HeaderManifestSlotSelection::RecoveryRequired {
                reason: HeaderManifestSlotRecoveryReason::CorruptManifestSlot
            }
        ));
    }

    #[test]
    fn manifest_payload_location_is_page_based_not_embedded() {
        let manifest_payload = b"payload bytes must live on manifest payload pages";
        let checksum = crc32fast::hash(manifest_payload);
        let descriptor =
            HeaderManifestSlot::new(12, 345, 3, manifest_payload.len() as u64, checksum)
                .expect("slot descriptor should be valid");
        let extension = HeaderExtension::new(descriptor, HeaderManifestSlot::empty());
        let bytes = extension.to_bytes().expect("extension should encode");

        assert_eq!(descriptor.manifest_page_start(), 345);
        assert_eq!(descriptor.manifest_page_count(), 3);
        assert_eq!(descriptor.manifest_len(), manifest_payload.len() as u64);
        assert_eq!(descriptor.manifest_checksum(), checksum);
        assert!(
            !bytes
                .windows(manifest_payload.len())
                .any(|window| window == manifest_payload)
        );
    }
}
