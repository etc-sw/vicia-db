use crate::graph::types::{
    Attribute, EntityId, Fact, RETRACT_ALL_VALID_FROM, TransactOptions, TxId, VALID_TIME_FOREVER,
    Value, tx_id_now,
};
use crate::query::datalog::types::AsOf;
use crate::storage::index::{AevtKey, FactRef, Indexes, encode_value};
use anyhow::Result;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Compact key for O(1) duplicate detection in `FactData::pending_keys`.
///
/// Mirrors the equality predicate used by `load_fact`:
/// (entity, attribute, encoded_value, valid_from, valid_to, tx_count, tx_id, asserted).
/// `encode_value` is used for the value field because `Value` contains `f64`
/// and therefore cannot implement `Hash` directly.
type PendingKey = (EntityId, String, Vec<u8>, i64, i64, u64, TxId, bool);

#[derive(Clone, Copy)]
pub(crate) enum CurrentValidTime {
    At(i64),
    Any,
}

#[derive(Default)]
struct CurrentValueState {
    max_unscoped_retract_tx: u64,
    max_scoped_retract_tx: std::collections::HashMap<(i64, i64), u64>,
    assertions: std::collections::HashMap<(i64, i64), (u64, FactRef)>,
}

fn pending_key(f: &Fact) -> PendingKey {
    (
        f.entity,
        f.attribute.clone(),
        encode_value(&f.value),
        f.valid_from,
        f.valid_to,
        f.tx_count,
        f.tx_id,
        f.asserted,
    )
}

// ============================================================================
// Datalog Fact Storage (Phase 3+)
// ============================================================================

/// Private container that co-locates the fact list and all four indexes under
/// a single `RwLock`. This ensures facts and indexes are always updated together
/// without needing a second lock.
struct FactData {
    facts: Vec<Fact>,
    /// O(1) duplicate-detection set for `load_fact`.
    ///
    /// Maintained in sync with `facts` by every method that appends to `facts`.
    /// Replaces the O(n) linear scan that made `load_fact` O(n²) for large
    /// fact sets (e.g. 1M-fact benchmark setup).
    pending_keys: HashSet<PendingKey>,
    pending_indexes: Indexes,
    /// Resolves committed (on-disk) FactRefs to Fact objects.
    /// None for in-memory databases or before load() is called.
    committed: Option<Arc<dyn crate::storage::CommittedFactReader>>,
    /// Provides bounded range scans over the four committed (on-disk) covering indexes.
    /// Set by `set_committed_index_reader()` after open/migration/checkpoint.
    committed_index_reader: Option<Arc<dyn crate::storage::CommittedIndexReader>>,
}

/// In-memory storage for Datalog facts with transaction support
///
/// FactStorage maintains an append-only log of facts. Facts are never deleted,
/// only retracted (with asserted=false). This enables:
/// - Full history tracking
/// - Time travel queries (Phase 4)
/// - Audit trails
///
/// # Storage Model (Phase 3-6)
///
/// This is a simple in-memory store using `Vec<Fact>` plus four covering
/// indexes (EAVT, AEVT, AVET, VAET). For persistence, see `PersistentFactStorage`
/// which wraps this with a "load all, save all" strategy.
///
/// # Examples
/// ```ignore
/// use crate::graph::storage::FactStorage;
/// use crate::graph::types::Value;
/// use uuid::Uuid;
///
/// let storage = FactStorage::new();
///
/// // Add facts (automatic timestamping)
/// let alice = Uuid::new_v4();
/// storage.transact(vec![
///     (alice, ":person/name".to_string(), Value::String("Alice".to_string())),
///     (alice, ":person/age".to_string(), Value::Integer(30)),
/// ], None).unwrap();
///
/// // Query facts
/// let facts = storage.get_facts_by_entity(&alice).unwrap();
/// assert_eq!(facts.len(), 2);
/// ```
#[derive(Clone)]
pub(crate) struct FactStorage {
    /// Append-only log of all facts (assertions and retractions) plus indexes.
    data: Arc<RwLock<FactData>>,
    /// Monotonically incrementing batch counter — increments once per transact/retract call.
    tx_counter: Arc<AtomicU64>,
}

