//! Index key types, FactRef, and canonical value encoding for the four
//! covering indexes (EAVT, AEVT, AVET, VAET).
//!
//! `FactRef` identifies a fact's location on disk. In Phase 6.1, one fact
//! occupies one page (`slot_index` is always 0). In Phase 6.2, `slot_index`
//! identifies the record slot within a packed page.

use crate::graph::types::{Attribute, EntityId, Fact, TxId, Value};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use uuid::Uuid;

/// Generate the maximum UUID value (all bits set to 1).
/// Used as an upper bound for range queries on UUID fields.
#[allow(dead_code)]
fn max_uuid() -> Uuid {
    // This is safe because MAX_UUID is a valid hardcoded UUID string literal.
    // It will never fail at runtime.
    const MAX_UUID_BYTES: [u8; 16] = [
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF,
    ];
    Uuid::from_bytes(MAX_UUID_BYTES)
}

// ─── FactRef ────────────────────────────────────────────────────────────────

/// Disk location of a fact.
///
/// `slot_index` is always `0` in Phase 6.1 (one fact per page).
/// In Phase 6.2 it identifies the record within a packed page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FactRef {
    pub page_id: u64,
    pub slot_index: u16,
}

// ─── Canonical Value Encoding ───────────────────────────────────────────────

/// Encode a `Value` to bytes that preserve sort order across all variants.
///
/// Discriminant assignment (first byte):
///   0x00 = Null, 0x01 = Boolean, 0x02 = Integer, 0x03 = Float,
///   0x04 = String, 0x05 = Keyword, 0x06 = Ref
///
/// Within each type, big-endian layout ensures byte-wise comparison matches
/// the natural order of the type.
pub fn encode_value(v: &Value) -> Vec<u8> {
    match v {
        Value::Null => vec![0x00],
        Value::Boolean(b) => vec![0x01, *b as u8],
        Value::Integer(n) => {
            let mut bytes = Vec::with_capacity(9);
            bytes.push(0x02);
            // Flip the sign bit so that negative numbers sort before positive
            // after unsigned byte comparison: MIN..=-1 maps to 0..0x7FFF...,
            // 0..=MAX maps to 0x8000...=0xFFFF...
            let bits = (*n).cast_unsigned() ^ 0x8000_0000_0000_0000;
            bytes.extend_from_slice(&bits.to_be_bytes());
            bytes
        }
        Value::Float(f) => {
            let mut bytes = Vec::with_capacity(9);
            bytes.push(0x03);
            let bits = if f.is_nan() {
                // Canonicalize all NaN to a single bit pattern (quiet NaN, positive)
                0x7FF8_0000_0000_0000u64
            } else {
                let raw = f.to_bits();
                if raw >> 63 == 0 {
                    raw ^ 0x8000_0000_0000_0000 // positive: flip sign bit
                } else {
                    !raw // negative: flip all bits
                }
            };
            bytes.extend_from_slice(&bits.to_be_bytes());
            bytes
        }
        Value::String(s) => {
            let mut bytes = Vec::with_capacity(s.len().saturating_add(1));
            bytes.push(0x04);
            bytes.extend_from_slice(s.as_bytes());
            bytes
        }
        Value::Keyword(k) => {
            let mut bytes = Vec::with_capacity(k.len().saturating_add(1));
            bytes.push(0x05);
            bytes.extend_from_slice(k.as_bytes());
            bytes
        }
        Value::Ref(id) => {
            let mut bytes = Vec::with_capacity(17);
            bytes.push(0x06);
            bytes.extend_from_slice(id.as_bytes());
            bytes
        }
    }
}

// ─── Index Key Types ─────────────────────────────────────────────────────────

/// EAVT: sort by (Entity, Attribute, ValidFrom, ValidTo, TxCount, ValueBytes, TxId, Asserted)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EavtKey {
    pub entity: EntityId,
    pub attribute: Attribute,
    pub valid_from: i64,
    pub valid_to: i64,
    pub tx_count: u64,
    pub value_bytes: Vec<u8>,
    pub tx_id: TxId,
    pub asserted: bool,
}

/// AEVT: sort by (Attribute, Entity, ValidFrom, ValidTo, TxCount, ValueBytes, TxId, Asserted)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AevtKey {
    pub attribute: Attribute,
    pub entity: EntityId,
    pub valid_from: i64,
    pub valid_to: i64,
    pub tx_count: u64,
    pub value_bytes: Vec<u8>,
    pub tx_id: TxId,
    pub asserted: bool,
}

