//! Detached, generation-bound page image for the current projection.
//!
//! The bytes defined here are the durable R2 projection material. R2-B keeps
//! them detached from page 0 so corruption, identity, and rebuild behavior can
//! be proven before publication adds another selectable root.

// Fixed-size encoder offsets are derived from a freshly allocated, fully sized
// image. Untrusted decoder reads use checked helpers before these fixed ranges.
#![allow(clippy::indexing_slicing)]

use crate::graph::current_projection::{
    CurrentProjectionCandidate, EncodedProjectionColumns, TemporalColumnBuilder,
    validate_encoded_value, validate_temporal_columns,
};
use crate::graph::types::EntityId;
use crate::storage::PAGE_SIZE;
use anyhow::{Result, anyhow, bail};
use crc32fast::Hasher;

const MAGIC: [u8; 8] = *b"MGCPG001";
const CODEC_VERSION: u32 = 1;
const SECTION_COUNT: usize = 6;
const DIRECTORY_OFFSET: usize = 128;
const DIRECTORY_ENTRY_LEN: usize = 24;
const HEADER_CHECKSUM_OFFSET: usize = 80;
const ATTRIBUTE: u32 = 1;
const ENTITIES: u32 = 2;
const VALUE_OFFSETS: u32 = 3;
const VALUE_BYTES: u32 = 4;
const VALID_FROM: u32 = 5;
const VALID_TO: u32 = 6;

/// Persistent ledger state to which a detached projection image belongs.
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
    pub fn base_generation(self) -> u64 {
        self.base_generation
    }

    /// Selected delta-manifest generation, or zero when no manifest exists.
    #[must_use]
    pub fn manifest_generation(self) -> u64 {
        self.manifest_generation
    }

    /// Exact transaction watermark represented by the image.
    #[must_use]
    pub fn tx_count(self) -> u64 {
        self.tx_count
    }
}

/// A complete, page-aligned current-projection image not yet published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentProjectionPageImage {
    bytes: Vec<u8>,
    logical_bytes: u64,
    identity: ProjectionLedgerIdentity,
    row_count: u64,
    fingerprint: u64,
}