impl Default for FactStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl FactStorage {
    /// Create a new empty fact storage
    pub(crate) fn new() -> Self {
        FactStorage {
            data: Arc::new(RwLock::new(FactData {
                facts: Vec::new(),
                pending_keys: HashSet::new(),
                pending_indexes: Indexes::new(),
                committed: None,
                committed_index_reader: None,
            })),
            tx_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Transact a batch of facts with automatic timestamping
    ///
    /// All facts in a single transaction get the same timestamp (TxId) and the same
    /// `tx_count`. The `tx_count` increments once per call (not per fact), so all
    /// facts in a batch share the same counter value.
    ///
    /// # Arguments
    /// * `fact_tuples` - Vec of (EntityId, Attribute, Value) tuples to assert
    /// * `opts` - Optional TransactOptions to override valid_from / valid_to
    ///
    /// # Returns
    /// The TxId (timestamp) assigned to these facts
    pub(crate) fn transact(
        &self,
        fact_tuples: Vec<(EntityId, Attribute, Value)>,
        opts: Option<TransactOptions>,
    ) -> Result<TxId> {
        let tx_id = tx_id_now();
        let tx_count = self
            .tx_counter
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let opts = opts.unwrap_or_default();

        let facts: Vec<Fact> = fact_tuples
            .into_iter()
            .map(|(entity, attribute, value)| {
                let valid_from = opts
                    .valid_from
                    .unwrap_or_else(|| i64::try_from(tx_id).unwrap_or(i64::MAX));
                let valid_to = opts.valid_to.unwrap_or(VALID_TIME_FOREVER);
                Fact::with_valid_time(
                    entity, attribute, value, tx_id, tx_count, valid_from, valid_to,
                )
            })
            .collect();

        let mut d = self
            .data
            .write()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        for (slot, fact) in (u16::try_from(d.facts.len()).unwrap_or(u16::MAX)..).zip(facts.iter()) {
            d.pending_keys.insert(pending_key(fact));
            d.pending_indexes.insert(
                fact,
                FactRef {
                    page_id: 0,
                    slot_index: slot,
                },
            );
        }
        d.facts.extend(facts);

        Ok(tx_id)
    }

    /// Transact a batch of facts where each fact may carry its own valid-time opts.
    ///
    /// All facts share **one** `tx_count` (incremented once for the whole batch),
    /// matching the semantics of a single user-level `(transact [...])` command.
    /// Per-fact opts override `default_opts` for that individual fact only.
    ///
    /// # Arguments
    /// * `fact_tuples` - Vec of `(entity, attribute, value, per_fact_opts)`
    /// * `default_opts` - Transaction-level valid-time opts applied when a fact
    ///   has no per-fact override
    ///
    /// # Returns
    /// `(tx_id, tx_count)` — the Unix-ms timestamp and the monotonic counter
    /// assigned to all facts in this batch.
    pub(crate) fn transact_batch(
        &self,
        fact_tuples: Vec<(EntityId, Attribute, Value, Option<TransactOptions>)>,
        default_opts: Option<TransactOptions>,
    ) -> Result<(TxId, u64)> {
        let tx_id = tx_id_now();
        let tx_count = self
            .tx_counter
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let default_opts = default_opts.unwrap_or_default();

        let facts: Vec<Fact> = fact_tuples
            .into_iter()
            .map(|(entity, attribute, value, per_fact_opts)| {
                let opts = per_fact_opts.unwrap_or_else(|| default_opts.clone());
                let valid_from = opts
                    .valid_from
                    .unwrap_or_else(|| i64::try_from(tx_id).unwrap_or(i64::MAX));
                let valid_to = opts.valid_to.unwrap_or(VALID_TIME_FOREVER);
                Fact::with_valid_time(
                    entity, attribute, value, tx_id, tx_count, valid_from, valid_to,
                )
            })
            .collect();

        let mut d = self
            .data
            .write()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        for (slot, fact) in (u16::try_from(d.facts.len()).unwrap_or(u16::MAX)..).zip(facts.iter()) {
            d.pending_keys.insert(pending_key(fact));
            d.pending_indexes.insert(
                fact,
                FactRef {
                    page_id: 0,
                    slot_index: slot,
                },
            );
        }
        d.facts.extend(facts);

        Ok((tx_id, tx_count))
    }

    /// Retract a batch of facts with automatic timestamping
    ///
    /// Retractions are new facts with asserted=false. The original facts remain
    /// in the log for history tracking.
    ///
    /// # Arguments
    /// * `fact_tuples` - Vec of (EntityId, Attribute, Value) tuples to retract
    ///
    /// # Returns
    /// `(tx_id, tx_count)` — the Unix-ms timestamp and the monotonic counter
    /// assigned to these retractions.
    #[cfg(test)]
    pub(crate) fn retract(
        &self,
        fact_tuples: Vec<(EntityId, Attribute, Value)>,
    ) -> Result<(TxId, u64)> {
        let fact_tuples = fact_tuples
            .into_iter()
            .map(|(entity, attribute, value)| (entity, attribute, value, None))
            .collect();
        self.retract_batch(fact_tuples, None)
    }

    /// Retract a batch of facts where each fact may carry its own valid-time opts.
    ///
    /// No valid-time opts means legacy/unscoped retraction: cancel every
    /// valid-time window of the same EAV triple. Any transaction-level or
    /// per-fact valid-time opts make the retraction scoped to that exact window.
    pub(crate) fn retract_batch(
        &self,
        fact_tuples: Vec<(EntityId, Attribute, Value, Option<TransactOptions>)>,
        default_opts: Option<TransactOptions>,
    ) -> Result<(TxId, u64)> {
        let tx_id = tx_id_now();
        let tx_count = self
            .tx_counter
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let has_default_opts = default_opts.is_some();
        let default_opts = default_opts.unwrap_or_default();

        let retractions: Vec<Fact> = fact_tuples
            .into_iter()
            .map(|(entity, attribute, value, per_fact_opts)| {
                if per_fact_opts.is_some() || has_default_opts {
                    let opts = per_fact_opts.unwrap_or_else(|| default_opts.clone());
                    let valid_from = opts
                        .valid_from
                        .unwrap_or_else(|| i64::try_from(tx_id).unwrap_or(i64::MAX));
                    let valid_to = opts.valid_to.unwrap_or(VALID_TIME_FOREVER);
                    Fact::retract_with_valid_time(
                        entity, attribute, value, tx_id, tx_count, valid_from, valid_to,
                    )
                } else {
                    let mut f = Fact::retract(entity, attribute, value, tx_id);
                    f.tx_count = tx_count;
                    f
                }
            })
            .collect();

        let mut d = self
            .data
            .write()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        for (slot, fact) in
            (u16::try_from(d.facts.len()).unwrap_or(u16::MAX)..).zip(retractions.iter())
        {
            d.pending_keys.insert(pending_key(fact));
            d.pending_indexes.insert(
                fact,
                FactRef {
                    page_id: 0,
                    slot_index: slot,
                },
            );
        }
        d.facts.extend(retractions);

        Ok((tx_id, tx_count))
    }

    /// Insert a fact with its original tx_id and tx_count preserved.
    ///
    /// Used by the load and migration paths only — bypasses tx_counter entirely.
    /// After loading all facts, call `restore_tx_counter()` to re-synchronise the
    /// counter so subsequent `transact()` calls get correct tx_count values.
    ///
    /// Checks for duplicate facts before loading (based on entity, attribute, value,
    /// valid_from, valid_to, tx_count, tx_id, and asserted).
    pub(crate) fn load_fact(&self, fact: Fact) -> Result<bool> {
        let mut d = self
            .data
            .write()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;

        // O(1) duplicate check via the pending_keys HashSet.
        // Previously this was an O(n) linear scan over d.facts, causing O(n²)
        // total complexity when loading n facts (e.g. 1M-fact benchmarks).
        let key = pending_key(&fact);
        if !d.pending_keys.insert(key) {
            return Ok(false); // Already exists, not loaded
        }

        let slot = u16::try_from(d.facts.len()).unwrap_or(u16::MAX);
        d.pending_indexes.insert(
            &fact,
            FactRef {
                page_id: 0,
                slot_index: slot,
            },
        );
        d.facts.push(fact);
        Ok(true)
    }

    /// Set tx_counter to max(tx_count) across all loaded facts.
    ///
    /// Must be called after all `load_fact()` calls complete so that the next
    /// `transact()` call picks up from the right sequence number.
    pub(crate) fn restore_tx_counter(&self) -> Result<()> {
        let d = self
            .data
            .read()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        let max = d.facts.iter().map(|f| f.tx_count).max().unwrap_or(0);
        self.tx_counter.store(max, Ordering::SeqCst);
        Ok(())
    }

    /// Return the current value of the monotonic tx counter.
    ///
    /// Useful for persisting `last_checkpointed_tx_count` into the file header.
    pub(crate) fn current_tx_count(&self) -> u64 {
        self.tx_counter.load(Ordering::SeqCst)
    }

    /// Atomically increment the tx counter and return the new value.
    ///
    /// Used by explicit transactions to claim a tx_count at commit time,
    /// without creating any facts in FactStorage.
    pub(crate) fn allocate_tx_count(&self) -> u64 {
        self.tx_counter
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1)
    }

    /// Get all facts (including retractions)
    ///
    /// Returns the complete append-only log. For current state, filter by
    /// asserted=true and take the most recent fact for each (E, A) pair.
    /// Includes both committed (on-disk) facts and pending (in-memory) facts.
    pub(crate) fn get_all_facts(&self) -> Result<Vec<Fact>> {
        let d = self
            .data
            .read()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        let mut all = Vec::new();
        // Committed facts first (on disk, via CommittedFactReader)
        if let Some(loader) = &d.committed {
            all.extend(loader.stream_all()?);
        }
        // Then pending facts (post-checkpoint, in memory)
        all.extend(d.facts.iter().cloned());
        Ok(all)
    }

    /// Visit all facts in deterministic storage order without materializing a
    /// full intermediate `Vec<Fact>` for committed records.
    pub(crate) fn for_each_fact(&self, mut visit: impl FnMut(Fact) -> Result<()>) -> Result<()> {
        let d = self
            .data
            .read()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        if let Some(loader) = &d.committed {
            loader.for_each_fact(&mut visit)?;
        }
        for fact in d.facts.iter().cloned() {
            visit(fact)?;
        }
        Ok(())
    }

    /// Visit only facts with `tx_count > since_tx_count`, in the same
    /// deterministic storage order as [`Self::for_each_fact`].
    ///
    /// Committed facts go through the reader's since-aware path, which locates
    /// the tail without a committed full scan; pending facts are filtered in
    /// memory (cost proportional to the pending set).
    pub(crate) fn for_each_fact_since(
        &self,
        since_tx_count: u64,
        mut visit: impl FnMut(Fact) -> Result<()>,
    ) -> Result<()> {
        let d = self
            .data
            .read()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        if let Some(loader) = &d.committed {
            loader.for_each_fact_since(since_tx_count, &mut visit)?;
        }
        for fact in d.facts.iter() {
            if fact.tx_count > since_tx_count {
                visit(fact.clone())?;
            }
        }
        Ok(())
    }

    /// Get all asserted facts (filters out retractions)
    ///
    /// Returns only facts where asserted=true. This gives you the currently
    /// valid facts, but includes all historical versions.
    pub(crate) fn get_asserted_facts(&self) -> Result<Vec<Fact>> {
        let all = self.get_all_facts()?;
        Ok(all.into_iter().filter(|f| f.is_asserted()).collect())
    }

    /// Clear all facts (for testing)
    pub(crate) fn clear(&self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| anyhow::anyhow!("data lock poisoned"))?;
        d.facts.clear();
        d.pending_keys.clear();
        d.pending_indexes = Indexes::new();
        d.committed = None;
        d.committed_index_reader = None;
        self.tx_counter.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Replace the pending in-memory indexes with a freshly rebuilt set.
    ///
    /// Used by `PersistentFactStorage` after detecting an index checksum
    /// mismatch (e.g. after crash recovery).
    #[allow(dead_code)]
    pub(crate) fn replace_pending_indexes(&self, indexes: Indexes) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.pending_indexes = indexes;
    }

    /// Return the pending (uncommitted) facts held in memory.
    pub(crate) fn get_pending_facts(&self) -> Vec<Fact> {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        d.facts.clone()
    }

    /// Clear pending facts and pending indexes after a successful checkpoint.
    pub(crate) fn post_checkpoint_clear(&self) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.facts.clear();
        d.pending_keys.clear();
        d.pending_indexes = Indexes::new();
    }

    /// Set the tx_counter to `max` (used on load to restore from persisted state).
    pub(crate) fn restore_tx_counter_from(&self, max: u64) {
        self.tx_counter.store(max, Ordering::SeqCst);
    }

    /// Return a snapshot (clone) of the current pending in-memory indexes.
    ///
    /// Used by `PersistentFactStorage::save()` to write index B+tree pages.
    /// Clones the BTreeMaps — acceptable since `save()` is not on the hot path.
    pub(crate) fn pending_indexes_snapshot(&self) -> Indexes {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        Indexes {
            eavt: d.pending_indexes.eavt.clone(),
            aevt: d.pending_indexes.aevt.clone(),
            avet: d.pending_indexes.avet.clone(),
            vaet: d.pending_indexes.vaet.clone(),
        }
    }

    /// Set the committed fact reader. Called by PersistentFactStorage::load() after
    /// opening a v5 file so index-driven reads can resolve FactRefs via page cache.
    pub(crate) fn set_committed_reader(
        &self,
        reader: Arc<dyn crate::storage::CommittedFactReader>,
    ) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.committed = Some(reader);
    }

    /// Set the committed index reader. Called by PersistentFactStorage after
    /// each open/migration/checkpoint so queries can range-scan the on-disk B+tree.
    #[cfg(test)]
    pub(crate) fn set_committed_index_reader(
        &self,
        reader: Arc<dyn crate::storage::CommittedIndexReader>,
    ) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.committed_index_reader = Some(reader);
    }

    /// Publish a complete committed-reader replacement and retire the pending
    /// view under one storage write barrier. Readers therefore observe either
    /// the pre-checkpoint pending facts or the post-checkpoint committed view,
    /// never a mixture of both.
    pub(crate) fn publish_committed_readers(
        &self,
        fact_reader: Arc<dyn crate::storage::CommittedFactReader>,
        index_reader: Option<Arc<dyn crate::storage::CommittedIndexReader>>,
    ) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.committed = Some(fact_reader);
        d.committed_index_reader = index_reader;
        d.facts.clear();
        d.pending_keys.clear();
        d.pending_indexes = Indexes::new();
    }

    /// Extend an already-published shared committed reader and retire the
    /// matching pending facts under the same barrier used by point queries.
    pub(crate) fn publish_incremental_committed(&self, extend: impl FnOnce()) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        extend();
        d.facts.clear();
        d.pending_keys.clear();
        d.pending_indexes = Indexes::new();
    }

    /// Count of in-memory (pending, not yet checkpointed) fact records.
    /// Cheap: no committed scan. Backs the A6 session `status` op.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn pending_fact_count(&self) -> usize {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        d.facts.len()
    }

    /// `true` when this storage has a committed reader — some facts live on
    /// disk, so [`Self::pending_fact_count`] is not the exact total.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn has_committed_reader(&self) -> bool {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        d.committed.is_some()
    }

    /// Returns (eavt_len, aevt_len, avet_len, vaet_len) for the pending indexes.
    /// Used in tests to verify pending index state.
    #[allow(dead_code)]
    pub(crate) fn pending_index_counts(&self) -> (usize, usize, usize, usize) {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        (
            d.pending_indexes.eavt.len(),
            d.pending_indexes.aevt.len(),
            d.pending_indexes.avet.len(),
            d.pending_indexes.vaet.len(),
        )
    }
}

