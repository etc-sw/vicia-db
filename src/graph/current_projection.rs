//! Temporal current-projection model used by explicit maintenance rebuilds.
//!
//! The append-only fact ledger remains authoritative. Maintenance rebuilds
//! encode this deterministic derived state; production query routing is a
//! separate admission boundary.

#![cfg_attr(not(any(test, feature = "bench-internals")), allow(dead_code))]

use crate::graph::types::{EntityId, Value};
use crate::storage::index::encode_value;
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) type IntervalVisitor<'a> = dyn FnMut(EntityId, &[u8], i64, i64) -> Result<()> + 'a;

/// Result of refreshing a candidate from the authoritative ledger tail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentProjectionRefreshDiagnostics {
    /// Ledger-tail records inspected after the candidate watermark.
    pub tail_facts_visited: u64,
    /// Distinct entities whose selected attribute changed.
    pub touched_entities: u64,
    /// Exact surviving interval rows installed in the mutable overlay.
    pub replacement_rows: u64,
    /// Publication generation captured after a successful refresh.
    pub publication_generation: u64,
    /// Transaction watermark captured after a successful refresh.
    pub tx_count: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ProjectedInterval {
    value: Vec<u8>,
    valid_from: i64,
    valid_to: i64,
}

impl ProjectedInterval {
    pub(crate) fn new(value: &Value, valid_from: i64, valid_to: i64) -> Self {
        Self {
            value: encode_value(value),
            valid_from,
            valid_to,
        }
    }
}

#[derive(Debug, Default)]
struct OverlayEntry {
    intervals: Vec<ProjectedInterval>,
}

#[derive(Debug, Default)]
pub(crate) struct TemporalColumnBuilder {
    pub(crate) bytes: Vec<u8>,
    predictors: [u64; 8],
    next_predictor: usize,
}

impl TemporalColumnBuilder {
    pub(crate) fn push(&mut self, value: i64) {
        let bits = u64::from_ne_bytes(value.to_ne_bytes());
        let (predictor, delta) = self
            .predictors
            .iter()
            .enumerate()
            .map(|(index, previous)| (index, bits ^ previous))
            .min_by_key(|(index, delta)| (varint_len(*delta), *index))
            .unwrap_or((0, bits));
        self.bytes.push(u8::try_from(predictor).unwrap_or_default());
        encode_varint(delta, &mut self.bytes);
        if let Some(slot) = self.predictors.get_mut(self.next_predictor) {
            *slot = bits;
        }
        self.next_predictor = self.next_predictor.saturating_add(1) % self.predictors.len();
    }
}

struct TemporalColumnDecoder<'a> {
    bytes: &'a [u8],
    position: usize,
    predictors: [u64; 8],
    next_predictor: usize,
    rows: usize,
}

impl<'a> TemporalColumnDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            predictors: [0; 8],
            next_predictor: 0,
            rows: 0,
        }
    }

    fn next(&mut self) -> Result<i64> {
        let predictor = usize::from(
            *self
                .bytes
                .get(self.position)
                .ok_or_else(|| anyhow!("truncated current projection temporal predictor"))?,
        );
        self.position = self.position.saturating_add(1);
        let previous = *self
            .predictors
            .get(predictor)
            .ok_or_else(|| anyhow!("invalid current projection temporal predictor"))?;
        let delta = decode_varint(self.bytes, &mut self.position)?;
        let bits = previous ^ delta;
        if let Some(slot) = self.predictors.get_mut(self.next_predictor) {
            *slot = bits;
        }
        self.next_predictor = self.next_predictor.saturating_add(1) % self.predictors.len();
        self.rows = self.rows.saturating_add(1);
        Ok(i64::from_ne_bytes(bits.to_ne_bytes()))
    }

    fn finish(self, expected_rows: usize) -> Result<()> {
        if self.rows != expected_rows {
            bail!(
                "current projection temporal row mismatch: decoded {}, expected {}",
                self.rows,
                expected_rows
            )
        }
        if self.position != self.bytes.len() {
            bail!("current projection temporal column has trailing bytes")
        }
        Ok(())
    }
}