impl CurrentProjectionPageImage {
    /// Page-aligned encoded bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Total padded bytes.
    #[must_use]
    pub fn padded_bytes(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    /// Sum of metadata and logical section bytes, excluding alignment padding.
    #[must_use]
    pub fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    /// Number of 4 KiB pages in the detached image.
    #[must_use]
    pub fn page_count(&self) -> u64 {
        u64::try_from(self.bytes.len() / PAGE_SIZE).unwrap_or(u64::MAX)
    }

    /// Ledger identity encoded in page 0.
    #[must_use]
    pub fn identity(&self) -> ProjectionLedgerIdentity {
        self.identity
    }

    /// Canonical row count encoded in page 0.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Logical projection fingerprint encoded in page 0.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug)]
struct Section {
    kind: u32,
    offset: usize,
    len: usize,
}

pub(crate) fn encode(
    candidate: &CurrentProjectionCandidate,
    identity: ProjectionLedgerIdentity,
) -> Result<CurrentProjectionPageImage> {
    if candidate.tx_count() != identity.tx_count {
        bail!("current projection image transaction watermark mismatch")
    }

    let mut row_count = 0_usize;
    let mut value_bytes_len = 0_usize;
    let mut valid_from = TemporalColumnBuilder::default();
    let mut valid_to = TemporalColumnBuilder::default();
    candidate.visit_merged_encoded(&mut |_, value, from, to| {
        row_count = row_count
            .checked_add(1)
            .ok_or_else(|| anyhow!("current projection image row count overflow"))?;
        value_bytes_len = value_bytes_len
            .checked_add(value.len())
            .ok_or_else(|| anyhow!("current projection image value bytes overflow"))?;
        valid_from.push(from);
        valid_to.push(to);
        Ok(())
    })?;
    if row_count != candidate.row_count() {
        bail!("current projection image merged row count changed during encoding")
    }
    u32::try_from(value_bytes_len)
        .map_err(|_| anyhow!("current projection image exceeds the 4 GiB value limit"))?;

    let offsets_len = row_count
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| anyhow!("current projection image offset column overflow"))?;
    let entity_len = row_count
        .checked_mul(16)
        .ok_or_else(|| anyhow!("current projection image entity column overflow"))?;
    let lengths = [
        candidate.attribute().len(),
        entity_len,
        offsets_len,
        value_bytes_len,
        valid_from.bytes.len(),
        valid_to.bytes.len(),
    ];
    let kinds = [
        ATTRIBUTE,
        ENTITIES,
        VALUE_OFFSETS,
        VALUE_BYTES,
        VALID_FROM,
        VALID_TO,
    ];
    let mut sections = Vec::with_capacity(SECTION_COUNT);
    let mut next = PAGE_SIZE;
    for (kind, len) in kinds.into_iter().zip(lengths) {
        sections.push(Section {
            kind,
            offset: next,
            len,
        });
        next = align_page(
            next.checked_add(len)
                .ok_or_else(|| anyhow!("current projection image layout overflow"))?,
        )?;
    }
    let mut bytes = vec![0_u8; next];

    let fingerprint = candidate.fingerprint()?;
    write_header(
        &mut bytes[..PAGE_SIZE],
        identity,
        candidate.valid_time_floor(),
        row_count,
        fingerprint,
        &sections,
    )?;
    copy_section(&mut bytes, sections[0], candidate.attribute().as_bytes())?;

    let mut row = 0_usize;
    let mut value_position = 0_usize;
    let mut encoded_from = TemporalColumnBuilder::default();
    let mut encoded_to = TemporalColumnBuilder::default();
    candidate.visit_merged_encoded(&mut |entity, value, from, to| {
        let entity_start = sections[1]
            .offset
            .checked_add(
                row.checked_mul(16)
                    .ok_or_else(|| anyhow!("entity offset overflow"))?,
            )
            .ok_or_else(|| anyhow!("entity offset overflow"))?;
        bytes
            .get_mut(entity_start..entity_start + 16)
            .ok_or_else(|| anyhow!("entity section out of bounds"))?
            .copy_from_slice(entity.as_bytes());

        let offset_start = sections[2]
            .offset
            .checked_add(
                row.checked_mul(4)
                    .ok_or_else(|| anyhow!("value offset overflow"))?,
            )
            .ok_or_else(|| anyhow!("value offset overflow"))?;
        bytes
            .get_mut(offset_start..offset_start + 4)
            .ok_or_else(|| anyhow!("value offset section out of bounds"))?
            .copy_from_slice(&u32::try_from(value_position)?.to_le_bytes());
        let value_start = sections[3]
            .offset
            .checked_add(value_position)
            .ok_or_else(|| anyhow!("value payload offset overflow"))?;
        bytes
            .get_mut(value_start..value_start + value.len())
            .ok_or_else(|| anyhow!("value section out of bounds"))?
            .copy_from_slice(value);
        value_position = value_position
            .checked_add(value.len())
            .ok_or_else(|| anyhow!("value position overflow"))?;
        encoded_from.push(from);
        encoded_to.push(to);
        row = row
            .checked_add(1)
            .ok_or_else(|| anyhow!("row position overflow"))?;
        Ok(())
    })?;
    let final_offset = sections[2]
        .offset
        .checked_add(
            row.checked_mul(4)
                .ok_or_else(|| anyhow!("final offset overflow"))?,
        )
        .ok_or_else(|| anyhow!("final offset overflow"))?;
    bytes
        .get_mut(final_offset..final_offset + 4)
        .ok_or_else(|| anyhow!("final value offset out of bounds"))?
        .copy_from_slice(&u32::try_from(value_position)?.to_le_bytes());
    copy_section(&mut bytes, sections[4], &encoded_from.bytes)?;
    copy_section(&mut bytes, sections[5], &encoded_to.bytes)?;

    let payload_checksum = checksum_payload(&bytes);
    bytes[76..80].copy_from_slice(&payload_checksum.to_le_bytes());
    let header_checksum = checksum_header(&bytes[..PAGE_SIZE]);
    bytes[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&header_checksum.to_le_bytes());

    let logical_bytes = u64::try_from(PAGE_SIZE)?
        .checked_add(lengths.into_iter().try_fold(0_u64, |total, len| {
            total
                .checked_add(u64::try_from(len)?)
                .ok_or_else(|| anyhow!("logical byte count overflow"))
        })?)
        .ok_or_else(|| anyhow!("logical byte count overflow"))?;
    Ok(CurrentProjectionPageImage {
        bytes,
        logical_bytes,
        identity,
        row_count: u64::try_from(row_count)?,
        fingerprint,
    })
}

pub(crate) fn decode(
    bytes: &[u8],
    expected_identity: ProjectionLedgerIdentity,
    expected_attribute: &str,
    expected_floor: i64,
    publication_generation: u64,
) -> Result<CurrentProjectionCandidate> {
    if bytes.len() < PAGE_SIZE || !bytes.len().is_multiple_of(PAGE_SIZE) {
        bail!("current projection image is not a complete page range")
    }
    if bytes.get(0..8) != Some(MAGIC.as_slice()) {
        bail!("invalid current projection image magic")
    }
    if read_u32(bytes, 8)? != CODEC_VERSION {
        bail!("unsupported current projection image codec version")
    }
    if usize::try_from(read_u32(bytes, 12)?)? != PAGE_SIZE {
        bail!("current projection image page size mismatch")
    }
    let page_count = usize::try_from(read_u64(bytes, 16)?)?;
    if page_count.checked_mul(PAGE_SIZE) != Some(bytes.len()) {
        bail!("current projection image page count mismatch")
    }
    let stored_header_checksum = read_u32(bytes, HEADER_CHECKSUM_OFFSET)?;
    if stored_header_checksum != checksum_header(&bytes[..PAGE_SIZE]) {
        bail!("current projection image header checksum mismatch")
    }
    if bytes[84..DIRECTORY_OFFSET].iter().any(|byte| *byte != 0) {
        bail!("current projection image reserved header bytes are non-zero")
    }
    let directory_end = DIRECTORY_OFFSET + SECTION_COUNT * DIRECTORY_ENTRY_LEN;
    if bytes[directory_end..PAGE_SIZE]
        .iter()
        .any(|byte| *byte != 0)
    {
        bail!("current projection image unused header bytes are non-zero")
    }

    let identity = ProjectionLedgerIdentity::new(
        read_u64(bytes, 24)?,
        read_u64(bytes, 32)?,
        read_u64(bytes, 40)?,
    );
    if identity != expected_identity {
        bail!("current projection image ledger identity mismatch")
    }
    let valid_time_floor = read_i64(bytes, 48)?;
    if valid_time_floor != expected_floor {
        bail!("current projection image valid-time floor mismatch")
    }
    let row_count = usize::try_from(read_u64(bytes, 56)?)?;
    let fingerprint = read_u64(bytes, 64)?;
    if usize::try_from(read_u32(bytes, 72)?)? != SECTION_COUNT {
        bail!("current projection image section count mismatch")
    }
    if read_u32(bytes, 76)? != checksum_payload(bytes) {
        bail!("current projection image payload checksum mismatch")
    }
    let sections = read_sections(bytes)?;
    validate_sections(bytes, &sections)?;
    let attribute = std::str::from_utf8(section_bytes(bytes, sections[0])?)?;
    if attribute != expected_attribute {
        bail!("current projection image attribute mismatch")
    }

    if sections[1].len
        != row_count
            .checked_mul(16)
            .ok_or_else(|| anyhow!("entity length overflow"))?
        || sections[2].len
            != row_count
                .checked_add(1)
                .and_then(|count| count.checked_mul(4))
                .ok_or_else(|| anyhow!("value offset length overflow"))?
    {
        bail!("current projection image row-aligned column length mismatch")
    }
    validate_borrowed_columns(bytes, &sections, row_count, valid_time_floor)?;
    let mut entities = Vec::new();
    entities.try_reserve_exact(row_count)?;
    for bytes in section_bytes(bytes, sections[1])?.chunks_exact(16) {
        entities.push(EntityId::from_bytes(bytes.try_into()?));
    }
    let mut value_offsets = Vec::new();
    value_offsets.try_reserve_exact(row_count.saturating_add(1))?;
    for bytes in section_bytes(bytes, sections[2])?.chunks_exact(4) {
        value_offsets.push(u32::from_le_bytes(bytes.try_into()?));
    }
    let candidate = CurrentProjectionCandidate::from_encoded_columns(EncodedProjectionColumns {
        attribute: attribute.to_owned(),
        valid_time_floor,
        publication_generation,
        tx_count: identity.tx_count,
        entities,
        value_offsets,
        value_bytes: section_bytes(bytes, sections[3])?.to_vec(),
        valid_from_bytes: section_bytes(bytes, sections[4])?.to_vec(),
        valid_to_bytes: section_bytes(bytes, sections[5])?.to_vec(),
    })?;
    if candidate.row_count() != row_count || candidate.fingerprint()? != fingerprint {
        bail!("current projection image logical fingerprint mismatch")
    }
    Ok(candidate)
}

fn write_header(
    page: &mut [u8],
    identity: ProjectionLedgerIdentity,
    valid_time_floor: i64,
    row_count: usize,
    fingerprint: u64,
    sections: &[Section],
) -> Result<()> {
    page[0..8].copy_from_slice(&MAGIC);
    page[8..12].copy_from_slice(&CODEC_VERSION.to_le_bytes());
    page[12..16].copy_from_slice(&u32::try_from(PAGE_SIZE)?.to_le_bytes());
    let page_count = sections
        .last()
        .map(|section| align_page(section.offset.saturating_add(section.len)))
        .transpose()?
        .unwrap_or(PAGE_SIZE)
        / PAGE_SIZE;
    page[16..24].copy_from_slice(&u64::try_from(page_count)?.to_le_bytes());
    page[24..32].copy_from_slice(&identity.base_generation.to_le_bytes());
    page[32..40].copy_from_slice(&identity.manifest_generation.to_le_bytes());
    page[40..48].copy_from_slice(&identity.tx_count.to_le_bytes());
    page[48..56].copy_from_slice(&valid_time_floor.to_le_bytes());
    page[56..64].copy_from_slice(&u64::try_from(row_count)?.to_le_bytes());
    page[64..72].copy_from_slice(&fingerprint.to_le_bytes());
    page[72..76].copy_from_slice(&u32::try_from(SECTION_COUNT)?.to_le_bytes());
    for (index, section) in sections.iter().enumerate() {
        let start = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_LEN;
        page[start..start + 4].copy_from_slice(&section.kind.to_le_bytes());
        page[start + 8..start + 16].copy_from_slice(&u64::try_from(section.offset)?.to_le_bytes());
        page[start + 16..start + 24].copy_from_slice(&u64::try_from(section.len)?.to_le_bytes());
    }
    Ok(())
}

fn read_sections(bytes: &[u8]) -> Result<[Section; SECTION_COUNT]> {
    let mut sections = [Section {
        kind: 0,
        offset: 0,
        len: 0,
    }; SECTION_COUNT];
    for (index, expected_kind) in [
        ATTRIBUTE,
        ENTITIES,
        VALUE_OFFSETS,
        VALUE_BYTES,
        VALID_FROM,
        VALID_TO,
    ]
    .into_iter()
    .enumerate()
    {
        let start = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_LEN;
        let kind = read_u32(bytes, start)?;
        if kind != expected_kind || read_u32(bytes, start + 4)? != 0 {
            bail!("current projection image section directory mismatch")
        }
        sections[index] = Section {
            kind,
            offset: usize::try_from(read_u64(bytes, start + 8)?)?,
            len: usize::try_from(read_u64(bytes, start + 16)?)?,
        };
    }
    Ok(sections)
}

fn validate_sections(bytes: &[u8], sections: &[Section; SECTION_COUNT]) -> Result<()> {
    let mut previous_end = PAGE_SIZE;
    for section in sections {
        if section.offset < PAGE_SIZE || section.offset % PAGE_SIZE != 0 {
            bail!("current projection image section is not page aligned")
        }
        if section.offset != align_page(previous_end)? {
            bail!("current projection image section layout is non-canonical")
        }
        if bytes
            .get(previous_end..section.offset)
            .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
        {
            bail!("current projection image inter-section padding is non-zero")
        }
        previous_end = section
            .offset
            .checked_add(section.len)
            .ok_or_else(|| anyhow!("current projection image section range overflow"))?;
        if previous_end > bytes.len() {
            bail!("current projection image section exceeds page range")
        }
    }
    if align_page(previous_end)? != bytes.len() {
        bail!("current projection image has trailing pages")
    }
    if bytes
        .get(previous_end..)
        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        bail!("current projection image trailing padding is non-zero")
    }
    Ok(())
}