/// Apply transaction-time snapshot semantics to a batch of facts.
///
/// Shared by planned Datalog reads and transactional overlay reads.
pub(crate) fn filter_facts_as_of(facts: Vec<Fact>, as_of: &AsOf) -> Vec<Fact> {
    facts
        .into_iter()
        .filter(|f| match as_of {
            AsOf::Counter(n) => f.tx_count <= *n,
            AsOf::Timestamp(t) => f.tx_id <= u64::try_from(*t).unwrap_or(0),
            AsOf::Slot(_) => false,
        })
        .collect()
}

/// Compute the net-asserted view of a fact set.
///
/// For each unique `(entity, attribute, value)` triple:
/// 1. Track legacy/unscoped retractions that cancel every valid-time window.
/// 2. Track scoped retractions that cancel only one exact valid-time window.
/// 3. Keep assertions whose `tx_count` is greater than both applicable
///    retraction counters.
/// 4. Deduplicate surviving assertions by `(valid_from, valid_to)`, keeping the
///    one with the highest `tx_count` for each validity window.
///
/// This allows the same EAV triple to be asserted at multiple non-overlapping
/// valid-time intervals (e.g., salary=$100k valid 2020–2022 AND 2024–2026).
/// A legacy retraction still cancels all prior assertions of that triple, but a
/// valid-time scoped retraction cancels only the matching interval.
/// Re-assertions after either retraction are preserved.
///
/// Value keys use the same canonical floating-point identity as
/// [`encode_value`] (all NaNs equal, ±0.0 distinct) without allocating an
/// encoded `Vec<u8>` for every input row.
///
/// This is the single source of truth for retraction semantics, shared by
/// `get_current_value` and `filter_facts_for_query`.
///
/// # Implementation note
///
/// This function uses borrowed keys into the immutable input vector. The maps
/// retain only identity references and winning row indexes; they never clone
/// attributes, values, or complete facts. After the maps are dropped, winning
/// facts are moved out of the original vector.
///
/// - `max_unscoped_retract_tx`: EAV → highest unscoped retraction `tx_count`.
/// - `max_scoped_retract_tx`: EAV + window → highest scoped retraction `tx_count`.
/// - `by_window`: (EAV + valid_from + valid_to) → highest-`tx_count` assertion
///   for that time window.
///
pub(crate) fn net_asserted_facts(facts: Vec<Fact>) -> Vec<Fact> {
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};

    #[derive(Clone, Copy)]
    struct CanonicalValueRef<'a>(&'a Value);

    impl PartialEq for CanonicalValueRef<'_> {
        fn eq(&self, other: &Self) -> bool {
            match (self.0, other.0) {
                (Value::Float(left), Value::Float(right)) => {
                    canonical_float_bits(*left) == canonical_float_bits(*right)
                }
                (left, right) => left == right,
            }
        }
    }

    impl Eq for CanonicalValueRef<'_> {}

    impl Hash for CanonicalValueRef<'_> {
        fn hash<H: Hasher>(&self, state: &mut H) {
            std::mem::discriminant(self.0).hash(state);
            match self.0 {
                Value::String(value) | Value::Keyword(value) => value.hash(state),
                Value::Integer(value) => value.hash(state),
                Value::Float(value) => canonical_float_bits(*value).hash(state),
                Value::Boolean(value) => value.hash(state),
                Value::Ref(value) => value.hash(state),
                Value::Null => {}
            }
        }
    }

    fn canonical_float_bits(value: f64) -> u64 {
        if value.is_nan() {
            0x7FF8_0000_0000_0000
        } else {
            value.to_bits()
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct EavKey<'a> {
        entity: EntityId,
        attribute: &'a str,
        value: CanonicalValueRef<'a>,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct WindowKey<'a> {
        eav: EavKey<'a>,
        valid_from: i64,
        valid_to: i64,
    }

    fn eav_key(fact: &Fact) -> EavKey<'_> {
        EavKey {
            entity: fact.entity,
            attribute: &fact.attribute,
            value: CanonicalValueRef(&fact.value),
        }
    }

    fn window_key(fact: &Fact) -> WindowKey<'_> {
        WindowKey {
            eav: eav_key(fact),
            valid_from: fact.valid_from,
            valid_to: fact.valid_to,
        }
    }

    let mut max_unscoped_retract_tx: HashMap<EavKey<'_>, u64> = HashMap::new();
    let mut max_scoped_retract_tx: HashMap<WindowKey<'_>, u64> = HashMap::new();
    let mut by_window: HashMap<WindowKey<'_>, usize> = HashMap::new();

    for (index, fact) in facts.iter().enumerate() {
        if fact.asserted {
            let key = window_key(fact);
            let replace = by_window
                .get(&key)
                .and_then(|existing| facts.get(*existing))
                .is_none_or(|existing| fact.tx_count > existing.tx_count);
            if replace {
                by_window.insert(key, index);
            }
        } else {
            let tx_count = fact.tx_count;
            if fact.valid_from == RETRACT_ALL_VALID_FROM && fact.valid_to == VALID_TIME_FOREVER {
                max_unscoped_retract_tx
                    .entry(eav_key(fact))
                    .and_modify(|max_tx| *max_tx = (*max_tx).max(tx_count))
                    .or_insert(tx_count);
            } else {
                max_scoped_retract_tx
                    .entry(window_key(fact))
                    .and_modify(|max_tx| *max_tx = (*max_tx).max(tx_count))
                    .or_insert(tx_count);
            }
        }
    }

    let mut keep = vec![false; facts.len()];
    for index in by_window.values().copied() {
        let Some(fact) = facts.get(index) else {
            continue;
        };
        let unscoped_retract_tx = max_unscoped_retract_tx
            .get(&eav_key(fact))
            .copied()
            .unwrap_or(0);
        let scoped_retract_tx = max_scoped_retract_tx
            .get(&window_key(fact))
            .copied()
            .unwrap_or(0);
        if let Some(keep_fact) = keep.get_mut(index) {
            *keep_fact = fact.tx_count > unscoped_retract_tx.max(scoped_retract_tx);
        }
    }
    drop(by_window);
    drop(max_scoped_retract_tx);
    drop(max_unscoped_retract_tx);

    facts
        .into_iter()
        .zip(keep)
        .filter_map(|(fact, keep)| keep.then_some(fact))
        .collect()
}

/// Resolve a [FactRef] to a [Fact] using the committed reader (for on-disk facts)
/// or the pending facts vector (for in-memory facts with page_id=0).
/// Used by the production index-driven lookup methods (`get_facts_by_entity`,
/// `get_facts_by_attribute`).
fn resolve_fact_ref(d: &FactData, fr: FactRef) -> Result<Fact> {
    if fr.page_id == 0 {
        d.facts
            .get(fr.slot_index as usize)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pending fact index {} out of bounds", fr.slot_index))
    } else {
        match &d.committed {
            Some(loader) => loader.resolve(fr),
            None => anyhow::bail!(
                "no CommittedFactReader but got committed FactRef (page_id={})",
                fr.page_id
            ),
        }
    }
}

