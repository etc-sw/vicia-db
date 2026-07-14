//! Repository-only current-projection feasibility model.
//!
//! This is deliberately not a production cache or a persisted format. It
//! measures the compact-base plus exact-ledger-refresh shape proposed for R1
//! without creating a second authority beside the append-only fact ledger.

use crate::graph::types::{EntityId, Value};
use crate::storage::index::encode_value;
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Result of refreshing a candidate from the authoritative ledger tail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentProjectionRefreshDiagnostics {
    /// Ledger-tail records inspected after the candidate watermark.
    pub tail_facts_visited: u64,
    /// Distinct entities whose selected attribute changed.
    pub touched_entities: u64,
    /// Exact current rows installed in the mutable overlay.
    pub replacement_rows: u64,
    /// Publication generation captured after a successful refresh.
    pub publication_generation: u64,
    /// Transaction watermark captured after a successful refresh.
    pub tx_count: u64,
}

#[derive(Debug, Default)]
struct OverlayEntry {
    values: Vec<Vec<u8>>,
}

/// Bench-only compact current-state projection candidate for one attribute.
///
/// The immutable base is row-aligned entity and canonical-value columns. A
/// small entity-keyed overlay replaces base rows after writes. Overlay values
/// are always recomputed through Vicia's exact current-view cursor; this type
/// never interprets assertions or retractions independently.
#[derive(Debug)]
pub struct CurrentProjectionCandidate {
    attribute: String,
    valid_at: i64,
    publication_generation: u64,
    tx_count: u64,
    entities: Vec<EntityId>,
    value_offsets: Vec<u32>,
    value_bytes: Vec<u8>,
    overlay: BTreeMap<EntityId, OverlayEntry>,
}

impl CurrentProjectionCandidate {
    /// Selected attribute represented by this candidate.
    #[must_use]
    pub fn attribute(&self) -> &str {
        &self.attribute
    }

    /// Fixed valid-time point represented by this candidate.
    #[must_use]
    pub fn valid_at(&self) -> i64 {
        self.valid_at
    }

    /// Current number of projected EAV rows after overlay replacement.
    #[must_use]
    pub fn row_count(&self) -> usize {
        let replaced = self.overlay.keys().fold(0_usize, |total, entity| {
            let (start, end) = self.base_entity_range(*entity);
            total.saturating_add(end.saturating_sub(start))
        });
        let overlay_rows = self.overlay.values().fold(0_usize, |total, entry| {
            total.saturating_add(entry.values.len())
        });
        self.entities
            .len()
            .saturating_sub(replaced)
            .saturating_add(overlay_rows)
    }

    /// Accounted resident bytes for compact columns and overlay payloads.
    ///
    /// B-tree node and allocator metadata are excluded and separately visible
    /// in process RSS, matching the existing benchmark accounting convention.
    #[must_use]
    pub fn accounted_bytes(&self) -> u64 {
        let base = self
            .attribute
            .capacity()
            .saturating_add(
                self.entities
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EntityId>()),
            )
            .saturating_add(
                self.value_offsets
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(self.value_bytes.capacity());
        let overlay = self.overlay.values().fold(0_usize, |total, entry| {
            let values = entry
                .values
                .capacity()
                .saturating_mul(std::mem::size_of::<Vec<u8>>());
            let payload = entry.values.iter().fold(0_usize, |bytes, value| {
                bytes.saturating_add(value.capacity())
            });
            total.saturating_add(values).saturating_add(payload)
        });
        u64::try_from(base.saturating_add(overlay)).unwrap_or(u64::MAX)
    }

    /// Stable content fingerprint independent of generation and build order.
    pub fn fingerprint(&self) -> Result<u64> {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        hash_bytes(&mut hash, self.attribute.as_bytes());
        hash_bytes(&mut hash, &self.valid_at.to_be_bytes());
        self.visit_merged_encoded(&mut |entity, value| {
            hash_bytes(&mut hash, entity.as_bytes());
            hash_bytes(&mut hash, value);
            Ok(())
        })?;
        Ok(hash)
    }

    pub(crate) fn publication_generation(&self) -> u64 {
        self.publication_generation
    }

    pub(crate) fn tx_count(&self) -> u64 {
        self.tx_count
    }

    pub(crate) fn set_watermark(&mut self, publication_generation: u64, tx_count: u64) {
        self.publication_generation = publication_generation;
        self.tx_count = tx_count;
    }