fn validate_borrowed_columns(
    bytes: &[u8],
    sections: &[Section; SECTION_COUNT],
    row_count: usize,
    valid_time_floor: i64,
) -> Result<()> {
    let entity_bytes = section_bytes(bytes, sections[1])?;
    let mut entity_chunks = entity_bytes.chunks_exact(16);
    if let Some(mut previous) = entity_chunks.next() {
        for entity in entity_chunks {
            if previous > entity {
                bail!("current projection image entities are not sorted")
            }
            previous = entity;
        }
    }

    let offsets = section_bytes(bytes, sections[2])?;
    let values = section_bytes(bytes, sections[3])?;
    let mut previous = None;
    for offset_bytes in offsets.chunks_exact(4) {
        let offset = usize::try_from(u32::from_le_bytes(offset_bytes.try_into()?))?;
        if offset > values.len() || previous.is_some_and(|previous| offset < previous) {
            bail!("current projection image value offsets are invalid")
        }
        if let Some(start) = previous {
            validate_encoded_value(
                values
                    .get(start..offset)
                    .ok_or_else(|| anyhow!("current projection image value range is invalid"))?,
            )?;
        } else if offset != 0 {
            bail!("current projection image value offsets must start at zero")
        }
        previous = Some(offset);
    }
    if previous != Some(values.len()) {
        bail!("current projection image final value offset does not match payload")
    }
    validate_temporal_columns(
        section_bytes(bytes, sections[4])?,
        section_bytes(bytes, sections[5])?,
        row_count,
        valid_time_floor,
    )
}