/// Bench-only compact current-state projection candidate for one attribute.
///
/// The immutable base is composed of row-aligned entity, canonical-value, and
/// compressed valid-time columns. A small entity-keyed overlay replaces base
/// rows after writes. Overlay intervals are always recomputed through Vicia's
/// exact current-view cursor; this type never interprets assertions or
/// retractions independently.
#[derive(Debug)]
pub struct CurrentProjectionCandidate {
    attribute: String,
    valid_time_floor: i64,
    publication_generation: u64,
    tx_count: u64,
    entities: Vec<EntityId>,
    value_offsets: Vec<u32>,
    value_bytes: Vec<u8>,
    valid_from_bytes: Vec<u8>,
    valid_to_bytes: Vec<u8>,
    overlay: BTreeMap<EntityId, OverlayEntry>,
}

pub(crate) struct EncodedProjectionColumns {
    pub(crate) attribute: String,
    pub(crate) valid_time_floor: i64,
    pub(crate) publication_generation: u64,
    pub(crate) tx_count: u64,
    pub(crate) entities: Vec<EntityId>,
    pub(crate) value_offsets: Vec<u32>,
    pub(crate) value_bytes: Vec<u8>,
    pub(crate) valid_from_bytes: Vec<u8>,
    pub(crate) valid_to_bytes: Vec<u8>,
}

impl CurrentProjectionCandidate {
    /// Selected attribute represented by this candidate.
    #[must_use]
    pub fn attribute(&self) -> &str {
        &self.attribute
    }

    /// Earliest valid-time point this current-only candidate can answer.
    #[must_use]
    pub fn valid_time_floor(&self) -> i64 {
        self.valid_time_floor
    }

    /// Compatibility name for the R1 fixed-time benchmark hook.
    #[must_use]
    pub fn valid_at(&self) -> i64 {
        self.valid_time_floor
    }

    /// Number of surviving interval rows retained after overlay replacement.
    #[must_use]
    pub fn row_count(&self) -> usize {
        let replaced = self.overlay.keys().fold(0_usize, |total, entity| {
            let (start, end) = self.base_entity_range(*entity);
            total.saturating_add(end.saturating_sub(start))
        });
        let overlay_rows = self.overlay.values().fold(0_usize, |total, entry| {
            total.saturating_add(entry.intervals.len())
        });
        self.entities
            .len()
            .saturating_sub(replaced)
            .saturating_add(overlay_rows)
    }

    /// Encoded bytes occupied by both immutable valid-time columns.
    #[must_use]
    pub fn temporal_payload_bytes(&self) -> u64 {
        u64::try_from(
            self.valid_from_bytes
                .capacity()
                .saturating_add(self.valid_to_bytes.capacity()),
        )
        .unwrap_or(u64::MAX)
    }

    /// Encoded bytes occupied by the valid-from column.
    #[must_use]
    pub fn valid_from_payload_bytes(&self) -> u64 {
        u64::try_from(self.valid_from_bytes.capacity()).unwrap_or(u64::MAX)
    }

    /// Encoded bytes occupied by the valid-to column.
    #[must_use]
    pub fn valid_to_payload_bytes(&self) -> u64 {
        u64::try_from(self.valid_to_bytes.capacity()).unwrap_or(u64::MAX)
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
            .saturating_add(self.value_bytes.capacity())
            .saturating_add(self.valid_from_bytes.capacity())
            .saturating_add(self.valid_to_bytes.capacity());
        let overlay = self.overlay.values().fold(0_usize, |total, entry| {
            let rows = entry
                .intervals
                .capacity()
                .saturating_mul(std::mem::size_of::<ProjectedInterval>());
            let payload = entry.intervals.iter().fold(0_usize, |bytes, interval| {
                bytes.saturating_add(interval.value.capacity())
            });
            total.saturating_add(rows).saturating_add(payload)
        });
        u64::try_from(base.saturating_add(overlay)).unwrap_or(u64::MAX)
    }

