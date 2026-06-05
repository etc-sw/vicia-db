#![allow(dead_code)]

use crate::storage::{FileHeader, PAGE_SIZE};
use anyhow::{Result, bail};

pub(crate) const HEADER_EXTENSION_OFFSET: usize = 84;
const HEADER_EXTENSION_MAGIC: [u8; 8] = *b"MGHEX001";
pub(crate) const HEADER_EXTENSION_FILE_FORMAT_VERSION: u32 = 10;
pub(crate) const HEADER_EXTENSION_LEN: usize =
    HeaderExtension::PREFIX_LEN + (HeaderManifestSlot::LEN * 2);
const _: () = assert!(HEADER_EXTENSION_OFFSET + HEADER_EXTENSION_LEN <= PAGE_SIZE);

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

    fn is_selectable(self) -> bool {
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

    fn is_empty(self) -> bool {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderExtension {
    primary: HeaderManifestSlot,
    secondary: HeaderManifestSlot,
}

impl HeaderExtension {
    pub(crate) const PREFIX_LEN: usize = 12;

    pub(crate) fn new(primary: HeaderManifestSlot, secondary: HeaderManifestSlot) -> Self {
        Self { primary, secondary }
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

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_EXTENSION_LEN);
        bytes.extend_from_slice(&HEADER_EXTENSION_MAGIC);
        bytes.extend_from_slice(&HEADER_EXTENSION_FILE_FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.primary.to_bytes());
        bytes.extend_from_slice(&self.secondary.to_bytes());
        debug_assert_eq!(bytes.len(), HEADER_EXTENSION_LEN);
        bytes
    }

    pub(crate) fn write_to_page0(&self, page: &mut [u8]) -> Result<()> {
        if page.len() != PAGE_SIZE {
            bail!("Header extension write requires a full page 0");
        }
        let bytes = self.to_bytes();
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
        if file_format_version < HEADER_EXTENSION_FILE_FORMAT_VERSION {
            return Ok(None);
        }
        if file_format_version != HEADER_EXTENSION_FILE_FORMAT_VERSION {
            bail!("Unsupported header extension file format version");
        }

        let Some(extension_bytes) =
            page.get(HEADER_EXTENSION_OFFSET..HEADER_EXTENSION_OFFSET + HEADER_EXTENSION_LEN)
        else {
            bail!("Header extension is truncated");
        };
        if extension_bytes.iter().all(|byte| *byte == 0) {
            bail!("Header extension is missing for v10 header");
        }

        let magic = extension_bytes
            .get(0..HEADER_EXTENSION_MAGIC.len())
            .ok_or_else(|| anyhow::anyhow!("Header extension missing magic"))?;
        if magic != HEADER_EXTENSION_MAGIC {
            bail!("Header extension magic mismatch");
        }

        let extension_file_format_version = read_u32_le(
            extension_bytes,
            HEADER_EXTENSION_MAGIC.len(),
            "header extension file format version",
        )?;
        if extension_file_format_version != HEADER_EXTENSION_FILE_FORMAT_VERSION {
            bail!("Unsupported header extension file format version");
        }

        let primary_start = Self::PREFIX_LEN;
        let secondary_start = primary_start
            .checked_add(HeaderManifestSlot::LEN)
            .ok_or_else(|| anyhow::anyhow!("Header extension secondary slot offset overflow"))?;
        let secondary_end = secondary_start
            .checked_add(HeaderManifestSlot::LEN)
            .ok_or_else(|| anyhow::anyhow!("Header extension end offset overflow"))?;

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

        Ok(Some(Self { primary, secondary }))
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
    let mut page = header.to_bytes();
    if page.len() > PAGE_SIZE {
        bail!("Header bytes exceed page size");
    }
    page.resize(PAGE_SIZE, 0);

    if header.version == HEADER_EXTENSION_FILE_FORMAT_VERSION {
        HeaderExtension::empty().write_to_page0(&mut page)?;
    } else if header.version > HEADER_EXTENSION_FILE_FORMAT_VERSION {
        bail!("Unsupported header extension file format version");
    }

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
        HEADER_EXTENSION_FILE_FORMAT_VERSION, HEADER_EXTENSION_LEN, HEADER_EXTENSION_OFFSET,
        HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName,
        HeaderManifestSlotRecoveryReason, HeaderManifestSlotSelection, build_header_page,
        select_header_manifest_slot, select_header_manifest_slot_from_page0,
    };
    use crate::storage::{FORMAT_VERSION, FileHeader, PAGE_SIZE};

    fn slot(generation: u64, page_start: u64) -> HeaderManifestSlot {
        HeaderManifestSlot::new(generation, page_start, 2, 128, 0xCAFE_BABE)
            .expect("slot descriptor should be valid")
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
            .write_to_page0(&mut page)
            .expect("extension should fit in page 0");
        page
    }

    #[test]
    fn current_format_publishes_v10_header_extension_gate() {
        assert_eq!(FORMAT_VERSION, 10);
        assert_eq!(FileHeader::new().version, 10);
        assert_eq!(HEADER_EXTENSION_FILE_FORMAT_VERSION, 10);
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
        let header = FileHeader::new();
        let mut page = header.to_bytes();
        page.resize(PAGE_SIZE, 0);
        let result = select_header_manifest_slot_from_page0(header.version, &page);

        assert!(
            result.is_err(),
            "v10 page must include a non-empty header extension"
        );
    }

    #[test]
    fn v9_header_with_extension_like_tail_is_not_selected() {
        let extension = HeaderExtension::new(slot(7, 100), slot(6, 200));
        let page = page_with_header_version_and_extension(9, &extension);
        let header = FileHeader::from_bytes(&page).expect("legacy header should parse");
        let extension = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension read should work");

        assert_eq!(header.version, 9);
        assert!(extension.is_none(), "v9 tail bytes must be inactive");
    }

    #[test]
    fn v10_header_extension_round_trips_after_legacy_header() {
        let extension = HeaderExtension::new(slot(7, 100), slot(6, 200));
        let page = page_with_extension(&extension);
        let header = FileHeader::from_bytes(&page).expect("v10 header should parse");
        let decoded = HeaderExtension::read_from_page0(header.version, &page)
            .expect("extension should decode")
            .expect("extension should be present");

        assert_eq!(decoded, extension);
        assert_eq!(HEADER_EXTENSION_OFFSET, FileHeader::new().to_bytes().len());
        assert_eq!(extension.to_bytes().len(), HEADER_EXTENSION_LEN);
    }

    #[test]
    fn v10_header_with_empty_slots_is_no_delta_manifest() {
        let extension =
            HeaderExtension::new(HeaderManifestSlot::empty(), HeaderManifestSlot::empty());
        let page = page_with_extension(&extension);
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
        let page = page_with_extension(&extension);
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
        let mut page = page_with_extension(&extension);
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
        let mut page = page_with_extension(&extension);
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
        let bytes = extension.to_bytes();

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