    pub(crate) fn replace_entity(&mut self, entity: EntityId, mut values: Vec<Vec<u8>>) {
        values.sort_unstable();
        self.overlay.insert(entity, OverlayEntry { values });
    }

    pub(crate) fn rows(&self) -> Result<Vec<(EntityId, Value)>> {
        let mut rows = Vec::with_capacity(self.row_count());
        self.visit_merged_encoded(&mut |entity, encoded| {
            rows.push((entity, decode_value(encoded)?));
            Ok(())
        })?;
        Ok(rows)
    }

    pub(crate) fn integer_count_sum(&self) -> Result<(u64, i128)> {
        let mut count = 0_u64;
        let mut sum = 0_i128;
        self.visit_merged_encoded(&mut |_, encoded| {
            let value = decode_integer(encoded)?;
            count = count.saturating_add(1);
            sum = sum.saturating_add(i128::from(value));
            Ok(())
        })?;
        Ok((count, sum))
    }

    fn base_entity_range(&self, entity: EntityId) -> (usize, usize) {
        let start = self
            .entities
            .partition_point(|candidate| *candidate < entity);
        let end = self
            .entities
            .partition_point(|candidate| *candidate <= entity);
        (start, end)
    }

    fn base_value(&self, index: usize) -> Result<&[u8]> {
        let start = self
            .value_offsets
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("current projection value offset missing"))?;
        let end = self
            .value_offsets
            .get(index.saturating_add(1))
            .copied()
            .ok_or_else(|| anyhow!("current projection value end offset missing"))?;
        self.value_bytes
            .get(usize::try_from(start)?..usize::try_from(end)?)
            .ok_or_else(|| anyhow!("current projection value range is corrupt"))
    }

    fn visit_merged_encoded(
        &self,
        visit: &mut dyn FnMut(EntityId, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let mut overlay_iter = self.overlay.iter();
        let mut overlay = overlay_iter.next();
        let mut base = 0_usize;

        while base < self.entities.len() {
            let entity = *self
                .entities
                .get(base)
                .ok_or_else(|| anyhow!("current projection entity missing"))?;
            let mut base_end = base.saturating_add(1);
            while self.entities.get(base_end) == Some(&entity) {
                base_end = base_end.saturating_add(1);
            }

            while overlay.is_some_and(|(overlay_entity, _)| *overlay_entity < entity) {
                if let Some((overlay_entity, entry)) = overlay {
                    visit_overlay(*overlay_entity, entry, visit)?;
                }
                overlay = overlay_iter.next();
            }

            if let Some((overlay_entity, entry)) = overlay
                && *overlay_entity == entity
            {
                visit_overlay(entity, entry, visit)?;
                overlay = overlay_iter.next();
            } else {
                for index in base..base_end {
                    visit(entity, self.base_value(index)?)?;
                }
            }
            base = base_end;
        }

        while let Some((entity, entry)) = overlay {
            visit_overlay(*entity, entry, visit)?;
            overlay = overlay_iter.next();
        }
        Ok(())
    }
}

fn visit_overlay(
    entity: EntityId,
    entry: &OverlayEntry,
    visit: &mut dyn FnMut(EntityId, &[u8]) -> Result<()>,
) -> Result<()> {
    for value in &entry.values {
        visit(entity, value)?;
    }
    Ok(())
}

pub(crate) struct CurrentProjectionBuilder {
    attribute: String,
    valid_at: i64,
    publication_generation: u64,
    tx_count: u64,
    entities: Vec<EntityId>,
    value_offsets: Vec<u32>,
    value_bytes: Vec<u8>,
    current_entity: Option<EntityId>,
    current_values: Vec<Vec<u8>>,
}

