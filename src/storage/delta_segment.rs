#![allow(dead_code)]

use crate::graph::types::Fact;
use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, Indexes, VaetKey};
use crate::storage::packed_pages::pack_facts;
use crate::storage::{PAGE_SIZE, StorageBackend};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const DELTA_SEGMENT_MAGIC: [u8; 8] = *b"MGDSG001";
const DELTA_SEGMENT_COMMIT_MARKER: [u8; 8] = *b"MGDSDONE";
const DELTA_SEGMENT_CODEC_VERSION: u16 = 1;
const PREFIX_LEN: usize = 8 + 4 + 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeltaKeyRange<K> {
    min: K,
    max: K,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeltaSegmentKeyRanges {
    eavt: Option<DeltaKeyRange<EavtKey>>,
    aevt: Option<DeltaKeyRange<AevtKey>>,
    avet: Option<DeltaKeyRange<AvetKey>>,
    vaet: Option<DeltaKeyRange<VaetKey>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeltaSegmentHeader {
    codec_version: u16,
    pub(crate) fact_count: u64,
    pub(crate) fact_page_start: u64,
    pub(crate) fact_page_count: u64,
    pub(crate) low_tx_count: u64,
    pub(crate) high_tx_count: u64,
    pub(crate) eavt_count: u64,
    pub(crate) aevt_count: u64,
    pub(crate) avet_count: u64,
    pub(crate) vaet_count: u64,
    key_ranges: DeltaSegmentKeyRanges,
}

impl DeltaSegmentHeader {
    fn from_payload(
        payload: &DeltaSegmentPayload,
        fact_page_start: u64,
        fact_page_count: u64,
    ) -> Result<Self> {
        let (low_tx_count, high_tx_count) = tx_range(&payload.facts);

        Ok(Self {
            codec_version: DELTA_SEGMENT_CODEC_VERSION,
            fact_count: len_to_u64(payload.facts.len(), "fact count")?,
            fact_page_start,
            fact_page_count,
            low_tx_count,
            high_tx_count,
            eavt_count: len_to_u64(payload.eavt.len(), "EAVT count")?,
            aevt_count: len_to_u64(payload.aevt.len(), "AEVT count")?,
            avet_count: len_to_u64(payload.avet.len(), "AVET count")?,
            vaet_count: len_to_u64(payload.vaet.len(), "VAET count")?,
            key_ranges: DeltaSegmentKeyRanges {
                eavt: key_range(&payload.eavt),
                aevt: key_range(&payload.aevt),
                avet: key_range(&payload.avet),
                vaet: key_range(&payload.vaet),
            },
        })
    }

    pub(crate) fn may_overlap_eavt(&self, start: &EavtKey, end: Option<&EavtKey>) -> bool {
        range_may_overlap(&self.key_ranges.eavt, start, end)
    }

    pub(crate) fn may_overlap_aevt(&self, start: &AevtKey, end: Option<&AevtKey>) -> bool {
        range_may_overlap(&self.key_ranges.aevt, start, end)
    }

    pub(crate) fn may_overlap_avet(&self, start: &AvetKey, end: Option<&AvetKey>) -> bool {
        range_may_overlap(&self.key_ranges.avet, start, end)
    }

    pub(crate) fn may_overlap_vaet(&self, start: &VaetKey, end: Option<&VaetKey>) -> bool {
        range_may_overlap(&self.key_ranges.vaet, start, end)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct DeltaSegmentPayload {
    facts: Vec<(FactRef, Fact)>,
    pub(crate) eavt: Vec<(EavtKey, FactRef)>,
    pub(crate) aevt: Vec<(AevtKey, FactRef)>,
    pub(crate) avet: Vec<(AvetKey, FactRef)>,
    pub(crate) vaet: Vec<(VaetKey, FactRef)>,
}

impl DeltaSegmentPayload {
    pub(crate) fn facts(&self) -> &[(FactRef, Fact)] {
        &self.facts
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeltaSegmentTrailer {
    body_checksum: u32,
    commit_marker: [u8; 8],
}

impl DeltaSegmentTrailer {
    const LEN: usize = 4 + DELTA_SEGMENT_COMMIT_MARKER.len();

    fn new(body_checksum: u32) -> Self {
        Self {
            body_checksum,
            commit_marker: DELTA_SEGMENT_COMMIT_MARKER,
        }
    }

    fn append_to(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.body_checksum.to_le_bytes());
        out.extend_from_slice(&self.commit_marker);
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::LEN {
            bail!("Delta segment trailer length is invalid");
        }
        let body_checksum = read_u32_le(bytes, 0, "delta segment checksum")?;
        let marker_start = 4usize;
        let marker = bytes
            .get(marker_start..marker_start.saturating_add(DELTA_SEGMENT_COMMIT_MARKER.len()))
            .ok_or_else(|| anyhow::anyhow!("Delta segment trailer missing commit marker"))?;
        if marker != DELTA_SEGMENT_COMMIT_MARKER {
            bail!("Delta segment missing commit marker");
        }
        Ok(Self::new(body_checksum))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeltaSegment {
    header: DeltaSegmentHeader,
    payload: DeltaSegmentPayload,
}

impl DeltaSegment {
    pub(crate) fn from_facts(facts: Vec<Fact>, fact_page_start: u64) -> Result<Self> {
        let (fact_page_count, fact_refs) = if facts.is_empty() {
            (0, Vec::new())
        } else {
            let (pages, fact_refs) = pack_facts(&facts, fact_page_start)?;
            (len_to_u64(pages.len(), "delta fact page count")?, fact_refs)
        };

        let fact_rows: Vec<(FactRef, Fact)> = fact_refs.into_iter().zip(facts).collect();
        let (eavt, aevt, avet, vaet) = build_index_entries(&fact_rows);
        let payload = DeltaSegmentPayload {
            facts: fact_rows,
            eavt,
            aevt,
            avet,
            vaet,
        };
        let header = DeltaSegmentHeader::from_payload(&payload, fact_page_start, fact_page_count)?;
        Self::from_parts(header, payload)
    }

    pub(crate) fn from_parts(
        header: DeltaSegmentHeader,
        payload: DeltaSegmentPayload,
    ) -> Result<Self> {
        let segment = Self { header, payload };
        segment.validate()?;
        Ok(segment)
    }

    pub(crate) fn header(&self) -> &DeltaSegmentHeader {
        &self.header
    }

    pub(crate) fn payload(&self) -> &DeltaSegmentPayload {
        &self.payload
    }

    pub(crate) fn commit_marker_len() -> usize {
        DELTA_SEGMENT_COMMIT_MARKER.len()
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;

        let header_bytes = postcard::to_allocvec(&self.header)?;
        let payload_bytes = postcard::to_allocvec(&self.payload)?;
        let header_len = u32::try_from(header_bytes.len())
            .map_err(|_| anyhow::anyhow!("Delta segment header exceeds u32 length"))?;
        let payload_len = u64::try_from(payload_bytes.len())
            .map_err(|_| anyhow::anyhow!("Delta segment payload exceeds u64 length"))?;

        let mut body = Vec::new();
        body.extend_from_slice(&DELTA_SEGMENT_MAGIC);
        body.extend_from_slice(&header_len.to_le_bytes());
        body.extend_from_slice(&payload_len.to_le_bytes());
        body.extend_from_slice(&header_bytes);
        body.extend_from_slice(&payload_bytes);

        let trailer = DeltaSegmentTrailer::new(crc32fast::hash(&body));
        trailer.append_to(&mut body);
        Ok(body)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let min_len = PREFIX_LEN
            .checked_add(DeltaSegmentTrailer::LEN)
            .ok_or_else(|| anyhow::anyhow!("Delta segment minimum length overflow"))?;
        if bytes.len() < min_len {
            bail!("Delta segment is too short");
        }

        let trailer_offset = bytes
            .len()
            .checked_sub(DeltaSegmentTrailer::LEN)
            .ok_or_else(|| anyhow::anyhow!("Delta segment trailer offset underflow"))?;
        let trailer_bytes = bytes
            .get(trailer_offset..)
            .ok_or_else(|| anyhow::anyhow!("Delta segment trailer out of bounds"))?;
        let trailer = DeltaSegmentTrailer::from_bytes(trailer_bytes)?;
        let body = bytes
            .get(..trailer_offset)
            .ok_or_else(|| anyhow::anyhow!("Delta segment body out of bounds"))?;
        let actual_checksum = crc32fast::hash(body);
        if trailer.body_checksum != actual_checksum {
            bail!("Delta segment checksum mismatch");
        }

        let magic = body
            .get(..DELTA_SEGMENT_MAGIC.len())
            .ok_or_else(|| anyhow::anyhow!("Delta segment missing magic"))?;
        if magic != DELTA_SEGMENT_MAGIC {
            bail!("Delta segment magic mismatch");
        }

        let header_len = usize::try_from(read_u32_le(body, 8, "delta segment header length")?)
            .map_err(|_| anyhow::anyhow!("Delta segment header length exceeds usize"))?;
        let payload_len =
            usize::try_from(read_u64_le(body, 12, "delta segment payload length")?)
                .map_err(|_| anyhow::anyhow!("Delta segment payload length exceeds usize"))?;
        let header_start = PREFIX_LEN;
        let header_end = header_start
            .checked_add(header_len)
            .ok_or_else(|| anyhow::anyhow!("Delta segment header end overflow"))?;
        let payload_end = header_end
            .checked_add(payload_len)
            .ok_or_else(|| anyhow::anyhow!("Delta segment payload end overflow"))?;
        if payload_end != body.len() {
            bail!("Delta segment length fields do not match body length");
        }

        let header_bytes = body
            .get(header_start..header_end)
            .ok_or_else(|| anyhow::anyhow!("Delta segment header out of bounds"))?;
        let payload_bytes = body
            .get(header_end..payload_end)
            .ok_or_else(|| anyhow::anyhow!("Delta segment payload out of bounds"))?;
        let header: DeltaSegmentHeader = postcard::from_bytes(header_bytes)?;
        let payload: DeltaSegmentPayload = postcard::from_bytes(payload_bytes)?;

        Self::from_parts(header, payload)
    }

    pub(crate) fn decode_from_page_bytes(bytes: &[u8]) -> Result<Self> {
        let min_len = PREFIX_LEN
            .checked_add(DeltaSegmentTrailer::LEN)
            .ok_or_else(|| anyhow::anyhow!("Delta segment minimum length overflow"))?;
        if bytes.len() < min_len {
            bail!("Delta segment page bytes are too short");
        }

        let magic = bytes
            .get(..DELTA_SEGMENT_MAGIC.len())
            .ok_or_else(|| anyhow::anyhow!("Delta segment missing magic"))?;
        if magic != DELTA_SEGMENT_MAGIC {
            bail!("Delta segment magic mismatch");
        }

        let header_len = usize::try_from(read_u32_le(bytes, 8, "delta segment header length")?)
            .map_err(|_| anyhow::anyhow!("Delta segment header length exceeds usize"))?;
        let payload_len = usize::try_from(read_u64_le(bytes, 12, "delta segment payload length")?)
            .map_err(|_| anyhow::anyhow!("Delta segment payload length exceeds usize"))?;
        let exact_len = PREFIX_LEN
            .checked_add(header_len)
            .and_then(|len| len.checked_add(payload_len))
            .and_then(|len| len.checked_add(DeltaSegmentTrailer::LEN))
            .ok_or_else(|| anyhow::anyhow!("Delta segment encoded length overflow"))?;
        if exact_len > bytes.len() {
            bail!("Delta segment encoded length exceeds descriptor pages");
        }

        Self::decode(
            bytes
                .get(..exact_len)
                .ok_or_else(|| anyhow::anyhow!("Delta segment exact bytes out of bounds"))?,
        )
    }

    fn validate(&self) -> Result<()> {
        if self.header.codec_version != DELTA_SEGMENT_CODEC_VERSION {
            bail!("Unsupported delta segment codec version");
        }

        if self.header.fact_count != len_to_u64(self.payload.facts.len(), "fact count")? {
            bail!("Delta segment fact count mismatch");
        }
        if self.header.eavt_count != len_to_u64(self.payload.eavt.len(), "EAVT count")? {
            bail!("Delta segment EAVT count mismatch");
        }
        if self.header.aevt_count != len_to_u64(self.payload.aevt.len(), "AEVT count")? {
            bail!("Delta segment AEVT count mismatch");
        }
        if self.header.avet_count != len_to_u64(self.payload.avet.len(), "AVET count")? {
            bail!("Delta segment AVET count mismatch");
        }
        if self.header.vaet_count != len_to_u64(self.payload.vaet.len(), "VAET count")? {
            bail!("Delta segment VAET count mismatch");
        }

        let expected_ranges = DeltaSegmentKeyRanges {
            eavt: key_range(&self.payload.eavt),
            aevt: key_range(&self.payload.aevt),
            avet: key_range(&self.payload.avet),
            vaet: key_range(&self.payload.vaet),
        };
        if self.header.key_ranges != expected_ranges {
            bail!("Delta segment key range metadata mismatch");
        }

        let (low_tx_count, high_tx_count) = tx_range(&self.payload.facts);
        if self.header.low_tx_count != low_tx_count || self.header.high_tx_count != high_tx_count {
            bail!("Delta segment tx_count range mismatch");
        }

        let fact_page_end = self
            .header
            .fact_page_start
            .checked_add(self.header.fact_page_count)
            .ok_or_else(|| anyhow::anyhow!("Delta segment fact page range overflow"))?;
        let mut fact_refs = HashSet::new();
        for (fact_ref, _) in &self.payload.facts {
            validate_fact_ref_in_range(
                *fact_ref,
                self.header.fact_page_start,
                fact_page_end,
                "delta fact row",
            )?;
            if !fact_refs.insert(*fact_ref) {
                bail!("Delta segment duplicate fact reference");
            }
        }

        validate_index_refs(&self.payload.eavt, &fact_refs, "EAVT")?;
        validate_index_refs(&self.payload.aevt, &fact_refs, "AEVT")?;
        validate_index_refs(&self.payload.avet, &fact_refs, "AVET")?;
        validate_index_refs(&self.payload.vaet, &fact_refs, "VAET")?;

        let (eavt, aevt, avet, vaet) = build_index_entries(&self.payload.facts);
        if self.payload.eavt != eavt
            || self.payload.aevt != aevt
            || self.payload.avet != avet
            || self.payload.vaet != vaet
        {
            bail!("Delta segment index entries do not match facts");
        }

        Ok(())
    }
}

pub(crate) fn write_segment_pages<B: StorageBackend>(
    backend: &mut B,
    page_start: u64,
    segment: &DeltaSegment,
) -> Result<u64> {
    if page_start == 0 {
        bail!("Delta segment must not start on page 0");
    }

    let bytes = segment.encode()?;
    let page_count = bytes.len().div_ceil(PAGE_SIZE);
    let page_count_u64 = u64::try_from(page_count)
        .map_err(|_| anyhow::anyhow!("Delta segment page count exceeds u64"))?;

    for page_index in 0..page_count {
        let byte_start = page_index
            .checked_mul(PAGE_SIZE)
            .ok_or_else(|| anyhow::anyhow!("Delta segment byte offset overflow"))?;
        let byte_end = byte_start.saturating_add(PAGE_SIZE).min(bytes.len());
        let mut page = vec![0u8; PAGE_SIZE];
        page.get_mut(..byte_end.saturating_sub(byte_start))
            .ok_or_else(|| anyhow::anyhow!("Delta segment page slice out of bounds"))?
            .copy_from_slice(
                bytes
                    .get(byte_start..byte_end)
                    .ok_or_else(|| anyhow::anyhow!("Delta segment bytes out of bounds"))?,
            );
        let page_offset = u64::try_from(page_index)
            .map_err(|_| anyhow::anyhow!("Delta segment page index exceeds u64"))?;
        let page_id = page_start
            .checked_add(page_offset)
            .ok_or_else(|| anyhow::anyhow!("Delta segment page id overflow"))?;
        backend.write_page(page_id, &page)?;
    }

    Ok(page_count_u64)
}

type DeltaIndexEntryVectors = (
    Vec<(EavtKey, FactRef)>,
    Vec<(AevtKey, FactRef)>,
    Vec<(AvetKey, FactRef)>,
    Vec<(VaetKey, FactRef)>,
);

fn build_index_entries(facts: &[(FactRef, Fact)]) -> DeltaIndexEntryVectors {
    let mut indexes = Indexes::new();
    for (fact_ref, fact) in facts {
        indexes.insert(fact, *fact_ref);
    }
    (
        indexes.eavt.into_iter().collect(),
        indexes.aevt.into_iter().collect(),
        indexes.avet.into_iter().collect(),
        indexes.vaet.into_iter().collect(),
    )
}

fn key_range<K: Clone>(entries: &[(K, FactRef)]) -> Option<DeltaKeyRange<K>> {
    let first = entries.first()?;
    let last = entries.last()?;
    Some(DeltaKeyRange {
        min: first.0.clone(),
        max: last.0.clone(),
    })
}

fn range_may_overlap<K: Ord>(range: &Option<DeltaKeyRange<K>>, start: &K, end: Option<&K>) -> bool {
    let Some(range) = range else {
        return false;
    };
    if &range.max < start {
        return false;
    }
    if let Some(end) = end
        && &range.min >= end
    {
        return false;
    }
    true
}

fn tx_range(facts: &[(FactRef, Fact)]) -> (u64, u64) {
    let mut iter = facts.iter().map(|(_, fact)| fact.tx_count);
    let Some(first) = iter.next() else {
        return (0, 0);
    };
    iter.fold((first, first), |(low, high), tx_count| {
        (low.min(tx_count), high.max(tx_count))
    })
}

fn validate_fact_ref_in_range(
    fact_ref: FactRef,
    fact_page_start: u64,
    fact_page_end: u64,
    label: &str,
) -> Result<()> {
    if fact_ref.page_id < fact_page_start || fact_ref.page_id >= fact_page_end {
        bail!("{label} page reference out of bounds");
    }
    Ok(())
}

fn validate_index_refs<K>(
    entries: &[(K, FactRef)],
    fact_refs: &HashSet<FactRef>,
    label: &str,
) -> Result<()> {
    for (_, fact_ref) in entries {
        if !fact_refs.contains(fact_ref) {
            bail!("{label} entry references missing fact");
        }
    }
    Ok(())
}

fn len_to_u64(len: usize, label: &str) -> Result<u64> {
    u64::try_from(len).map_err(|_| anyhow::anyhow!("{label} exceeds u64"))
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
    use super::DeltaSegment;
    use crate::graph::types::{Fact, VALID_TIME_FOREVER, Value};
    use crate::storage::index::{EavtKey, FactRef, encode_value};
    use uuid::Uuid;

    fn fact(entity: Uuid, attribute: &str, value: Value, tx_count: u64) -> Fact {
        Fact::with_valid_time(
            entity,
            attribute.to_string(),
            value,
            1_000 + tx_count,
            tx_count,
            10,
            VALID_TIME_FOREVER,
        )
    }

    fn eavt_start(entity: Uuid, attribute: &str) -> EavtKey {
        EavtKey {
            entity,
            attribute: attribute.to_string(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        }
    }

    fn eavt_end(entity: Uuid, attribute: &str) -> EavtKey {
        EavtKey {
            entity,
            attribute: attribute.to_string(),
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            tx_count: u64::MAX,
            value_bytes: vec![0xFF],
            tx_id: u64::MAX,
            asserted: true,
        }
    }

    #[test]
    fn segment_round_trip_preserves_index_counts_and_tx_range() {
        let source = Uuid::from_u128(1);
        let target = Uuid::from_u128(2);
        let facts = vec![
            fact(source, ":name", Value::String("Alice".to_string()), 2),
            fact(source, ":edge/to", Value::Ref(target), 5),
            Fact::retract_with_valid_time(
                source,
                ":edge/to".to_string(),
                Value::Ref(target),
                1_004,
                4,
                10,
                VALID_TIME_FOREVER,
            ),
        ];

        let segment = DeltaSegment::from_facts(facts, 40).expect("segment should build");
        let bytes = segment.encode().expect("segment should encode");
        let decoded = DeltaSegment::decode(&bytes).expect("segment should decode");
        let header = decoded.header();

        assert_eq!(header.fact_count, 3, "fact count must round-trip");
        assert_eq!(header.eavt_count, 3, "EAVT count must round-trip");
        assert_eq!(header.aevt_count, 3, "AEVT count must round-trip");
        assert_eq!(header.avet_count, 3, "AVET count must round-trip");
        assert_eq!(
            header.vaet_count, 2,
            "VAET count must include only ref facts"
        );
        assert_eq!(header.low_tx_count, 2, "low tx_count must round-trip");
        assert_eq!(header.high_tx_count, 5, "high tx_count must round-trip");
    }

    #[test]
    fn missing_commit_marker_rejects_segment() {
        let entity = Uuid::from_u128(3);
        let segment = DeltaSegment::from_facts(
            vec![fact(entity, ":name", Value::String("Ada".to_string()), 1)],
            50,
        )
        .expect("segment should build");
        let mut bytes = segment.encode().expect("segment should encode");
        let new_len = bytes
            .len()
            .checked_sub(DeltaSegment::commit_marker_len())
            .expect("encoded segment must include marker");
        bytes.truncate(new_len);

        assert!(
            DeltaSegment::decode(&bytes).is_err(),
            "missing marker must reject"
        );
    }

    #[test]
    fn checksum_mismatch_rejects_segment() {
        let entity = Uuid::from_u128(4);
        let segment = DeltaSegment::from_facts(
            vec![fact(entity, ":name", Value::String("Ada".to_string()), 1)],
            60,
        )
        .expect("segment should build");
        let mut bytes = segment.encode().expect("segment should encode");
        let byte = bytes.get_mut(12).expect("encoded segment has a body byte");
        *byte ^= 0x01;

        assert!(
            DeltaSegment::decode(&bytes).is_err(),
            "checksum mismatch must reject"
        );
    }

    #[test]
    fn out_of_bounds_page_reference_rejects_segment() {
        let entity = Uuid::from_u128(5);
        let segment = DeltaSegment::from_facts(
            vec![fact(entity, ":name", Value::String("Ada".to_string()), 1)],
            70,
        )
        .expect("segment should build");
        let mut payload = segment.payload().clone();
        payload.eavt[0].1 = FactRef {
            page_id: segment.header().fact_page_start + segment.header().fact_page_count,
            slot_index: 0,
        };

        assert!(
            DeltaSegment::from_parts(segment.header().clone(), payload).is_err(),
            "out-of-bounds FactRef must reject"
        );
    }

    #[test]
    fn segment_min_max_keys_skip_irrelevant_range() {
        let entity = Uuid::from_u128(6);
        let other = Uuid::from_u128(7);
        let segment = DeltaSegment::from_facts(
            vec![fact(entity, ":name", Value::String("Ada".to_string()), 1)],
            80,
        )
        .expect("segment should build");
        let header = segment.header();

        assert!(
            header.may_overlap_eavt(
                &eavt_start(entity, ":name"),
                Some(&eavt_end(entity, ":name"))
            ),
            "matching EAVT range must overlap"
        );
        assert!(
            !header.may_overlap_eavt(&eavt_start(other, ":name"), Some(&eavt_end(other, ":name"))),
            "irrelevant EAVT range must be skippable"
        );
    }

    #[test]
    fn vaet_segment_contains_only_ref_rows() {
        let source = Uuid::from_u128(8);
        let target = Uuid::from_u128(9);
        let segment = DeltaSegment::from_facts(
            vec![
                fact(source, ":name", Value::String("Ada".to_string()), 1),
                fact(source, ":edge/to", Value::Ref(target), 2),
            ],
            90,
        )
        .expect("segment should build");
        let decoded = DeltaSegment::decode(&segment.encode().expect("segment should encode"))
            .expect("segment should decode");

        assert_eq!(
            decoded.header().vaet_count,
            1,
            "VAET must contain one ref row"
        );
        assert_eq!(
            decoded.payload().vaet[0].0.ref_target,
            target,
            "VAET target must come from the ref value"
        );

        let mut payload = decoded.payload().clone();
        payload.vaet[0].1 = payload
            .eavt
            .iter()
            .find(|(key, _)| key.value_bytes == encode_value(&Value::String("Ada".to_string())))
            .map(|(_, fact_ref)| *fact_ref)
            .expect("string fact ref should exist");

        assert!(
            DeltaSegment::from_parts(decoded.header().clone(), payload).is_err(),
            "VAET entry pointing at a non-ref fact must reject"
        );
    }
}