    /// Stable logical-content fingerprint independent of generation.
    pub fn fingerprint(&self) -> Result<u64> {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        hash_bytes(&mut hash, self.attribute.as_bytes());
        hash_bytes(&mut hash, &self.valid_time_floor.to_be_bytes());
        self.visit_merged_encoded(&mut |entity, value, valid_from, valid_to| {
            hash_bytes(&mut hash, entity.as_bytes());
            hash_bytes(&mut hash, value);
            hash_bytes(&mut hash, &valid_from.to_be_bytes());
            hash_bytes(&mut hash, &valid_to.to_be_bytes());
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

    pub(crate) fn replace_entity(
        &mut self,
        entity: EntityId,
        mut intervals: Vec<ProjectedInterval>,
    ) {
        intervals.sort_unstable_by(|left, right| {
            left.value
                .cmp(&right.value)
                .then_with(|| left.valid_from.cmp(&right.valid_from))
                .then_with(|| left.valid_to.cmp(&right.valid_to))
        });
        self.overlay.insert(entity, OverlayEntry { intervals });
    }

    pub(crate) fn rows(&self) -> Result<Vec<(EntityId, Value)>> {
        self.rows_at(self.valid_time_floor)
    }

    pub(crate) fn rows_at(&self, valid_at: i64) -> Result<Vec<(EntityId, Value)>> {
        self.ensure_valid_at(valid_at)?;
        let mut rows = Vec::new();
        self.visit_merged_encoded(&mut |entity, encoded, valid_from, valid_to| {
            if interval_contains(valid_from, valid_to, valid_at) {
                rows.push((entity, decode_value(encoded)?));
            }
            Ok(())
        })?;
        Ok(rows)
    }

    pub(crate) fn integer_count_sum(&self) -> Result<(u64, i128)> {
        self.integer_count_sum_at(self.valid_time_floor)
    }

    pub(crate) fn integer_count_sum_at(&self, valid_at: i64) -> Result<(u64, i128)> {
        self.ensure_valid_at(valid_at)?;
        let mut count = 0_u64;
        let mut sum = 0_i128;
        self.visit_merged_encoded(&mut |_, encoded, valid_from, valid_to| {
            if interval_contains(valid_from, valid_to, valid_at) {
                let value = decode_integer(encoded)?;
                count = count.saturating_add(1);
                sum = sum.saturating_add(i128::from(value));
            }
            Ok(())
        })?;
        Ok((count, sum))
    }

    fn ensure_valid_at(&self, valid_at: i64) -> Result<()> {
        if valid_at < self.valid_time_floor {
            bail!("current projection cannot answer valid time before its floor")
        }
        Ok(())
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

    pub(crate) fn visit_merged_encoded(&self, visit: &mut IntervalVisitor<'_>) -> Result<()> {
        let mut valid_from = TemporalColumnDecoder::new(&self.valid_from_bytes);
        let mut valid_to = TemporalColumnDecoder::new(&self.valid_to_bytes);
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

            let replaced = overlay.is_some_and(|(overlay_entity, _)| *overlay_entity == entity);
            for index in base..base_end {
                let from = valid_from.next()?;
                let to = valid_to.next()?;
                if !replaced {
                    visit(entity, self.base_value(index)?, from, to)?;
                }
            }
            if replaced {
                if let Some((_, entry)) = overlay {
                    visit_overlay(entity, entry, visit)?;
                }
                overlay = overlay_iter.next();
            }
            base = base_end;
        }

        valid_from.finish(self.entities.len())?;
        valid_to.finish(self.entities.len())?;
        while let Some((entity, entry)) = overlay {
            visit_overlay(*entity, entry, visit)?;
            overlay = overlay_iter.next();
        }
        Ok(())
    }

    pub(crate) fn from_encoded_columns(columns: EncodedProjectionColumns) -> Result<Self> {
        let EncodedProjectionColumns {
            attribute,
            valid_time_floor,
            publication_generation,
            tx_count,
            entities,
            value_offsets,
            value_bytes,
            valid_from_bytes,
            valid_to_bytes,
        } = columns;
        if !entities
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left <= right))
        {
            bail!("current projection entities are not sorted")
        }
        if value_offsets.len() != entities.len().saturating_add(1) {
            bail!("current projection value offset count does not match rows")
        }
        if value_offsets.first().copied() != Some(0) {
            bail!("current projection value offsets must start at zero")
        }
        if !value_offsets
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left <= right))
        {
            bail!("current projection value offsets are not monotonic")
        }
        if value_offsets
            .last()
            .copied()
            .map(usize::try_from)
            .transpose()?
            != Some(value_bytes.len())
        {
            bail!("current projection final value offset does not match payload")
        }
        for range in value_offsets.windows(2) {
            let [start, end] = range else {
                continue;
            };
            let start = usize::try_from(*start)?;
            let end = usize::try_from(*end)?;
            validate_encoded_value(
                value_bytes
                    .get(start..end)
                    .ok_or_else(|| anyhow!("current projection value range is corrupt"))?,
            )?;
        }