impl CurrentProjectionBuilder {
    pub(crate) fn new(
        attribute: &str,
        valid_at: i64,
        publication_generation: u64,
        tx_count: u64,
    ) -> Self {
        Self {
            attribute: attribute.to_owned(),
            valid_at,
            publication_generation,
            tx_count,
            entities: Vec::new(),
            value_offsets: Vec::new(),
            value_bytes: Vec::new(),
            current_entity: None,
            current_values: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, entity: EntityId, value: &Value) -> Result<()> {
        if self.current_entity.is_some_and(|current| current != entity) {
            self.flush_entity()?;
        }
        self.current_entity = Some(entity);
        self.current_values.push(encode_value(value));
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<CurrentProjectionCandidate> {
        self.flush_entity()?;
        self.value_offsets
            .push(u32::try_from(self.value_bytes.len()).map_err(|_| {
                anyhow!("current projection candidate exceeds the 4 GiB feasibility limit")
            })?);
        Ok(CurrentProjectionCandidate {
            attribute: self.attribute,
            valid_at: self.valid_at,
            publication_generation: self.publication_generation,
            tx_count: self.tx_count,
            entities: self.entities,
            value_offsets: self.value_offsets,
            value_bytes: self.value_bytes,
            overlay: BTreeMap::new(),
        })
    }

    fn flush_entity(&mut self) -> Result<()> {
        let Some(entity) = self.current_entity.take() else {
            return Ok(());
        };
        self.current_values.sort_unstable();
        for value in self.current_values.drain(..) {
            self.value_offsets
                .push(u32::try_from(self.value_bytes.len()).map_err(|_| {
                    anyhow!("current projection candidate exceeds the 4 GiB feasibility limit")
                })?);
            self.entities.push(entity);
            self.value_bytes.extend_from_slice(&value);
        }
        Ok(())
    }
}

pub(crate) fn encode_values(values: &[Value]) -> Vec<Vec<u8>> {
    values.iter().map(encode_value).collect()
}

fn decode_integer(encoded: &[u8]) -> Result<i64> {
    let payload = encoded
        .get(1..)
        .ok_or_else(|| anyhow!("empty current projection value"))?;
    if encoded.first() != Some(&0x02) || payload.len() != 8 {
        bail!("current projection integer aggregate encountered a non-integer value")
    }
    let bits = u64::from_be_bytes(payload.try_into()?);
    Ok((bits ^ 0x8000_0000_0000_0000).cast_signed())
}

fn decode_value(encoded: &[u8]) -> Result<Value> {
    let payload = encoded
        .get(1..)
        .ok_or_else(|| anyhow!("empty current projection value"))?;
    match encoded.first().copied() {
        Some(0x00) if payload.is_empty() => Ok(Value::Null),
        Some(0x01) if payload == [0] => Ok(Value::Boolean(false)),
        Some(0x01) if payload == [1] => Ok(Value::Boolean(true)),
        Some(0x02) if payload.len() == 8 => Ok(Value::Integer(decode_integer(encoded)?)),
        Some(0x03) if payload.len() == 8 => {
            let ordered = u64::from_be_bytes(payload.try_into()?);
            let raw = if ordered >> 63 == 0 {
                !ordered
            } else {
                ordered ^ 0x8000_0000_0000_0000
            };
            Ok(Value::Float(f64::from_bits(raw)))
        }
        Some(0x04) => Ok(Value::String(std::str::from_utf8(payload)?.to_owned())),
        Some(0x05) => Ok(Value::Keyword(std::str::from_utf8(payload)?.to_owned())),
        Some(0x06) if payload.len() == 16 => {
            Ok(Value::Ref(EntityId::from_bytes(payload.try_into()?)))
        }
        Some(tag) => bail!("malformed current projection value tag 0x{tag:02x}"),
        None => bail!("empty current projection value"),
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}

#[cfg(test)]
mod tests {
    use super::{CurrentProjectionBuilder, decode_value};
    use crate::graph::types::Value;
    use crate::storage::index::encode_value;
    use uuid::Uuid;

    #[test]
    fn value_codec_round_trips_every_projection_type() {
        let values = [
            Value::Null,
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            Value::Float(-0.0),
            Value::Float(12.5),
            Value::String("vetch".to_owned()),
            Value::Keyword(":state/ready".to_owned()),
            Value::Ref(Uuid::from_u128(42)),
        ];
        for value in values {
            assert_eq!(decode_value(&encode_value(&value)).unwrap(), value);
        }
    }

    #[test]
    fn compact_base_and_overlay_have_deterministic_merged_rows() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let mut builder = CurrentProjectionBuilder::new(":v", 100, 4, 3);
        builder.push(first, &Value::Integer(2)).unwrap();
        builder.push(first, &Value::Integer(1)).unwrap();
        builder.push(second, &Value::Integer(3)).unwrap();
        let mut candidate = builder.finish().unwrap();
        let original = candidate.fingerprint().unwrap();
        assert_eq!(candidate.integer_count_sum().unwrap(), (3, 6));

        candidate.replace_entity(first, vec![encode_value(&Value::Integer(7))]);
        assert_eq!(candidate.integer_count_sum().unwrap(), (2, 10));
        assert_ne!(candidate.fingerprint().unwrap(), original);
        assert_eq!(candidate.row_count(), 2);
    }
}