fn section_bytes(bytes: &[u8], section: Section) -> Result<&[u8]> {
    let end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| anyhow!("current projection image section range overflow"))?;
    bytes
        .get(section.offset..end)
        .ok_or_else(|| anyhow!("current projection image section is truncated"))
}

fn copy_section(bytes: &mut [u8], section: Section, source: &[u8]) -> Result<()> {
    if source.len() != section.len {
        bail!("current projection image section length changed during encoding")
    }
    let end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| anyhow!("section range overflow"))?;
    bytes
        .get_mut(section.offset..end)
        .ok_or_else(|| anyhow!("section range is out of bounds"))?
        .copy_from_slice(source);
    Ok(())
}

fn checksum_payload(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(b"minigraf.current-projection.payload.v1\0");
    hasher.update(bytes.get(PAGE_SIZE..).unwrap_or_default());
    hasher.finalize()
}

fn checksum_header(page: &[u8]) -> u32 {
    let mut header = page.to_vec();
    header[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4].fill(0);
    let mut hasher = Hasher::new();
    hasher.update(b"minigraf.current-projection.header.v1\0");
    hasher.update(&header);
    hasher.finalize()
}

fn align_page(value: usize) -> Result<usize> {
    value
        .checked_add(PAGE_SIZE.saturating_sub(1))
        .map(|value| value / PAGE_SIZE * PAGE_SIZE)
        .ok_or_else(|| anyhow!("current projection image page alignment overflow"))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| anyhow!("current projection image header is truncated"))?
            .try_into()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(|| anyhow!("current projection image header is truncated"))?
            .try_into()?,
    ))
}

