// Pending IDs and run cursors are constructed exclusively from these owned
// vectors; indexing expresses that internal invariant without adding a branch
// to every comparison in the hot merge path.
#![allow(clippy::indexing_slicing)]

use crate::graph::types::{Fact, Value};
use crate::storage::index::{AevtKey, EavtKey, encode_value};
use anyhow::Result;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Stable in-memory identity for one pending fact.
///
/// This is deliberately distinct from on-disk `FactRef`: packed-page slots are
/// `u16`, while a WAL-backed pending overlay may contain millions of facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PendingFactId(u32);

impl PendingFactId {
    fn from_index(index: usize) -> Result<Self> {
        Ok(Self(u32::try_from(index).map_err(|_| {
            anyhow::anyhow!("pending overlay exceeds u32 fact identity space")
        })?))
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug)]
pub(crate) struct PendingFactRecord {
    pub(crate) entity: uuid::Uuid,
    pub(crate) attribute: Arc<str>,
    pub(crate) value: Value,
    pub(crate) value_bytes: Box<[u8]>,
    pub(crate) tx_id: u64,
    pub(crate) tx_count: u64,
    pub(crate) valid_from: i64,
    pub(crate) valid_to: i64,
    pub(crate) asserted: bool,
}

impl PendingFactRecord {
    fn from_fact(fact: Fact, attribute: Arc<str>) -> Self {
        let value_bytes = encode_value(&fact.value).into_boxed_slice();
        Self {
            entity: fact.entity,
            attribute,
            value: fact.value,
            value_bytes,
            tx_id: fact.tx_id,
            tx_count: fact.tx_count,
            valid_from: fact.valid_from,
            valid_to: fact.valid_to,
            asserted: fact.asserted,
        }
    }

    pub(crate) fn to_fact(&self) -> Fact {
        Fact {
            entity: self.entity,
            attribute: self.attribute.to_string(),
            value: self.value.clone(),
            tx_id: self.tx_id,
            tx_count: self.tx_count,
            valid_from: self.valid_from,
            valid_to: self.valid_to,
            asserted: self.asserted,
        }
    }

    pub(crate) fn to_aevt_key(&self) -> AevtKey {
        AevtKey {
            attribute: self.attribute.to_string(),
            entity: self.entity,
            valid_from: self.valid_from,
            valid_to: self.valid_to,
            tx_count: self.tx_count,
            value_bytes: self.value_bytes.to_vec(),
            tx_id: self.tx_id,
            asserted: self.asserted,
        }
    }

    fn equals_fact(&self, fact: &Fact, encoded: &[u8]) -> bool {
        self.entity == fact.entity
            && self.attribute.as_ref() == fact.attribute
            && self.value_bytes.as_ref() == encoded
            && self.valid_from == fact.valid_from
            && self.valid_to == fact.valid_to
            && self.tx_count == fact.tx_count
            && self.tx_id == fact.tx_id
            && self.asserted == fact.asserted
    }
}

#[derive(Default)]
enum DuplicateBucket {
    #[default]
    Empty,
    One(PendingFactId),
    Many(Vec<PendingFactId>),
}

#[derive(Clone, Copy)]
enum IndexOrder {
    Eavt,
    Aevt,
    Avet,
    Vaet,
}

#[derive(Default)]
struct SortedRuns {
    levels: Vec<Option<Vec<PendingFactId>>>,
    len: usize,
    merge_count: u64,
}

impl SortedRuns {
    fn clear(&mut self) {
        self.levels.clear();
        self.len = 0;
        self.merge_count = 0;
    }

    fn insert(
        &mut self,
        mut run: Vec<PendingFactId>,
        records: &[PendingFactRecord],
        order: IndexOrder,
    ) {
        if run.is_empty() {
            return;
        }
        run.sort_unstable_by(|left, right| compare_ids(records, order, *left, *right));
        self.len = self.len.saturating_add(run.len());
        let mut level = run.len().next_power_of_two().trailing_zeros() as usize;
        loop {
            if self.levels.len() <= level {
                self.levels.resize_with(level + 1, || None);
            }
            match self.levels[level].take() {
                None => {
                    self.levels[level] = Some(run);
                    return;
                }
                Some(existing) => {
                    run = merge_runs(existing, run, records, order);
                    self.merge_count = self.merge_count.saturating_add(1);
                    level = level.saturating_add(1);
                }
            }
        }
    }

