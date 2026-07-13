use crate::graph::pending_overlay::{PendingFactId, PendingOverlay};
use crate::graph::types::{
    Attribute, EntityId, Fact, RETRACT_ALL_VALID_FROM, TransactOptions, TxId, VALID_TIME_FOREVER,
    Value, tx_id_now,
};
use crate::query::datalog::types::AsOf;
use crate::storage::index::{
    AevtKey, CurrentAevtEntryRef, CurrentEavtEntryRef, EavtKey, FactRef, encode_value,
};
use anyhow::Result;
#[cfg(any(test, feature = "bench-internals"))]
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Accounted owned memory for one pending in-memory container.
///
/// `inline_payload_bytes` is exact for the `Vec<Fact>` allocation, a lower
/// bound based on reported capacity for `HashSet`, and logical entry payload
/// only for `BTreeMap` indexes. B-tree node headers, hash control bytes, and
/// allocator metadata are intentionally left to the RSS residual.
#[cfg(any(test, feature = "bench-internals"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMemoryComponent {
    /// Number of live entries.
    pub entries: u64,
    /// Container capacity where exposed; otherwise equal to `entries`.
    pub capacity: u64,
    /// Inline entry payload reserved or logically occupied by the container.
    pub inline_payload_bytes: u64,
    /// Heap capacity owned by attribute strings.
    pub owned_attribute_bytes: u64,
    /// Separately allocated attribute buffers.
    pub owned_attribute_allocations: u64,
    /// Heap capacity owned by encoded or string-like values.
    pub owned_value_bytes: u64,
    /// Separately allocated encoded or string-like value buffers.
    pub owned_value_allocations: u64,
    /// Sum of the accounted byte fields above.
    pub accounted_bytes: u64,
}

/// Live pending-memory ownership snapshot.
///
/// Exposed only to tests and the non-default `bench-internals` feature. This
/// is diagnostic evidence, not a stable public API or an on-disk format.
#[cfg(any(test, feature = "bench-internals"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMemoryDiagnostics {
    /// Pending fact log allocation and nested strings.
    pub facts: PendingMemoryComponent,
    /// Duplicate-detection hash set and its separately owned key bytes.
    pub duplicate_keys: PendingMemoryComponent,
    /// Pending EAVT index payload.
    pub eavt: PendingMemoryComponent,
    /// Pending AEVT index payload.
    pub aevt: PendingMemoryComponent,
    /// Pending AVET index payload.
    pub avet: PendingMemoryComponent,
    /// Pending VAET index payload (only `Value::Ref` facts).
    pub vaet: PendingMemoryComponent,
    /// Number of live sorted runs in EAVT, AEVT, AVET, and VAET order.
    pub index_run_counts: [u64; 4],
    /// Sum across all accounted live components.
    pub total_accounted_bytes: u64,
    /// Bytes not represented by the accounting: tree nodes, hash controls,
    /// allocator metadata/fragmentation, reader caches, and process runtime.
    pub excludes_container_and_allocator_overhead: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum CurrentValidTime {
    At(i64),
    Any,
}

#[derive(Default)]
struct CurrentValueState {
    max_unscoped_retract_tx: u64,
    max_scoped_retract_tx: std::collections::HashMap<(i64, i64), u64>,
    assertions: std::collections::HashMap<(i64, i64), (u64, CursorFactRef)>,
}

#[derive(Clone, Copy)]
enum CursorFactRef {
    Pending(PendingFactId),
    Committed(FactRef),
}

/// Repository-only counters for one selected-attribute current-view cursor.
///
/// The type exists only for tests and the non-default `bench-internals`
/// feature. It is not part of the default application API or file format.
#[cfg(any(test, feature = "bench-internals"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentAttributeCursorDiagnostics {
    /// Selected pending AEVT entries cloned into the cursor snapshot.
    pub selected_pending_entries: u64,
    /// Inline entry structs plus owned attribute/value bytes in that snapshot.
    pub selected_pending_snapshot_bytes: u64,
    /// Logical committed AEVT entries consumed inside the selected range.
    pub committed_entries_visited: u64,
    /// Logical pending AEVT entries consumed inside the selected range.
    pub pending_entries_visited: u64,
    /// Covering entries that required resolving the exact fact (currently Float).
    pub exact_fact_resolutions: u64,
    /// Current-view entity/value rows successfully emitted to the query sink.
    pub emitted_rows: u64,
    /// Maximum distinct encoded values retained for one entity.
    pub peak_entity_values: u64,
    /// Maximum assertion/scoped-retraction windows retained for one entity.
    pub peak_entity_windows: u64,
    /// Number of bounded cursor steps that yielded before completion.
    pub yield_count: u64,
    /// Number of calls that resumed a previously yielded cursor.
    pub resume_count: u64,
}

pub(crate) struct CurrentAttributeCursor {
    end: Option<AevtKey>,
    next_start: AevtKey,
    pending: Vec<PendingFactId>,
    pending_position: usize,
    current_entity: Option<EntityId>,
    values: std::collections::HashMap<Vec<u8>, CurrentValueState>,
    as_of: Option<AsOf>,
    valid_time: CurrentValidTime,
    committed_fact_reader: Option<Arc<dyn crate::storage::CommittedFactReader>>,
    committed_index_reader: Option<Arc<dyn crate::storage::CommittedIndexReader>>,
    publication_generation: u64,
    last_key: Option<AevtKey>,
    committed_complete: bool,
    complete: bool,
    yielded: bool,
    #[cfg(any(test, feature = "bench-internals"))]
    current_entity_windows: u64,
    #[cfg(any(test, feature = "bench-internals"))]
    diagnostics: CurrentAttributeCursorDiagnostics,
    #[cfg(any(test, feature = "bench-internals"))]
    diagnostics_slot: Arc<Mutex<Option<CurrentAttributeCursorDiagnostics>>>,
}

/// Resumable current-view reduction for one exact `(entity, attribute)` range.
pub(crate) struct CurrentEntityAttributeCursor {
    end: EavtKey,
    next_start: EavtKey,
    pending: Vec<PendingFactId>,
    pending_position: usize,
    values: std::collections::HashMap<Vec<u8>, CurrentValueState>,
    as_of: Option<AsOf>,
    valid_time: CurrentValidTime,
    committed_fact_reader: Option<Arc<dyn crate::storage::CommittedFactReader>>,
    committed_index_reader: Option<Arc<dyn crate::storage::CommittedIndexReader>>,
    publication_generation: u64,
    last_key: Option<EavtKey>,
    committed_complete: bool,
    complete: bool,
}

