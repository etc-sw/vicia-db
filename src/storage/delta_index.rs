use crate::storage::CommittedIndexReader;
use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, VaetKey};
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

pub(crate) trait KeyedIndexReader: Send + Sync {
    fn range_scan_eavt_entries(
        &self,
        start: &EavtKey,
        end: Option<&EavtKey>,
    ) -> Result<Vec<(EavtKey, FactRef)>>;

    fn range_scan_aevt_entries(
        &self,
        start: &AevtKey,
        end: Option<&AevtKey>,
    ) -> Result<Vec<(AevtKey, FactRef)>>;

    fn visit_aevt_entries(
        &self,
        start: &AevtKey,
        end: Option<&AevtKey>,
        visit: &mut dyn FnMut(&AevtKey, FactRef) -> Result<()>,
    ) -> Result<()> {
        for (key, fact_ref) in self.range_scan_aevt_entries(start, end)? {
            visit(&key, fact_ref)?;
        }
        Ok(())
    }

    fn range_scan_avet_entries(
        &self,
        start: &AvetKey,
        end: Option<&AvetKey>,
    ) -> Result<Vec<(AvetKey, FactRef)>>;

    fn range_scan_vaet_entries(
        &self,
        start: &VaetKey,
        end: Option<&VaetKey>,
    ) -> Result<Vec<(VaetKey, FactRef)>>;
}

#[derive(Clone, Default)]
pub(crate) struct DeltaIndexEntries {
    eavt: BTreeMap<EavtKey, FactRef>,
    aevt: BTreeMap<AevtKey, FactRef>,
    avet: BTreeMap<AvetKey, FactRef>,
    vaet: BTreeMap<VaetKey, FactRef>,
}

impl DeltaIndexEntries {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub(crate) fn from_entries(
        eavt: Vec<(EavtKey, FactRef)>,
        aevt: Vec<(AevtKey, FactRef)>,
        avet: Vec<(AvetKey, FactRef)>,
        vaet: Vec<(VaetKey, FactRef)>,
    ) -> Self {
        Self {
            eavt: eavt.into_iter().collect(),
            aevt: aevt.into_iter().collect(),
            avet: avet.into_iter().collect(),
            vaet: vaet.into_iter().collect(),
        }
    }

    pub(crate) fn extend_from_entries(
        &mut self,
        eavt: &[(EavtKey, FactRef)],
        aevt: &[(AevtKey, FactRef)],
        avet: &[(AvetKey, FactRef)],
        vaet: &[(VaetKey, FactRef)],
    ) {
        self.eavt.extend(eavt.iter().cloned());
        self.aevt.extend(aevt.iter().cloned());
        self.avet.extend(avet.iter().cloned());
        self.vaet.extend(vaet.iter().cloned());
    }
}

pub(crate) struct LayeredIndexReader {
    base: Arc<dyn KeyedIndexReader>,
    delta: Arc<RwLock<DeltaIndexEntries>>,
}

impl LayeredIndexReader {
    #[allow(dead_code)]
    pub(crate) fn new(base: Arc<dyn KeyedIndexReader>, delta: DeltaIndexEntries) -> Self {
        Self {
            base,
            delta: Arc::new(RwLock::new(delta)),
        }
    }

    pub(crate) fn new_shared(
        base: Arc<dyn KeyedIndexReader>,
        delta: Arc<RwLock<DeltaIndexEntries>>,
    ) -> Self {
        Self { base, delta }
    }
}

impl CommittedIndexReader for LayeredIndexReader {
    fn range_scan_eavt(&self, start: &EavtKey, end: Option<&EavtKey>) -> Result<Vec<FactRef>> {
        let base = self.base.range_scan_eavt_entries(start, end)?;
        let delta = self.delta.read().unwrap_or_else(|error| error.into_inner());
        let delta = range_delta_entries(&delta.eavt, start, end);
        Ok(merge_entry_refs(base, delta))
    }

    fn range_scan_aevt(&self, start: &AevtKey, end: Option<&AevtKey>) -> Result<Vec<FactRef>> {
        let base = self.base.range_scan_aevt_entries(start, end)?;
        let delta = self.delta.read().unwrap_or_else(|error| error.into_inner());
        let delta = range_delta_entries(&delta.aevt, start, end);
        Ok(merge_entry_refs(base, delta))
    }