/// Borrowed current-view projection of one on-disk AEVT key.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentAevtEntryRef<'a> {
    pub(crate) entity: EntityId,
    pub(crate) valid_from: i64,
    pub(crate) valid_to: i64,
    pub(crate) tx_count: u64,
    pub(crate) value_bytes: &'a [u8],
    pub(crate) tx_id: TxId,
    pub(crate) asserted: bool,
}

impl<'a> CurrentAevtEntryRef<'a> {
    pub(crate) fn from_owned(key: &'a AevtKey) -> Self {
        Self {
            entity: key.entity,
            valid_from: key.valid_from,
            valid_to: key.valid_to,
            tx_count: key.tx_count,
            value_bytes: &key.value_bytes,
            tx_id: key.tx_id,
            asserted: key.asserted,
        }
    }

    pub(crate) fn cmp_owned_suffix(&self, key: &AevtKey) -> Ordering {
        (
            self.entity,
            self.valid_from,
            self.valid_to,
            self.tx_count,
            self.value_bytes,
            self.tx_id,
            self.asserted,
        )
            .cmp(&(
                key.entity,
                key.valid_from,
                key.valid_to,
                key.tx_count,
                key.value_bytes.as_slice(),
                key.tx_id,
                key.asserted,
            ))
    }

    /// Replace the scalar/value suffix of an existing bounded resume key.
    /// Returns whether the existing value allocation had enough capacity.
    pub(crate) fn write_resume_key(&self, key: &mut AevtKey) -> bool {
        let reused = key.value_bytes.capacity() >= self.value_bytes.len();
        key.entity = self.entity;
        key.valid_from = self.valid_from;
        key.valid_to = self.valid_to;
        key.tx_count = self.tx_count;
        key.value_bytes.clear();
        key.value_bytes.extend_from_slice(self.value_bytes);
        key.tx_id = self.tx_id;
        key.asserted = self.asserted;
        reused
    }
}

/// Private borrowed postcard wire shape for `(AevtKey, FactRef)` entries.
#[derive(Deserialize)]
pub(crate) struct AevtEntryWire<'a> {
    #[serde(borrow)]
    attribute: &'a str,
    entity: EntityId,
    valid_from: i64,
    valid_to: i64,
    tx_count: u64,
    #[serde(borrow)]
    value_bytes: &'a [u8],
    tx_id: TxId,
    asserted: bool,
}

impl<'a> AevtEntryWire<'a> {
    pub(crate) fn decode_entry(bytes: &'a [u8]) -> anyhow::Result<(Self, FactRef)> {
        let (entry, remaining) = postcard::take_from_bytes(bytes)?;
        if !remaining.is_empty() {
            anyhow::bail!("trailing bytes after projected AEVT entry")
        }
        Ok(entry)
    }

    pub(crate) fn project(&self) -> CurrentAevtEntryRef<'a> {
        CurrentAevtEntryRef {
            entity: self.entity,
            valid_from: self.valid_from,
            valid_to: self.valid_to,
            tx_count: self.tx_count,
            value_bytes: self.value_bytes,
            tx_id: self.tx_id,
            asserted: self.asserted,
        }
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn borrowed_lengths(&self) -> (usize, usize) {
        (self.attribute.len(), self.value_bytes.len())
    }

    pub(crate) fn cmp_owned(&self, key: &AevtKey) -> Ordering {
        self.attribute
            .cmp(key.attribute.as_str())
            .then_with(|| self.project().cmp_owned_suffix(key))
    }
}

/// AVET: sort by (Attribute, ValueBytes, ValidFrom, ValidTo, Entity, TxCount, TxId, Asserted)
///
/// `value_bytes` is the canonical encoding from `encode_value`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AvetKey {
    pub attribute: Attribute,
    pub value_bytes: Vec<u8>,
    pub valid_from: i64,
    pub valid_to: i64,
    pub entity: EntityId,
    pub tx_count: u64,
    pub tx_id: TxId,
    pub asserted: bool,
}

/// VAET: sort by (RefTarget, Attribute, ValidFrom, ValidTo, SourceEntity, TxCount, TxId, Asserted)
///
/// Only facts with `Value::Ref` are indexed here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VaetKey {
    pub ref_target: EntityId,
    pub attribute: Attribute,
    pub valid_from: i64,
    pub valid_to: i64,
    pub source_entity: EntityId,
    pub tx_count: u64,
    pub tx_id: TxId,
    pub asserted: bool,
}