    fn range(
        &self,
        records: &[PendingFactRecord],
        order: IndexOrder,
        lower: impl Fn(&PendingFactRecord) -> Ordering,
        upper: impl Fn(&PendingFactRecord) -> Ordering,
    ) -> Vec<PendingFactId> {
        struct Cursor<'a> {
            run: &'a [PendingFactId],
            position: usize,
            end: usize,
        }

        let mut cursors = self
            .levels
            .iter()
            .filter_map(Option::as_deref)
            .filter_map(|run| {
                let start = run.partition_point(|id| lower(&records[id.index()]).is_lt());
                let end = run.partition_point(|id| upper(&records[id.index()]).is_lt());
                (start < end).then_some(Cursor {
                    run,
                    position: start,
                    end,
                })
            })
            .collect::<Vec<_>>();
        let capacity = cursors.iter().fold(0_usize, |total, cursor| {
            total.saturating_add(cursor.end.saturating_sub(cursor.position))
        });
        let mut result = Vec::with_capacity(capacity);
        while !cursors.is_empty() {
            let mut selected = 0usize;
            for index in 1..cursors.len() {
                let candidate = cursors[index].run[cursors[index].position];
                let current = cursors[selected].run[cursors[selected].position];
                if compare_ids(records, order, candidate, current).is_lt() {
                    selected = index;
                }
            }
            let cursor = &mut cursors[selected];
            result.push(cursor.run[cursor.position]);
            cursor.position = cursor.position.saturating_add(1);
            if cursor.position == cursor.end {
                cursors.swap_remove(selected);
            }
        }
        result
    }

    fn run_count(&self) -> usize {
        self.levels.iter().filter(|run| run.is_some()).count()
    }
}

#[derive(Default)]
struct PendingIndexes {
    eavt: SortedRuns,
    aevt: SortedRuns,
    avet: SortedRuns,
    vaet: SortedRuns,
}

/// Canonical append-only pending fact owner plus lightweight sorted index runs.
#[derive(Default)]
pub(crate) struct PendingOverlay {
    records: Vec<PendingFactRecord>,
    attributes: HashSet<Arc<str>>,
    duplicates: HashMap<u64, DuplicateBucket>,
    indexes: PendingIndexes,
}