    fn visit_aevt_entries(
        &self,
        start: &AevtKey,
        end: Option<&AevtKey>,
        visit: &mut dyn FnMut(&AevtKey, FactRef) -> Result<()>,
    ) -> Result<()> {
        // Resident delta segments are bounded by checkpoint policy. Buffer the
        // delta only; stream the immutable base and interleave delta entries.
        let delta = self.delta.read().unwrap_or_else(|error| error.into_inner());
        let mut delta = range_delta_entries(&delta.aevt, start, end)
            .into_iter()
            .peekable();
        self.base
            .visit_aevt_entries(start, end, &mut |base_key, base_ref| {
                while delta
                    .peek()
                    .is_some_and(|(delta_key, _)| delta_key < base_key)
                {
                    if let Some((key, fact_ref)) = delta.next() {
                        visit(&key, fact_ref)?;
                    }
                }
                visit(base_key, base_ref)
            })?;
        for (key, fact_ref) in delta {
            visit(&key, fact_ref)?;
        }
        Ok(())
    }

    fn range_scan_avet(&self, start: &AvetKey, end: Option<&AvetKey>) -> Result<Vec<FactRef>> {
        let base = self.base.range_scan_avet_entries(start, end)?;
        let delta = self.delta.read().unwrap_or_else(|error| error.into_inner());
        let delta = range_delta_entries(&delta.avet, start, end);
        Ok(merge_entry_refs(base, delta))
    }

    fn range_scan_vaet(&self, start: &VaetKey, end: Option<&VaetKey>) -> Result<Vec<FactRef>> {
        let base = self.base.range_scan_vaet_entries(start, end)?;
        let delta = self.delta.read().unwrap_or_else(|error| error.into_inner());
        let delta = range_delta_entries(&delta.vaet, start, end);
        Ok(merge_entry_refs(base, delta))
    }
}

fn range_delta_entries<K: Clone + Ord>(
    entries: &BTreeMap<K, FactRef>,
    start: &K,
    end: Option<&K>,
) -> Vec<(K, FactRef)> {
    if let Some(end) = end {
        entries
            .range(start.clone()..end.clone())
            .map(|(key, fact_ref)| (key.clone(), *fact_ref))
            .collect()
    } else {
        entries
            .range(start.clone()..)
            .map(|(key, fact_ref)| (key.clone(), *fact_ref))
            .collect()
    }
}