fn read_i64(bytes: &[u8], offset: usize) -> Result<i64> {
    Ok(i64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(|| anyhow!("current projection image header is truncated"))?
            .try_into()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CurrentProjectionPageImage, ProjectionLedgerIdentity, checksum_header, checksum_payload,
        decode, encode, read_u64,
    };
    use crate::graph::current_projection::{CurrentProjectionBuilder, ProjectedInterval};
    use crate::graph::types::Value;
    use uuid::Uuid;

    fn candidate() -> crate::CurrentProjectionCandidate {
        let mut builder = CurrentProjectionBuilder::new(":상태/value", -10, 7, 11);
        builder
            .push(Uuid::from_u128(1), &Value::Integer(i64::MIN), -20, 5)
            .unwrap();
        builder
            .push(
                Uuid::from_u128(2),
                &Value::String("vetch".into()),
                -10,
                i64::MAX,
            )
            .unwrap();
        builder
            .push(Uuid::from_u128(3), &Value::Ref(Uuid::from_u128(9)), 0, 10)
            .unwrap();
        builder
            .push(Uuid::from_u128(4), &Value::Float(-0.0), 1, 11)
            .unwrap();
        builder
            .push(Uuid::from_u128(5), &Value::Boolean(true), 2, 12)
            .unwrap();
        builder
            .push(
                Uuid::from_u128(6),
                &Value::Keyword(":state/ready".into()),
                3,
                13,
            )
            .unwrap();
        builder
            .push(Uuid::from_u128(7), &Value::Null, 4, 14)
            .unwrap();
        builder.finish().unwrap()
    }

    fn identity() -> ProjectionLedgerIdentity {
        ProjectionLedgerIdentity::new(3, 5, 11)
    }

    fn round_trip(image: &CurrentProjectionPageImage) -> crate::CurrentProjectionCandidate {
        decode(image.as_bytes(), identity(), ":상태/value", -10, 99).unwrap()
    }

    #[test]
    fn page_image_round_trips_and_is_deterministic() {
        let candidate = candidate();
        let first = encode(&candidate, identity()).unwrap();
        let second = encode(&candidate, identity()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.padded_bytes() % 4096, 0);
        let decoded = round_trip(&first);
        assert_eq!(
            decoded.fingerprint().unwrap(),
            candidate.fingerprint().unwrap()
        );
        assert_eq!(decoded.rows_at(0).unwrap(), candidate.rows_at(0).unwrap());
    }

    #[test]
    fn empty_page_image_round_trips() {
        let candidate = CurrentProjectionBuilder::new(":empty/value", -1, 1, 0)
            .finish()
            .unwrap();
        let identity = ProjectionLedgerIdentity::new(1, 0, 0);
        let image = encode(&candidate, identity).unwrap();
        let decoded = decode(image.as_bytes(), identity, ":empty/value", -1, 2).unwrap();
        assert_eq!(decoded.row_count(), 0);
        assert_eq!(
            decoded.fingerprint().unwrap(),
            candidate.fingerprint().unwrap()
        );
    }

    #[test]
    fn overlay_is_flattened_into_the_same_canonical_bytes() {
        let mut overlaid = candidate();
        overlaid.replace_entity(
            Uuid::from_u128(2),
            vec![ProjectedInterval::new(&Value::Boolean(true), -5, 50)],
        );
        let rebuilt = {
            let mut builder = CurrentProjectionBuilder::new(":상태/value", -10, 7, 11);
            builder
                .push(Uuid::from_u128(1), &Value::Integer(i64::MIN), -20, 5)
                .unwrap();
            builder
                .push(Uuid::from_u128(2), &Value::Boolean(true), -5, 50)
                .unwrap();
            builder
                .push(Uuid::from_u128(3), &Value::Ref(Uuid::from_u128(9)), 0, 10)
                .unwrap();
            builder
                .push(Uuid::from_u128(4), &Value::Float(-0.0), 1, 11)
                .unwrap();
            builder
                .push(Uuid::from_u128(5), &Value::Boolean(true), 2, 12)
                .unwrap();
            builder
                .push(
                    Uuid::from_u128(6),
                    &Value::Keyword(":state/ready".into()),
                    3,
                    13,
                )
                .unwrap();
            builder
                .push(Uuid::from_u128(7), &Value::Null, 4, 14)
                .unwrap();
            builder.finish().unwrap()
        };
        assert_eq!(
            encode(&overlaid, identity()).unwrap(),
            encode(&rebuilt, identity()).unwrap()
        );
    }

    #[test]
    fn corruption_and_identity_mismatch_fail_closed() {
        let image = encode(&candidate(), identity()).unwrap();
        let mut unknown_version = image.as_bytes().to_vec();
        unknown_version[8..12].copy_from_slice(&2_u32.to_le_bytes());
        assert!(decode(&unknown_version, identity(), ":상태/value", -10, 99).is_err());

        let mut corrupt = image.as_bytes().to_vec();
        corrupt[4096] ^= 1;
        assert!(decode(&corrupt, identity(), ":상태/value", -10, 99).is_err());
        assert!(
            decode(
                image.as_bytes(),
                ProjectionLedgerIdentity::new(4, 5, 11),
                ":상태/value",
                -10,
                99
            )
            .is_err()
        );
        assert!(decode(image.as_bytes(), identity(), ":다른/value", -10, 99).is_err());
        assert!(
            decode(
                &image.as_bytes()[..image.as_bytes().len() - 1],
                identity(),
                ":상태/value",
                -10,
                99
            )
            .is_err()
        );
        let mut trailing = image.as_bytes().to_vec();
        trailing.extend_from_slice(&[0; 4096]);
        let trailing_pages = u64::try_from(trailing.len() / 4096).unwrap();
        trailing[16..24].copy_from_slice(&trailing_pages.to_le_bytes());
        reseal(&mut trailing);
        assert!(decode(&trailing, identity(), ":상태/value", -10, 99).is_err());

        let mut overlap = image.as_bytes().to_vec();
        let attribute_offset = read_u64(&overlap, 136).unwrap();
        overlap[160..168].copy_from_slice(&attribute_offset.to_le_bytes());
        reseal(&mut overlap);
        assert!(decode(&overlap, identity(), ":상태/value", -10, 99).is_err());

        let mut header_reserved = image.as_bytes().to_vec();
        header_reserved[300] = 1;
        reseal(&mut header_reserved);
        assert!(decode(&header_reserved, identity(), ":상태/value", -10, 99).is_err());

        let mut unknown_section = image.as_bytes().to_vec();
        unknown_section[128..132].copy_from_slice(&99_u32.to_le_bytes());
        reseal(&mut unknown_section);
        assert!(decode(&unknown_section, identity(), ":상태/value", -10, 99).is_err());

        let mut nonzero_padding = image.as_bytes().to_vec();
        let attribute_offset = usize::try_from(read_u64(&nonzero_padding, 136).unwrap()).unwrap();
        let attribute_len = usize::try_from(read_u64(&nonzero_padding, 144).unwrap()).unwrap();
        nonzero_padding[attribute_offset + attribute_len] = 1;
        reseal(&mut nonzero_padding);
        assert!(decode(&nonzero_padding, identity(), ":상태/value", -10, 99).is_err());

        let mut malformed_value = image.as_bytes().to_vec();
        let value_offset = usize::try_from(read_u64(&malformed_value, 208).unwrap()).unwrap();
        malformed_value[value_offset] = 0xff;
        reseal(&mut malformed_value);
        assert!(decode(&malformed_value, identity(), ":상태/value", -10, 99).is_err());

        let mut malformed_temporal = image.as_bytes().to_vec();
        let temporal_offset = usize::try_from(read_u64(&malformed_temporal, 232).unwrap()).unwrap();
        malformed_temporal[temporal_offset] = 8;
        reseal(&mut malformed_temporal);
        assert!(decode(&malformed_temporal, identity(), ":상태/value", -10, 99).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn persistent_identity_survives_reopen_and_rejects_an_older_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("projection.graph");
        let db = crate::Minigraf::open(&path).unwrap();
        db.execute(&format!(
            "(transact [[#uuid \"{}\" :projection/value 1]])",
            Uuid::from_u128(1)
        ))
        .unwrap();
        db.checkpoint().unwrap();
        let first = db
            .benchmark_build_current_projection(":projection/value", i64::MIN)
            .unwrap();
        let first_image = db
            .benchmark_encode_current_projection_page_image(&first)
            .unwrap();
        assert_eq!(first_image.identity().manifest_generation(), 0);

        db.execute(&format!(
            "(transact [[#uuid \"{}\" :projection/value 2]])",
            Uuid::from_u128(2)
        ))
        .unwrap();
        db.checkpoint().unwrap();
        assert!(
            db.benchmark_decode_current_projection_page_image(
                &first_image,
                ":projection/value",
                i64::MIN,
            )
            .is_err()
        );
        let current = db
            .benchmark_build_current_projection(":projection/value", i64::MIN)
            .unwrap();
        let current_image = db
            .benchmark_encode_current_projection_page_image(&current)
            .unwrap();
        assert!(current_image.identity().manifest_generation() > 0);
        drop(db);

        let reopened = crate::Minigraf::open(&path).unwrap();
        let decoded = reopened
            .benchmark_decode_current_projection_page_image(
                &current_image,
                ":projection/value",
                i64::MIN,
            )
            .unwrap();
        assert_eq!(decoded.row_count(), 2);
    }

    fn reseal(bytes: &mut [u8]) {
        let payload = checksum_payload(bytes);
        bytes[76..80].copy_from_slice(&payload.to_le_bytes());
        let header = checksum_header(&bytes[..4096]);
        bytes[80..84].copy_from_slice(&header.to_le_bytes());
    }
}