impl PendingOverlay {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.records.clear();
        self.attributes.clear();
        self.duplicates.clear();
        self.indexes.eavt.clear();
        self.indexes.aevt.clear();
        self.indexes.avet.clear();
        self.indexes.vaet.clear();
    }

    pub(crate) fn insert_batch(
        &mut self,
        facts: Vec<Fact>,
        reject_duplicates: bool,
    ) -> Result<usize> {
        let mut inserted = Vec::with_capacity(facts.len());
        for fact in facts {
            if self.find_duplicate(&fact).is_some() && reject_duplicates {
                continue;
            }
            let attribute = if let Some(existing) = self.attributes.get(fact.attribute.as_str()) {
                existing.clone()
            } else {
                let attribute: Arc<str> = Arc::from(fact.attribute.as_str());
                self.attributes.insert(attribute.clone());
                attribute
            };
            let hash = identity_hash(&fact);
            let id = PendingFactId::from_index(self.records.len())?;
            self.records
                .push(PendingFactRecord::from_fact(fact, attribute));
            self.add_duplicate(hash, id);
            inserted.push(id);
        }
        if inserted.is_empty() {
            return Ok(0);
        }
        self.indexes
            .eavt
            .insert(inserted.clone(), &self.records, IndexOrder::Eavt);
        self.indexes
            .aevt
            .insert(inserted.clone(), &self.records, IndexOrder::Aevt);
        self.indexes
            .avet
            .insert(inserted.clone(), &self.records, IndexOrder::Avet);
        let refs = inserted
            .iter()
            .copied()
            .filter(|id| matches!(self.records[id.index()].value, Value::Ref(_)))
            .collect();
        self.indexes
            .vaet
            .insert(refs, &self.records, IndexOrder::Vaet);
        Ok(inserted.len())
    }

    pub(crate) fn get(&self, id: PendingFactId) -> Result<&PendingFactRecord> {
        self.records
            .get(id.index())
            .ok_or_else(|| anyhow::anyhow!("pending fact id {} out of bounds", id.index()))
    }

    pub(crate) fn facts(&self) -> impl Iterator<Item = Fact> + '_ {
        self.records.iter().map(PendingFactRecord::to_fact)
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = &PendingFactRecord> {
        self.records.iter()
    }

    pub(crate) fn range_eavt(&self, start: &EavtKey, end: &EavtKey) -> Vec<PendingFactId> {
        self.indexes.eavt.range(
            &self.records,
            IndexOrder::Eavt,
            |record| compare_eavt_bound(record, start),
            |record| compare_eavt_bound(record, end),
        )
    }

    pub(crate) fn range_aevt(&self, start: &AevtKey, end: Option<&AevtKey>) -> Vec<PendingFactId> {
        self.indexes.aevt.range(
            &self.records,
            IndexOrder::Aevt,
            |record| compare_aevt_bound(record, start),
            |record| end.map_or(Ordering::Less, |end| compare_aevt_bound(record, end)),
        )
    }

    pub(crate) fn index_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.indexes.eavt.len,
            self.indexes.aevt.len,
            self.indexes.avet.len,
            self.indexes.vaet.len,
        )
    }

    pub(crate) fn compare_aevt_key(&self, id: PendingFactId, key: &AevtKey) -> Result<Ordering> {
        Ok(compare_aevt_bound(self.get(id)?, key))
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn memory_shape(&self) -> PendingOverlayMemoryShape {
        let attribute_bytes = self.attributes.iter().map(|value| value.len()).sum();
        let value_owned = |record: &PendingFactRecord| match &record.value {
            Value::String(value) | Value::Keyword(value) => value.capacity(),
            _ => 0,
        };
        let value_bytes = self
            .records
            .iter()
            .map(|record| record.value_bytes.len().saturating_add(value_owned(record)))
            .sum();
        let duplicate_ids = self
            .duplicates
            .values()
            .map(|bucket| match bucket {
                DuplicateBucket::Empty => 0,
                DuplicateBucket::One(_) => 1,
                DuplicateBucket::Many(ids) => ids.capacity(),
            })
            .sum();
        let run_shape = |runs: &SortedRuns| {
            let capacity = runs
                .levels
                .iter()
                .filter_map(Option::as_ref)
                .map(Vec::capacity)
                .sum();
            (runs.len, capacity, runs.run_count())
        };
        PendingOverlayMemoryShape {
            records_len: self.records.len(),
            records_capacity: self.records.capacity(),
            attribute_bytes,
            attribute_allocations: self.attributes.len(),
            value_bytes,
            value_allocations: self
                .records
                .iter()
                .map(|record| {
                    usize::from(!record.value_bytes.is_empty())
                        .saturating_add(usize::from(value_owned(record) > 0))
                })
                .sum(),
            duplicate_buckets: self.duplicates.len(),
            duplicate_capacity: self.duplicates.capacity(),
            duplicate_ids,
            eavt: run_shape(&self.indexes.eavt),
            aevt: run_shape(&self.indexes.aevt),
            avet: run_shape(&self.indexes.avet),
            vaet: run_shape(&self.indexes.vaet),
        }
    }

    fn find_duplicate(&self, fact: &Fact) -> Option<PendingFactId> {
        let encoded = encode_value(&fact.value);
        match self.duplicates.get(&identity_hash(fact))? {
            DuplicateBucket::Empty => None,
            DuplicateBucket::One(id) => self.records[id.index()]
                .equals_fact(fact, &encoded)
                .then_some(*id),
            DuplicateBucket::Many(ids) => ids
                .iter()
                .copied()
                .find(|id| self.records[id.index()].equals_fact(fact, &encoded)),
        }
    }

    fn add_duplicate(&mut self, hash: u64, id: PendingFactId) {
        use std::collections::hash_map::Entry;
        match self.duplicates.entry(hash) {
            Entry::Vacant(entry) => {
                entry.insert(DuplicateBucket::One(id));
            }
            Entry::Occupied(mut entry) => match entry.get_mut() {
                DuplicateBucket::Empty => *entry.get_mut() = DuplicateBucket::One(id),
                DuplicateBucket::One(existing) => {
                    *entry.get_mut() = DuplicateBucket::Many(vec![*existing, id]);
                }
                DuplicateBucket::Many(ids) => ids.push(id),
            },
        }
    }
}