/// Increment the last byte of a string for prefix upper-bound construction.
/// Returns `None` if all bytes are 0xFF (true unbounded scan needed).
/// Used by the production index-driven lookup methods (`get_facts_by_attribute`).
fn next_string_prefix(s: &str) -> Option<String> {
    let mut bytes = s.as_bytes().to_vec();
    for i in (0..bytes.len()).rev() {
        if let Some(b) = bytes.get_mut(i)
            && *b < 0xFF
        {
            *b += 1;
            bytes.truncate(i + 1);
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

/// Production helpers on FactStorage: index-driven entity/attribute lookups used by the query executor.
impl FactStorage {
    /// Get all facts for a specific entity (index-driven).
    pub(crate) fn get_facts_by_entity(&self, entity_id: &EntityId) -> Result<Vec<Fact>> {
        use crate::storage::index::EavtKey;
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());

        let start = EavtKey {
            entity: *entity_id,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let next_entity = uuid::Uuid::from_u128(entity_id.as_u128().wrapping_add(1));
        let end = EavtKey {
            entity: next_entity,
            attribute: String::new(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };

        // Fallback: no indexes built yet
        if d.pending_indexes.eavt.is_empty() && d.committed_index_reader.is_none() {
            if d.committed.is_none() {
                return Ok(d
                    .facts
                    .iter()
                    .filter(|f| &f.entity == entity_id)
                    .cloned()
                    .collect());
            }
            let mut result: Vec<Fact> = d
                .facts
                .iter()
                .filter(|f| &f.entity == entity_id)
                .cloned()
                .collect();
            if let Some(loader) = &d.committed {
                for fact in loader.stream_all()? {
                    if &fact.entity == entity_id {
                        result.push(fact);
                    }
                }
            }
            return Ok(result);
        }

        let mut facts = Vec::new();

        // Pending: in-memory BTreeMap bounded range.
        for (key, &fr) in d.pending_indexes.eavt.range(start.clone()..end.clone()) {
            if key.entity != *entity_id {
                break;
            }
            facts.push(resolve_fact_ref(&d, fr)?);
        }

        // Committed: on-disk B+tree range scan
        if let Some(reader) = &d.committed_index_reader {
            let mut committed_refs = reader.range_scan_eavt(&start, Some(&end))?;
            // Index order is a query-planning concern, not a result-order
            // contract. Resolve in packed-page order so broad entity histories
            // do not churn the bounded page cache.
            committed_refs.sort_unstable();
            for fr in committed_refs {
                facts.push(resolve_fact_ref(&d, fr)?);
            }
        }

        Ok(facts)
    }

    /// Get every fact record (assertions and retractions, all valid-time
    /// windows) for one exact EAV triple. Index-driven via the entity scan;
    /// used by `(forget ...)` to discover the windows to close.
    pub(crate) fn facts_for_triple(
        &self,
        entity_id: &EntityId,
        attribute: &Attribute,
        value: &Value,
    ) -> Result<Vec<Fact>> {
        let value_bytes = encode_value(value);
        Ok(self
            .get_facts_by_entity(entity_id)?
            .into_iter()
            .filter(|f| &f.attribute == attribute && encode_value(&f.value) == value_bytes)
            .collect())
    }

    /// Get all facts for a specific attribute (index-driven).
    pub(crate) fn get_facts_by_attribute(&self, attribute: &Attribute) -> Result<Vec<Fact>> {
        use crate::storage::index::AevtKey;
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());

        // Fallback: no index
        if d.pending_indexes.aevt.is_empty() && d.committed_index_reader.is_none() {
            drop(d);
            return Ok(self
                .get_all_facts()?
                .into_iter()
                .filter(|f| &f.attribute == attribute)
                .collect());
        }

        let start = AevtKey {
            attribute: attribute.clone(),
            entity: uuid::Uuid::nil(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end_opt: Option<AevtKey> = next_string_prefix(attribute).map(|next_attr| AevtKey {
            attribute: next_attr,
            entity: uuid::Uuid::nil(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        });

        let mut facts = Vec::new();

        // Pending
        let pending_range: Vec<FactRef> = match &end_opt {
            Some(end) => d
                .pending_indexes
                .aevt
                .range(start.clone()..end.clone())
                .filter(|(k, _)| k.attribute == *attribute)
                .map(|(_, &r)| r)
                .collect(),
            None => d
                .pending_indexes
                .aevt
                .range(start.clone()..)
                .take_while(|(k, _)| k.attribute == *attribute)
                .map(|(_, &r)| r)
                .collect(),
        };
        for fr in pending_range {
            facts.push(resolve_fact_ref(&d, fr)?);
        }

        // Committed
        if let Some(reader) = &d.committed_index_reader {
            let mut committed_refs = reader.range_scan_aevt(&start, end_opt.as_ref())?;
            // AEVT key order can revisit the same packed fact page many times.
            // Physical order makes each page cache-resident while all of its
            // referenced slots are decoded. Net-assertion and Datalog result
            // semantics are order-independent.
            committed_refs.sort_unstable();
            for fr in committed_refs {
                let fact = resolve_fact_ref(&d, fr)?;
                if &fact.attribute == attribute {
                    facts.push(fact);
                }
            }
        }

        Ok(facts)
    }

    /// Visit the net-asserted current view for one attribute without building
    /// an attribute-sized `Vec<Fact>`. AEVT keeps all history for one entity
    /// adjacent, so reducer memory is bounded by that entity's distinct
    /// values/windows rather than total matching entities.
    pub(crate) fn visit_current_attribute_values(
        &self,
        attribute: &Attribute,
        as_of: Option<&AsOf>,
        valid_time: CurrentValidTime,
        visit: &mut dyn FnMut(EntityId, &Value) -> Result<()>,
    ) -> Result<()> {
        let d = self.data.read().unwrap_or_else(|error| error.into_inner());
        let start = AevtKey {
            attribute: attribute.clone(),
            entity: uuid::Uuid::nil(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let end = next_string_prefix(attribute).map(|attribute| AevtKey {
            attribute,
            entity: uuid::Uuid::nil(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        });
        let pending: Vec<(AevtKey, FactRef)> = match &end {
            Some(end) => d
                .pending_indexes
                .aevt
                .range(start.clone()..end.clone())
                .map(|(key, fact_ref)| (key.clone(), *fact_ref))
                .collect(),
            None => d
                .pending_indexes
                .aevt
                .range(start.clone()..)
                .take_while(|(key, _)| key.attribute == *attribute)
                .map(|(key, fact_ref)| (key.clone(), *fact_ref))
                .collect(),
        };
        let mut pending = pending.into_iter().peekable();
        let mut current_entity = None;
        let mut values = std::collections::HashMap::<Vec<u8>, CurrentValueState>::new();

        let mut flush = |entity: Option<EntityId>,
                         values: &mut std::collections::HashMap<Vec<u8>, CurrentValueState>|
         -> Result<()> {
            let Some(entity) = entity else { return Ok(()) };
            for (encoded, state) in values.drain() {
                for ((valid_from, valid_to), (assert_tx, fact_ref)) in state.assertions {
                    let scoped_retract = state
                        .max_scoped_retract_tx
                        .get(&(valid_from, valid_to))
                        .copied()
                        .unwrap_or(0);
                    if assert_tx <= state.max_unscoped_retract_tx.max(scoped_retract) {
                        continue;
                    }
                    if matches!(valid_time, CurrentValidTime::At(at) if !(valid_from <= at && at < valid_to))
                    {
                        continue;
                    }
                    let value = if encoded.first() == Some(&0x03) {
                        resolve_fact_ref(&d, fact_ref)?.value
                    } else {
                        decode_index_value(&encoded)?
                    };
                    visit(entity, &value)?;
                }
            }
            Ok(())
        };

        let mut accept = |key: &AevtKey, fact_ref: FactRef| -> Result<()> {
            if !entry_visible_as_of(key, as_of) {
                return Ok(());
            }
            if current_entity.is_some_and(|entity| entity != key.entity) {
                flush(current_entity.take(), &mut values)?;
            }
            current_entity = Some(key.entity);
            let state = values.entry(key.value_bytes.clone()).or_default();
            if key.asserted {
                state
                    .assertions
                    .entry((key.valid_from, key.valid_to))
                    .and_modify(|winner| {
                        if key.tx_count > winner.0 {
                            *winner = (key.tx_count, fact_ref);
                        }
                    })
                    .or_insert((key.tx_count, fact_ref));
            } else if key.valid_from == RETRACT_ALL_VALID_FROM && key.valid_to == VALID_TIME_FOREVER
            {
                state.max_unscoped_retract_tx = state.max_unscoped_retract_tx.max(key.tx_count);
            } else {
                state
                    .max_scoped_retract_tx
                    .entry((key.valid_from, key.valid_to))
                    .and_modify(|tx| *tx = (*tx).max(key.tx_count))
                    .or_insert(key.tx_count);
            }
            Ok(())
        };

        if let Some(reader) = &d.committed_index_reader {
            reader.visit_aevt_entries(&start, end.as_ref(), &mut |key, fact_ref| {
                while pending
                    .peek()
                    .is_some_and(|(pending_key, _)| pending_key < key)
                {
                    if let Some((pending_key, pending_ref)) = pending.next() {
                        accept(&pending_key, pending_ref)?;
                    }
                }
                accept(key, fact_ref)
            })?;
        }
        for (key, fact_ref) in pending {
            accept(&key, fact_ref)?;
        }
        flush(current_entity, &mut values)
    }
}

fn entry_visible_as_of(key: &AevtKey, as_of: Option<&AsOf>) -> bool {
    match as_of {
        None => true,
        Some(AsOf::Counter(counter)) => key.tx_count <= *counter,
        Some(AsOf::Timestamp(timestamp)) => key.tx_id <= u64::try_from(*timestamp).unwrap_or(0),
        Some(AsOf::Slot(_)) => false,
    }
}

fn decode_index_value(encoded: &[u8]) -> Result<Value> {
    let payload = encoded
        .get(1..)
        .ok_or_else(|| anyhow::anyhow!("empty encoded index value"))?;
    match encoded.first().copied() {
        Some(0x00) if payload.is_empty() => Ok(Value::Null),
        Some(0x01) if payload.len() == 1 => Ok(Value::Boolean(payload.first() == Some(&1))),
        Some(0x02) if payload.len() == 8 => {
            let bits = u64::from_be_bytes(payload.try_into()?);
            Ok(Value::Integer((bits ^ 0x8000_0000_0000_0000).cast_signed()))
        }
        Some(0x04) => Ok(Value::String(std::str::from_utf8(payload)?.to_owned())),
        Some(0x05) => Ok(Value::Keyword(std::str::from_utf8(payload)?.to_owned())),
        Some(0x06) if payload.len() == 16 => {
            Ok(Value::Ref(uuid::Uuid::from_bytes(payload.try_into()?)))
        }
        Some(0x03) => anyhow::bail!("float index values require exact fact resolution"),
        Some(tag) => anyhow::bail!("malformed encoded index value tag 0x{tag:02x}"),
        None => anyhow::bail!("empty encoded index value"),
    }
}

/// Test-only helpers on FactStorage: for use in tests, not the production query path.
#[cfg(test)]
impl FactStorage {
    /// Get all facts for a specific entity and attribute.
    ///
    /// Note: uses a full scan via `get_all_facts()` rather than an index-driven range scan.
    /// For index-driven lookups, use `get_facts_by_entity` and filter by attribute in the caller.
    pub(crate) fn get_facts_by_entity_attribute(
        &self,
        entity_id: &EntityId,
        attribute: &Attribute,
    ) -> Result<Vec<Fact>> {
        let all = self.get_all_facts()?;
        Ok(all
            .into_iter()
            .filter(|f| &f.entity == entity_id && &f.attribute == attribute)
            .collect())
    }
    /// Return all asserted facts valid at the given timestamp.
    ///
    /// A fact is valid at `ts` when `valid_from <= ts < valid_to` and it is asserted.
    pub(crate) fn get_facts_valid_at(&self, ts: i64) -> Result<Vec<Fact>> {
        let all = self.get_all_facts()?;
        let filtered = all
            .into_iter()
            .filter(|f| f.is_asserted() && f.valid_from <= ts && ts < f.valid_to)
            .collect();
        Ok(filtered)
    }

    /// Get the current value for an entity-attribute pair (test use only).
    pub(crate) fn get_current_value(
        &self,
        entity_id: &EntityId,
        attribute: &Attribute,
    ) -> Result<Option<Value>> {
        let relevant_facts = self.get_facts_by_entity_attribute(entity_id, attribute)?;
        let mut net = net_asserted_facts(relevant_facts);
        net.sort_by_key(|fact| std::cmp::Reverse(fact.tx_count));
        Ok(net.first().map(|f| f.value.clone()))
    }

    /// Get the count of all facts in storage (committed + pending). Test use only.
    pub(crate) fn fact_count(&self) -> usize {
        let d = self.data.read().unwrap();
        let committed_count = d
            .committed
            .as_ref()
            .and_then(|l| l.stream_all().ok())
            .map(|v| v.len())
            .unwrap_or(0);
        committed_count + d.facts.len()
    }

    /// Get the count of currently asserted facts. Test use only.
    pub(crate) fn asserted_fact_count(&self) -> usize {
        self.get_asserted_facts().map(|v| v.len()).unwrap_or(0)
    }

    /// Returns (eavt_len, aevt_len, avet_len, vaet_len). Test use only.
    pub(crate) fn index_counts(&self) -> (usize, usize, usize, usize) {
        let d = self.data.read().unwrap();
        (
            d.pending_indexes.eavt.len(),
            d.pending_indexes.aevt.len(),
            d.pending_indexes.avet.len(),
            d.pending_indexes.vaet.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fact_storage_transact() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        // Transact facts
        let tx_id = storage
            .transact(
                vec![
                    (
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (alice, ":person/age".to_string(), Value::Integer(30)),
                ],
                None,
            )
            .unwrap();

        // Verify facts were stored
        assert_eq!(storage.fact_count(), 2);
        assert_eq!(storage.asserted_fact_count(), 2);

        // Verify all facts have same tx_id
        let facts = storage.get_facts_by_entity(&alice).unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.tx_id == tx_id));
        assert!(facts.iter().all(|f| f.is_asserted()));
    }

    #[test]
    fn test_fact_storage_retract() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        // Assert a fact
        let _tx1 = storage
            .transact(
                vec![(
                    alice,
                    ":person/name".to_string(),
                    Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        // Retract the fact
        let (tx2, _) = storage
            .retract(vec![(
                alice,
                ":person/name".to_string(),
                Value::String("Alice".to_string()),
            )])
            .unwrap();

        // Both facts exist in storage (assertion + retraction)
        assert_eq!(storage.fact_count(), 2);
        // But only 1 is asserted
        assert_eq!(storage.asserted_fact_count(), 1);

        let facts = storage.get_facts_by_entity(&alice).unwrap();
        assert_eq!(facts.len(), 2);

        // Find the retraction
        let retraction = facts.iter().find(|f| f.tx_id == tx2).unwrap();
        assert!(retraction.is_retracted());
    }

    #[test]
    fn test_fact_storage_get_by_entity() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        storage
            .transact(
                vec![
                    (
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (
                        bob,
                        ":person/name".to_string(),
                        Value::String("Bob".to_string()),
                    ),
                ],
                None,
            )
            .unwrap();

        let alice_facts = storage.get_facts_by_entity(&alice).unwrap();
        assert_eq!(alice_facts.len(), 1);
        assert_eq!(alice_facts[0].value, Value::String("Alice".to_string()));

        let bob_facts = storage.get_facts_by_entity(&bob).unwrap();
        assert_eq!(bob_facts.len(), 1);
        assert_eq!(bob_facts[0].value, Value::String("Bob".to_string()));
    }

    #[test]
    fn test_fact_storage_get_by_attribute() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        storage
            .transact(
                vec![
                    (
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (alice, ":person/age".to_string(), Value::Integer(30)),
                    (
                        bob,
                        ":person/name".to_string(),
                        Value::String("Bob".to_string()),
                    ),
                ],
                None,
            )
            .unwrap();

        // Get all :person/name facts
        let name_facts = storage
            .get_facts_by_attribute(&":person/name".to_string())
            .unwrap();
        assert_eq!(name_facts.len(), 2);

        // Get all :person/age facts
        let age_facts = storage
            .get_facts_by_attribute(&":person/age".to_string())
            .unwrap();
        assert_eq!(age_facts.len(), 1);
    }

    #[test]
    fn test_fact_storage_get_current_value() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        // Set initial value
        storage
            .transact(
                vec![(
                    alice,
                    ":person/name".to_string(),
                    Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();

        // Update value
        storage
            .transact(
                vec![(
                    alice,
                    ":person/name".to_string(),
                    Value::String("Alice Smith".to_string()),
                )],
                None,
            )
            .unwrap();

        // Current value should be the most recent
        let current = storage
            .get_current_value(&alice, &":person/name".to_string())
            .unwrap();
        assert_eq!(current, Some(Value::String("Alice Smith".to_string())));

        // Retract "Alice Smith" specifically
        storage
            .retract(vec![(
                alice,
                ":person/name".to_string(),
                Value::String("Alice Smith".to_string()),
            )])
            .unwrap();

        // "Alice Smith" was retracted, but "Alice" is still asserted (value-level
        // retraction semantics: each distinct value is tracked independently).
        // get_current_value returns the highest-tx_count surviving asserted fact.
        let current = storage
            .get_current_value(&alice, &":person/name".to_string())
            .unwrap();
        assert_eq!(current, Some(Value::String("Alice".to_string())));

        // Now retract "Alice" as well — the attribute should have no asserted value.
        storage
            .retract(vec![(
                alice,
                ":person/name".to_string(),
                Value::String("Alice".to_string()),
            )])
            .unwrap();

        let current = storage
            .get_current_value(&alice, &":person/name".to_string())
            .unwrap();
        assert_eq!(current, None);
    }

    #[test]
    fn test_fact_storage_entity_references() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        // Alice is friends with Bob (using Ref)
        storage
            .transact(
                vec![
                    (
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (alice, ":friend".to_string(), Value::Ref(bob)),
                    (
                        bob,
                        ":person/name".to_string(),
                        Value::String("Bob".to_string()),
                    ),
                ],
                None,
            )
            .unwrap();

        // Get friendship
        let friendship_facts = storage
            .get_facts_by_entity_attribute(&alice, &":friend".to_string())
            .unwrap();
        assert_eq!(friendship_facts.len(), 1);
        assert_eq!(friendship_facts[0].value.as_ref(), Some(bob));
    }

    #[test]
    fn test_fact_storage_history_tracking() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        // Create multiple versions over time
        let tx1 = storage
            .transact(
                vec![(alice, ":person/age".to_string(), Value::Integer(30))],
                None,
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        let tx2 = storage
            .transact(
                vec![(alice, ":person/age".to_string(), Value::Integer(31))],
                None,
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        let tx3 = storage
            .transact(
                vec![(alice, ":person/age".to_string(), Value::Integer(32))],
                None,
            )
            .unwrap();

        // All versions are in history
        let history = storage
            .get_facts_by_entity_attribute(&alice, &":person/age".to_string())
            .unwrap();
        assert_eq!(history.len(), 3);

        // TxIds should be increasing (chronological)
        assert!(tx1 < tx2);
        assert!(tx2 < tx3);

        // Current value should be most recent
        let current = storage
            .get_current_value(&alice, &":person/age".to_string())
            .unwrap();
        assert_eq!(current, Some(Value::Integer(32)));
    }

    #[test]
    fn test_fact_storage_batch_transact() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        // Transact multiple facts at once
        let tx_id = storage
            .transact(
                vec![
                    (
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (alice, ":person/age".to_string(), Value::Integer(30)),
                    (
                        alice,
                        ":person/email".to_string(),
                        Value::String("alice@example.com".to_string()),
                    ),
                ],
                None,
            )
            .unwrap();

        // All facts should have same tx_id (atomic batch)
        let facts = storage.get_facts_by_entity(&alice).unwrap();
        assert_eq!(facts.len(), 3);
        assert!(facts.iter().all(|f| f.tx_id == tx_id));
    }

    // =========================================================================
    // Phase 4: tx_counter, load_fact, temporal query tests
    // =========================================================================

    #[test]
    fn test_tx_count_increments_per_call() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        storage
            .transact(
                vec![(
                    alice,
                    ":person/name".to_string(),
                    Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        storage
            .transact(
                vec![(alice, ":person/age".to_string(), Value::Integer(30))],
                None,
            )
            .unwrap();

        let facts = storage.get_all_facts().unwrap();
        let name_fact = facts
            .iter()
            .find(|f| f.attribute == ":person/name")
            .unwrap();
        let age_fact = facts.iter().find(|f| f.attribute == ":person/age").unwrap();

        assert_eq!(name_fact.tx_count, 1);
        assert_eq!(age_fact.tx_count, 2);
    }

    #[test]
    fn test_batch_facts_share_tx_count() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        storage
            .transact(
                vec![
                    (
                        alice,
                        ":person/name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (alice, ":person/age".to_string(), Value::Integer(30)),
                ],
                None,
            )
            .unwrap();

        let facts = storage.get_all_facts().unwrap();
        assert!(facts.iter().all(|f| f.tx_count == 1));
    }

    #[test]
    fn test_load_fact_preserves_tx_id_and_tx_count() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let entity = Uuid::new_v4();

        let original_fact = Fact::with_valid_time(
            entity,
            ":person/name".to_string(),
            Value::String("Alice".to_string()),
            12345_u64, // original tx_id
            7,         // original tx_count
            12345_i64,
            VALID_TIME_FOREVER,
        );

        storage.load_fact(original_fact.clone()).unwrap();

        let facts = storage.get_all_facts().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].tx_id, 12345);
        assert_eq!(facts[0].tx_count, 7);
    }

    #[test]
    fn test_get_facts_as_of_counter() {
        use crate::query::datalog::types::AsOf;
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        // tx_count = 1
        storage
            .transact(
                vec![(
                    alice,
                    ":person/name".to_string(),
                    Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        // tx_count = 2
        storage
            .transact(
                vec![(alice, ":person/age".to_string(), Value::Integer(30))],
                None,
            )
            .unwrap();

        // as-of tx 1: only name fact visible
        let snapshot = filter_facts_as_of(storage.get_all_facts().unwrap(), &AsOf::Counter(1));
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].attribute, ":person/name");
    }

    #[test]
    fn test_get_facts_valid_at() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();

        let opts = TransactOptions::new(
            Some(1672531200000_i64), // 2023-01-01
            Some(1685577600000_i64), // 2023-06-01
        );

        storage
            .transact(
                vec![(
                    alice,
                    ":employment/status".to_string(),
                    Value::Keyword(":active".to_string()),
                )],
                Some(opts),
            )
            .unwrap();

        // Valid on 2023-03-01 (inside range)
        let inside = storage.get_facts_valid_at(1677628800000_i64).unwrap();
        assert_eq!(inside.len(), 1);

        // Valid on 2024-01-01 (outside range)
        let outside = storage.get_facts_valid_at(1704067200000_i64).unwrap();
        assert_eq!(outside.len(), 0);
    }

    #[test]
    fn test_tx_counter_restored_after_load_fact() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let entity = Uuid::new_v4();

        // Load a fact with tx_count = 5 (simulating migration/load)
        let fact = Fact::with_valid_time(
            entity,
            ":a".to_string(),
            Value::Integer(1),
            1000,
            5,
            1000_i64,
            VALID_TIME_FOREVER,
        );
        storage.load_fact(fact).unwrap();
        storage.restore_tx_counter().unwrap();

        // Next transact should get tx_count = 6
        storage
            .transact(vec![(entity, ":b".to_string(), Value::Integer(2))], None)
            .unwrap();

        let facts = storage.get_all_facts().unwrap();
        let b_fact = facts.iter().find(|f| f.attribute == ":b").unwrap();
        assert_eq!(b_fact.tx_count, 6);
    }

    // =========================================================================
    // Phase 5: current_tx_count, allocate_tx_count helpers
    // =========================================================================

    #[test]
    fn test_current_tx_count_starts_at_zero() {
        let storage = FactStorage::new();
        assert_eq!(storage.current_tx_count(), 0);
    }

    #[test]
    fn test_current_tx_count_reflects_transacts() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        storage
            .transact(
                vec![(
                    alice,
                    ":name".to_string(),
                    Value::String("Alice".to_string()),
                )],
                None,
            )
            .unwrap();
        assert_eq!(storage.current_tx_count(), 1);
        storage
            .transact(vec![(alice, ":age".to_string(), Value::Integer(30))], None)
            .unwrap();
        assert_eq!(storage.current_tx_count(), 2);
    }

    #[test]
    fn test_allocate_tx_count_increments() {
        let storage = FactStorage::new();
        let c1 = storage.allocate_tx_count();
        let c2 = storage.allocate_tx_count();
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
        assert_eq!(storage.current_tx_count(), 2);
    }

    // =========================================================================
    // Phase 6.1: index population tests
    // =========================================================================

    #[test]
    fn test_indexes_populated_on_transact() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        storage
            .transact(
                vec![
                    (
                        alice,
                        ":name".to_string(),
                        Value::String("Alice".to_string()),
                    ),
                    (alice, ":friend".to_string(), Value::Ref(bob)),
                ],
                None,
            )
            .unwrap();
        let (eavt, aevt, avet, vaet) = storage.index_counts();
        assert_eq!(eavt, 2);
        assert_eq!(aevt, 2);
        assert_eq!(avet, 2);
        assert_eq!(vaet, 1, "Only Ref values go into VAET");
    }

    #[test]
    fn test_slot_index_is_zero_in_6_1() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let e = Uuid::new_v4();
        storage
            .transact(vec![(e, ":x".to_string(), Value::Integer(1))], None)
            .unwrap();
        let (eavt, _, _, _) = storage.index_counts();
        assert_eq!(eavt, 1);
    }

    #[test]
    fn test_load_fact_populates_indexes() {
        use uuid::Uuid;

        let storage = FactStorage::new();
        let e = Uuid::new_v4();
        let fact = crate::graph::types::Fact::with_valid_time(
            e,
            ":name".to_string(),
            Value::String("Test".to_string()),
            0,
            1,
            0,
            crate::graph::types::VALID_TIME_FOREVER,
        );
        storage.load_fact(fact).unwrap();
        storage.restore_tx_counter().unwrap();
        let (eavt, _, _, _) = storage.index_counts();
        assert_eq!(eavt, 1);
    }

    // =========================================================================
    // Phase 6.2: CommittedFactReader integration tests
    // =========================================================================

    #[test]
    fn test_committed_reader_resolves_facts() {
        use crate::storage::CommittedFactReader;
        use crate::storage::index::{FactRef, Indexes};
        use std::sync::Arc;
        use uuid::Uuid;

        /// Mock loader: resolves FactRefs by slot_index into a fixed Vec<Fact>.
        struct MockLoader {
            facts: Vec<Fact>,
        }
        impl CommittedFactReader for MockLoader {
            fn resolve(&self, fr: FactRef) -> anyhow::Result<Fact> {
                self.facts
                    .get(fr.slot_index as usize)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("MockLoader: no fact at slot {}", fr.slot_index))
            }
            fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
                Ok(self.facts.clone())
            }
            fn committed_page_count(&self) -> u64 {
                1
            }
        }

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        let committed_fact = Fact::with_valid_time(
            alice,
            ":name".to_string(),
            Value::String("Alice".to_string()),
            0,
            1,
            0,
            VALID_TIME_FOREVER,
        );
        let loader = Arc::new(MockLoader {
            facts: vec![committed_fact.clone()],
        });

        // Insert a committed FactRef into the indexes (page_id > 0 → committed path).
        let mut indexes = Indexes::new();
        indexes.insert(
            &committed_fact,
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
        );
        storage.replace_pending_indexes(indexes);
        storage.set_committed_reader(loader);

        // get_facts_by_entity must resolve via CommittedFactReader (EAVT range scan).
        let entity_facts = storage.get_facts_by_entity(&alice).unwrap();
        assert_eq!(
            entity_facts.len(),
            1,
            "EAVT range scan should resolve committed fact"
        );
        assert_eq!(entity_facts[0].entity, alice);
        assert_eq!(entity_facts[0].attribute, ":name");

        // get_facts_by_attribute must resolve via CommittedFactReader (AEVT range scan).
        let attr_facts = storage
            .get_facts_by_attribute(&":name".to_string())
            .unwrap();
        assert_eq!(
            attr_facts.len(),
            1,
            "AEVT range scan should resolve committed fact"
        );
        assert_eq!(attr_facts[0].value, Value::String("Alice".to_string()));

        // get_all_facts must include committed facts via stream_all().
        let all = storage.get_all_facts().unwrap();
        assert_eq!(all.len(), 1, "get_all_facts must include committed facts");
        assert_eq!(all[0].entity, alice);

        // Transaction-time filtering should see committed facts.
        let as_of = filter_facts_as_of(
            storage.get_all_facts().unwrap(),
            &crate::query::datalog::types::AsOf::Counter(10),
        );
        assert_eq!(
            as_of.len(),
            1,
            "transaction-time filtering should include committed facts"
        );

        // get_facts_valid_at should see committed facts valid at time 0.
        let valid_at = storage.get_facts_valid_at(0).unwrap();
        assert_eq!(
            valid_at.len(),
            1,
            "get_facts_valid_at should include committed facts"
        );
    }

    #[test]
    fn test_for_each_fact_streams_committed_without_stream_all() {
        use crate::storage::CommittedFactReader;
        use crate::storage::index::FactRef;
        use std::sync::Arc;
        use uuid::Uuid;

        struct StreamingOnlyLoader {
            facts: Vec<Fact>,
        }
        impl CommittedFactReader for StreamingOnlyLoader {
            fn resolve(&self, fr: FactRef) -> anyhow::Result<Fact> {
                self.facts
                    .get(fr.slot_index as usize)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("slot not found"))
            }

            fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
                anyhow::bail!("stream_all should not be called by for_each_fact")
            }

            fn for_each_fact(
                &self,
                visit: &mut dyn FnMut(Fact) -> anyhow::Result<()>,
            ) -> anyhow::Result<()> {
                for fact in self.facts.iter().cloned() {
                    visit(fact)?;
                }
                Ok(())
            }

            fn committed_page_count(&self) -> u64 {
                1
            }
        }

        let committed = Fact::with_valid_time(
            Uuid::new_v4(),
            ":committed".to_string(),
            Value::Integer(1),
            1000,
            1,
            1000,
            VALID_TIME_FOREVER,
        );
        let pending = Fact::with_valid_time(
            Uuid::new_v4(),
            ":pending".to_string(),
            Value::Integer(2),
            2000,
            2,
            2000,
            VALID_TIME_FOREVER,
        );

        let storage = FactStorage::new();
        storage.set_committed_reader(Arc::new(StreamingOnlyLoader {
            facts: vec![committed],
        }));
        storage
            .load_fact(pending)
            .expect("pending fact should load");

        let mut attributes = Vec::new();
        storage
            .for_each_fact(|fact| {
                attributes.push(fact.attribute);
                Ok(())
            })
            .expect("streaming fact visit should succeed");

        assert_eq!(
            attributes,
            vec![":committed".to_string(), ":pending".to_string()],
            "streaming visitor must preserve committed-then-pending order"
        );
    }

    #[test]
    fn test_for_each_fact_since_never_full_scans_committed() {
        use crate::storage::CommittedFactReader;
        use crate::storage::index::FactRef;
        use std::sync::Arc;
        use uuid::Uuid;

        // NoFullScanFactReader discipline: both full-stream entry points bail,
        // so this test proves FactStorage::for_each_fact_since routes tail
        // reads through the reader's since-aware path only.
        struct SinceOnlyLoader {
            facts: Vec<Fact>,
        }
        impl CommittedFactReader for SinceOnlyLoader {
            fn resolve(&self, fr: FactRef) -> anyhow::Result<Fact> {
                self.facts
                    .get(fr.slot_index as usize)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("slot not found"))
            }

            fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
                anyhow::bail!("committed full scan must not run for a since-tail read")
            }

            fn for_each_fact(
                &self,
                _visit: &mut dyn FnMut(Fact) -> anyhow::Result<()>,
            ) -> anyhow::Result<()> {
                anyhow::bail!("committed full stream must not run for a since-tail read")
            }

            fn for_each_fact_since(
                &self,
                since_tx_count: u64,
                visit: &mut dyn FnMut(Fact) -> anyhow::Result<()>,
            ) -> anyhow::Result<()> {
                for fact in self.facts.iter() {
                    if fact.tx_count > since_tx_count {
                        visit(fact.clone())?;
                    }
                }
                Ok(())
            }

            fn committed_page_count(&self) -> u64 {
                1
            }
        }

        let committed_old = Fact::with_valid_time(
            Uuid::new_v4(),
            ":committed/old".to_string(),
            Value::Integer(1),
            1000,
            1,
            1000,
            VALID_TIME_FOREVER,
        );
        let committed_new = Fact::with_valid_time(
            Uuid::new_v4(),
            ":committed/new".to_string(),
            Value::Integer(2),
            2000,
            2,
            2000,
            VALID_TIME_FOREVER,
        );
        let pending_old = Fact::with_valid_time(
            Uuid::new_v4(),
            ":pending/old".to_string(),
            Value::Integer(3),
            3000,
            1,
            3000,
            VALID_TIME_FOREVER,
        );
        let pending_new = Fact::with_valid_time(
            Uuid::new_v4(),
            ":pending/new".to_string(),
            Value::Integer(4),
            4000,
            3,
            4000,
            VALID_TIME_FOREVER,
        );

        let storage = FactStorage::new();
        storage.set_committed_reader(Arc::new(SinceOnlyLoader {
            facts: vec![committed_old, committed_new],
        }));
        storage
            .load_fact(pending_old)
            .expect("pending fact should load");
        storage
            .load_fact(pending_new)
            .expect("pending fact should load");

        let mut attributes = Vec::new();
        storage
            .for_each_fact_since(1, |fact| {
                attributes.push(fact.attribute);
                Ok(())
            })
            .expect("since-tail visit should succeed without a full scan");

        assert_eq!(
            attributes,
            vec![":committed/new".to_string(), ":pending/new".to_string()],
            "since-tail must filter both committed and pending layers in order"
        );
    }

    #[test]
    fn test_committed_reader_combined_with_pending() {
        use crate::storage::CommittedFactReader;
        use crate::storage::index::{FactRef, Indexes};
        use std::sync::Arc;
        use uuid::Uuid;

        struct MockLoader {
            facts: Vec<Fact>,
        }
        impl CommittedFactReader for MockLoader {
            fn resolve(&self, fr: FactRef) -> anyhow::Result<Fact> {
                self.facts
                    .get(fr.slot_index as usize)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("slot {} not found", fr.slot_index))
            }
            fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
                Ok(self.facts.clone())
            }
            fn committed_page_count(&self) -> u64 {
                1
            }
        }

        let storage = FactStorage::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        // One committed fact (Alice, on disk)
        let alice_fact = Fact::with_valid_time(
            alice,
            ":name".to_string(),
            Value::String("Alice".to_string()),
            1000,
            1,
            1000,
            VALID_TIME_FOREVER,
        );
        let loader = Arc::new(MockLoader {
            facts: vec![alice_fact.clone()],
        });
        let mut indexes = Indexes::new();
        indexes.insert(
            &alice_fact,
            FactRef {
                page_id: 1,
                slot_index: 0,
            },
        );
        storage.replace_pending_indexes(indexes);
        storage.set_committed_reader(loader);

        // Restore tx_counter so pending transact gets tx_count = 2
        storage.restore_tx_counter_from(1);

        // One pending fact (Bob, in memory)
        storage
            .transact(
                vec![(bob, ":name".to_string(), Value::String("Bob".to_string()))],
                None,
            )
            .unwrap();

        // get_all_facts should see both
        let all = storage.get_all_facts().unwrap();
        assert_eq!(
            all.len(),
            2,
            "Both committed and pending facts must be visible"
        );

        // get_facts_by_attribute uses AEVT — must also see both
        let name_facts = storage
            .get_facts_by_attribute(&":name".to_string())
            .unwrap();
        assert_eq!(
            name_facts.len(),
            2,
            "AEVT scan must return both committed and pending facts"
        );
    }

    #[test]
    fn test_post_checkpoint_clear_clears_indexes() {
        use uuid::Uuid;
        let storage = FactStorage::new();
        let e = Uuid::new_v4();
        storage
            .transact(
                vec![(e, ":name".to_string(), Value::String("Alice".to_string()))],
                None,
            )
            .unwrap();

        assert_eq!(
            storage.pending_index_counts().0,
            1,
            "one pending EAVT entry"
        );
        storage.post_checkpoint_clear();
        assert_eq!(
            storage.pending_index_counts().0,
            0,
            "pending indexes cleared"
        );
        assert_eq!(
            storage.get_pending_facts().len(),
            0,
            "pending facts cleared"
        );
    }

    #[test]
    fn test_set_committed_index_reader_accepted() {
        use crate::storage::CommittedIndexReader;
        use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, VaetKey};
        use std::sync::Arc;

        struct NoopReader;
        impl CommittedIndexReader for NoopReader {
            fn range_scan_eavt(
                &self,
                _: &EavtKey,
                _: Option<&EavtKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(vec![])
            }
            fn range_scan_aevt(
                &self,
                _: &AevtKey,
                _: Option<&AevtKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(vec![])
            }
            fn range_scan_avet(
                &self,
                _: &AvetKey,
                _: Option<&AvetKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(vec![])
            }
            fn range_scan_vaet(
                &self,
                _: &VaetKey,
                _: Option<&VaetKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(vec![])
            }
        }

        let storage = FactStorage::new();
        // Verify set_committed_index_reader wires the reader (no panic, usable storage)
        storage.set_committed_index_reader(Arc::new(NoopReader));
        // After setting, get_facts_by_entity should use the index path without panicking
        let result = storage.get_facts_by_entity(&uuid::Uuid::nil());
        assert!(
            result.is_ok(),
            "storage should be usable after setting committed index reader"
        );
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn attribute_lookup_resolves_committed_refs_in_physical_page_order() {
        use crate::storage::index::{AevtKey, AvetKey, EavtKey, FactRef, VaetKey};
        use crate::storage::{CommittedFactReader, CommittedIndexReader};
        use std::sync::Arc;

        struct Loader;
        impl CommittedFactReader for Loader {
            fn resolve(&self, fact_ref: FactRef) -> anyhow::Result<Fact> {
                Ok(Fact::with_valid_time(
                    uuid::Uuid::from_u128(u128::from(fact_ref.page_id)),
                    ":physical".to_string(),
                    Value::Integer(i64::try_from(fact_ref.page_id)?),
                    1,
                    fact_ref.page_id,
                    1,
                    VALID_TIME_FOREVER,
                ))
            }

            fn stream_all(&self) -> anyhow::Result<Vec<Fact>> {
                Ok(Vec::new())
            }

            fn committed_page_count(&self) -> u64 {
                4
            }
        }

        struct LogicalOrderIndex;
        impl CommittedIndexReader for LogicalOrderIndex {
            fn range_scan_eavt(
                &self,
                _: &EavtKey,
                _: Option<&EavtKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(Vec::new())
            }

            fn range_scan_aevt(
                &self,
                _: &AevtKey,
                _: Option<&AevtKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(vec![
                    FactRef {
                        page_id: 3,
                        slot_index: 0,
                    },
                    FactRef {
                        page_id: 1,
                        slot_index: 0,
                    },
                    FactRef {
                        page_id: 2,
                        slot_index: 0,
                    },
                ])
            }

            fn range_scan_avet(
                &self,
                _: &AvetKey,
                _: Option<&AvetKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(Vec::new())
            }

            fn range_scan_vaet(
                &self,
                _: &VaetKey,
                _: Option<&VaetKey>,
            ) -> anyhow::Result<Vec<FactRef>> {
                Ok(Vec::new())
            }
        }

        let storage = FactStorage::new();
        storage.set_committed_reader(Arc::new(Loader));
        storage.set_committed_index_reader(Arc::new(LogicalOrderIndex));

        let values: Vec<Value> = storage
            .get_facts_by_attribute(&":physical".to_string())
            .expect("attribute lookup should resolve")
            .into_iter()
            .map(|fact| fact.value)
            .collect();
        assert_eq!(
            values,
            vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)],
            "committed refs should be resolved while each packed page is local"
        );
    }

    #[test]
    fn test_load_fact_prevents_duplicates() {
        use crate::graph::types::Value;

        let storage = FactStorage::new();

        let entity = uuid::Uuid::new_v4();
        let attr = ":test/attr".to_string();
        let value = Value::Integer(42);

        let fact1 = Fact::new(entity, attr.clone(), value.clone(), 1);
        let fact1_key = (entity, attr.clone(), value.clone());

        let fact2 = Fact::new(uuid::Uuid::new_v4(), attr.clone(), value.clone(), 1);

        // Different entities - should load both
        assert!(storage.load_fact(fact1).unwrap());
        assert!(storage.load_fact(fact2).unwrap());

        let count = storage.fact_count();
        assert_eq!(count, 2);

        // Try loading the exact same fact again - should be rejected as duplicate
        let fact1_dup = Fact::new(fact1_key.0, fact1_key.1, fact1_key.2, 1);
        assert!(!storage.load_fact(fact1_dup).unwrap());

        // Count should remain the same
        assert_eq!(storage.fact_count(), 2);
    }

    #[test]
    fn test_load_fact_duplicate_detection_includes_asserted() {
        let storage = FactStorage::new();
        let entity = uuid::Uuid::new_v4();
        let attr = ":test/attr".to_string();
        let value = Value::Integer(42);

        // Load an asserted fact
        let mut fact1 = Fact::new(entity, attr.clone(), value.clone(), 1);
        fact1.asserted = true;
        assert!(storage.load_fact(fact1).unwrap());

        // Load a retraction for the same entity/attr/value/tx_count but different asserted
        let mut fact2 = Fact::new(entity, attr.clone(), value.clone(), 1);
        fact2.asserted = false;
        // Should NOT be deduplicated - different asserted values should both survive
        assert!(storage.load_fact(fact2).unwrap());

        // Both facts should be present
        assert_eq!(storage.fact_count(), 2);
    }

    #[test]
    fn test_indexes_preserve_same_ref_assert_and_retract_identity() -> Result<()> {
        let storage = FactStorage::new();
        let entity = uuid::Uuid::new_v4();
        let target = uuid::Uuid::new_v4();
        let attr = ":edge/to".to_string();
        let mut asserted = Fact::with_valid_time(
            entity,
            attr.clone(),
            Value::Ref(target),
            100,
            7,
            0,
            VALID_TIME_FOREVER,
        );
        asserted.asserted = true;
        let mut retracted = Fact::with_valid_time(
            entity,
            attr,
            Value::Ref(target),
            100,
            7,
            0,
            VALID_TIME_FOREVER,
        );
        retracted.asserted = false;

        assert!(storage.load_fact(asserted)?);
        assert!(storage.load_fact(retracted)?);
        assert_eq!(storage.index_counts(), (2, 2, 2, 2));
        assert_eq!(
            storage.get_facts_by_entity(&entity)?.len(),
            2,
            "entity index lookup must preserve both ledger facts"
        );

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Unit tests for net_asserted_facts directly
    // -------------------------------------------------------------------------

    /// Helper: build an asserted fact with explicit tx_count and valid window.
    fn make_assert(
        entity: uuid::Uuid,
        attr: &str,
        value: Value,
        tx_count: u64,
        valid_from: i64,
        valid_to: i64,
    ) -> Fact {
        Fact {
            entity,
            attribute: attr.to_string(),
            value,
            tx_id: tx_count,
            tx_count,
            valid_from,
            valid_to,
            asserted: true,
        }
    }

    /// Helper: build a retraction with explicit tx_count and default valid window.
    fn make_retract(entity: uuid::Uuid, attr: &str, value: Value, tx_count: u64) -> Fact {
        Fact {
            entity,
            attribute: attr.to_string(),
            value,
            tx_id: tx_count,
            tx_count,
            valid_from: RETRACT_ALL_VALID_FROM,
            valid_to: VALID_TIME_FOREVER,
            asserted: false,
        }
    }

    /// Helper: build a scoped retraction with explicit tx_count and valid window.
    fn make_scoped_retract(
        entity: uuid::Uuid,
        attr: &str,
        value: Value,
        tx_count: u64,
        valid_from: i64,
        valid_to: i64,
    ) -> Fact {
        Fact {
            entity,
            attribute: attr.to_string(),
            value,
            tx_id: tx_count,
            tx_count,
            valid_from,
            valid_to,
            asserted: false,
        }
    }

    /// Multiple retractions of the same EAV: `max_retract_tx` must be the
    /// global maximum, not just the first retraction seen.
    ///
    /// Timeline:
    ///   tx=1  assert W1        (should be wiped by retraction at tx=5)
    ///   tx=3  retract          (max_retract so far = 3)
    ///   tx=4  assert W1        (should survive — tx 4 > 3, BUT a later retraction at tx=5 wipes it)
    ///   tx=5  retract          (max_retract = 5, wipes tx=4 assertion)
    ///   tx=6  assert W1        (should survive — tx 6 > 5)
    #[test]
    fn test_net_asserted_multiple_retractions_max_wins() {
        let entity = uuid::Uuid::new_v4();
        let attr = ":salary";
        let value = Value::Integer(100_000);
        let w = (1_000_i64, VALID_TIME_FOREVER);

        let facts = vec![
            make_assert(entity, attr, value.clone(), 1, w.0, w.1),
            make_retract(entity, attr, value.clone(), 3),
            make_assert(entity, attr, value.clone(), 4, w.0, w.1),
            make_retract(entity, attr, value.clone(), 5),
            make_assert(entity, attr, value.clone(), 6, w.0, w.1),
        ];

        let result = net_asserted_facts(facts);
        assert_eq!(
            result.len(),
            1,
            "only the post-retraction assertion should survive"
        );
        assert_eq!(result[0].tx_count, 6);
    }

    #[test]
    fn test_net_asserted_preserves_canonical_float_identity() {
        let entity = uuid::Uuid::new_v4();
        let attr = ":measure";
        let window = (1_000_i64, VALID_TIME_FOREVER);
        let facts = vec![
            make_assert(entity, attr, Value::Float(f64::NAN), 1, window.0, window.1),
            make_retract(
                entity,
                attr,
                Value::Float(f64::from_bits(0x7FF0_0000_0000_0001)),
                2,
            ),
            make_assert(entity, attr, Value::Float(-0.0), 3, window.0, window.1),
            make_assert(entity, attr, Value::Float(0.0), 4, window.0, window.1),
        ];

        let result = net_asserted_facts(facts);
        assert_eq!(
            result.len(),
            2,
            "NaNs must coalesce while signed zero stays distinct"
        );
        assert!(result.iter().any(|fact| {
            fact.value
                .as_float()
                .is_some_and(|value| value == 0.0 && value.is_sign_negative())
        }));
        assert!(result.iter().any(|fact| {
            fact.value
                .as_float()
                .is_some_and(|value| value == 0.0 && value.is_sign_positive())
        }));
    }

    /// A single retraction wipes all valid-time windows of the same EAV triple,
    /// not just the window that was explicitly retracted.
    ///
    /// Timeline:
    ///   tx=1  assert W1 (2020–2022)
    ///   tx=2  assert W2 (2024–2026)
    ///   tx=3  retract          → both windows must disappear
    #[test]
    fn test_net_asserted_retraction_wipes_all_windows() {
        let entity = uuid::Uuid::new_v4();
        let attr = ":salary";
        let value = Value::Integer(100_000);

        let facts = vec![
            make_assert(
                entity,
                attr,
                value.clone(),
                1,
                1_577_836_800_000,
                1_640_995_200_000,
            ),
            make_assert(
                entity,
                attr,
                value.clone(),
                2,
                1_704_067_200_000,
                VALID_TIME_FOREVER,
            ),
            make_retract(entity, attr, value.clone(), 3),
        ];

        let result = net_asserted_facts(facts);
        assert_eq!(
            result.len(),
            0,
            "retraction should wipe all windows for the EAV triple"
        );
    }

    /// A scoped retraction only wipes the matching valid-time window of the
    /// same EAV triple. Other windows with the same E/A/V must survive.
    #[test]
    fn test_net_asserted_scoped_retraction_only_wipes_matching_window() {
        let entity = uuid::Uuid::new_v4();
        let attr = ":salary";
        let value = Value::Integer(100_000);
        let w1 = (1_577_836_800_000, 1_640_995_200_000);
        let w2 = (1_704_067_200_000, VALID_TIME_FOREVER);

        let facts = vec![
            make_assert(entity, attr, value.clone(), 1, w1.0, w1.1),
            make_assert(entity, attr, value.clone(), 2, w2.0, w2.1),
            make_scoped_retract(entity, attr, value.clone(), 3, w1.0, w1.1),
        ];

        let result = net_asserted_facts(facts);
        assert_eq!(
            result.len(),
            1,
            "scoped retraction should only remove one valid-time window"
        );
        assert_eq!(result[0].valid_from, w2.0);
        assert_eq!(result[0].valid_to, w2.1);
    }
}
