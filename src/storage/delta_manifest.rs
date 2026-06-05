#![allow(dead_code)]

use crate::storage::delta_segment::DeltaSegmentHeader;
use crate::storage::header_extension::{
    HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName,
};
use crate::storage::{FileHeader, PAGE_SIZE, StorageBackend};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const DELTA_MANIFEST_MAGIC: [u8; 8] = *b"MGDMF001";
const DELTA_MANIFEST_COMMIT_MARKER: [u8; 8] = *b"MGDMDONE";
const DELTA_MANIFEST_CODEC_VERSION: u16 = 1;
const PREFIX_LEN: usize = 8 + 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeltaManifestSegment {
    segment_page_start: u64,
    segment_page_count: u64,
    fact_page_start: u64,
    fact_page_count: u64,
    low_tx_count: u64,
    high_tx_count: u64,
}

impl DeltaManifestSegment {
    pub(crate) fn from_segment_header(
        segment_page_start: u64,
        segment_page_count: u64,
        segment_header: &DeltaSegmentHeader,
    ) -> Result<Self> {
        if segment_page_count == 0 {
            bail!("Delta manifest segment page count must be non-zero");
        }
        Ok(Self {
            segment_page_start,
            segment_page_count,
            fact_page_start: segment_header.fact_page_start,
            fact_page_count: segment_header.fact_page_count,
            low_tx_count: segment_header.low_tx_count,
            high_tx_count: segment_header.high_tx_count,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeltaManifest {
    codec_version: u16,
    generation: u64,
    low_tx_count: u64,
    high_tx_count: u64,
    segments: Vec<DeltaManifestSegment>,
}

impl DeltaManifest {
    pub(crate) fn new(generation: u64, segments: Vec<DeltaManifestSegment>) -> Result<Self> {
        let (low_tx_count, high_tx_count) = segment_tx_range(&segments);
        Self::from_parts(generation, low_tx_count, high_tx_count, segments)
    }

    pub(crate) fn from_parts(
        generation: u64,
        low_tx_count: u64,
        high_tx_count: u64,
        segments: Vec<DeltaManifestSegment>,
    ) -> Result<Self> {
        let manifest = Self {
            codec_version: DELTA_MANIFEST_CODEC_VERSION,
            generation,
            low_tx_count,
            high_tx_count,
            segments,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn high_tx_count(&self) -> u64 {
        self.high_tx_count
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;

        let payload = postcard::to_allocvec(self)?;
        let payload_len = u64::try_from(payload.len())
            .map_err(|_| anyhow::anyhow!("Delta manifest payload exceeds u64 length"))?;
        let mut body = Vec::new();
        body.extend_from_slice(&DELTA_MANIFEST_MAGIC);
        body.extend_from_slice(&payload_len.to_le_bytes());
        body.extend_from_slice(&payload);

        let trailer = DeltaManifestTrailer::new(crc32fast::hash(&body));
        trailer.append_to(&mut body);
        Ok(body)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let min_len = PREFIX_LEN
            .checked_add(DeltaManifestTrailer::LEN)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest minimum length overflow"))?;
        if bytes.len() < min_len {
            bail!("Delta manifest is too short");
        }

        let trailer_offset = bytes
            .len()
            .checked_sub(DeltaManifestTrailer::LEN)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest trailer offset underflow"))?;
        let trailer = DeltaManifestTrailer::from_bytes(
            bytes
                .get(trailer_offset..)
                .ok_or_else(|| anyhow::anyhow!("Delta manifest trailer out of bounds"))?,
        )?;
        let body = bytes
            .get(..trailer_offset)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest body out of bounds"))?;
        if trailer.body_checksum != crc32fast::hash(body) {
            bail!("Delta manifest checksum mismatch");
        }

        let magic = body
            .get(..DELTA_MANIFEST_MAGIC.len())
            .ok_or_else(|| anyhow::anyhow!("Delta manifest missing magic"))?;
        if magic != DELTA_MANIFEST_MAGIC {
            bail!("Delta manifest magic mismatch");
        }

        let payload_len =
            usize::try_from(read_u64_le(body, 8, "delta manifest payload length")?)
                .map_err(|_| anyhow::anyhow!("Delta manifest payload length exceeds usize"))?;
        let payload_start = PREFIX_LEN;
        let payload_end = payload_start
            .checked_add(payload_len)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest payload end overflow"))?;
        if payload_end != body.len() {
            bail!("Delta manifest length field does not match body length");
        }

        let manifest: DeltaManifest = postcard::from_bytes(
            body.get(payload_start..payload_end)
                .ok_or_else(|| anyhow::anyhow!("Delta manifest payload out of bounds"))?,
        )?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.codec_version != DELTA_MANIFEST_CODEC_VERSION {
            bail!("Unsupported delta manifest codec version");
        }

        let (expected_low, expected_high) = segment_tx_range(&self.segments);
        if self.low_tx_count != expected_low {
            bail!("Delta manifest low tx_count does not match referenced segments");
        }
        if self.high_tx_count < expected_high {
            bail!("Delta manifest high tx_count is below referenced segment high tx_count");
        }
        if self.high_tx_count != expected_high {
            bail!("Delta manifest high tx_count does not match referenced segments");
        }

        for segment in &self.segments {
            if segment.segment_page_count == 0 {
                bail!("Delta manifest segment page count must be non-zero");
            }
            segment
                .segment_page_start
                .checked_add(segment.segment_page_count)
                .ok_or_else(|| anyhow::anyhow!("Delta manifest segment page range overflow"))?;
            segment
                .fact_page_start
                .checked_add(segment.fact_page_count)
                .ok_or_else(|| anyhow::anyhow!("Delta manifest fact page range overflow"))?;
            if segment.low_tx_count > segment.high_tx_count {
                bail!("Delta manifest segment tx range is invalid");
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeltaManifestTrailer {
    body_checksum: u32,
    commit_marker: [u8; 8],
}

impl DeltaManifestTrailer {
    const LEN: usize = 4 + DELTA_MANIFEST_COMMIT_MARKER.len();

    fn new(body_checksum: u32) -> Self {
        Self {
            body_checksum,
            commit_marker: DELTA_MANIFEST_COMMIT_MARKER,
        }
    }

    fn append_to(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.body_checksum.to_le_bytes());
        out.extend_from_slice(&self.commit_marker);
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::LEN {
            bail!("Delta manifest trailer length is invalid");
        }
        let body_checksum = read_u32_le(bytes, 0, "delta manifest checksum")?;
        let marker_start = 4usize;
        let marker = bytes
            .get(marker_start..marker_start.saturating_add(DELTA_MANIFEST_COMMIT_MARKER.len()))
            .ok_or_else(|| anyhow::anyhow!("Delta manifest trailer missing commit marker"))?;
        if marker != DELTA_MANIFEST_COMMIT_MARKER {
            bail!("Delta manifest missing commit marker");
        }
        Ok(Self::new(body_checksum))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManifestSlot {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManifestRecoveryReason {
    NoValidManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManifestSelection {
    Use {
        slot: ManifestSlot,
        manifest: DeltaManifest,
    },
    RecoveryRequired {
        reason: ManifestRecoveryReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistedManifestRecoveryReason {
    CorruptManifestSlot,
    NoValidManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersistedManifestSelection {
    NoDeltaManifest,
    Use {
        slot: HeaderManifestSlotName,
        manifest: DeltaManifest,
    },
    RecoveryRequired {
        reason: PersistedManifestRecoveryReason,
    },
}

impl PersistedManifestSelection {
    pub(crate) fn manifest(&self) -> Option<&DeltaManifest> {
        match self {
            PersistedManifestSelection::Use { manifest, .. } => Some(manifest),
            PersistedManifestSelection::NoDeltaManifest
            | PersistedManifestSelection::RecoveryRequired { .. } => None,
        }
    }
}

impl ManifestSelection {
    pub(crate) fn manifest(&self) -> Option<&DeltaManifest> {
        match self {
            ManifestSelection::Use { manifest, .. } => Some(manifest),
            ManifestSelection::RecoveryRequired { .. } => None,
        }
    }
}

pub(crate) fn select_manifest_candidate(
    primary: Option<&[u8]>,
    secondary: Option<&[u8]>,
) -> ManifestSelection {
    let primary = decode_candidate(ManifestSlot::Primary, primary);
    let secondary = decode_candidate(ManifestSlot::Secondary, secondary);

    match (primary, secondary) {
        (Some(primary), Some(secondary)) => {
            if primary.1.generation() >= secondary.1.generation() {
                ManifestSelection::Use {
                    slot: primary.0,
                    manifest: primary.1,
                }
            } else {
                ManifestSelection::Use {
                    slot: secondary.0,
                    manifest: secondary.1,
                }
            }
        }
        (Some((slot, manifest)), None) | (None, Some((slot, manifest))) => {
            ManifestSelection::Use { slot, manifest }
        }
        (None, None) => ManifestSelection::RecoveryRequired {
            reason: ManifestRecoveryReason::NoValidManifest,
        },
    }
}

pub(crate) fn write_manifest_pages<B: StorageBackend>(
    backend: &mut B,
    page_start: u64,
    manifest: &DeltaManifest,
) -> Result<HeaderManifestSlot> {
    if page_start == 0 {
        bail!("Delta manifest payload must not start on page 0");
    }
    let bytes = manifest.encode()?;
    let page_count = bytes.len().div_ceil(PAGE_SIZE);
    let page_count_u64 = u64::try_from(page_count)
        .map_err(|_| anyhow::anyhow!("Delta manifest page count exceeds u64"))?;

    for page_index in 0..page_count {
        let byte_start = page_index
            .checked_mul(PAGE_SIZE)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest byte offset overflow"))?;
        let byte_end = byte_start.saturating_add(PAGE_SIZE).min(bytes.len());
        let mut page = vec![0u8; PAGE_SIZE];
        page.get_mut(..byte_end.saturating_sub(byte_start))
            .ok_or_else(|| anyhow::anyhow!("Delta manifest page slice out of bounds"))?
            .copy_from_slice(
                bytes
                    .get(byte_start..byte_end)
                    .ok_or_else(|| anyhow::anyhow!("Delta manifest bytes out of bounds"))?,
            );
        let page_offset = u64::try_from(page_index)
            .map_err(|_| anyhow::anyhow!("Delta manifest page index exceeds u64"))?;
        let page_id = page_start
            .checked_add(page_offset)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest page id overflow"))?;
        backend.write_page(page_id, &page)?;
    }

    HeaderManifestSlot::new(
        manifest.generation(),
        page_start,
        page_count_u64,
        u64::try_from(bytes.len())
            .map_err(|_| anyhow::anyhow!("Delta manifest length exceeds u64"))?,
        crc32fast::hash(&bytes),
    )
}

pub(crate) fn load_persisted_manifest_selection<B: StorageBackend>(
    backend: &B,
    header: &FileHeader,
    page0: &[u8],
) -> Result<PersistedManifestSelection> {
    let Some(extension) = HeaderExtension::read_from_page0(header.version, page0)? else {
        return Ok(PersistedManifestSelection::NoDeltaManifest);
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
            return Ok(PersistedManifestSelection::RecoveryRequired {
                reason: PersistedManifestRecoveryReason::CorruptManifestSlot,
            });
        }
        return Ok(PersistedManifestSelection::NoDeltaManifest);
    }

    candidates.sort_by(|a, b| b.1.generation().cmp(&a.1.generation()));
    for (slot, descriptor) in candidates {
        if let Ok(manifest) = read_manifest_from_descriptor(backend, header, descriptor) {
            return Ok(PersistedManifestSelection::Use { slot, manifest });
        }
    }

    Ok(PersistedManifestSelection::RecoveryRequired {
        reason: PersistedManifestRecoveryReason::NoValidManifest,
    })
}

pub(crate) fn read_manifest_from_descriptor<B: StorageBackend>(
    backend: &B,
    header: &FileHeader,
    descriptor: HeaderManifestSlot,
) -> Result<DeltaManifest> {
    validate_manifest_descriptor_bounds(header, descriptor)?;

    let manifest_len = usize::try_from(descriptor.manifest_len())
        .map_err(|_| anyhow::anyhow!("Delta manifest payload length exceeds usize"))?;
    let page_count = usize::try_from(descriptor.manifest_page_count())
        .map_err(|_| anyhow::anyhow!("Delta manifest payload page count exceeds usize"))?;
    let capacity = page_count
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| anyhow::anyhow!("Delta manifest payload capacity overflow"))?;
    if manifest_len > capacity {
        bail!("Delta manifest payload length exceeds descriptor pages");
    }

    let mut bytes = Vec::with_capacity(capacity);
    for offset in 0..descriptor.manifest_page_count() {
        let page_id = descriptor
            .manifest_page_start()
            .checked_add(offset)
            .ok_or_else(|| anyhow::anyhow!("Delta manifest page id overflow"))?;
        let page = backend.read_page(page_id)?;
        if page.len() != PAGE_SIZE {
            bail!("Delta manifest page has invalid size");
        }
        bytes.extend_from_slice(&page);
    }
    bytes.truncate(manifest_len);

    if crc32fast::hash(&bytes) != descriptor.manifest_checksum() {
        bail!("Delta manifest payload checksum mismatch");
    }

    let manifest = DeltaManifest::decode(&bytes)?;
    if manifest.generation() != descriptor.generation() {
        bail!("Delta manifest generation does not match header descriptor");
    }
    Ok(manifest)
}

fn validate_manifest_descriptor_bounds(
    header: &FileHeader,
    descriptor: HeaderManifestSlot,
) -> Result<()> {
    let end = descriptor
        .manifest_page_start()
        .checked_add(descriptor.manifest_page_count())
        .ok_or_else(|| anyhow::anyhow!("Delta manifest payload page range overflow"))?;
    if end > header.page_count {
        bail!("Delta manifest payload page range out of bounds");
    }
    Ok(())
}

fn decode_candidate(
    slot: ManifestSlot,
    candidate: Option<&[u8]>,
) -> Option<(ManifestSlot, DeltaManifest)> {
    candidate
        .and_then(|bytes| DeltaManifest::decode(bytes).ok())
        .map(|manifest| (slot, manifest))
}

fn segment_tx_range(segments: &[DeltaManifestSegment]) -> (u64, u64) {
    let mut iter = segments.iter().map(|segment| segment.low_tx_count);
    let Some(first_low) = iter.next() else {
        return (0, 0);
    };
    let mut low = first_low;
    let mut high = segments
        .first()
        .map(|segment| segment.high_tx_count)
        .unwrap_or(0);
    for segment in segments.iter().skip(1) {
        low = low.min(segment.low_tx_count);
        high = high.max(segment.high_tx_count);
    }
    (low, high)
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
        DeltaManifest, DeltaManifestSegment, ManifestRecoveryReason, ManifestSelection,
        ManifestSlot, PersistedManifestRecoveryReason, PersistedManifestSelection,
        load_persisted_manifest_selection, read_manifest_from_descriptor,
        select_manifest_candidate, write_manifest_pages,
    };
    use crate::graph::types::{Fact, VALID_TIME_FOREVER, Value};
    use crate::storage::backend::MemoryBackend;
    use crate::storage::delta_segment::DeltaSegment;
    use crate::storage::header_extension::{
        HeaderExtension, HeaderManifestSlot, HeaderManifestSlotName,
        build_header_page_with_extension,
    };
    use crate::storage::{FileHeader, StorageBackend};
    use uuid::Uuid;

    fn fact(entity: Uuid, attribute: &str, value: Value, tx_count: u64) -> Fact {
        Fact::with_valid_time(
            entity,
            attribute.to_string(),
            value,
            2_000 + tx_count,
            tx_count,
            10,
            VALID_TIME_FOREVER,
        )
    }

    fn segment(tx_count: u64) -> DeltaSegment {
        let entity = Uuid::from_u128(u128::from(tx_count));
        DeltaSegment::from_facts(
            vec![fact(
                entity,
                ":edge/to",
                Value::Ref(Uuid::from_u128(10_000 + u128::from(tx_count))),
                tx_count,
            )],
            100 + tx_count,
        )
        .expect("segment should build")
    }

    fn manifest_bytes(generation: u64, segment: &DeltaSegment) -> Vec<u8> {
        DeltaManifest::new(
            generation,
            vec![
                DeltaManifestSegment::from_segment_header(500 + generation, 1, segment.header())
                    .expect("manifest segment should build"),
            ],
        )
        .expect("manifest should build")
        .encode()
        .expect("manifest should encode")
    }

    fn corrupt(mut bytes: Vec<u8>) -> Vec<u8> {
        let byte = bytes.get_mut(12).expect("manifest has body byte");
        *byte ^= 0x01;
        bytes
    }

    fn empty_manifest(generation: u64) -> DeltaManifest {
        DeltaManifest::from_parts(generation, 0, 0, Vec::new())
            .expect("empty manifest should be valid")
    }

    fn header_with_extension(extension: HeaderExtension, page_count: u64) -> (FileHeader, Vec<u8>) {
        let mut header = FileHeader::new();
        header.page_count = page_count;
        header.header_checksum = crate::storage::persistent_facts::compute_header_checksum(&header);
        let page0 = build_header_page_with_extension(header, extension)
            .expect("header page with extension should build");
        (header, page0)
    }

    #[test]
    fn manifest_candidate_encode_decode_round_trips() {
        let segment = segment(3);
        let manifest = DeltaManifest::new(
            7,
            vec![
                DeltaManifestSegment::from_segment_header(900, 1, segment.header())
                    .expect("manifest segment should build"),
            ],
        )
        .expect("manifest should build");
        let decoded = DeltaManifest::decode(&manifest.encode().expect("manifest should encode"))
            .expect("manifest should decode");

        assert_eq!(decoded.generation(), 7, "generation must round-trip");
        assert_eq!(
            decoded.high_tx_count(),
            segment.header().high_tx_count,
            "high tx_count must round-trip"
        );
    }

    #[test]
    fn manifest_pages_write_read_round_trips_descriptor_checksum() {
        let manifest = empty_manifest(7);
        let mut backend = MemoryBackend::new();
        let descriptor =
            write_manifest_pages(&mut backend, 1, &manifest).expect("manifest pages should write");
        let (header, _) = header_with_extension(
            HeaderExtension::new(descriptor, HeaderManifestSlot::empty()),
            1 + descriptor.manifest_page_count(),
        );
        let loaded = read_manifest_from_descriptor(&backend, &header, descriptor)
            .expect("manifest should read from descriptor pages");

        assert_eq!(descriptor.generation(), 7);
        assert_eq!(loaded.generation(), 7);
        assert_eq!(
            descriptor.manifest_checksum(),
            crc32fast::hash(&manifest.encode().expect("manifest should encode"))
        );
    }

    #[test]
    fn persisted_selection_loads_selected_manifest_from_page0() {
        let manifest = empty_manifest(3);
        let mut backend = MemoryBackend::new();
        let descriptor =
            write_manifest_pages(&mut backend, 1, &manifest).expect("manifest pages should write");
        let (header, page0) = header_with_extension(
            HeaderExtension::new(descriptor, HeaderManifestSlot::empty()),
            1 + descriptor.manifest_page_count(),
        );
        let selection = load_persisted_manifest_selection(&backend, &header, &page0)
            .expect("manifest selection should load");

        assert!(matches!(
            selection,
            PersistedManifestSelection::Use {
                slot: HeaderManifestSlotName::Primary,
                ..
            }
        ));
        assert_eq!(
            selection
                .manifest()
                .expect("manifest should be selected")
                .generation(),
            3
        );
    }

    #[test]
    fn persisted_selection_falls_back_from_corrupt_newer_payload_to_valid_older() {
        let older = empty_manifest(1);
        let newer = empty_manifest(2);
        let mut backend = MemoryBackend::new();
        let older_descriptor =
            write_manifest_pages(&mut backend, 1, &older).expect("older manifest should write");
        let newer_start = 1 + older_descriptor.manifest_page_count();
        let newer_descriptor = write_manifest_pages(&mut backend, newer_start, &newer)
            .expect("newer manifest should write");

        let mut newer_page = backend
            .read_page(newer_descriptor.manifest_page_start())
            .expect("newer manifest page should read");
        newer_page[12] ^= 0x55;
        backend
            .write_page(newer_descriptor.manifest_page_start(), &newer_page)
            .expect("corrupt newer manifest page should write");

        let page_count =
            newer_descriptor.manifest_page_start() + newer_descriptor.manifest_page_count();
        let (header, page0) = header_with_extension(
            HeaderExtension::new(older_descriptor, newer_descriptor),
            page_count,
        );
        let selection = load_persisted_manifest_selection(&backend, &header, &page0)
            .expect("manifest selection should load");

        assert!(matches!(
            selection,
            PersistedManifestSelection::Use {
                slot: HeaderManifestSlotName::Primary,
                ..
            }
        ));
        assert_eq!(
            selection
                .manifest()
                .expect("older manifest should be selected")
                .generation(),
            1
        );
    }

    #[test]
    fn out_of_bounds_manifest_descriptor_requires_recovery() {
        let manifest = empty_manifest(4);
        let encoded = manifest.encode().expect("manifest should encode");
        let descriptor =
            HeaderManifestSlot::new(4, 99, 1, encoded.len() as u64, crc32fast::hash(&encoded))
                .expect("descriptor should build");
        let (header, page0) = header_with_extension(
            HeaderExtension::new(descriptor, HeaderManifestSlot::empty()),
            2,
        );
        let selection = load_persisted_manifest_selection(&MemoryBackend::new(), &header, &page0)
            .expect("selection should resolve to recovery");

        assert!(matches!(
            selection,
            PersistedManifestSelection::RecoveryRequired {
                reason: PersistedManifestRecoveryReason::NoValidManifest
            }
        ));
    }

    #[test]
    fn both_invalid_header_manifest_slots_require_recovery() {
        let manifest = empty_manifest(5);
        let encoded = manifest.encode().expect("manifest should encode");
        let primary =
            HeaderManifestSlot::new(5, 1, 1, encoded.len() as u64, crc32fast::hash(&encoded))
                .expect("primary descriptor should build");
        let secondary =
            HeaderManifestSlot::new(4, 2, 1, encoded.len() as u64, crc32fast::hash(&encoded))
                .expect("secondary descriptor should build");
        let (header, mut page0) =
            header_with_extension(HeaderExtension::new(primary, secondary), 3);
        let primary_checksum_offset = crate::storage::header_extension::HEADER_EXTENSION_OFFSET
            + HeaderExtension::PREFIX_LEN
            + 36;
        let secondary_checksum_offset = primary_checksum_offset + HeaderManifestSlot::LEN;
        page0[primary_checksum_offset] ^= 0xAA;
        page0[secondary_checksum_offset] ^= 0x55;

        let selection = load_persisted_manifest_selection(&MemoryBackend::new(), &header, &page0)
            .expect("selection should resolve to recovery");

        assert!(matches!(
            selection,
            PersistedManifestSelection::RecoveryRequired {
                reason: PersistedManifestRecoveryReason::CorruptManifestSlot
            }
        ));
    }

    #[test]
    fn valid_newer_manifest_wins() {
        let old_segment = segment(1);
        let new_segment = segment(2);
        let primary = manifest_bytes(1, &old_segment);
        let secondary = manifest_bytes(2, &new_segment);
        let selection = select_manifest_candidate(Some(&primary), Some(&secondary));

        assert!(matches!(
            selection,
            ManifestSelection::Use {
                slot: ManifestSlot::Secondary,
                ..
            }
        ));
        assert_eq!(
            selection
                .manifest()
                .expect("manifest should be selected")
                .generation(),
            2,
            "newer generation must win"
        );
    }

    #[test]
    fn corrupt_newer_manifest_is_ignored_when_older_valid_exists() {
        let old_segment = segment(3);
        let new_segment = segment(4);
        let primary = manifest_bytes(1, &old_segment);
        let secondary = corrupt(manifest_bytes(2, &new_segment));
        let selection = select_manifest_candidate(Some(&primary), Some(&secondary));

        assert!(matches!(
            selection,
            ManifestSelection::Use {
                slot: ManifestSlot::Primary,
                ..
            }
        ));
        assert_eq!(
            selection
                .manifest()
                .expect("manifest should be selected")
                .generation(),
            1,
            "older valid manifest must survive corrupt newer candidate"
        );
    }

    #[test]
    fn both_corrupt_manifests_return_explicit_recovery_required() {
        let primary_segment = segment(5);
        let secondary_segment = segment(6);
        let primary = corrupt(manifest_bytes(2, &primary_segment));
        let secondary = corrupt(manifest_bytes(1, &secondary_segment));
        let selection = select_manifest_candidate(Some(&primary), Some(&secondary));

        assert!(
            matches!(
                selection,
                ManifestSelection::RecoveryRequired {
                    reason: ManifestRecoveryReason::NoValidManifest
                }
            ),
            "both corrupt candidates must not become an empty delta view"
        );
    }

    #[test]
    fn manifest_lower_high_tx_than_referenced_segment_rejects() {
        let segment = segment(7);
        let manifest_segment = DeltaManifestSegment::from_segment_header(950, 1, segment.header())
            .expect("manifest segment should build");
        assert!(
            DeltaManifest::from_parts(
                1,
                segment.header().low_tx_count,
                segment.header().high_tx_count - 1,
                vec![manifest_segment],
            )
            .is_err(),
            "manifest high_tx below referenced segment high_tx must reject"
        );
    }
}