#[cfg(any(test, feature = "bench-internals"))]
pub(crate) struct PendingOverlayMemoryShape {
    pub(crate) records_len: usize,
    pub(crate) records_capacity: usize,
    pub(crate) attribute_bytes: usize,
    pub(crate) attribute_allocations: usize,
    pub(crate) value_bytes: usize,
    pub(crate) value_allocations: usize,
    pub(crate) duplicate_buckets: usize,
    pub(crate) duplicate_capacity: usize,
    pub(crate) duplicate_ids: usize,
    pub(crate) eavt: (usize, usize, usize),
    pub(crate) aevt: (usize, usize, usize),
    pub(crate) avet: (usize, usize, usize),
    pub(crate) vaet: (usize, usize, usize),
}

fn identity_hash(fact: &Fact) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fact.entity.hash(&mut hasher);
    fact.attribute.hash(&mut hasher);
    encode_value(&fact.value).hash(&mut hasher);
    fact.valid_from.hash(&mut hasher);
    fact.valid_to.hash(&mut hasher);
    fact.tx_count.hash(&mut hasher);
    fact.tx_id.hash(&mut hasher);
    fact.asserted.hash(&mut hasher);
    hasher.finish()
}

fn merge_runs(
    left: Vec<PendingFactId>,
    right: Vec<PendingFactId>,
    records: &[PendingFactRecord],
    order: IndexOrder,
) -> Vec<PendingFactId> {
    let mut merged = Vec::with_capacity(left.len().saturating_add(right.len()));
    let mut left_pos = 0usize;
    let mut right_pos = 0usize;
    while left_pos < left.len() && right_pos < right.len() {
        if compare_ids(records, order, left[left_pos], right[right_pos]).is_le() {
            merged.push(left[left_pos]);
            left_pos = left_pos.saturating_add(1);
        } else {
            merged.push(right[right_pos]);
            right_pos = right_pos.saturating_add(1);
        }
    }
    merged.extend_from_slice(&left[left_pos..]);
    merged.extend_from_slice(&right[right_pos..]);
    merged
}

fn compare_ids(
    records: &[PendingFactRecord],
    order: IndexOrder,
    left: PendingFactId,
    right: PendingFactId,
) -> Ordering {
    let left_record = &records[left.index()];
    let right_record = &records[right.index()];
    compare_records(left_record, right_record, order).then_with(|| left.cmp(&right))
}

fn compare_records(
    left: &PendingFactRecord,
    right: &PendingFactRecord,
    order: IndexOrder,
) -> Ordering {
    match order {
        IndexOrder::Eavt => left
            .entity
            .cmp(&right.entity)
            .then_with(|| left.attribute.cmp(&right.attribute))
            .then_with(|| left.valid_from.cmp(&right.valid_from))
            .then_with(|| left.valid_to.cmp(&right.valid_to))
            .then_with(|| left.tx_count.cmp(&right.tx_count))
            .then_with(|| left.value_bytes.cmp(&right.value_bytes))
            .then_with(|| left.tx_id.cmp(&right.tx_id))
            .then_with(|| left.asserted.cmp(&right.asserted)),
        IndexOrder::Aevt => left
            .attribute
            .cmp(&right.attribute)
            .then_with(|| left.entity.cmp(&right.entity))
            .then_with(|| left.valid_from.cmp(&right.valid_from))
            .then_with(|| left.valid_to.cmp(&right.valid_to))
            .then_with(|| left.tx_count.cmp(&right.tx_count))
            .then_with(|| left.value_bytes.cmp(&right.value_bytes))
            .then_with(|| left.tx_id.cmp(&right.tx_id))
            .then_with(|| left.asserted.cmp(&right.asserted)),
        IndexOrder::Avet => left
            .attribute
            .cmp(&right.attribute)
            .then_with(|| left.value_bytes.cmp(&right.value_bytes))
            .then_with(|| left.valid_from.cmp(&right.valid_from))
            .then_with(|| left.valid_to.cmp(&right.valid_to))
            .then_with(|| left.entity.cmp(&right.entity))
            .then_with(|| left.tx_count.cmp(&right.tx_count))
            .then_with(|| left.tx_id.cmp(&right.tx_id))
            .then_with(|| left.asserted.cmp(&right.asserted)),
        IndexOrder::Vaet => ref_target(left)
            .cmp(&ref_target(right))
            .then_with(|| left.attribute.cmp(&right.attribute))
            .then_with(|| left.valid_from.cmp(&right.valid_from))
            .then_with(|| left.valid_to.cmp(&right.valid_to))
            .then_with(|| left.entity.cmp(&right.entity))
            .then_with(|| left.tx_count.cmp(&right.tx_count))
            .then_with(|| left.tx_id.cmp(&right.tx_id))
            .then_with(|| left.asserted.cmp(&right.asserted)),
    }
}

