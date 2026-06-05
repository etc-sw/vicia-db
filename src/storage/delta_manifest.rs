#![allow(dead_code)]

use crate::storage::delta_segment::DeltaSegmentHeader;
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
        ManifestSlot, select_manifest_candidate,
    };
    use crate::graph::types::{Fact, VALID_TIME_FOREVER, Value};
    use crate::storage::delta_segment::DeltaSegment;
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