        validate_temporal_columns(
            &valid_from_bytes,
            &valid_to_bytes,
            entities.len(),
            valid_time_floor,
        )?;

        Ok(Self {
            attribute,
            valid_time_floor,
            publication_generation,
            tx_count,
            entities,
            value_offsets,
            value_bytes,
            valid_from_bytes,
            valid_to_bytes,
            overlay: BTreeMap::new(),
        })
    }
}

fn visit_overlay(
    entity: EntityId,
    entry: &OverlayEntry,
    visit: &mut IntervalVisitor<'_>,
) -> Result<()> {
    for interval in &entry.intervals {
        visit(
            entity,
            &interval.value,
            interval.valid_from,
            interval.valid_to,
        )?;
    }
    Ok(())
}

pub(crate) struct CurrentProjectionBuilder {
    attribute: String,
    valid_time_floor: i64,
    publication_generation: u64,
    tx_count: u64,
    entities: Vec<EntityId>,
    value_offsets: Vec<u32>,
    value_bytes: Vec<u8>,
    valid_from: TemporalColumnBuilder,
    valid_to: TemporalColumnBuilder,
    current_entity: Option<EntityId>,
    current_intervals: Vec<ProjectedInterval>,
}

impl CurrentProjectionBuilder {
    pub(crate) fn new(
        attribute: &str,
        valid_time_floor: i64,
        publication_generation: u64,
        tx_count: u64,
    ) -> Self {
        Self {
            attribute: attribute.to_owned(),
            valid_time_floor,
            publication_generation,
            tx_count,
            entities: Vec::new(),
            value_offsets: Vec::new(),
            value_bytes: Vec::new(),
            valid_from: TemporalColumnBuilder::default(),
            valid_to: TemporalColumnBuilder::default(),
            current_entity: None,
            current_intervals: Vec::new(),
        }
    }

    pub(crate) fn push(
        &mut self,
        entity: EntityId,
        value: &Value,
        valid_from: i64,
        valid_to: i64,
    ) -> Result<()> {
        if valid_to <= self.valid_time_floor {
            return Ok(());
        }
        if self.current_entity.is_some_and(|current| current != entity) {
            self.flush_entity()?;
        }
        self.current_entity = Some(entity);
        self.current_intervals
            .push(ProjectedInterval::new(value, valid_from, valid_to));
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
            valid_time_floor: self.valid_time_floor,
            publication_generation: self.publication_generation,
            tx_count: self.tx_count,
            entities: self.entities,
            value_offsets: self.value_offsets,
            value_bytes: self.value_bytes,
            valid_from_bytes: self.valid_from.bytes,
            valid_to_bytes: self.valid_to.bytes,
            overlay: BTreeMap::new(),
        })
    }

    fn flush_entity(&mut self) -> Result<()> {
        let Some(entity) = self.current_entity.take() else {
            return Ok(());
        };
        self.current_intervals.sort_unstable_by(|left, right| {
            left.value
                .cmp(&right.value)
                .then_with(|| left.valid_from.cmp(&right.valid_from))
                .then_with(|| left.valid_to.cmp(&right.valid_to))
        });
        for interval in self.current_intervals.drain(..) {
            self.value_offsets
                .push(u32::try_from(self.value_bytes.len()).map_err(|_| {
                    anyhow!("current projection candidate exceeds the 4 GiB feasibility limit")
                })?);
            self.entities.push(entity);
            self.value_bytes.extend_from_slice(&interval.value);
            self.valid_from.push(interval.valid_from);
            self.valid_to.push(interval.valid_to);
        }
        Ok(())
    }
}

fn interval_contains(valid_from: i64, valid_to: i64, valid_at: i64) -> bool {
    valid_from <= valid_at && valid_at < valid_to
}

fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value.to_le_bytes()[0] & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value.to_le_bytes()[0]);
}

fn varint_len(mut value: u64) -> u8 {
    let mut bytes = 1_u8;
    while value >= 0x80 {
        value >>= 7;
        bytes = bytes.saturating_add(1);
    }
    bytes
}