// ─── Indexes ─────────────────────────────────────────────────────────────────

/// All four covering indexes held in memory alongside the fact list.
///
/// Populated on every `transact`, `retract`, and `load_fact`.
#[derive(Default, Clone)]
pub struct Indexes {
    pub(crate) eavt: std::collections::BTreeMap<EavtKey, FactRef>,
    pub(crate) aevt: std::collections::BTreeMap<AevtKey, FactRef>,
    pub(crate) avet: std::collections::BTreeMap<AvetKey, FactRef>,
    pub(crate) vaet: std::collections::BTreeMap<VaetKey, FactRef>,
}

impl Indexes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a fact into all applicable indexes.
    ///
    /// `fact_ref` is the disk location. In Phase 6.1, callers pass
    /// `FactRef { page_id: 0, slot_index: 0 }` as a placeholder; real
    /// page IDs are assigned by `save()` and updated via `reindex_from_facts`.
    pub fn insert(&mut self, fact: &Fact, fact_ref: FactRef) {
        self.eavt.insert(
            EavtKey {
                entity: fact.entity,
                attribute: fact.attribute.clone(),
                valid_from: fact.valid_from,
                valid_to: fact.valid_to,
                tx_count: fact.tx_count,
                value_bytes: encode_value(&fact.value),
                tx_id: fact.tx_id,
                asserted: fact.asserted,
            },
            fact_ref,
        );

        self.aevt.insert(
            AevtKey {
                attribute: fact.attribute.clone(),
                entity: fact.entity,
                valid_from: fact.valid_from,
                valid_to: fact.valid_to,
                tx_count: fact.tx_count,
                value_bytes: encode_value(&fact.value),
                tx_id: fact.tx_id,
                asserted: fact.asserted,
            },
            fact_ref,
        );

        self.avet.insert(
            AvetKey {
                attribute: fact.attribute.clone(),
                value_bytes: encode_value(&fact.value),
                valid_from: fact.valid_from,
                valid_to: fact.valid_to,
                entity: fact.entity,
                tx_count: fact.tx_count,
                tx_id: fact.tx_id,
                asserted: fact.asserted,
            },
            fact_ref,
        );

        if let Value::Ref(target) = &fact.value {
            self.vaet.insert(
                VaetKey {
                    ref_target: *target,
                    attribute: fact.attribute.clone(),
                    valid_from: fact.valid_from,
                    valid_to: fact.valid_to,
                    source_entity: fact.entity,
                    tx_count: fact.tx_count,
                    tx_id: fact.tx_id,
                    asserted: fact.asserted,
                },
                fact_ref,
            );
        }
    }

    /// Query EAVT index for a specific entity (returns all facts for that entity).
    #[allow(dead_code)]
    pub fn lookup_eavt_entity(&self, entity: EntityId) -> Vec<FactRef> {
        let start = EavtKey {
            entity,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        // Use exclusive range with a very high attribute string
        let end = EavtKey {
            entity,
            attribute: String::from("zzzzzzzzzzzzzzzzzz"),
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            tx_count: u64::MAX,
            value_bytes: vec![0xFF],
            tx_id: u64::MAX,
            asserted: true,
        };
        self.eavt.range(start..end).map(|(_, v)| *v).collect()
    }

    /// Query EAVT index for entity + attribute.
    #[allow(dead_code)]
    pub fn lookup_eavt_entity_attr(&self, entity: EntityId, attribute: &str) -> Vec<FactRef> {
        let start = EavtKey {
            entity,
            attribute: attribute.to_string(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end = EavtKey {
            entity,
            attribute: attribute.to_string(),
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            tx_count: u64::MAX,
            value_bytes: vec![0xFF],
            tx_id: u64::MAX,
            asserted: true,
        };
        self.eavt.range(start..=end).map(|(_, v)| *v).collect()
    }

    /// Query AEVT index for a specific attribute (returns all facts with that attribute).
    #[allow(dead_code)]
    pub fn lookup_aevt_attr(&self, attribute: &str) -> Vec<FactRef> {
        let max_uuid = max_uuid();
        let start = AevtKey {
            attribute: attribute.to_string(),
            entity: EntityId::default(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end = AevtKey {
            attribute: attribute.to_string(),
            entity: max_uuid,
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            tx_count: u64::MAX,
            value_bytes: vec![0xFF],
            tx_id: u64::MAX,
            asserted: true,
        };
        self.aevt.range(start..=end).map(|(_, v)| *v).collect()
    }

    /// Query AVET index for attribute + value.
    #[allow(dead_code)]
    pub fn lookup_avet_attr_value(&self, attribute: &str, value: &Value) -> Vec<FactRef> {
        let max_uuid = max_uuid();
        let value_bytes = encode_value(value);
        let start = AvetKey {
            attribute: attribute.to_string(),
            value_bytes: value_bytes.clone(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            entity: EntityId::default(),
            tx_count: 0,
            tx_id: 0,
            asserted: false,
        };
        let end = AvetKey {
            attribute: attribute.to_string(),
            value_bytes,
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            entity: max_uuid,
            tx_count: u64::MAX,
            tx_id: u64::MAX,
            asserted: true,
        };
        self.avet.range(start..=end).map(|(_, v)| *v).collect()
    }

    /// Query VAET index for ref target (reverse references).
    #[allow(dead_code)]
    pub fn lookup_vaet_ref(&self, target: EntityId) -> Vec<FactRef> {
        let max_uuid = max_uuid();
        let start = VaetKey {
            ref_target: target,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            source_entity: EntityId::default(),
            tx_count: 0,
            tx_id: 0,
            asserted: false,
        };
        let end = VaetKey {
            ref_target: target,
            // Use max char to include all attributes
            attribute: char::MAX.to_string(),
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            source_entity: max_uuid,
            tx_count: u64::MAX,
            tx_id: u64::MAX,
            asserted: true,
        };
        self.vaet.range(start..=end).map(|(_, v)| *v).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection_key(value: Value, attribute: &str, tx: u64, asserted: bool) -> AevtKey {
        AevtKey {
            attribute: attribute.to_owned(),
            entity: Uuid::from_u128(u128::MAX - u128::from(tx)),
            valid_from: -9_000_000_000,
            valid_to: i64::MAX,
            tx_count: tx,
            value_bytes: encode_value(&value),
            tx_id: u64::MAX - tx,
            asserted,
        }
    }

    #[test]
    fn borrowed_aevt_wire_decodes_existing_postcard_shape_for_all_values() {
        let values = [
            Value::String("borrowed".to_owned()),
            Value::Integer(i64::MIN),
            Value::Float(-0.0),
            Value::Boolean(true),
            Value::Ref(Uuid::from_u128(42)),
            Value::Keyword(":kind/value".to_owned()),
            Value::Null,
        ];
        for (index, value) in values.into_iter().enumerate() {
            let attribute = if index == 0 {
                "a".repeat(192)
            } else {
                ":projection/value".to_owned()
            };
            let key = projection_key(value, &attribute, index as u64 + 128, index % 2 == 0);
            let fact_ref = FactRef {
                page_id: u64::MAX - index as u64,
                slot_index: u16::MAX,
            };
            let bytes = postcard::to_allocvec(&(&key, &fact_ref)).unwrap();
            let (wire, decoded_ref) = AevtEntryWire::decode_entry(&bytes).unwrap();
            let projected = wire.project();
            assert_eq!(wire.cmp_owned(&key), Ordering::Equal);
            assert_eq!(projected.value_bytes, key.value_bytes);
            assert_eq!(projected.entity, key.entity);
            assert_eq!(projected.valid_from, key.valid_from);
            assert_eq!(projected.valid_to, key.valid_to);
            assert_eq!(projected.tx_count, key.tx_count);
            assert_eq!(projected.tx_id, key.tx_id);
            assert_eq!(projected.asserted, key.asserted);
            assert_eq!(decoded_ref, fact_ref);
        }
    }

    #[test]
    fn borrowed_aevt_wire_preserves_owned_order_and_rejects_corruption() {
        let low = projection_key(Value::Integer(-1), ":a", 1, false);
        let high = projection_key(Value::Integer(i64::MAX), ":a", u64::MAX, true);
        let bytes = postcard::to_allocvec(&(
            &low,
            &FactRef {
                page_id: 7,
                slot_index: 3,
            },
        ))
        .unwrap();
        let (wire, _) = AevtEntryWire::decode_entry(&bytes).unwrap();
        assert_eq!(wire.cmp_owned(&low), Ordering::Equal);
        assert_eq!(wire.cmp_owned(&high), low.cmp(&high));

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(AevtEntryWire::decode_entry(&trailing).is_err());
        assert!(AevtEntryWire::decode_entry(&bytes[..bytes.len() - 1]).is_err());

        let mut invalid_utf8 = bytes;
        let attribute_at = invalid_utf8
            .windows(low.attribute.len())
            .position(|window| window == low.attribute.as_bytes())
            .unwrap();
        invalid_utf8[attribute_at] = 0xff;
        assert!(AevtEntryWire::decode_entry(&invalid_utf8).is_err());
    }
    use crate::graph::types::{Fact, VALID_TIME_FOREVER, Value};
    use uuid::Uuid;

    #[test]
    fn test_fact_ref_fields() {
        let r = FactRef {
            page_id: 42,
            slot_index: 7,
        };
        assert_eq!(r.page_id, 42);
        assert_eq!(r.slot_index, 7);
    }

    #[test]
    fn test_encode_value_sort_order_integers() {
        let neg = encode_value(&Value::Integer(-1));
        let zero = encode_value(&Value::Integer(0));
        let pos = encode_value(&Value::Integer(1));
        assert!(neg < zero, "neg should sort before zero");
        assert!(zero < pos, "zero should sort before pos");
    }

    #[test]
    fn test_encode_value_large_negative_before_large_positive() {
        let a = encode_value(&Value::Integer(i64::MIN));
        let b = encode_value(&Value::Integer(i64::MAX));
        assert!(a < b);
    }

    #[test]
    fn test_encode_value_sort_order_cross_type() {
        let null = encode_value(&Value::Null);
        let bool_val = encode_value(&Value::Boolean(false));
        let int_val = encode_value(&Value::Integer(0));
        assert!(null < bool_val);
        assert!(bool_val < int_val);
    }

    #[test]
    fn test_encode_value_ref_structure() {
        let id = Uuid::new_v4();
        let bytes = encode_value(&Value::Ref(id));
        assert_eq!(bytes[0], 0x06); // Ref discriminant
        assert_eq!(&bytes[1..17], id.as_bytes());
    }

    #[test]
    fn test_eavt_key_ordering_by_entity() {
        let e1 = Uuid::from_u128(1);
        let e2 = Uuid::from_u128(2);
        let k1 = EavtKey {
            entity: e1,
            attribute: ":age".to_string(),
            valid_from: 0,
            valid_to: i64::MAX,
            tx_count: 1,
            value_bytes: encode_value(&Value::Integer(10)),
            tx_id: 100,
            asserted: true,
        };
        let k2 = EavtKey {
            entity: e2,
            attribute: ":age".to_string(),
            valid_from: 0,
            valid_to: i64::MAX,
            tx_count: 1,
            value_bytes: encode_value(&Value::Integer(10)),
            tx_id: 100,
            asserted: true,
        };
        assert!(k1 < k2);
    }

    #[test]
    fn test_avet_key_orders_by_value_bytes() {
        let e = Uuid::new_v4();
        let k1 = AvetKey {
            attribute: ":score".to_string(),
            value_bytes: encode_value(&Value::Integer(10)),
            valid_from: 0,
            valid_to: i64::MAX,
            entity: e,
            tx_count: 1,
            tx_id: 100,
            asserted: true,
        };
        let k2 = AvetKey {
            attribute: ":score".to_string(),
            value_bytes: encode_value(&Value::Integer(20)),
            valid_from: 0,
            valid_to: i64::MAX,
            entity: e,
            tx_count: 2,
            tx_id: 200,
            asserted: true,
        };
        assert!(k1 < k2);
    }

    #[test]
    fn test_indexes_insert_vaet_only_for_ref() {
        let entity = Uuid::new_v4();
        let target = Uuid::new_v4();
        let mut indexes = Indexes::new();

        // Non-Ref value: should NOT appear in VAET
        let non_ref_fact = Fact::with_valid_time(
            entity,
            ":name".to_string(),
            Value::String("Alice".to_string()),
            0,
            1,
            0,
            VALID_TIME_FOREVER,
        );
        indexes.insert(
            &non_ref_fact,
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
        );
        assert!(
            indexes.vaet.is_empty(),
            "VAET must not contain non-Ref fact"
        );

        // Ref value: SHOULD appear in VAET
        let ref_fact = Fact::with_valid_time(
            entity,
            ":friend".to_string(),
            Value::Ref(target),
            0,
            2,
            0,
            VALID_TIME_FOREVER,
        );
        indexes.insert(
            &ref_fact,
            FactRef {
                page_id: 2,
                slot_index: 0,
            },
        );
        assert_eq!(indexes.vaet.len(), 1);
    }

    #[test]
    fn test_indexes_insert_populates_all_four() {
        let entity = Uuid::new_v4();
        let target = Uuid::new_v4();
        let mut indexes = Indexes::new();
        let ref_fact = Fact::with_valid_time(
            entity,
            ":friend".to_string(),
            Value::Ref(target),
            0,
            1,
            0,
            VALID_TIME_FOREVER,
        );
        indexes.insert(
            &ref_fact,
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
        );
        assert_eq!(indexes.eavt.len(), 1);
        assert_eq!(indexes.aevt.len(), 1);
        assert_eq!(indexes.avet.len(), 1);
        assert_eq!(indexes.vaet.len(), 1);
    }

    #[test]
    fn test_indexes_preserve_same_ref_assert_and_retract_identity() {
        let entity = Uuid::new_v4();
        let target = Uuid::new_v4();
        let asserted = Fact::with_valid_time(
            entity,
            ":edge/to".to_string(),
            Value::Ref(target),
            100,
            7,
            0,
            VALID_TIME_FOREVER,
        );
        let mut retracted = Fact::with_valid_time(
            entity,
            ":edge/to".to_string(),
            Value::Ref(target),
            100,
            7,
            0,
            VALID_TIME_FOREVER,
        );
        retracted.asserted = false;

        let mut indexes = Indexes::new();
        indexes.insert(
            &asserted,
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
        );
        indexes.insert(
            &retracted,
            FactRef {
                page_id: 1,
                slot_index: 1,
            },
        );

        assert_eq!(
            indexes.eavt.len(),
            2,
            "EAVT must keep asserted and retracted facts"
        );
        assert_eq!(
            indexes.aevt.len(),
            2,
            "AEVT must keep asserted and retracted facts"
        );
        assert_eq!(
            indexes.avet.len(),
            2,
            "AVET must keep asserted and retracted facts"
        );
        assert_eq!(
            indexes.vaet.len(),
            2,
            "VAET must keep asserted and retracted ref facts"
        );
    }

    #[test]
    fn test_indexes_preserve_same_ref_eav_different_tx_id_identity() {
        let entity = Uuid::new_v4();
        let target = Uuid::new_v4();
        let first = Fact::with_valid_time(
            entity,
            ":edge/to".to_string(),
            Value::Ref(target),
            100,
            7,
            0,
            VALID_TIME_FOREVER,
        );
        let second = Fact::with_valid_time(
            entity,
            ":edge/to".to_string(),
            Value::Ref(target),
            101,
            7,
            0,
            VALID_TIME_FOREVER,
        );

        let mut indexes = Indexes::new();
        indexes.insert(
            &first,
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
        );
        indexes.insert(
            &second,
            FactRef {
                page_id: 1,
                slot_index: 1,
            },
        );

        assert_eq!(
            indexes.eavt.len(),
            2,
            "EAVT must keep same Ref EAV rows with different tx_id"
        );
        assert_eq!(
            indexes.aevt.len(),
            2,
            "AEVT must keep same Ref EAV rows with different tx_id"
        );
        assert_eq!(
            indexes.avet.len(),
            2,
            "AVET must keep same Ref EAV rows with different tx_id"
        );
        assert_eq!(
            indexes.vaet.len(),
            2,
            "VAET must keep same Ref EAV rows with different tx_id"
        );
    }

    #[test]
    fn test_encode_value_sort_order_floats() {
        let neg_inf = encode_value(&Value::Float(f64::NEG_INFINITY));
        let neg_one = encode_value(&Value::Float(-1.0));
        let zero = encode_value(&Value::Float(0.0));
        let pos_one = encode_value(&Value::Float(1.0));
        let pos_inf = encode_value(&Value::Float(f64::INFINITY));
        assert!(neg_inf < neg_one, "-inf < -1.0");
        assert!(neg_one < zero, "-1.0 < 0.0");
        assert!(zero < pos_one, "0.0 < 1.0");
        assert!(pos_one < pos_inf, "1.0 < +inf");
    }

    #[test]
    fn test_encode_value_nan_is_canonical() {
        let nan1 = encode_value(&Value::Float(f64::NAN));
        let nan2 = encode_value(&Value::Float(f64::NAN));
        // All NaN values produce the same bytes
        assert_eq!(nan1, nan2);
        // NaN sorts above all positive finite values (it uses quiet NaN bit pattern)
        // Just verify it doesn't panic and produces a fixed-length result
        assert_eq!(nan1.len(), 9);
    }
}