impl CurrentAttributeCursor {
    fn begin_step(&mut self) {
        if self.yielded {
            self.yielded = false;
            #[cfg(any(test, feature = "bench-internals"))]
            {
                self.diagnostics.resume_count = self.diagnostics.resume_count.saturating_add(1);
            }
        }
    }

    fn note_yield(&mut self) {
        self.yielded = true;
        #[cfg(any(test, feature = "bench-internals"))]
        {
            self.diagnostics.yield_count = self.diagnostics.yield_count.saturating_add(1);
        }
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn note_committed_entry(&mut self) {
        self.diagnostics.committed_entries_visited =
            self.diagnostics.committed_entries_visited.saturating_add(1);
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn note_pending_entry(&mut self) {
        self.diagnostics.pending_entries_visited =
            self.diagnostics.pending_entries_visited.saturating_add(1);
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn note_reducer_shape(&mut self, added_window: bool) {
        if added_window {
            self.current_entity_windows = self.current_entity_windows.saturating_add(1);
        }
        self.diagnostics.peak_entity_values = self
            .diagnostics
            .peak_entity_values
            .max(usize_to_u64(self.values.len()));
        self.diagnostics.peak_entity_windows = self
            .diagnostics
            .peak_entity_windows
            .max(self.current_entity_windows);
    }
}

#[cfg(any(test, feature = "bench-internals"))]
impl CurrentAttributeCursor {
    #[allow(dead_code)]
    pub(crate) fn diagnostics(&self) -> CurrentAttributeCursorDiagnostics {
        self.diagnostics
    }

    fn persist_diagnostics(&self) {
        let mut slot = self
            .diagnostics_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *slot = Some(self.diagnostics);
    }
}

#[cfg(any(test, feature = "bench-internals"))]
impl Drop for CurrentAttributeCursor {
    fn drop(&mut self) {
        self.persist_diagnostics();
    }
}

pub(crate) enum CurrentAttributeStep {
    #[cfg_attr(
        not(all(target_arch = "wasm32", feature = "browser")),
        allow(dead_code)
    )]
    Yielded {
        entries: usize,
    },
    Complete,
}

pub(crate) enum CurrentEntityAttributeStep {
    Yielded { entries: usize },
    Complete { entries: usize },
}

#[cfg(any(test, feature = "bench-internals"))]
fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(any(test, feature = "bench-internals"))]
fn pending_snapshot_bytes(pending: &Vec<PendingFactId>) -> u64 {
    usize_to_u64(
        pending
            .capacity()
            .saturating_mul(std::mem::size_of::<PendingFactId>()),
    )
}

#[cfg(any(test, feature = "bench-internals"))]
fn memory_component(
    entries: usize,
    capacity: usize,
    inline_payload_bytes: usize,
    owned_attribute_bytes: usize,
    owned_attribute_allocations: usize,
    owned_value_bytes: usize,
    owned_value_allocations: usize,
) -> PendingMemoryComponent {
    let inline_payload_bytes = usize_to_u64(inline_payload_bytes);
    let owned_attribute_bytes = usize_to_u64(owned_attribute_bytes);
    let owned_value_bytes = usize_to_u64(owned_value_bytes);
    PendingMemoryComponent {
        entries: usize_to_u64(entries),
        capacity: usize_to_u64(capacity),
        inline_payload_bytes,
        owned_attribute_bytes,
        owned_attribute_allocations: usize_to_u64(owned_attribute_allocations),
        owned_value_bytes,
        owned_value_allocations: usize_to_u64(owned_value_allocations),
        accounted_bytes: inline_payload_bytes
            .saturating_add(owned_attribute_bytes)
            .saturating_add(owned_value_bytes),
    }
}

#[cfg(any(test, feature = "bench-internals"))]
fn sum_component_bytes(components: &[PendingMemoryComponent]) -> u64 {
    components.iter().fold(0_u64, |total, component| {
        total.saturating_add(component.accounted_bytes)
    })
}

// ============================================================================
// Datalog Fact Storage (Phase 3+)
// ============================================================================