fn merge_entry_refs<K: Ord>(base: Vec<(K, FactRef)>, delta: Vec<(K, FactRef)>) -> Vec<FactRef> {
    let mut base = base.into_iter().peekable();
    let mut delta = delta.into_iter().peekable();
    let mut merged = Vec::new();

    loop {
        match (base.peek(), delta.peek()) {
            (Some((base_key, _)), Some((delta_key, _))) => {
                if base_key <= delta_key {
                    if let Some((_, fact_ref)) = base.next() {
                        merged.push(fact_ref);
                    }
                } else if let Some((_, fact_ref)) = delta.next() {
                    merged.push(fact_ref);
                }
            }
            (Some(_), None) => {
                merged.extend(base.map(|(_, fact_ref)| fact_ref));
                break;
            }
            (None, Some(_)) => {
                merged.extend(delta.map(|(_, fact_ref)| fact_ref));
                break;
            }
            (None, None) => break,
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::{DeltaIndexEntries, KeyedIndexReader, LayeredIndexReader};
    use crate::storage::CommittedIndexReader;
    use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, VaetKey, encode_value};
    use anyhow::Result;
    use std::sync::Arc;
    use uuid::Uuid;

    #[derive(Default)]
    struct FakeKeyedIndexReader {
        eavt: Vec<(EavtKey, FactRef)>,
        aevt: Vec<(AevtKey, FactRef)>,
        avet: Vec<(AvetKey, FactRef)>,
        vaet: Vec<(VaetKey, FactRef)>,
    }

    impl FakeKeyedIndexReader {
        fn with_eavt(mut self, entries: Vec<(EavtKey, FactRef)>) -> Self {
            self.eavt = entries;
            self.eavt.sort_by(|a, b| a.0.cmp(&b.0));
            self
        }

        fn with_vaet(mut self, entries: Vec<(VaetKey, FactRef)>) -> Self {
            self.vaet = entries;
            self.vaet.sort_by(|a, b| a.0.cmp(&b.0));
            self
        }
    }

    impl KeyedIndexReader for FakeKeyedIndexReader {
        fn range_scan_eavt_entries(
            &self,
            start: &EavtKey,
            end: Option<&EavtKey>,
        ) -> Result<Vec<(EavtKey, FactRef)>> {
            Ok(filter_range(&self.eavt, start, end))
        }

        fn range_scan_aevt_entries(
            &self,
            start: &AevtKey,
            end: Option<&AevtKey>,
        ) -> Result<Vec<(AevtKey, FactRef)>> {
            Ok(filter_range(&self.aevt, start, end))
        }

        fn range_scan_avet_entries(
            &self,
            start: &AvetKey,
            end: Option<&AvetKey>,
        ) -> Result<Vec<(AvetKey, FactRef)>> {
            Ok(filter_range(&self.avet, start, end))
        }

        fn range_scan_vaet_entries(
            &self,
            start: &VaetKey,
            end: Option<&VaetKey>,
        ) -> Result<Vec<(VaetKey, FactRef)>> {
            Ok(filter_range(&self.vaet, start, end))
        }
    }

    fn filter_range<K: Ord + Clone>(
        entries: &[(K, FactRef)],
        start: &K,
        end: Option<&K>,
    ) -> Vec<(K, FactRef)> {
        entries
            .iter()
            .filter(|(key, _)| key >= start && end.is_none_or(|end| key < end))
            .cloned()
            .collect()
    }

    fn fact_ref(page_id: u64) -> FactRef {
        FactRef {
            page_id,
            slot_index: 0,
        }
    }

    fn eavt(entity: Uuid, attribute: &str, tx_count: u64, tx_id: u64, asserted: bool) -> EavtKey {
        eavt_with_valid_window(entity, attribute, 10, 20, tx_count, tx_id, asserted)
    }

    fn eavt_with_valid_window(
        entity: Uuid,
        attribute: &str,
        valid_from: i64,
        valid_to: i64,
        tx_count: u64,
        tx_id: u64,
        asserted: bool,
    ) -> EavtKey {
        EavtKey {
            entity,
            attribute: attribute.to_string(),
            valid_from,
            valid_to,
            tx_count,
            value_bytes: encode_value(&crate::graph::types::Value::Ref(Uuid::from_u128(900))),
            tx_id,
            asserted,
        }
    }

    fn aevt(entity: Uuid, attribute: &str, tx_count: u64, tx_id: u64, asserted: bool) -> AevtKey {
        AevtKey {
            attribute: attribute.to_string(),
            entity,
            valid_from: 10,
            valid_to: 20,
            tx_count,
            value_bytes: encode_value(&crate::graph::types::Value::Ref(Uuid::from_u128(900))),
            tx_id,
            asserted,
        }
    }

    fn avet(entity: Uuid, attribute: &str, tx_count: u64, tx_id: u64, asserted: bool) -> AvetKey {
        AvetKey {
            attribute: attribute.to_string(),
            value_bytes: encode_value(&crate::graph::types::Value::Ref(Uuid::from_u128(900))),
            valid_from: 10,
            valid_to: 20,
            entity,
            tx_count,
            tx_id,
            asserted,
        }
    }

    fn vaet(
        source_entity: Uuid,
        ref_target: Uuid,
        attribute: &str,
        tx_count: u64,
        tx_id: u64,
        asserted: bool,
    ) -> VaetKey {
        vaet_with_valid_window(
            source_entity,
            ref_target,
            attribute,
            10,
            20,
            tx_count,
            tx_id,
            asserted,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn vaet_with_valid_window(
        source_entity: Uuid,
        ref_target: Uuid,
        attribute: &str,
        valid_from: i64,
        valid_to: i64,
        tx_count: u64,
        tx_id: u64,
        asserted: bool,
    ) -> VaetKey {
        VaetKey {
            ref_target,
            attribute: attribute.to_string(),
            valid_from,
            valid_to,
            source_entity,
            tx_count,
            tx_id,
            asserted,
        }
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

    fn vaet_start(target: Uuid) -> VaetKey {
        VaetKey {
            ref_target: target,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            source_entity: Uuid::nil(),
            tx_count: 0,
            tx_id: 0,
            asserted: false,
        }
    }

    fn vaet_end(target: Uuid) -> VaetKey {
        VaetKey {
            ref_target: target,
            attribute: char::MAX.to_string(),
            valid_from: i64::MAX,
            valid_to: i64::MAX,
            source_entity: Uuid::from_u128(u128::MAX),
            tx_count: u64::MAX,
            tx_id: u64::MAX,
            asserted: true,
        }
    }

    #[test]
    fn delta_only_fact_visible_in_eavt_aevt_avet() {
        let entity = Uuid::from_u128(1);
        let key_eavt = eavt(entity, ":edge/to", 1, 100, true);
        let key_aevt = aevt(entity, ":edge/to", 1, 100, true);
        let key_avet = avet(entity, ":edge/to", 1, 100, true);
        let delta = DeltaIndexEntries::from_entries(
            vec![(key_eavt.clone(), fact_ref(10))],
            vec![(key_aevt.clone(), fact_ref(10))],
            vec![(key_avet.clone(), fact_ref(10))],
            Vec::new(),
        );
        let reader = LayeredIndexReader::new(Arc::new(FakeKeyedIndexReader::default()), delta);

        assert_eq!(
            reader
                .range_scan_eavt(
                    &eavt_start(entity, ":edge/to"),
                    Some(&eavt_end(entity, ":edge/to"))
                )
                .expect("eavt scan should succeed"),
            vec![fact_ref(10)]
        );
        assert_eq!(
            reader
                .range_scan_aevt(&key_aevt, None)
                .expect("aevt scan should succeed"),
            vec![fact_ref(10)]
        );
        assert_eq!(
            reader
                .range_scan_avet(&key_avet, None)
                .expect("avet scan should succeed"),
            vec![fact_ref(10)]
        );
    }

    #[test]
    fn delta_only_ref_edge_visible_in_vaet() {
        let source = Uuid::from_u128(1);
        let target = Uuid::from_u128(2);
        let delta = DeltaIndexEntries::from_entries(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![(vaet(source, target, ":edge/to", 1, 100, true), fact_ref(20))],
        );
        let reader = LayeredIndexReader::new(Arc::new(FakeKeyedIndexReader::default()), delta);

        assert_eq!(
            reader
                .range_scan_vaet(&vaet_start(target), Some(&vaet_end(target)))
                .expect("vaet scan should succeed"),
            vec![fact_ref(20)]
        );
    }

    #[test]
    fn base_and_delta_ref_edge_range_scan_merges_both() {
        let target = Uuid::from_u128(7);
        let base = FakeKeyedIndexReader::default().with_vaet(vec![(
            vaet(Uuid::from_u128(1), target, ":edge/to", 1, 100, true),
            fact_ref(30),
        )]);
        let delta = DeltaIndexEntries::from_entries(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![(
                vaet(Uuid::from_u128(2), target, ":edge/to", 2, 200, true),
                fact_ref(31),
            )],
        );
        let reader = LayeredIndexReader::new(Arc::new(base), delta);

        assert_eq!(
            reader
                .range_scan_vaet(&vaet_start(target), Some(&vaet_end(target)))
                .expect("vaet scan should succeed"),
            vec![fact_ref(30), fact_ref(31)]
        );
    }

    #[test]
    fn same_ref_eav_different_tx_id_not_collapsed() {
        let entity = Uuid::from_u128(11);
        let delta = DeltaIndexEntries::from_entries(
            vec![
                (eavt(entity, ":edge/to", 1, 100, true), fact_ref(40)),
                (eavt(entity, ":edge/to", 2, 200, true), fact_ref(41)),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let reader = LayeredIndexReader::new(Arc::new(FakeKeyedIndexReader::default()), delta);

        assert_eq!(
            reader
                .range_scan_eavt(
                    &eavt_start(entity, ":edge/to"),
                    Some(&eavt_end(entity, ":edge/to"))
                )
                .expect("eavt scan should succeed"),
            vec![fact_ref(40), fact_ref(41)]
        );
    }

    #[test]
    fn same_ref_eav_assert_and_retract_not_collapsed() {
        let entity = Uuid::from_u128(12);
        let delta = DeltaIndexEntries::from_entries(
            vec![
                (eavt(entity, ":edge/to", 1, 100, false), fact_ref(50)),
                (eavt(entity, ":edge/to", 1, 100, true), fact_ref(51)),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let reader = LayeredIndexReader::new(Arc::new(FakeKeyedIndexReader::default()), delta);

        assert_eq!(
            reader
                .range_scan_eavt(
                    &eavt_start(entity, ":edge/to"),
                    Some(&eavt_end(entity, ":edge/to"))
                )
                .expect("eavt scan should succeed"),
            vec![fact_ref(50), fact_ref(51)]
        );
    }

    #[test]
    fn same_ref_eav_different_valid_window_not_collapsed() {
        let entity = Uuid::from_u128(13);
        let delta = DeltaIndexEntries::from_entries(
            vec![
                (
                    eavt_with_valid_window(entity, ":edge/to", 10, 20, 1, 100, true),
                    fact_ref(52),
                ),
                (
                    eavt_with_valid_window(entity, ":edge/to", 15, 25, 1, 100, true),
                    fact_ref(53),
                ),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let reader = LayeredIndexReader::new(Arc::new(FakeKeyedIndexReader::default()), delta);

        assert_eq!(
            reader
                .range_scan_eavt(
                    &eavt_start(entity, ":edge/to"),
                    Some(&eavt_end(entity, ":edge/to"))
                )
                .expect("eavt scan should succeed"),
            vec![fact_ref(52), fact_ref(53)]
        );
    }

    #[test]
    fn same_ref_vaet_assert_and_retract_not_collapsed() {
        let source = Uuid::from_u128(14);
        let target = Uuid::from_u128(15);
        let delta = DeltaIndexEntries::from_entries(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                (
                    vaet(source, target, ":edge/to", 1, 100, false),
                    fact_ref(54),
                ),
                (vaet(source, target, ":edge/to", 1, 100, true), fact_ref(55)),
            ],
        );
        let reader = LayeredIndexReader::new(Arc::new(FakeKeyedIndexReader::default()), delta);

        assert_eq!(
            reader
                .range_scan_vaet(&vaet_start(target), Some(&vaet_end(target)))
                .expect("vaet scan should succeed"),
            vec![fact_ref(54), fact_ref(55)]
        );
    }

    #[test]
    fn range_scan_respects_start_end_across_base_and_delta() {
        let base = FakeKeyedIndexReader::default().with_eavt(vec![
            (
                eavt(Uuid::from_u128(1), ":edge/to", 1, 100, true),
                fact_ref(60),
            ),
            (
                eavt(Uuid::from_u128(3), ":edge/to", 3, 300, true),
                fact_ref(62),
            ),
        ]);
        let delta = DeltaIndexEntries::from_entries(
            vec![
                (
                    eavt(Uuid::from_u128(2), ":edge/to", 2, 200, true),
                    fact_ref(61),
                ),
                (
                    eavt(Uuid::from_u128(4), ":edge/to", 4, 400, true),
                    fact_ref(63),
                ),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let reader = LayeredIndexReader::new(Arc::new(base), delta);

        assert_eq!(
            reader
                .range_scan_eavt(
                    &eavt_start(Uuid::from_u128(2), ":edge/to"),
                    Some(&eavt_start(Uuid::from_u128(4), ":edge/to")),
                )
                .expect("eavt scan should succeed"),
            vec![fact_ref(61), fact_ref(62)]
        );
    }

    #[test]
    fn empty_delta_delegates_to_base_reader() {
        let entity = Uuid::from_u128(21);
        let base = FakeKeyedIndexReader::default()
            .with_eavt(vec![(eavt(entity, ":edge/to", 1, 100, true), fact_ref(70))]);
        let reader = LayeredIndexReader::new(Arc::new(base), DeltaIndexEntries::new());

        assert_eq!(
            reader
                .range_scan_eavt(
                    &eavt_start(entity, ":edge/to"),
                    Some(&eavt_end(entity, ":edge/to"))
                )
                .expect("eavt scan should succeed"),
            vec![fact_ref(70)]
        );
    }
}