fn decode_varint(bytes: &[u8], position: &mut usize) -> Result<u64> {
    let mut value = 0_u64;
    for index in 0..10_u32 {
        let byte = *bytes
            .get(*position)
            .ok_or_else(|| anyhow!("truncated current projection temporal varint"))?;
        *position = position.saturating_add(1);
        let payload = u64::from(byte & 0x7f);
        if index == 9 && payload > 1 {
            bail!("current projection temporal varint exceeds u64")
        }
        value |= payload << (index * 7);
        if byte & 0x80 == 0 {
            if index > 0 && payload == 0 {
                bail!("current projection temporal varint is overlong")
            }
            return Ok(value);
        }
    }
    bail!("current projection temporal varint is overlong")
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

pub(crate) fn decode_value(encoded: &[u8]) -> Result<Value> {
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

pub(crate) fn validate_encoded_value(encoded: &[u8]) -> Result<()> {
    let payload = encoded
        .get(1..)
        .ok_or_else(|| anyhow!("empty current projection value"))?;
    match encoded.first().copied() {
        Some(0x00) if payload.is_empty() => Ok(()),
        Some(0x01) if payload == [0] || payload == [1] => Ok(()),
        Some(0x02 | 0x03) if payload.len() == 8 => Ok(()),
        Some(0x04 | 0x05) => {
            std::str::from_utf8(payload)?;
            Ok(())
        }
        Some(0x06) if payload.len() == 16 => Ok(()),
        Some(tag) => bail!("malformed current projection value tag 0x{tag:02x}"),
        None => bail!("empty current projection value"),
    }
}

pub(crate) fn validate_temporal_columns(
    valid_from_bytes: &[u8],
    valid_to_bytes: &[u8],
    rows: usize,
    valid_time_floor: i64,
) -> Result<()> {
    let mut valid_from = TemporalColumnDecoder::new(valid_from_bytes);
    let mut valid_to = TemporalColumnDecoder::new(valid_to_bytes);
    for _ in 0..rows {
        let from = valid_from.next()?;
        let to = valid_to.next()?;
        if from >= to {
            bail!("current projection interval is empty or inverted")
        }
        if to <= valid_time_floor {
            bail!("current projection interval ends at or before its floor")
        }
    }
    valid_from.finish(rows)?;
    valid_to.finish(rows)?;
    Ok(())
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
    use super::{
        CurrentProjectionBuilder, TemporalColumnBuilder, TemporalColumnDecoder, decode_value,
        decode_varint,
    };
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
    fn temporal_column_round_trips_repeated_and_extreme_times() {
        let times = [i64::MIN, i64::MIN, -1, 0, 1, i64::MAX, 42, 42];
        let mut builder = TemporalColumnBuilder::default();
        for time in times {
            builder.push(time);
        }
        let mut decoder = TemporalColumnDecoder::new(&builder.bytes);
        for expected in times {
            assert_eq!(decoder.next().unwrap(), expected);
        }
        decoder.finish(times.len()).unwrap();
    }

    #[test]
    fn temporal_varint_rejects_truncation_overlong_and_overflow() {
        let mut position = 0;
        assert!(decode_varint(&[0x80], &mut position).is_err());
        position = 0;
        assert!(decode_varint(&[0x80, 0x00], &mut position).is_err());
        position = 0;
        assert!(
            decode_varint(
                &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],
                &mut position,
            )
            .is_err()
        );
        assert!(TemporalColumnDecoder::new(&[8, 0]).next().is_err());
        assert!(TemporalColumnDecoder::new(&[0]).next().is_err());
    }

    #[test]
    fn compact_base_and_overlay_filter_time_deterministically() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let mut builder = CurrentProjectionBuilder::new(":v", 100, 4, 3);
        builder.push(first, &Value::Integer(2), 50, 150).unwrap();
        builder.push(first, &Value::Integer(1), 100, 200).unwrap();
        builder.push(second, &Value::Integer(3), 101, 300).unwrap();
        let mut candidate = builder.finish().unwrap();
        let original = candidate.fingerprint().unwrap();
        assert_eq!(candidate.integer_count_sum_at(100).unwrap(), (2, 3));
        assert_eq!(candidate.integer_count_sum_at(101).unwrap(), (3, 6));
        assert!(candidate.integer_count_sum_at(99).is_err());

        candidate.replace_entity(
            first,
            vec![super::ProjectedInterval::new(&Value::Integer(7), 150, 250)],
        );
        assert_eq!(candidate.integer_count_sum_at(149).unwrap(), (1, 3));
        assert_eq!(candidate.integer_count_sum_at(150).unwrap(), (2, 10));
        assert_ne!(candidate.fingerprint().unwrap(), original);
        assert_eq!(candidate.row_count(), 2);
    }
}