/// Private container that co-locates the fact list and all four indexes under
/// a single `RwLock`. This ensures facts and indexes are always updated together
/// without needing a second lock.
struct FactData {
    pending: PendingOverlay,
    /// Resolves committed (on-disk) FactRefs to Fact objects.
    /// None for in-memory databases or before load() is called.
    committed: Option<Arc<dyn crate::storage::CommittedFactReader>>,
    /// Provides bounded range scans over the four committed (on-disk) covering indexes.
    /// Set by `set_committed_index_reader()` after open/migration/checkpoint.
    committed_index_reader: Option<Arc<dyn crate::storage::CommittedIndexReader>>,
    /// Changes whenever a committed reader or pending publication is replaced.
    /// Resumable cursors capture this identity and fail if it changes mid-session.
    publication_generation: u64,
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
    #[cfg(any(test, feature = "bench-internals"))]
    last_current_attribute_cursor_diagnostics:
        Arc<Mutex<Option<CurrentAttributeCursorDiagnostics>>>,
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
                pending: PendingOverlay::new(),
                committed: None,
                committed_index_reader: None,
                publication_generation: 0,
            })),
            tx_counter: Arc::new(AtomicU64::new(0)),
            #[cfg(any(test, feature = "bench-internals"))]
            last_current_attribute_cursor_diagnostics: Arc::new(Mutex::new(None)),
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
        d.pending.insert_batch(facts, false)?;
        d.publication_generation = d.publication_generation.saturating_add(1);

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
        d.pending.insert_batch(facts, false)?;
        d.publication_generation = d.publication_generation.saturating_add(1);

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
        d.pending.insert_batch(retractions, false)?;
        d.publication_generation = d.publication_generation.saturating_add(1);

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

        let inserted = d.pending.insert_batch(vec![fact], true)? == 1;
        if inserted {
            d.publication_generation = d.publication_generation.saturating_add(1);
        }
        Ok(inserted)
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
        let max = d
            .pending
            .records()
            .map(|fact| fact.tx_count)
            .max()
            .unwrap_or(0);
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
        all.extend(d.pending.facts());
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
        for fact in d.pending.facts() {
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
        for fact in d.pending.records() {
            if fact.tx_count > since_tx_count {
                visit(fact.to_fact())?;
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
        d.pending.clear();
        d.committed = None;
        d.committed_index_reader = None;
        d.publication_generation = d.publication_generation.saturating_add(1);
        self.tx_counter.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Return the pending (uncommitted) facts held in memory.
    pub(crate) fn get_pending_facts(&self) -> Vec<Fact> {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        d.pending.facts().collect()
    }

    /// Clear pending facts and pending indexes after a successful checkpoint.
    pub(crate) fn post_checkpoint_clear(&self) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.pending.clear();
        d.publication_generation = d.publication_generation.saturating_add(1);
    }

    /// Set the tx_counter to `max` (used on load to restore from persisted state).
    pub(crate) fn restore_tx_counter_from(&self, max: u64) {
        self.tx_counter.store(max, Ordering::SeqCst);
    }

    /// Set the committed fact reader. Called by PersistentFactStorage::load() after
    /// opening a v5 file so index-driven reads can resolve FactRefs via page cache.
    pub(crate) fn set_committed_reader(
        &self,
        reader: Arc<dyn crate::storage::CommittedFactReader>,
    ) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        d.committed = Some(reader);
        d.publication_generation = d.publication_generation.saturating_add(1);
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
        d.publication_generation = d.publication_generation.saturating_add(1);
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
        d.pending.clear();
        d.publication_generation = d.publication_generation.saturating_add(1);
    }

    /// Extend an already-published shared committed reader and retire the
    /// matching pending facts under the same barrier used by point queries.
    pub(crate) fn publish_incremental_committed(&self, extend: impl FnOnce()) {
        let mut d = self.data.write().unwrap_or_else(|e| e.into_inner());
        extend();
        d.pending.clear();
        d.publication_generation = d.publication_generation.saturating_add(1);
    }

    /// Count of in-memory (pending, not yet checkpointed) fact records.
    /// Cheap: no committed scan. Backs the A6 session `status` op.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn pending_fact_count(&self) -> usize {
        let d = self.data.read().unwrap_or_else(|e| e.into_inner());
        d.pending.len()
    }

    /// Account live ownership in the pending fact log, duplicate set, and
    /// covering indexes without cloning any of them.
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn pending_memory_diagnostics(&self) -> PendingMemoryDiagnostics {
        let d = self.data.read().unwrap_or_else(|error| error.into_inner());
        let shape = d.pending.memory_shape();
        let facts = memory_component(
            shape.records_len,
            shape.records_capacity,
            shape.records_capacity.saturating_mul(std::mem::size_of::<
                crate::graph::pending_overlay::PendingFactRecord,
            >()),
            shape.attribute_bytes,
            shape.attribute_allocations,
            shape.value_bytes,
            shape.value_allocations,
        );
        let duplicate_keys = memory_component(
            shape.duplicate_buckets,
            shape.duplicate_capacity,
            shape
                .duplicate_capacity
                .saturating_mul(std::mem::size_of::<(u64, usize)>())
                .saturating_add(
                    shape
                        .duplicate_ids
                        .saturating_mul(std::mem::size_of::<PendingFactId>()),
                ),
            0,
            0,
            0,
            0,
        );
        let index_component = |(entries, capacity, _)| {
            memory_component(
                entries,
                capacity,
                capacity.saturating_mul(std::mem::size_of::<PendingFactId>()),
                0,
                0,
                0,
                0,
            )
        };
        let eavt = index_component(shape.eavt);
        let aevt = index_component(shape.aevt);
        let avet = index_component(shape.avet);
        let vaet = index_component(shape.vaet);
        PendingMemoryDiagnostics {
            facts,
            duplicate_keys,
            eavt,
            aevt,
            avet,
            vaet,
            index_run_counts: [
                usize_to_u64(shape.eavt.2),
                usize_to_u64(shape.aevt.2),
                usize_to_u64(shape.avet.2),
                usize_to_u64(shape.vaet.2),
            ],
            total_accounted_bytes: sum_component_bytes(&[
                facts,
                duplicate_keys,
                eavt,
                aevt,
                avet,
                vaet,
            ]),
            excludes_container_and_allocator_overhead: true,
        }
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn last_current_attribute_cursor_diagnostics(
        &self,
    ) -> Option<CurrentAttributeCursorDiagnostics> {
        *self
            .last_current_attribute_cursor_diagnostics
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
            d.pending.index_counts().0,
            d.pending.index_counts().1,
            d.pending.index_counts().2,
            d.pending.index_counts().3,
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
        anyhow::bail!("pending facts must use PendingFactId, not on-disk FactRef")
    }
    match &d.committed {
        Some(loader) => loader.resolve(fr),
        None => anyhow::bail!(
            "no CommittedFactReader but got committed FactRef (page_id={})",
            fr.page_id
        ),
    }
}