fn compare_eavt_bound(record: &PendingFactRecord, key: &EavtKey) -> Ordering {
    record
        .entity
        .cmp(&key.entity)
        .then_with(|| record.attribute.as_ref().cmp(&key.attribute))
        .then_with(|| record.valid_from.cmp(&key.valid_from))
        .then_with(|| record.valid_to.cmp(&key.valid_to))
        .then_with(|| record.tx_count.cmp(&key.tx_count))
        .then_with(|| record.value_bytes.as_ref().cmp(&key.value_bytes))
        .then_with(|| record.tx_id.cmp(&key.tx_id))
        .then_with(|| record.asserted.cmp(&key.asserted))
}

fn compare_aevt_bound(record: &PendingFactRecord, key: &AevtKey) -> Ordering {
    record
        .attribute
        .as_ref()
        .cmp(&key.attribute)
        .then_with(|| record.entity.cmp(&key.entity))
        .then_with(|| record.valid_from.cmp(&key.valid_from))
        .then_with(|| record.valid_to.cmp(&key.valid_to))
        .then_with(|| record.tx_count.cmp(&key.tx_count))
        .then_with(|| record.value_bytes.as_ref().cmp(&key.value_bytes))
        .then_with(|| record.tx_id.cmp(&key.tx_id))
        .then_with(|| record.asserted.cmp(&key.asserted))
}

fn ref_target(record: &PendingFactRecord) -> uuid::Uuid {
    match record.value {
        Value::Ref(target) => target,
        _ => uuid::Uuid::nil(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::VALID_TIME_FOREVER;

    fn fact(entity: u128, attribute: &str, value: Value, tx_count: u64) -> Fact {
        Fact::with_valid_time(
            uuid::Uuid::from_u128(entity),
            attribute.to_string(),
            value,
            tx_count,
            tx_count,
            0,
            VALID_TIME_FOREVER,
        )
    }

    #[test]
    fn tiered_runs_preserve_eavt_and_aevt_order() {
        let mut overlay = PendingOverlay::new();
        for batch in 0..64_u64 {
            overlay
                .insert_batch(
                    vec![
                        fact(2, ":b", Value::Integer(batch as i64), batch),
                        fact(1, ":a", Value::Integer(batch as i64), batch),
                    ],
                    false,
                )
                .unwrap();
        }
        let eavt = overlay.range_eavt(
            &EavtKey {
                entity: uuid::Uuid::from_u128(1),
                attribute: String::new(),
                valid_from: i64::MIN,
                valid_to: i64::MIN,
                tx_count: 0,
                value_bytes: Vec::new(),
                tx_id: 0,
                asserted: false,
            },
            &EavtKey {
                entity: uuid::Uuid::from_u128(2),
                attribute: String::new(),
                valid_from: i64::MIN,
                valid_to: i64::MIN,
                tx_count: 0,
                value_bytes: Vec::new(),
                tx_id: 0,
                asserted: false,
            },
        );
        assert_eq!(eavt.len(), 64);
        assert!(eavt.windows(2).all(|pair| {
            compare_ids(&overlay.records, IndexOrder::Eavt, pair[0], pair[1]).is_le()
        }));
        assert!(overlay.memory_shape().eavt.2 <= 7);
    }

    #[test]
    fn duplicate_guard_uses_exact_identity() {
        let mut overlay = PendingOverlay::new();
        let value = fact(1, ":a", Value::Integer(1), 1);
        assert_eq!(overlay.insert_batch(vec![value.clone()], true).unwrap(), 1);
        assert_eq!(overlay.insert_batch(vec![value], true).unwrap(), 0);
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay.index_counts(), (1, 1, 1, 0));
    }

    #[test]
    fn pending_identity_exceeds_packed_slot_width() {
        let mut overlay = PendingOverlay::new();
        let facts = (0..70_000_u32)
            .map(|index| fact(index as u128, ":float", Value::Float(index as f64), 1))
            .collect();
        overlay.insert_batch(facts, false).unwrap();
        let id = PendingFactId::from_index(69_999).unwrap();
        assert_eq!(overlay.get(id).unwrap().value, Value::Float(69_999.0));
    }
}