/// Resolve against the readers captured when the cursor was created. The
/// generation check in each step prevents pending slot references from being
/// reused after checkpoint publication.
fn resolve_cursor_fact(
    d: &FactData,
    cursor: &CurrentAttributeCursor,
    fact_ref: CursorFactRef,
) -> Result<Fact> {
    match fact_ref {
        CursorFactRef::Pending(id) => Ok(d.pending.get(id)?.to_fact()),
        CursorFactRef::Committed(fact_ref) => cursor
            .committed_fact_reader
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "current attribute cursor has no committed fact reader for page {}",
                    fact_ref.page_id
                )
            })?
            .resolve(fact_ref),
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
        if d.pending.is_empty() && d.committed_index_reader.is_none() {
            if d.committed.is_none() {
                return Ok(d
                    .pending
                    .records()
                    .filter(|f| &f.entity == entity_id)
                    .map(|fact| fact.to_fact())
                    .collect());
            }
            let mut result: Vec<Fact> = d
                .pending
                .records()
                .filter(|f| &f.entity == entity_id)
                .map(|fact| fact.to_fact())
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

        // Pending: canonical arena plus bounded sorted-ID runs.
        for id in d.pending.range_eavt(&start, &end) {
            facts.push(d.pending.get(id)?.to_fact());
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
        if d.pending.is_empty() && d.committed_index_reader.is_none() {
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
        for id in d.pending.range_aevt(&start, end_opt.as_ref()) {
            facts.push(d.pending.get(id)?.to_fact());
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

    pub(crate) fn current_attribute_cursor(
        &self,
        attribute: &Attribute,
        as_of: Option<&AsOf>,
        valid_time: CurrentValidTime,
    ) -> CurrentAttributeCursor {
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
        let d = self.data.read().unwrap_or_else(|error| error.into_inner());
        let pending = d.pending.range_aevt(&start, end.as_ref());
        #[cfg(any(test, feature = "bench-internals"))]
        let diagnostics = CurrentAttributeCursorDiagnostics {
            selected_pending_entries: usize_to_u64(pending.len()),
            selected_pending_snapshot_bytes: pending_snapshot_bytes(&pending),
            ..CurrentAttributeCursorDiagnostics::default()
        };
        #[cfg(any(test, feature = "bench-internals"))]
        {
            let mut slot = self
                .last_current_attribute_cursor_diagnostics
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *slot = None;
        }
        CurrentAttributeCursor {
            end,
            next_start: start,
            pending,
            pending_position: 0,
            current_entity: None,
            values: Default::default(),
            as_of: as_of.cloned(),
            valid_time,
            committed_fact_reader: d.committed.clone(),
            committed_index_reader: d.committed_index_reader.clone(),
            publication_generation: d.publication_generation,
            last_key: None,
            committed_complete: false,
            complete: false,
            yielded: false,
            #[cfg(any(test, feature = "bench-internals"))]
            current_entity_windows: 0,
            #[cfg(any(test, feature = "bench-internals"))]
            diagnostics,
            #[cfg(any(test, feature = "bench-internals"))]
            diagnostics_slot: self.last_current_attribute_cursor_diagnostics.clone(),
        }
    }

    pub(crate) fn step_current_attribute_cursor(
        &self,
        cursor: &mut CurrentAttributeCursor,
        max_entries: usize,
        visit: &mut dyn FnMut(EntityId, &Value) -> Result<()>,
    ) -> Result<CurrentAttributeStep> {
        cursor.begin_step();
        let result = self.step_current_attribute_cursor_inner(cursor, max_entries, visit);
        #[cfg(any(test, feature = "bench-internals"))]
        cursor.persist_diagnostics();
        result
    }

    fn step_current_attribute_cursor_inner(
        &self,
        cursor: &mut CurrentAttributeCursor,
        max_entries: usize,
        visit: &mut dyn FnMut(EntityId, &Value) -> Result<()>,
    ) -> Result<CurrentAttributeStep> {
        if cursor.complete {
            return Ok(CurrentAttributeStep::Complete);
        }
        let d = self.data.read().unwrap_or_else(|error| error.into_inner());
        if d.publication_generation != cursor.publication_generation {
            anyhow::bail!(
                "current attribute cursor publication changed during the aggregate session"
            );
        }
        let mut processed = 0usize;
        let bounded = max_entries != usize::MAX;

        let flush_entity = |cursor: &mut CurrentAttributeCursor,
                            visit: &mut dyn FnMut(EntityId, &Value) -> Result<()>|
         -> Result<()> {
            let Some(entity) = cursor.current_entity else {
                return Ok(());
            };
            let mut output = Vec::new();
            for (encoded, state) in &cursor.values {
                for ((valid_from, valid_to), (assert_tx, fact_ref)) in &state.assertions {
                    let scoped_retract = state
                        .max_scoped_retract_tx
                        .get(&(*valid_from, *valid_to))
                        .copied()
                        .unwrap_or(0);
                    if *assert_tx <= state.max_unscoped_retract_tx.max(scoped_retract)
                        || matches!(cursor.valid_time, CurrentValidTime::At(at) if !(*valid_from <= at && at < *valid_to))
                    {
                        continue;
                    }
                    let value = if encoded.first() == Some(&0x03) {
                        #[cfg(any(test, feature = "bench-internals"))]
                        {
                            cursor.diagnostics.exact_fact_resolutions =
                                cursor.diagnostics.exact_fact_resolutions.saturating_add(1);
                        }
                        resolve_cursor_fact(&d, cursor, *fact_ref)?.value
                    } else {
                        decode_index_value(encoded)?
                    };
                    output.push(value);
                }
            }
            for value in &output {
                visit(entity, value)?;
                #[cfg(any(test, feature = "bench-internals"))]
                {
                    cursor.diagnostics.emitted_rows =
                        cursor.diagnostics.emitted_rows.saturating_add(1);
                }
            }
            cursor.values.clear();
            cursor.current_entity = None;
            #[cfg(any(test, feature = "bench-internals"))]
            {
                cursor.current_entity_windows = 0;
            }
            Ok(())
        };

        let accept = |cursor: &mut CurrentAttributeCursor,
                      key: CurrentAevtEntryRef<'_>,
                      fact_ref: CursorFactRef,
                      visit: &mut dyn FnMut(EntityId, &Value) -> Result<()>|
         -> Result<()> {
            if !entry_visible_as_of(key, cursor.as_of.as_ref()) {
                return Ok(());
            }
            if cursor
                .current_entity
                .is_some_and(|entity| entity != key.entity)
            {
                flush_entity(cursor, visit)?;
            }
            cursor.current_entity = Some(key.entity);
            let added_window = reduce_current_entry(&mut cursor.values, key, fact_ref);
            #[cfg(any(test, feature = "bench-internals"))]
            cursor.note_reducer_shape(added_window);
            #[cfg(not(any(test, feature = "bench-internals")))]
            let _ = added_window;
            Ok(())
        };

        if !cursor.committed_complete {
            let last_key = cursor.last_key.clone();
            let next_start = cursor.next_start.clone();
            let end = cursor.end.clone();
            let committed_index_reader = cursor.committed_index_reader.clone();
            let complete = committed_index_reader.as_ref().map_or(Ok(true), |reader| {
                reader.visit_current_aevt_entries(
                    &next_start,
                    end.as_ref(),
                    &mut |key, fact_ref| {
                        if last_key
                            .as_ref()
                            .is_some_and(|last| !key.cmp_owned_suffix(last).is_gt())
                        {
                            return Ok(true);
                        }
                        while cursor.pending_position < cursor.pending.len()
                            && cursor
                                .pending
                                .get(cursor.pending_position)
                                .is_some_and(|id| {
                                    d.pending
                                        .compare_aevt_projection(*id, key)
                                        .is_ok_and(|order| order.is_lt())
                                })
                        {
                            let pending_id = *cursor
                                .pending
                                .get(cursor.pending_position)
                                .ok_or_else(|| anyhow::anyhow!("pending cursor out of bounds"))?;
                            let pending_key = d.pending.get(pending_id)?.current_aevt_entry();
                            #[cfg(any(test, feature = "bench-internals"))]
                            cursor.note_pending_entry();
                            accept(
                                cursor,
                                pending_key,
                                CursorFactRef::Pending(pending_id),
                                visit,
                            )?;
                            cursor.pending_position += 1;
                            processed += 1;
                            if processed >= max_entries {
                                return Ok(false);
                            }
                        }
                        #[cfg(any(test, feature = "bench-internals"))]
                        cursor.note_committed_entry();
                        accept(cursor, key, CursorFactRef::Committed(fact_ref), visit)?;
                        if bounded {
                            let reused = if let Some(last) = &mut cursor.last_key {
                                key.write_resume_key(last)
                            } else {
                                let mut last = cursor.next_start.clone();
                                key.write_resume_key(&mut last);
                                cursor.last_key = Some(last);
                                false
                            };
                            crate::storage::btree_v6::note_resume_key(reused);
                        }
                        processed += 1;
                        Ok(processed < max_entries)
                    },
                )
            })?;
            cursor.committed_complete = complete;
            if bounded && let Some(last) = &cursor.last_key {
                cursor.next_start = last.clone();
            }
            if !complete {
                cursor.note_yield();
                return Ok(CurrentAttributeStep::Yielded { entries: processed });
            }
        }

        while cursor.pending_position < cursor.pending.len() && processed < max_entries {
            let pending_id = *cursor
                .pending
                .get(cursor.pending_position)
                .ok_or_else(|| anyhow::anyhow!("pending cursor out of bounds"))?;
            let key = d.pending.get(pending_id)?.current_aevt_entry();
            #[cfg(any(test, feature = "bench-internals"))]
            cursor.note_pending_entry();
            accept(cursor, key, CursorFactRef::Pending(pending_id), visit)?;
            cursor.pending_position += 1;
            processed += 1;
        }
        if cursor.pending_position < cursor.pending.len() {
            cursor.note_yield();
            return Ok(CurrentAttributeStep::Yielded { entries: processed });
        }
        flush_entity(cursor, visit)?;
        cursor.complete = true;
        Ok(CurrentAttributeStep::Complete)
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
        let mut cursor = self.current_attribute_cursor(attribute, as_of, valid_time);
        loop {
            if matches!(
                self.step_current_attribute_cursor(&mut cursor, usize::MAX, visit)?,
                CurrentAttributeStep::Complete
            ) {
                return Ok(());
            }
        }
    }

    pub(crate) fn current_entity_attribute_cursor(
        &self,
        entity: EntityId,
        attribute: &str,
        as_of: Option<&AsOf>,
        valid_time: CurrentValidTime,
    ) -> Result<CurrentEntityAttributeCursor> {
        let start = EavtKey {
            entity,
            attribute: attribute.to_owned(),
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let mut end_attribute = String::with_capacity(attribute.len() + 1);
        end_attribute.push_str(attribute);
        end_attribute.push('\0');
        let end = EavtKey {
            entity,
            attribute: end_attribute,
            valid_from: i64::MIN,
            valid_to: i64::MIN,
            tx_count: 0,
            value_bytes: Vec::new(),
            tx_id: 0,
            asserted: false,
        };
        let d = self.data.read().unwrap_or_else(|error| error.into_inner());
        Ok(CurrentEntityAttributeCursor {
            pending: d.pending.range_eavt_bounded(
                &start,
                &end,
                crate::db::CURRENT_ENTITIES_MAX_HISTORY_ENTRIES,
            )?,
            end,
            next_start: start,
            pending_position: 0,
            values: Default::default(),
            as_of: as_of.cloned(),
            valid_time,
            committed_fact_reader: d.committed.clone(),
            committed_index_reader: d.committed_index_reader.clone(),
            publication_generation: d.publication_generation,
            last_key: None,
            committed_complete: false,
            complete: false,
        })
    }

    pub(crate) fn step_current_entity_attribute_cursor(
        &self,
        cursor: &mut CurrentEntityAttributeCursor,
        max_entries: usize,
        visit: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<CurrentEntityAttributeStep> {
        if cursor.complete {
            return Ok(CurrentEntityAttributeStep::Complete { entries: 0 });
        }
        let d = self.data.read().unwrap_or_else(|error| error.into_inner());
        if d.publication_generation != cursor.publication_generation {
            anyhow::bail!(
                "current entity/attribute cursor publication changed during the read view"
            );
        }
        let mut processed = 0usize;
        let bounded = max_entries != usize::MAX;

        let accept = |cursor: &mut CurrentEntityAttributeCursor,
                      key: CurrentEavtEntryRef<'_>,
                      fact_ref: CursorFactRef| {
            let value_key = key.value_projection();
            if entry_visible_as_of(value_key, cursor.as_of.as_ref()) {
                reduce_current_entry(&mut cursor.values, value_key, fact_ref);
            }
        };

        if !cursor.committed_complete {
            let last_key = cursor.last_key.clone();
            let next_start = cursor.next_start.clone();
            let end = cursor.end.clone();
            let committed_index_reader = cursor.committed_index_reader.clone();
            let complete = committed_index_reader.as_ref().map_or(Ok(true), |reader| {
                reader.visit_current_eavt_entries(&next_start, Some(&end), &mut |key, fact_ref| {
                    if last_key
                        .as_ref()
                        .is_some_and(|last| !key.cmp_owned(last).is_gt())
                    {
                        return Ok(true);
                    }
                    while cursor.pending_position < cursor.pending.len()
                        && cursor
                            .pending
                            .get(cursor.pending_position)
                            .is_some_and(|id| {
                                d.pending
                                    .compare_eavt_projection(*id, key)
                                    .is_ok_and(|order| order.is_lt())
                            })
                    {
                        let pending_id = *cursor
                            .pending
                            .get(cursor.pending_position)
                            .ok_or_else(|| anyhow::anyhow!("pending cursor out of bounds"))?;
                        accept(
                            cursor,
                            d.pending.get(pending_id)?.current_eavt_entry(),
                            CursorFactRef::Pending(pending_id),
                        );
                        cursor.pending_position += 1;
                        processed += 1;
                        if processed >= max_entries {
                            return Ok(false);
                        }
                    }
                    accept(cursor, key, CursorFactRef::Committed(fact_ref));
                    if bounded {
                        let reused = if let Some(last) = &mut cursor.last_key {
                            key.write_resume_key(last)
                        } else {
                            let mut last = cursor.next_start.clone();
                            key.write_resume_key(&mut last);
                            cursor.last_key = Some(last);
                            false
                        };
                        crate::storage::btree_v6::note_resume_key(reused);
                    }
                    processed += 1;
                    Ok(processed < max_entries)
                })
            })?;
            cursor.committed_complete = complete;
            if bounded && let Some(last) = &cursor.last_key {
                cursor.next_start = last.clone();
            }
            if !complete {
                return Ok(CurrentEntityAttributeStep::Yielded { entries: processed });
            }
        }

        while cursor.pending_position < cursor.pending.len() && processed < max_entries {
            let pending_id = *cursor
                .pending
                .get(cursor.pending_position)
                .ok_or_else(|| anyhow::anyhow!("pending cursor out of bounds"))?;
            accept(
                cursor,
                d.pending.get(pending_id)?.current_eavt_entry(),
                CursorFactRef::Pending(pending_id),
            );
            cursor.pending_position += 1;
            processed += 1;
        }
        if cursor.pending_position < cursor.pending.len() {
            return Ok(CurrentEntityAttributeStep::Yielded { entries: processed });
        }

        let mut output = Vec::new();
        for (encoded, state) in &cursor.values {
            let visible_fact = state.assertions.iter().find_map(
                |((valid_from, valid_to), (assert_tx, fact_ref))| {
                    let scoped_retract = state
                        .max_scoped_retract_tx
                        .get(&(*valid_from, *valid_to))
                        .copied()
                        .unwrap_or(0);
                    (*assert_tx > state.max_unscoped_retract_tx.max(scoped_retract)
                        && !matches!(cursor.valid_time, CurrentValidTime::At(at) if !(*valid_from <= at && at < *valid_to)))
                    .then_some(*fact_ref)
                },
            );
            let Some(fact_ref) = visible_fact else {
                continue;
            };
            let value = if encoded.first() == Some(&0x03) {
                match fact_ref {
                    CursorFactRef::Pending(id) => d.pending.get(id)?.to_fact().value,
                    CursorFactRef::Committed(fact_ref) => {
                        cursor
                            .committed_fact_reader
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("current cursor has no fact reader"))?
                            .resolve(fact_ref)?
                            .value
                    }
                }
            } else {
                decode_index_value(encoded)?
            };
            output.push((encoded, value));
        }
        output.sort_unstable_by(|left, right| left.0.cmp(right.0));
        for (_, value) in &output {
            visit(value)?;
        }
        cursor.values.clear();
        cursor.complete = true;
        Ok(CurrentEntityAttributeStep::Complete { entries: processed })
    }
}

fn entry_visible_as_of(key: CurrentAevtEntryRef<'_>, as_of: Option<&AsOf>) -> bool {
    match as_of {
        None => true,
        Some(AsOf::Counter(counter)) => key.tx_count <= *counter,
        Some(AsOf::Timestamp(timestamp)) => key.tx_id <= u64::try_from(*timestamp).unwrap_or(0),
        Some(AsOf::Slot(_)) => false,
    }
}

fn reduce_current_entry(
    values: &mut std::collections::HashMap<Vec<u8>, CurrentValueState>,
    key: CurrentAevtEntryRef<'_>,
    fact_ref: CursorFactRef,
) -> bool {
    fn reduce_state(
        state: &mut CurrentValueState,
        key: CurrentAevtEntryRef<'_>,
        fact_ref: CursorFactRef,
    ) -> bool {
        let windows_before = state
            .assertions
            .len()
            .saturating_add(state.max_scoped_retract_tx.len());
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
        } else if key.valid_from == RETRACT_ALL_VALID_FROM && key.valid_to == VALID_TIME_FOREVER {
            state.max_unscoped_retract_tx = state.max_unscoped_retract_tx.max(key.tx_count);
        } else {
            state
                .max_scoped_retract_tx
                .entry((key.valid_from, key.valid_to))
                .and_modify(|tx| *tx = (*tx).max(key.tx_count))
                .or_insert(key.tx_count);
        }
        state
            .assertions
            .len()
            .saturating_add(state.max_scoped_retract_tx.len())
            > windows_before
    }

    // The common current-view shape starts each entity with one value. Avoid
    // a borrowed miss probe before the unavoidable first owned map key.
    if values.is_empty() {
        let mut state = CurrentValueState::default();
        let added = reduce_state(&mut state, key, fact_ref);
        values.insert(key.value_bytes.to_vec(), state);
        return added;
    }
    if let Some(state) = values.get_mut(key.value_bytes) {
        return reduce_state(state, key, fact_ref);
    }
    let mut state = CurrentValueState::default();
    let added = reduce_state(&mut state, key, fact_ref);
    values.insert(key.value_bytes.to_vec(), state);
    added
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
        committed_count + d.pending.len()
    }

    /// Get the count of currently asserted facts. Test use only.
    pub(crate) fn asserted_fact_count(&self) -> usize {
        self.get_asserted_facts().map(|v| v.len()).unwrap_or(0)
    }

    /// Returns (eavt_len, aevt_len, avet_len, vaet_len). Test use only.
    pub(crate) fn index_counts(&self) -> (usize, usize, usize, usize) {
        let d = self.data.read().unwrap();
        d.pending.index_counts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index::{AvetKey, EavtKey, Indexes, VaetKey};

    struct TestIndexReader(Indexes);

    impl crate::storage::CommittedIndexReader for TestIndexReader {
        fn range_scan_eavt(&self, start: &EavtKey, end: Option<&EavtKey>) -> Result<Vec<FactRef>> {
            Ok(self
                .0
                .eavt
                .range(start.clone()..)
                .take_while(|(key, _)| end.is_none_or(|end| *key < end))
                .map(|(_, value)| *value)
                .collect())
        }
        fn range_scan_aevt(&self, start: &AevtKey, end: Option<&AevtKey>) -> Result<Vec<FactRef>> {
            Ok(self
                .0
                .aevt
                .range(start.clone()..)
                .take_while(|(key, _)| end.is_none_or(|end| *key < end))
                .map(|(_, value)| *value)
                .collect())
        }
        fn range_scan_avet(&self, start: &AvetKey, end: Option<&AvetKey>) -> Result<Vec<FactRef>> {
            Ok(self
                .0
                .avet
                .range(start.clone()..)
                .take_while(|(key, _)| end.is_none_or(|end| *key < end))
                .map(|(_, value)| *value)
                .collect())
        }
        fn range_scan_vaet(&self, start: &VaetKey, end: Option<&VaetKey>) -> Result<Vec<FactRef>> {
            Ok(self
                .0
                .vaet
                .range(start.clone()..)
                .take_while(|(key, _)| end.is_none_or(|end| *key < end))
                .map(|(_, value)| *value)
                .collect())
        }
    }

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
        storage.set_committed_index_reader(Arc::new(TestIndexReader(indexes)));
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
        storage.set_committed_index_reader(Arc::new(TestIndexReader(indexes)));
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

    #[test]
    fn pending_memory_diagnostics_accounts_each_live_owner_without_cloning() {
        let storage = FactStorage::new();
        storage
            .transact(
                vec![
                    (
                        uuid::Uuid::from_u128(1),
                        ":memory/int".to_string(),
                        Value::Integer(1),
                    ),
                    (
                        uuid::Uuid::from_u128(2),
                        ":memory/string".to_string(),
                        Value::String("owned-value".to_string()),
                    ),
                    (
                        uuid::Uuid::from_u128(3),
                        ":memory/ref".to_string(),
                        Value::Ref(uuid::Uuid::from_u128(4)),
                    ),
                ],
                None,
            )
            .unwrap();

        let diagnostics = storage.pending_memory_diagnostics();
        assert_eq!(diagnostics.facts.entries, 3);
        assert!(diagnostics.facts.capacity >= 3);
        assert_eq!(diagnostics.duplicate_keys.entries, 3);
        assert!(diagnostics.duplicate_keys.capacity >= 3);
        assert_eq!(diagnostics.eavt.entries, 3);
        assert_eq!(diagnostics.aevt.entries, 3);
        assert_eq!(diagnostics.avet.entries, 3);
        assert_eq!(diagnostics.vaet.entries, 1);
        assert!(diagnostics.facts.owned_attribute_bytes > 0);
        assert_eq!(diagnostics.facts.owned_attribute_allocations, 3);
        assert!(diagnostics.facts.owned_value_bytes > 0);
        assert_eq!(diagnostics.facts.owned_value_allocations, 4);
        assert_eq!(diagnostics.duplicate_keys.owned_value_bytes, 0);
        assert_eq!(diagnostics.duplicate_keys.owned_attribute_allocations, 0);
        assert_eq!(diagnostics.duplicate_keys.owned_value_allocations, 0);
        assert_eq!(
            diagnostics.total_accounted_bytes,
            diagnostics
                .facts
                .accounted_bytes
                .saturating_add(diagnostics.duplicate_keys.accounted_bytes)
                .saturating_add(diagnostics.eavt.accounted_bytes)
                .saturating_add(diagnostics.aevt.accounted_bytes)
                .saturating_add(diagnostics.avet.accounted_bytes)
                .saturating_add(diagnostics.vaet.accounted_bytes)
        );
        assert!(diagnostics.excludes_container_and_allocator_overhead);
    }

    #[test]
    fn current_attribute_cursor_ignores_one_million_unrelated_pending_entries() {
        const UNRELATED: u128 = 1_000_000;
        let storage = FactStorage::new();
        {
            let facts = (0..UNRELATED)
                .map(|index| {
                    let attribute = if index < UNRELATED / 2 {
                        ":a/noise"
                    } else {
                        ":z/noise"
                    };
                    Fact::with_valid_time(
                        uuid::Uuid::from_u128(index.saturating_add(1)),
                        attribute.to_owned(),
                        Value::Integer(1),
                        1,
                        1,
                        0,
                        VALID_TIME_FOREVER,
                    )
                })
                .collect();
            storage
                .data
                .write()
                .unwrap()
                .pending
                .insert_batch(facts, false)
                .unwrap();
        }

        let mut cursor = storage.current_attribute_cursor(
            &":m/selected".to_owned(),
            None,
            CurrentValidTime::Any,
        );
        let initial = cursor.diagnostics();
        assert_eq!(initial.selected_pending_entries, 0);
        assert_eq!(initial.selected_pending_snapshot_bytes, 0);

        let step = storage
            .step_current_attribute_cursor(&mut cursor, usize::MAX, &mut |_, _| Ok(()))
            .unwrap();
        assert!(matches!(step, CurrentAttributeStep::Complete));
        let diagnostics = cursor.diagnostics();
        assert_eq!(diagnostics.pending_entries_visited, 0);
        assert_eq!(diagnostics.committed_entries_visited, 0);
        assert_eq!(diagnostics.emitted_rows, 0);
        assert_eq!(diagnostics.peak_entity_values, 0);
        assert_eq!(diagnostics.peak_entity_windows, 0);
    }

    #[test]
    fn selected_pending_control_counts_snapshot_visits_yields_and_reducer_peak() {
        const SELECTED: usize = 10_000;
        const STEP: usize = 257;
        let storage = FactStorage::new();
        let facts = (0..SELECTED)
            .map(|index| {
                (
                    uuid::Uuid::from_u128(u128::try_from(index).unwrap().saturating_add(1)),
                    ":m/selected".to_owned(),
                    Value::Integer(i64::try_from(index).unwrap()),
                )
            })
            .collect();
        storage.transact(facts, None).unwrap();

        let mut cursor = storage.current_attribute_cursor(
            &":m/selected".to_owned(),
            None,
            CurrentValidTime::Any,
        );
        let initial = cursor.diagnostics();
        assert_eq!(initial.selected_pending_entries, 10_000);
        assert_eq!(
            initial.selected_pending_snapshot_bytes,
            10_000_u64.saturating_mul(u64::try_from(std::mem::size_of::<PendingFactId>()).unwrap())
        );

        let mut emitted = 0_u64;
        loop {
            let step = storage
                .step_current_attribute_cursor(&mut cursor, STEP, &mut |_, _| {
                    emitted = emitted.saturating_add(1);
                    Ok(())
                })
                .unwrap();
            if matches!(step, CurrentAttributeStep::Complete) {
                break;
            }
        }
        let diagnostics = cursor.diagnostics();
        assert_eq!(emitted, 10_000);
        assert_eq!(diagnostics.pending_entries_visited, 10_000);
        assert_eq!(diagnostics.emitted_rows, 10_000);
        assert_eq!(diagnostics.peak_entity_values, 1);
        assert_eq!(diagnostics.peak_entity_windows, 1);
        assert!(diagnostics.yield_count > 0);
        assert_eq!(diagnostics.resume_count, diagnostics.yield_count);
    }

    fn cursor_semantic_fact(
        entity: uuid::Uuid,
        attribute: &str,
        value: Value,
        tx_count: u64,
        valid_from: i64,
        valid_to: i64,
        asserted: bool,
    ) -> Fact {
        Fact {
            entity,
            attribute: attribute.to_owned(),
            value,
            tx_id: tx_count,
            tx_count,
            valid_from,
            valid_to,
            asserted,
        }
    }

    fn collect_cursor_values(
        storage: &FactStorage,
        as_of: Option<&AsOf>,
        valid_time: CurrentValidTime,
    ) -> (Vec<(EntityId, Value)>, CurrentAttributeCursorDiagnostics) {
        let mut cursor =
            storage.current_attribute_cursor(&":m/selected".to_owned(), as_of, valid_time);
        let mut values = Vec::new();
        loop {
            let step = storage
                .step_current_attribute_cursor(&mut cursor, 2, &mut |entity, value| {
                    values.push((entity, value.clone()));
                    Ok(())
                })
                .unwrap();
            if matches!(step, CurrentAttributeStep::Complete) {
                return (values, cursor.diagnostics());
            }
        }
    }

    #[test]
    fn current_attribute_cursor_isolates_mixed_temporal_ref_and_float_entries() {
        let storage = FactStorage::new();
        let scoped = uuid::Uuid::from_u128(10);
        let unscoped = uuid::Uuid::from_u128(20);
        let ref_entity = uuid::Uuid::from_u128(30);
        let float_entity = uuid::Uuid::from_u128(40);
        let target = uuid::Uuid::from_u128(99);
        let facts = [
            cursor_semantic_fact(
                uuid::Uuid::from_u128(1),
                ":a/noise",
                Value::Float(999.0),
                1,
                0,
                VALID_TIME_FOREVER,
                true,
            ),
            cursor_semantic_fact(scoped, ":m/selected", Value::Integer(10), 1, 0, 100, true),
            cursor_semantic_fact(scoped, ":m/selected", Value::Integer(10), 2, 0, 100, false),
            cursor_semantic_fact(scoped, ":m/selected", Value::Integer(20), 3, 100, 200, true),
            cursor_semantic_fact(
                unscoped,
                ":m/selected",
                Value::Keyword(":active".to_owned()),
                1,
                0,
                VALID_TIME_FOREVER,
                true,
            ),
            cursor_semantic_fact(
                unscoped,
                ":m/selected",
                Value::Keyword(":active".to_owned()),
                2,
                RETRACT_ALL_VALID_FROM,
                VALID_TIME_FOREVER,
                false,
            ),
            cursor_semantic_fact(
                ref_entity,
                ":m/selected",
                Value::Ref(target),
                1,
                0,
                VALID_TIME_FOREVER,
                true,
            ),
            cursor_semantic_fact(
                float_entity,
                ":m/selected",
                Value::Float(2.5),
                1,
                0,
                VALID_TIME_FOREVER,
                true,
            ),
            cursor_semantic_fact(
                uuid::Uuid::from_u128(100),
                ":z/noise",
                Value::Ref(target),
                1,
                0,
                VALID_TIME_FOREVER,
                true,
            ),
        ];
        for fact in facts {
            assert!(storage.load_fact(fact).unwrap());
        }

        let (current, diagnostics) =
            collect_cursor_values(&storage, None, CurrentValidTime::At(150));
        assert_eq!(current.len(), 3);
        assert!(current.contains(&(scoped, Value::Integer(20))));
        assert!(current.contains(&(ref_entity, Value::Ref(target))));
        assert!(current.contains(&(float_entity, Value::Float(2.5))));
        assert_eq!(diagnostics.selected_pending_entries, 7);
        assert_eq!(diagnostics.pending_entries_visited, 7);
        assert_eq!(diagnostics.exact_fact_resolutions, 1);
        assert_eq!(diagnostics.emitted_rows, 3);
        assert_eq!(diagnostics.peak_entity_values, 2);
        assert_eq!(diagnostics.peak_entity_windows, 3);

        let (historical, historical_diagnostics) =
            collect_cursor_values(&storage, Some(&AsOf::Counter(1)), CurrentValidTime::At(50));
        assert_eq!(historical.len(), 4);
        assert!(historical.contains(&(scoped, Value::Integer(10))));
        assert!(historical.contains(&(unscoped, Value::Keyword(":active".to_owned()))));
        assert!(historical.contains(&(ref_entity, Value::Ref(target))));
        assert!(historical.contains(&(float_entity, Value::Float(2.5))));
        assert_eq!(historical_diagnostics.pending_entries_visited, 7);
        assert_eq!(historical_diagnostics.exact_fact_resolutions, 1);
        assert_eq!(historical_diagnostics.emitted_rows, 4);
    }

    #[test]
    fn cursor_diagnostics_survive_errors_and_publication_changes_fail_closed() {
        let storage = FactStorage::new();
        storage
            .transact(
                vec![(
                    uuid::Uuid::from_u128(1),
                    ":m/selected".to_owned(),
                    Value::Integer(1),
                )],
                None,
            )
            .unwrap();

        let mut visitor_error_cursor = storage.current_attribute_cursor(
            &":m/selected".to_owned(),
            None,
            CurrentValidTime::Any,
        );
        let error = match storage.step_current_attribute_cursor(
            &mut visitor_error_cursor,
            1,
            &mut |_, _| anyhow::bail!("injected aggregate sink failure"),
        ) {
            Err(error) => error,
            Ok(_) => panic!("injected aggregate sink failure must propagate"),
        };
        assert!(
            error
                .to_string()
                .contains("injected aggregate sink failure")
        );
        let failed = storage.last_current_attribute_cursor_diagnostics().unwrap();
        assert_eq!(failed.selected_pending_entries, 1);
        assert_eq!(failed.pending_entries_visited, 1);
        assert_eq!(failed.emitted_rows, 0);

        let mut publication_cursor = storage.current_attribute_cursor(
            &":m/selected".to_owned(),
            None,
            CurrentValidTime::Any,
        );
        storage.post_checkpoint_clear();
        let error =
            match storage
                .step_current_attribute_cursor(&mut publication_cursor, 1, &mut |_, _| Ok(()))
            {
                Err(error) => error,
                Ok(_) => panic!("publication replacement must invalidate the cursor"),
            };
        assert!(error.to_string().contains("publication changed"));
        let changed = storage.last_current_attribute_cursor_diagnostics().unwrap();
        assert_eq!(changed.selected_pending_entries, 1);
        assert_eq!(changed.pending_entries_visited, 0);
        assert_eq!(changed.emitted_rows, 0);
    }
}
