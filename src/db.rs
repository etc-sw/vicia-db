//! Public `Minigraf` facade with `WriteTransaction` and `OpenOptions`.
//!
//! This module provides the primary user-facing API for Minigraf:
//! - `Minigraf::open()` / `Minigraf::in_memory()` for database creation
//! - `Minigraf::execute()` for implicit (self-contained) transactions
//! - `Minigraf::begin_write()` / `WriteTransaction` for explicit transactions
//! - `Minigraf::checkpoint()` for manual WAL compaction
//! - `Minigraf::backup_to()` for linearized live-writer snapshots
//! - `Minigraf::export_fact_log()` for deterministic append-only audit export

use crate::graph::types::{Fact, FactRecord, TxId, VALID_TIME_FOREVER};

/// Sentinel value used in `materialize_transaction` to signal "no explicit `valid_from`
/// was provided; use the transaction timestamp at commit time."
///
/// `i64::MIN` is chosen because it is not a representable Unix millisecond timestamp
/// in any practical context, avoiding the collision that `0` would have with the Unix
/// epoch (1970-01-01T00:00:00Z), which is a legitimate `valid_from` value.
pub(crate) const VALID_FROM_USE_TX_TIME: i64 = i64::MIN;

/// Maximum command strings in one browser atomic write request.
///
/// This prevents an unbounded JavaScript array from becoming one WebAssembly
/// allocation and one IndexedDB publication.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
pub(crate) const BROWSER_ATOMIC_MAX_COMMANDS: usize = 256;
/// Maximum materialized facts in one browser atomic write request.
///
/// Vetch permits 32,768 payload chunks with four ledger facts per chunk
/// (131,072 facts) before operation metadata; this ceiling retains bounded
/// headroom for that complete operation envelope while still rejecting
/// unreasonable materialized batches.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
pub(crate) const BROWSER_ATOMIC_MAX_FACTS: usize = 262_144;
/// Maximum aggregate UTF-8 Datalog source bytes in one browser atomic write.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
pub(crate) const BROWSER_ATOMIC_MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum lexical tokens accepted before the normal parser allocates its AST.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
const BROWSER_ATOMIC_MAX_TOKENS: usize = BROWSER_ATOMIC_MAX_FACTS * 8;
use crate::graph::FactStorage;
use crate::graph::types::Value;
use crate::query::datalog::evaluator::DEFAULT_MAX_DERIVED_FACTS;
use crate::query::datalog::evaluator::DEFAULT_MAX_RESULTS;
use crate::query::datalog::executor::DatalogExecutor;
use crate::query::datalog::executor::QueryResult;
use crate::query::datalog::functions::{
    AggImpl, AggregateDesc, FunctionRegistry, PredicateDesc, UdfFinaliseFn, UdfOps, UdfStepFn,
};
use crate::query::datalog::parser::parse_datalog_command;
use crate::query::datalog::rules::RuleRegistry;
use crate::query::datalog::types::{
    AttributeSpec, DatalogCommand, ForgetSource, ForgetSpec, Transaction,
};
use crate::storage::backend::MemoryBackend;
#[cfg(not(target_arch = "wasm32"))]
use crate::storage::backend::file::FileBackend;
#[cfg(not(target_arch = "wasm32"))]
use crate::storage::delta_growth::DeltaMaintenanceDecision;
use crate::storage::persistent_facts::CheckpointOutcome;
use crate::storage::persistent_facts::PersistentFactStorage;
#[cfg(not(target_arch = "wasm32"))]
use crate::wal::WalWriter;
#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::{Result, bail};
use std::any::Any;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

#[cfg(not(target_arch = "wasm32"))]
struct BackupTempFile {
    path: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
struct BackupTarget {
    destination: PathBuf,
    parent: PathBuf,
    wal_path: PathBuf,
    lock_path: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BackupTempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ─── Thread-local reentrant-write detection ─────────────────────────────────

thread_local! {
    /// Set to `true` while a `WriteTransaction` is active on this thread.
    /// Prevents same-thread deadlock when `db.execute()` is called while
    /// a `WriteTransaction` is in progress.
    static WRITE_TX_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn set_write_tx_active(val: bool) {
    WRITE_TX_ACTIVE.with(|f| f.set(val));
}

fn is_write_tx_active() -> bool {
    WRITE_TX_ACTIVE.with(|f| f.get())
}

/// A fully parsed and materialized transact/retract command batch.
///
/// No database state has changed while this value is being built. BrowserDb
/// uses this as the prepare boundary before claiming its mutation guard.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
#[derive(Debug)]
pub(crate) struct MaterializedAtomicWrite {
    /// Facts in caller command order, with transaction metadata unstamped.
    pub(crate) facts: Vec<Fact>,
    /// Number of submitted command strings.
    pub(crate) command_count: usize,
    /// Number of materialized asserted facts.
    pub(crate) transacted_fact_count: usize,
    /// Number of materialized retraction facts.
    pub(crate) retracted_fact_count: usize,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
fn atomic_write_preflight(source: &str) -> Result<(usize, usize)> {
    let mut chars = source.chars().peekable();
    let mut bracket_depth = 0usize;
    let mut fact_vectors = 0usize;
    let mut tokens = 0usize;

    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' | '\n' | '\r' | ',' => {}
            '"' => {
                tokens = tokens.saturating_add(1);
                let mut escaped = false;
                for string_ch in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if string_ch == '\\' {
                        escaped = true;
                    } else if string_ch == '"' {
                        break;
                    }
                }
            }
            '[' => {
                tokens = tokens.saturating_add(1);
                if bracket_depth == 1 {
                    fact_vectors = fact_vectors.saturating_add(1);
                }
                bracket_depth = bracket_depth.saturating_add(1);
            }
            ']' => {
                tokens = tokens.saturating_add(1);
                bracket_depth = bracket_depth.saturating_sub(1);
            }
            '(' | ')' | '{' | '}' => {
                tokens = tokens.saturating_add(1);
            }
            _ => {
                tokens = tokens.saturating_add(1);
                while let Some(next) = chars.peek() {
                    if next.is_whitespace()
                        || matches!(*next, ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"')
                    {
                        break;
                    }
                    chars.next();
                }
            }
        }
        if tokens > BROWSER_ATOMIC_MAX_TOKENS {
            bail!(
                "executeAtomic accepts at most {} lexical tokens",
                BROWSER_ATOMIC_MAX_TOKENS
            );
        }
        if fact_vectors > BROWSER_ATOMIC_MAX_FACTS {
            bail!(
                "executeAtomic accepts at most {} facts",
                BROWSER_ATOMIC_MAX_FACTS
            );
        }
    }
    Ok((fact_vectors, tokens))
}

// ─── Maintenance Outcome ─────────────────────────────────────────────────────

/// Public summary of the checkpoint part of an idle maintenance call.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceCheckpointEffect {
    /// No checkpoint publish was needed.
    Noop,
    /// The call published pending or replayed writes to the main database file.
    Published,
}

impl MaintenanceCheckpointEffect {
    fn from_checkpoint_outcome(outcome: CheckpointOutcome) -> Self {
        match outcome {
            CheckpointOutcome::Noop => Self::Noop,
            CheckpointOutcome::FullRebuild
            | CheckpointOutcome::FullRebuildFromVisibleDelta
            | CheckpointOutcome::DeltaSegment => Self::Published,
        }
    }
}

/// Public summary of the delta-maintenance part of an idle maintenance call.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceDeltaEffect {
    /// No delta maintenance was needed.
    Noop,
    /// Visible delta segments were folded into a fresh copy-on-write base.
    Recompacted,
}

#[cfg(not(target_arch = "wasm32"))]
impl MaintenanceDeltaEffect {
    fn from_checkpoint_outcome(outcome: CheckpointOutcome) -> Self {
        match outcome {
            CheckpointOutcome::FullRebuildFromVisibleDelta => Self::Recompacted,
            CheckpointOutcome::Noop
            | CheckpointOutcome::FullRebuild
            | CheckpointOutcome::DeltaSegment => Self::Noop,
        }
    }
}

/// Public maintenance advice for the embedding application.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceAdvice {
    /// No caller cadence change is recommended.
    None,
    /// The database crossed a hard delta-growth threshold before maintenance.
    ///
    /// The caller should reduce tiny checkpoint cadence, batch writes more
    /// aggressively, or prioritize maintenance scheduling.
    ReduceCheckpointCadence,
}

#[cfg(not(target_arch = "wasm32"))]
impl MaintenanceAdvice {
    fn from_delta_decision(decision: DeltaMaintenanceDecision) -> Self {
        match decision {
            DeltaMaintenanceDecision::MaintenanceBackpressure => Self::ReduceCheckpointCadence,
            DeltaMaintenanceDecision::ContinueDeltaAppend
            | DeltaMaintenanceDecision::ScheduleBackgroundRecompact => Self::None,
        }
    }
}

/// Result of [`Minigraf::run_idle_maintenance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceOutcome {
    /// Whether the call first published pending WAL-backed writes.
    pub checkpoint: MaintenanceCheckpointEffect,
    /// Whether the call folded visible delta segments into a fresh base.
    pub delta: MaintenanceDeltaEffect,
    /// Caller-facing cadence advice derived from the pre-maintenance delta state.
    ///
    /// `ReduceCheckpointCadence` can co-occur with `delta = Recompacted`: the
    /// advice describes the state that triggered maintenance, not the state
    /// left behind after a successful fold.
    pub advice: MaintenanceAdvice,
}

impl MaintenanceOutcome {
    fn new(
        checkpoint: MaintenanceCheckpointEffect,
        delta: MaintenanceDeltaEffect,
        advice: MaintenanceAdvice,
    ) -> Self {
        Self {
            checkpoint,
            delta,
            advice,
        }
    }
}

/// Result of a successful [`Minigraf::backup_to`] call.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupOutcome {
    /// Exact source transaction watermark contained in the backup.
    pub tx_count: u64,
    /// Number of checkpointed `.graph` bytes copied and fsynced.
    pub bytes: u64,
}

/// Crate-internal status numbers for the A6 session `status` op.
/// File-only fields are `None` for in-memory databases.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct SessionStatusSnapshot {
    /// Exact total fact count when cheaply knowable (in-memory databases);
    /// `None` for file-backed databases, where an exact count would need a
    /// committed full scan. Callers wanting totals track them via `tx_count`
    /// deltas or `export_fact_log`.
    pub(crate) fact_count: Option<u64>,
    /// In-memory (pending, not yet checkpointed) fact records. Always exact.
    pub(crate) pending_facts: u64,
    pub(crate) tx_count: u64,
    pub(crate) wal_bytes: Option<u64>,
    pub(crate) delta_segments: Option<u64>,
    pub(crate) delta_pages: Option<u64>,
}

// ─── OpenOptions ─────────────────────────────────────────────────────────────

/// Configuration options for opening a `Minigraf` database.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Number of WAL entries committed before an automatic checkpoint is triggered.
    ///
    /// Defaults to 1000. Lower values mean more frequent checkpoints (smaller WAL,
    /// more I/O). Higher values mean less frequent checkpoints (larger WAL, less I/O).
    pub wal_checkpoint_threshold: usize,
    /// Number of pages to hold in the LRU page cache. Default: 256 (= 1MB at 4KB pages).
    pub page_cache_size: usize,
    /// Maximum facts that can be derived per recursive rule iteration.
    /// Defaults to 1_000_000. Use to prevent runaway recursive rules.
    pub max_derived_facts: usize,
    /// Maximum total query results. Defaults to 1_000_000.
    pub max_results: usize,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions {
            wal_checkpoint_threshold: 1000,
            page_cache_size: 256,
            max_derived_facts: DEFAULT_MAX_DERIVED_FACTS,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

impl OpenOptions {
    /// Create a new `OpenOptions` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the number of pages to hold in the LRU page cache.
    ///
    /// Each page is 4KB, so the default of 256 pages uses ~1MB of memory.
    pub fn page_cache_size(mut self, size: usize) -> Self {
        self.page_cache_size = size;
        self
    }

    /// Set the maximum facts that can be derived per recursive rule iteration.
    ///
    /// Defaults to 1_000_000. Use lower values to prevent runaway recursive rules
    /// from consuming excessive memory. Can be overridden per-query using
    /// `:max-derived-facts N` in the query vector.
    pub fn max_derived_facts(mut self, n: usize) -> Self {
        self.max_derived_facts = n;
        self
    }

    /// Set the maximum total query results.
    ///
    /// Defaults to 1_000_000. Use lower values to limit result set size.
    pub fn max_results(mut self, n: usize) -> Self {
        self.max_results = n;
        self
    }

    /// Set the path for a file-backed database.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn path(self, path: impl AsRef<Path>) -> OpenOptionsWithPath {
        OpenOptionsWithPath {
            opts: self,
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Open an in-memory (non-persistent) database.
    ///
    /// Uses the options set on the builder. WAL-related options are ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if the in-memory storage backend fails to initialise.
    pub fn open_memory(self) -> Result<Minigraf> {
        Minigraf::in_memory_with_options(self)
    }
}

/// `OpenOptions` combined with a file path, ready to open.
#[cfg(not(target_arch = "wasm32"))]
pub struct OpenOptionsWithPath {
    opts: OpenOptions,
    path: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl OpenOptionsWithPath {
    /// Open or create the file-backed database.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, the header is corrupt,
    /// or WAL replay fails.
    pub fn open(self) -> Result<Minigraf> {
        Minigraf::open_with_options(self.path, self.opts)
    }
}

// ─── WriteContext ─────────────────────────────────────────────────────────────

/// Internal write context: distinguishes in-memory from file-backed databases.
#[allow(clippy::large_enum_variant)]
enum WriteContext {
    /// In-memory database: no WAL, no persistence.
    Memory,
    /// File-backed database: has a WAL sidecar and a persistent storage layer.
    #[cfg(not(target_arch = "wasm32"))]
    File {
        pfs: PersistentFactStorage<FileBackend>,
        /// WAL writer. `None` after a checkpoint until the next write.
        wal: Option<WalWriter>,
        db_path: PathBuf,
        /// Count of WAL entries written since the last checkpoint (or since open).
        wal_entry_count: usize,
    },
}

// ─── Inner ────────────────────────────────────────────────────────────────────

struct Inner {
    /// The shared in-memory fact store. Cloning is cheap (Arc-based).
    fact_storage: FactStorage,
    /// Shared rule registry, persists across all `execute()` calls.
    rules: Arc<RwLock<RuleRegistry>>,
    /// Function registry for aggregates and window functions.
    /// `RwLock` is used in anticipation of the 7.7b `register_aggregate`/`register_predicate` mutation API.
    functions: Arc<RwLock<FunctionRegistry>>,
    /// Serialises all writes. Holds `WriteContext` which contains the PFS/WAL
    /// for file-backed databases.
    write_lock: Mutex<WriteContext>,
    /// Configuration options.
    options: OpenOptions,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // On clean close, perform a best-effort checkpoint to reduce WAL size.
        // Errors are silently ignored (can't propagate from Drop).
        // Skip if wal_checkpoint_threshold is usize::MAX — that sentinel suppresses
        // all checkpointing (used by benchmarks to keep WAL entries pending).
        if self.options.wal_checkpoint_threshold == usize::MAX {
            return;
        }
        if let Ok(mut ctx) = self.write_lock.lock() {
            let _ = Minigraf::do_checkpoint(&self.fact_storage, &mut ctx);
        }
    }
}

// ─── Fact size validation (moved to WAL serialization) ────────────────────────

// ─── Minigraf ─────────────────────────────────────────────────────────────────

/// The primary embedded graph database handle.
///
/// `Minigraf` is cheap to clone — all clones share the same underlying database.
///
/// # File-backed usage
/// ```no_run
/// # #[cfg(not(target_arch = "wasm32"))] {
/// use minigraf::db::Minigraf;
///
/// let db = Minigraf::open("mydb.graph").unwrap();
/// db.execute(r#"(transact [[:alice :person/name "Alice"]])"#).unwrap();
/// # }
/// ```
///
/// # In-memory usage
/// ```
/// use minigraf::db::Minigraf;
///
/// let db = Minigraf::in_memory().unwrap();
/// db.execute(r#"(transact [[:alice :person/name "Alice"]])"#).unwrap();
/// ```
///
/// # Fact Size Limit (file-backed databases only)
///
/// Each fact persisted to a `.graph` file must serialise to at most
/// 4 080 bytes (the per-page capacity limit).
///
/// In practice, `Value::String` content is limited to roughly **3 900–4 000 bytes**
/// depending on entity and attribute name lengths.
///
/// Facts that exceed this limit are rejected at insertion time with a descriptive
/// error. This check does **not** apply to `Minigraf::in_memory()`.
///
/// ## Workarounds for large payloads
///
/// - **External blob reference** — store the payload in a file or object store
///   and record its path, URL, or content-addressed hash as a `Value::String`:
///   ```text
///   (transact [[:doc123 :blob/sha256 "a3f5c9..."]])
///   ```
/// - **Entity decomposition** — split large values across multiple facts using
///   a continuation-entity pattern.
/// - **In-memory database** — `Minigraf::in_memory()` has no fact size limit
///   and is suitable for workloads that do not require persistence.
#[derive(Clone)]
pub struct Minigraf {
    inner: Arc<Inner>,
}

impl Minigraf {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Open or create a file-backed database with default options.
    ///
    /// A sidecar WAL file (`<path>.wal`) is created alongside the main file.
    /// Any existing WAL from a previous crash is replayed automatically.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, the header is corrupt,
    /// or WAL replay fails.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, OpenOptions::default())
    }

    /// Open or create a file-backed database with custom options.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, the header is corrupt,
    /// or WAL replay fails.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_with_options(path: impl AsRef<Path>, opts: OpenOptions) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();

        // Open the main .graph file
        let backend = FileBackend::open(&db_path)?;
        let pfs = PersistentFactStorage::new(backend, opts.page_cache_size)?;

        // Share the fact storage
        let fact_storage = pfs.storage().clone();

        // Derive WAL path: "<db_path>.wal"
        let wal_path = Self::wal_path_for(&db_path);

        // Replay any existing WAL entries before opening the writer
        let wal_entry_count = Self::replay_wal(&wal_path, &fact_storage, &pfs)?;

        // Open the WAL writer only if the WAL file already exists from a previous session.
        // Otherwise, create it lazily on the first write.
        let wal = if wal_path.exists() {
            Some(WalWriter::open_or_create(&wal_path)?)
        } else {
            None
        };

        let ctx = WriteContext::File {
            pfs,
            wal,
            db_path,
            wal_entry_count,
        };

        Ok(Minigraf {
            inner: Arc::new(Inner {
                fact_storage,
                rules: Arc::new(RwLock::new(RuleRegistry::new())),
                functions: Arc::new(RwLock::new(FunctionRegistry::with_builtins())),
                write_lock: Mutex::new(ctx),
                options: opts,
            }),
        })
    }

    /// Create an in-memory database (no WAL, no persistence). Suitable for tests and REPL.
    ///
    /// # Errors
    ///
    /// Returns an error if the in-memory storage backend fails to initialise.
    pub fn in_memory() -> Result<Self> {
        Self::in_memory_with_options(OpenOptions::default())
    }

    /// Create an in-memory database with custom options.
    ///
    /// Note: WAL-related options are ignored for in-memory databases.
    ///
    /// # Errors
    ///
    /// Returns an error if the in-memory storage backend fails to initialise.
    pub fn in_memory_with_options(opts: OpenOptions) -> Result<Self> {
        let backend = MemoryBackend::new();
        let pfs = PersistentFactStorage::new(backend, opts.page_cache_size)?;
        let fact_storage = pfs.storage().clone();

        // For in-memory databases we don't need the PFS beyond initialisation;
        // we just use the shared FactStorage directly.
        drop(pfs);

        Ok(Minigraf {
            inner: Arc::new(Inner {
                fact_storage,
                rules: Arc::new(RwLock::new(RuleRegistry::new())),
                functions: Arc::new(RwLock::new(FunctionRegistry::with_builtins())),
                write_lock: Mutex::new(WriteContext::Memory),
                options: opts,
            }),
        })
    }

    // ── WAL replay helper ────────────────────────────────────────────────────

    /// Replay any WAL entries that are newer than the main file's checkpoint.
    ///
    /// Returns the number of entries replayed (used to seed `wal_entry_count`).
    #[cfg(not(target_arch = "wasm32"))]
    fn replay_wal(
        wal_path: &Path,
        fact_storage: &FactStorage,
        pfs: &PersistentFactStorage<FileBackend>,
    ) -> Result<usize> {
        if !wal_path.exists() {
            return Ok(0);
        }

        let mut reader = crate::wal::WalReader::open(wal_path)?;
        let entries = reader.read_entries()?;
        let last_checkpointed = pfs.last_checkpointed_tx_count();

        let mut replayed = 0;
        for entry in &entries {
            if entry.tx_count <= last_checkpointed {
                // Already present in the main file; skip.
                continue;
            }
            for fact in &entry.facts {
                let _ = fact_storage.load_fact(fact.clone())?;
            }
            #[allow(clippy::arithmetic_side_effects)]
            {
                replayed += 1;
            }
        }

        // Re-synchronise tx_counter to the maximum tx_count across the
        // replayed facts. Committed facts live on disk, not in memory, so the
        // in-memory maximum alone can under-count: a crash can leave a WAL
        // with zero replayable entries (header-only file from a torn first
        // append, or a checkpoint/delete race), and a counter lowered below
        // the committed watermark would hand out already-committed tx_counts
        // whose WAL entries the next replay then skips — losing acknowledged
        // writes. Never go below `last_checkpointed`.
        fact_storage.restore_tx_counter()?;
        if fact_storage.current_tx_count() < last_checkpointed {
            fact_storage.restore_tx_counter_from(last_checkpointed);
        }

        Ok(replayed)
    }

    // ── Execute ──────────────────────────────────────────────────────────────

    /// Execute a Datalog command as a self-contained implicit transaction.
    ///
    /// For file-backed databases, the WAL entry is written **before** facts are
    /// applied to the in-memory store. A successful return means the facts are in
    /// both the WAL and the in-memory store; a crash after this call returns will
    /// replay the facts on next open.
    ///
    /// If the WAL write fails, an error is returned and the in-memory store is
    /// left unchanged. The database remains consistent for subsequent in-process
    /// operations.
    ///
    /// Returns `Err` if called from the same thread that holds an active
    /// `WriteTransaction` (use `tx.execute()` instead).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A `WriteTransaction` is already active on **this thread** (deadlock prevention).
    /// - Parsing fails.
    /// - Execution fails.
    /// - WAL write fails (file-backed databases).
    pub fn execute(&self, input: &str) -> Result<QueryResult> {
        // Detect same-thread reentrant write (would deadlock on the Mutex).
        if is_write_tx_active() {
            bail!(
                "a WriteTransaction is already in progress on this thread; use tx.execute() instead"
            );
        }

        let cmd = parse_datalog_command(input).map_err(|e| anyhow::anyhow!("{}", e))?;

        // Determine if this is a read-only command (query only).
        // Rule registration is treated as a write because it mutates the shared RuleRegistry.
        let is_write = matches!(
            cmd,
            DatalogCommand::Transact(_)
                | DatalogCommand::Retract(_)
                | DatalogCommand::Rule(_)
                | DatalogCommand::Forget(_)
        );

        if is_write {
            let mut ctx = self.inner.write_lock.lock().map_err(|_| {
                anyhow::anyhow!("write lock is poisoned; database may be in an inconsistent state")
            })?;

            // Forget evaluates its embedded query under the write lock (the
            // matched state cannot move between query and closure), then
            // reuses the same WAL-first batch apply as transact/retract.
            if let DatalogCommand::Forget(spec) = &cmd {
                return self.execute_forget_locked(&mut ctx, spec);
            }

            // Handle write commands with correct WAL-first ordering for Transact/Retract:
            // 1. Materialize facts (no storage mutation yet)
            // 2. Allocate tx_count + tx_id and stamp facts
            // 3. Write WAL entry FIRST — if this fails, FactStorage is unchanged
            // 4. Apply facts to shared FactStorage
            // For Rule registration: acquire lock to serialize rule changes, no WAL needed
            let (stamped, is_retract) = match &cmd {
                DatalogCommand::Transact(tx) => (Minigraf::materialize_transaction(tx)?, false),
                DatalogCommand::Retract(tx) => (Minigraf::materialize_retraction(tx)?, true),
                DatalogCommand::Rule(_) => {
                    // Rule registration: execute while holding write_lock to serialize
                    // rule registry changes (no WAL needed for rules)
                    let executor = DatalogExecutor::new_with_rules_and_functions(
                        self.inner.fact_storage.clone(),
                        self.inner.rules.clone(),
                        self.inner.functions.clone(),
                    );
                    return executor.execute(cmd);
                }
                _ => return Err(anyhow::anyhow!("unexpected command variant in write path")),
            };

            let tx_count = self.inner.fact_storage.allocate_tx_count();
            let tx_id = crate::graph::types::tx_id_now();

            let stamped: Vec<Fact> = stamped
                .into_iter()
                .map(|mut f| {
                    f.tx_id = tx_id;
                    f.tx_count = tx_count;
                    // Fix valid_from if it was left as the sentinel
                    if f.valid_from == VALID_FROM_USE_TX_TIME {
                        f.valid_from = tx_id.cast_signed();
                    }
                    f
                })
                .collect();

            // WAL write includes size validation — no separate check needed.
            // Write WAL BEFORE applying to shared FactStorage.
            // If this fails, FactStorage is still unchanged — clean rollback.
            let should_checkpoint = WriteTransaction::wal_write_stamped_batch(
                &mut ctx,
                &self.inner.options,
                tx_count,
                &stamped,
            )?;

            // WAL succeeded — now apply facts to shared FactStorage.
            for fact in &stamped {
                let _ = self.inner.fact_storage.load_fact(fact.clone())?;
            }

            // Trigger auto-checkpoint AFTER facts are in FactStorage so the
            // checkpoint captures the newly written facts.
            if should_checkpoint {
                Minigraf::do_checkpoint(&self.inner.fact_storage, &mut ctx)?;
            }

            // Return the same QueryResult the executor would have returned.
            if is_retract {
                Ok(QueryResult::Retracted(tx_id))
            } else {
                Ok(QueryResult::Transacted(tx_id))
            }
        } else {
            // Read-only: no lock needed
            let mut executor = DatalogExecutor::new_with_rules_and_functions(
                self.inner.fact_storage.clone(),
                self.inner.rules.clone(),
                self.inner.functions.clone(),
            );
            executor.set_limits(
                self.inner.options.max_derived_facts,
                self.inner.options.max_results,
            );
            executor.execute(cmd)
        }
    }

    /// Execute a `(forget ...)` bulk valid-time closure while holding the
    /// write lock.
    ///
    /// Every window of the matched EAV triples that contains the closure time
    /// `T` is replaced, in one transaction (one `tx_count`, one WAL entry), by
    /// a scoped retraction of the old window plus a re-assertion truncated to
    /// end at `T`. History is preserved: `:as-of` before the closure still
    /// shows the open windows.
    fn execute_forget_locked(
        &self,
        ctx: &mut WriteContext,
        spec: &ForgetSpec,
    ) -> Result<QueryResult> {
        let now = crate::graph::types::tx_id_now();
        let closure_time = spec.valid_to.unwrap_or_else(|| now.cast_signed());

        let mut executor = DatalogExecutor::new_with_rules_and_functions(
            self.inner.fact_storage.clone(),
            self.inner.rules.clone(),
            self.inner.functions.clone(),
        );
        executor.set_limits(
            self.inner.options.max_derived_facts,
            self.inner.options.max_results,
        );

        let triples = Minigraf::resolve_forget_triples(spec, &executor, closure_time)?;
        let (facts, count) =
            Minigraf::materialize_closure(&self.inner.fact_storage, &triples, closure_time)?;

        // Nothing matched: no tx_count consumed, no WAL entry — idempotent.
        if facts.is_empty() {
            return Ok(QueryResult::Forgotten {
                tx_id: None,
                count: 0,
            });
        }

        let tx_count = self.inner.fact_storage.allocate_tx_count();
        let tx_id = now;
        let stamped: Vec<Fact> = facts
            .into_iter()
            .map(|mut f| {
                f.tx_id = tx_id;
                f.tx_count = tx_count;
                f
            })
            .collect();

        // WAL first — if this fails, FactStorage is unchanged.
        let should_checkpoint = WriteTransaction::wal_write_stamped_batch(
            ctx,
            &self.inner.options,
            tx_count,
            &stamped,
        )?;

        for fact in &stamped {
            let _ = self.inner.fact_storage.load_fact(fact.clone())?;
        }

        if should_checkpoint {
            Minigraf::do_checkpoint(&self.inner.fact_storage, ctx)?;
        }

        Ok(QueryResult::Forgotten {
            tx_id: Some(tx_id),
            count,
        })
    }

    // ── Explicit transaction ──────────────────────────────────────────────────

    /// Begin an explicit write transaction.
    ///
    /// Acquires the write lock; held until `commit()`, `rollback()`, or drop.
    ///
    /// # Errors
    ///
    /// Returns an error if a `WriteTransaction` is already active on **this thread**.
    pub fn begin_write(&self) -> Result<WriteTransaction<'_>> {
        if is_write_tx_active() {
            bail!(
                "a WriteTransaction is already in progress on this thread; use tx.execute() instead"
            );
        }
        let guard = self.inner.write_lock.lock().map_err(|_| {
            anyhow::anyhow!("write lock is poisoned; database may be in an inconsistent state")
        })?;
        // Set flag only after successfully acquiring the lock
        set_write_tx_active(true);
        Ok(WriteTransaction {
            guard,
            inner: &self.inner,
            pending_facts: Vec::new(),
            next_pending_tx_count: self.inner.fact_storage.current_tx_count().saturating_add(1),
            next_pending_tx_id: crate::graph::types::tx_id_now(),
            committed: false,
        })
    }

    // ── Checkpoint ───────────────────────────────────────────────────────────

    /// Manually trigger a checkpoint: flush all in-memory facts to the main file
    /// and delete the WAL sidecar.
    ///
    /// No-op for in-memory databases.
    ///
    /// # Errors
    ///
    /// Returns an error if the write lock is poisoned or the checkpoint I/O fails.
    pub fn checkpoint(&self) -> Result<()> {
        let mut ctx = self.inner.write_lock.lock().map_err(|_| {
            anyhow::anyhow!("write lock is poisoned; database may be in an inconsistent state")
        })?;
        Self::do_checkpoint(&self.inner.fact_storage, &mut ctx).map(|_| ())
    }

    /// Create a linearized, checkpointed copy of a live file-backed database.
    ///
    /// The same write lock spans the source checkpoint, exact published-page
    /// copy, destination fsync, and atomic no-overwrite publish. A write that
    /// starts before this call may be included; a write that completes after
    /// the returned `tx_count` is not. The destination never includes a WAL
    /// sidecar and can be opened as an independent `.graph` file immediately
    /// after this method returns.
    ///
    /// Linearization covers this open handle and its [`Clone`]s, which share
    /// one writer mutex. Independently opening the same source pathname in the
    /// same process is outside the single-writer contract; route all access
    /// through the daemon-owned handle instead.
    ///
    /// The destination, its `.wal`, and its `.graph.lock` sidecar must not
    /// already exist. This method never overwrites a prior backup. Use a fresh
    /// sibling filename for each rollback point. Windows and Apple source
    /// aliases are compared with conservative case folding.
    ///
    /// If the final parent-directory fsync fails after the no-clobber publish,
    /// this method returns an error but may leave a complete, unacknowledged
    /// destination. Inspect or remove that fresh path before retrying.
    ///
    /// In-memory databases cannot be backed up with this file API.
    ///
    /// # Errors
    ///
    /// Returns an error if a `WriteTransaction` is active on this thread, the
    /// database is in memory, the target conflicts with the source or an
    /// existing destination/sidecar, or checkpoint/copy/sync/publish fails.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn backup_to(&self, destination: impl AsRef<Path>) -> Result<BackupOutcome> {
        self.backup_to_with_hook(destination.as_ref(), || {})
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn backup_to_with_hook(
        &self,
        destination: &Path,
        before_publish: impl FnOnce(),
    ) -> Result<BackupOutcome> {
        if is_write_tx_active() {
            bail!(
                "a WriteTransaction is already in progress on this thread; commit or roll it back before creating a backup"
            );
        }

        let mut ctx = self.inner.write_lock.lock().map_err(|_| {
            anyhow::anyhow!("write lock is poisoned; database may be in an inconsistent state")
        })?;
        let source_path = match &*ctx {
            WriteContext::Memory => bail!("backup_to requires a file-backed database"),
            WriteContext::File { db_path, .. } => db_path.clone(),
        };
        let target = Self::validate_backup_target(&source_path, destination)?;

        Self::do_checkpoint(&self.inner.fact_storage, &mut ctx)?;
        let (tx_count, bytes) = match &mut *ctx {
            WriteContext::Memory => unreachable!("file-backed context checked above"),
            WriteContext::File { pfs, .. } => {
                let tx_count = pfs.last_checkpointed_tx_count();
                let bytes = Self::copy_backup_candidate(pfs, &target, before_publish)?;
                (tx_count, bytes)
            }
        };
        // Keep the guard observably live through the atomic publish above.
        drop(ctx);

        Ok(BackupOutcome { tx_count, bytes })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn validate_backup_target(source: &Path, destination: &Path) -> Result<BackupTarget> {
        if destination.as_os_str().is_empty() {
            bail!("backup destination must not be empty");
        }
        let file_name = destination
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("backup destination must name a file"))?;
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = parent.canonicalize().with_context(|| {
            format!(
                "backup destination parent does not exist or is inaccessible: {}",
                parent.display()
            )
        })?;
        if !parent.is_dir() {
            bail!(
                "backup destination parent is not a directory: {}",
                parent.display()
            );
        }

        let destination = parent.join(file_name);
        let lexical_source = Self::resolve_lexical_source_path(source)?;
        let canonical_source = source
            .canonicalize()
            .context("failed to resolve source database path for backup")?;
        let conflicts = [
            lexical_source.clone(),
            Self::wal_path_for(&lexical_source),
            FileBackend::lock_path_for(&lexical_source),
            canonical_source.clone(),
            Self::wal_path_for(&canonical_source),
            FileBackend::lock_path_for(&canonical_source),
        ];
        if conflicts.iter().any(|path| {
            Self::backup_paths_share_namespace(
                path,
                &destination,
                cfg!(any(windows, target_vendor = "apple")),
            )
        }) {
            bail!(
                "backup destination conflicts with the source database or one of its sidecars: {}",
                destination.display()
            );
        }

        let target = BackupTarget {
            wal_path: Self::wal_path_for(&destination),
            lock_path: FileBackend::lock_path_for(&destination),
            destination,
            parent,
        };
        Self::ensure_backup_target_unoccupied(&target)?;
        Ok(target)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn resolve_lexical_source_path(source: &Path) -> Result<PathBuf> {
        let file_name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("source database path must name a file"))?;
        let parent = source
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = parent
            .canonicalize()
            .context("failed to resolve lexical source database parent")?;
        Ok(parent.join(file_name))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn backup_paths_share_namespace(left: &Path, right: &Path, case_insensitive: bool) -> bool {
        if left == right {
            return true;
        }
        case_insensitive
            && left
                .to_string_lossy()
                .to_lowercase()
                .eq(&right.to_string_lossy().to_lowercase())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_backup_target_unoccupied(target: &BackupTarget) -> Result<()> {
        for path in [&target.destination, &target.wal_path, &target.lock_path] {
            match std::fs::symlink_metadata(path) {
                Ok(_) => bail!(
                    "backup destination or sidecar already exists; refusing to overwrite: {}",
                    path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to inspect backup target {}", path.display())
                    });
                }
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn copy_backup_candidate(
        pfs: &mut PersistentFactStorage<FileBackend>,
        target: &BackupTarget,
        before_publish: impl FnOnce(),
    ) -> Result<u64> {
        let (temp_path, mut temp_file) = Self::create_backup_temp(target)?;
        let _cleanup = BackupTempFile {
            path: temp_path.clone(),
        };

        let bytes = pfs.copy_published_image_to(&mut temp_file)?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed to fsync backup candidate {}", temp_path.display()))?;
        drop(temp_file);

        before_publish();
        // Repeat all occupancy checks after the potentially long copy. The
        // hard link below is the final atomic no-clobber check for the graph
        // path itself.
        Self::ensure_backup_target_unoccupied(target)?;
        std::fs::hard_link(&temp_path, &target.destination).with_context(|| {
            format!(
                "failed to atomically publish backup at {}; destination must be absent and the filesystem must support hard links",
                target.destination.display()
            )
        })?;

        let _ = std::fs::remove_file(&temp_path);
        Self::sync_backup_parent(&target.parent)?;
        Ok(bytes)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn create_backup_temp(target: &BackupTarget) -> Result<(PathBuf, std::fs::File)> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let file_name = target
            .destination
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("backup destination must name a file"))?;

        for _ in 0..64 {
            let nonce = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut temp_name = std::ffi::OsString::from(".");
            temp_name.push(file_name);
            temp_name.push(format!(".vicia-backup-{}-{nonce}.tmp", std::process::id()));
            let path = target.parent.join(temp_name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to create backup candidate in {}",
                            target.parent.display()
                        )
                    });
                }
            }
        }
        bail!(
            "failed to allocate a unique backup candidate in {}",
            target.parent.display()
        )
    }

    #[cfg(all(not(target_arch = "wasm32"), unix))]
    fn sync_backup_parent(parent: &Path) -> Result<()> {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("failed to fsync backup directory {}", parent.display()))
    }

    #[cfg(all(not(target_arch = "wasm32"), not(unix)))]
    fn sync_backup_parent(_parent: &Path) -> Result<()> {
        Ok(())
    }

    /// Run idle/background maintenance for a file-backed database.
    ///
    /// This call is intended for embedding applications such as Vetch to invoke
    /// between interactive work slices, at startup/shutdown boundaries, or
    /// after imports. It first checkpoints any pending WAL-backed writes and
    /// then applies the internal delta-maintenance policy while holding the
    /// same write lock. It never runs automatically from foreground
    /// [`checkpoint`](Self::checkpoint).
    ///
    /// If checkpointing succeeds and later delta maintenance fails, the
    /// checkpoint remains durable and the retired WAL is not restored. That
    /// failure indicates maintenance should be retried on a later idle tick; it
    /// does not imply data loss.
    ///
    /// In-memory databases return a no-op outcome.
    ///
    /// # Errors
    ///
    /// Returns an error if a `WriteTransaction` is active on this thread, the
    /// write lock is poisoned, checkpoint I/O fails, or delta maintenance fails.
    pub fn run_idle_maintenance(&self) -> Result<MaintenanceOutcome> {
        if is_write_tx_active() {
            bail!(
                "a WriteTransaction is already in progress on this thread; commit or roll it back before running idle maintenance"
            );
        }

        let mut ctx = self.inner.write_lock.lock().map_err(|_| {
            anyhow::anyhow!("write lock is poisoned; database may be in an inconsistent state")
        })?;
        let checkpoint_outcome = Self::do_checkpoint(&self.inner.fact_storage, &mut ctx)?;
        let checkpoint_effect =
            MaintenanceCheckpointEffect::from_checkpoint_outcome(checkpoint_outcome);

        match &mut *ctx {
            WriteContext::Memory => Ok(MaintenanceOutcome::new(
                checkpoint_effect,
                MaintenanceDeltaEffect::Noop,
                MaintenanceAdvice::None,
            )),
            #[cfg(not(target_arch = "wasm32"))]
            WriteContext::File { pfs, .. } => {
                let decision = pfs.delta_maintenance_decision();
                let advice = MaintenanceAdvice::from_delta_decision(decision);
                let maintenance_outcome = pfs.run_idle_delta_maintenance()?;
                Ok(MaintenanceOutcome::new(
                    checkpoint_effect,
                    MaintenanceDeltaEffect::from_checkpoint_outcome(maintenance_outcome),
                    advice,
                ))
            }
        }
    }

    /// Returns the current monotonic transaction counter.
    ///
    /// This is the value that `:as-of N` compares against. After a successful
    /// [`execute`](Self::execute) that returns [`QueryResult::Transacted`] or
    /// [`QueryResult::Retracted`], this reflects the count of that transaction.
    ///
    /// Starts at 0 for a new database and increments once per `transact`/`retract` call,
    /// regardless of how many facts the batch contains.
    pub fn current_tx_count(&self) -> u64 {
        self.inner.fact_storage.current_tx_count()
    }

    /// Session-protocol status snapshot (A6). Crate-internal: the public
    /// surface is the `status` op on [`crate::session::Session`].
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn session_status(&self) -> Result<SessionStatusSnapshot> {
        let ctx = self.inner.write_lock.lock().map_err(|_| {
            anyhow::anyhow!("write lock is poisoned; database may be in an inconsistent state")
        })?;
        let (wal_bytes, delta_segments, delta_pages) = match &*ctx {
            WriteContext::Memory => (None, None, None),
            WriteContext::File { pfs, db_path, .. } => {
                let wal_path = Self::wal_path_for(db_path);
                let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).ok();
                let (segments, pages) = pfs.delta_growth_snapshot();
                (wal_bytes, Some(segments), Some(pages))
            }
        };
        let pending_facts = self.inner.fact_storage.pending_fact_count() as u64;
        // Exact total is only knowable without a committed full scan when
        // everything lives in memory; file-backed databases report None.
        let fact_count = if self.inner.fact_storage.has_committed_reader() {
            None
        } else {
            Some(pending_facts)
        };
        Ok(SessionStatusSnapshot {
            fact_count,
            pending_facts,
            tx_count: self.inner.fact_storage.current_tx_count(),
            wal_bytes,
            delta_segments,
            delta_pages,
        })
    }

    /// Whether the delta-growth thresholds currently advise maintenance.
    /// Backs the `maintenance_pending` durability classification on session
    /// write responses; `false` for in-memory databases and on lock poisoning.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn maintenance_advised(&self) -> bool {
        let Ok(ctx) = self.inner.write_lock.lock() else {
            return false;
        };
        match &*ctx {
            WriteContext::Memory => false,
            WriteContext::File { pfs, .. } => !matches!(
                pfs.delta_maintenance_decision(),
                DeltaMaintenanceDecision::ContinueDeltaAppend
            ),
        }
    }

    /// Export the complete append-only fact log in deterministic storage order.
    ///
    /// The returned records include assertions and retractions, including
    /// `tx_id`, `tx_count`, valid-time scope, and the `asserted` bit. Committed
    /// records are returned first, followed by pending in-memory records in the
    /// same order Minigraf would replay them.
    ///
    /// This is an audit/export surface, not a current-state projection. Use
    /// Datalog queries for net current views.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying fact reader fails.
    pub fn export_fact_log(&self) -> Result<Vec<FactRecord>> {
        let mut records = Vec::new();
        self.inner.fact_storage.for_each_fact(|fact| {
            records.push(FactRecord::from_fact(fact));
            Ok(())
        })?;
        Ok(records)
    }

    /// Export the append-only fact log tail: every record with
    /// `tx_count > since_tx_count`.
    ///
    /// Returns exactly the subsequence of [`Self::export_fact_log`] whose
    /// `tx_count` exceeds `since_tx_count` — same [`FactRecord`] shape, same
    /// deterministic storage order, assertions and retractions both included
    /// with their valid-time scope. `since_tx_count = 0` is equivalent to the
    /// full export; a `since_tx_count` at or past the current head returns an
    /// empty vec.
    ///
    /// Cost is proportional to the tail: committed pages are located with a
    /// tx-ordered page probe (no committed full scan), and delta/pending
    /// records are filtered in memory. This is the polling surface for
    /// "what changed since my last tick" callers — poll with the last seen
    /// `tx_count` as the cursor.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying fact reader fails.
    pub fn export_fact_log_since(&self, since_tx_count: u64) -> Result<Vec<FactRecord>> {
        let mut records = Vec::new();
        self.inner
            .fact_storage
            .for_each_fact_since(since_tx_count, |fact| {
                records.push(FactRecord::from_fact(fact));
                Ok(())
            })?;
        Ok(records)
    }

    /// Internal checkpoint logic (operates on an already-held write-lock guard).
    fn do_checkpoint(
        _fact_storage: &FactStorage,
        ctx: &mut WriteContext,
    ) -> Result<CheckpointOutcome> {
        match ctx {
            WriteContext::Memory => {
                // No-op for in-memory databases.
                Ok(CheckpointOutcome::Noop)
            }
            #[cfg(not(target_arch = "wasm32"))]
            WriteContext::File {
                pfs,
                wal,
                db_path,
                wal_entry_count,
            } => {
                // Skip checkpoint if nothing to flush.
                //
                // `wal_entry_count` is non-zero when this handle has made writes *or*
                // replayed WAL entries on open (crash-recovery path). `pfs.is_dirty()`
                // catches any facts marked dirty via the normal write path.
                //
                // File locking (`.graph.lock` sidecar, acquired by FileBackend::open)
                // already prevents a second *process* from opening the file while this
                // handle holds the lock, which covers the main multi-process exposure
                // described in issue #226. This guard closes the remaining edge cases:
                // same-process double-opens (same PID bypasses the stale-lock check)
                // and environments where the advisory lock can be bypassed (e.g.
                // network filesystems, manual lock deletion).
                if *wal_entry_count == 0 && !pfs.is_dirty() {
                    return Ok(CheckpointOutcome::Noop);
                }
                // `force_dirty` is needed for the WAL-replay case: facts were loaded
                // into memory during `replay_wal` but `pfs.dirty` was not set because
                // no write path was exercised. Without it `save()` would no-op and
                // the replayed facts would never reach the main file.
                pfs.force_dirty();
                let checkpoint_outcome = pfs.save()?;
                if *wal_entry_count > 0 && !checkpoint_outcome.permits_wal_retire() {
                    anyhow::bail!(
                        "Checkpoint produced no durable publish while WAL entries remain"
                    );
                }

                // Derive WAL path and delete the sidecar.
                let wal_path = Self::wal_path_for(db_path);

                // Close the WAL writer (drop it) before deleting the file.
                *wal = None;

                if wal_path.exists() {
                    WalWriter::delete_file(&wal_path)?;
                }

                // WAL writer will be recreated lazily on the next write.
                *wal_entry_count = 0;
                Ok(checkpoint_outcome)
            }
        }
    }

    /// Parse and plan a query once; bind slots (`$name`) are left unresolved.
    ///
    /// Returns a [`crate::query::datalog::prepared::PreparedQuery`] that can be executed
    /// many times with different bind values via
    /// [`crate::query::datalog::prepared::PreparedQuery::execute`].
    ///
    /// # Errors
    /// - Parse failure.
    /// - A bind slot appears in an attribute position (rejected at prepare time).
    /// - The command is not a `(query ...)` — `transact`, `retract`, and `rule`
    ///   are not preparable.
    pub fn prepare(
        &self,
        query_str: &str,
    ) -> Result<crate::query::datalog::prepared::PreparedQuery> {
        use crate::query::datalog::prepared::prepare_query;

        let cmd = parse_datalog_command(query_str).map_err(|e| anyhow::anyhow!("{}", e))?;

        let query = match cmd {
            DatalogCommand::Query(q) => q,
            DatalogCommand::Transact(_) => {
                anyhow::bail!("only (query ...) commands can be prepared; got transact")
            }
            DatalogCommand::Retract(_) => {
                anyhow::bail!("only (query ...) commands can be prepared; got retract")
            }
            DatalogCommand::Rule(_) => {
                anyhow::bail!("only (query ...) commands can be prepared; got rule")
            }
            DatalogCommand::Forget(_) => {
                anyhow::bail!("only (query ...) commands can be prepared; got forget")
            }
        };

        prepare_query(
            query,
            self.inner.fact_storage.clone(),
            self.inner.rules.clone(),
            self.inner.functions.clone(),
        )
    }

    /// Return an interactive REPL that reads commands from stdin.
    ///
    /// The REPL borrows the database for the duration of the session.
    /// Call [`crate::repl::Repl::run`] to start the interactive loop.
    ///
    /// # Example
    /// ```no_run
    /// # use minigraf::Minigraf;
    /// let db = Minigraf::in_memory().unwrap();
    /// db.repl().run();
    /// ```
    pub fn repl(&self) -> crate::repl::Repl<'_> {
        crate::repl::Repl::new(self)
    }

    /// Compute the WAL sidecar path for a given database path.
    #[cfg(not(target_arch = "wasm32"))]
    fn wal_path_for(db_path: &Path) -> PathBuf {
        let mut p = db_path.to_path_buf();
        let name = p
            .file_name()
            .map(|n| {
                let mut s = n.to_os_string();
                s.push(".wal");
                s
            })
            .unwrap_or_else(|| std::ffi::OsString::from("db.graph.wal"));
        p.set_file_name(name);
        p
    }

    // ── Materialize helpers ───────────────────────────────────────────────────

    /// Parse and materialize a bounded list of transact/retract commands
    /// without mutating database state.
    ///
    /// This is the shared preparation half of BrowserDb's atomic write API.
    /// Query, rule, and forget commands are intentionally excluded: their
    /// evaluation or registry semantics do not belong in a write-only batch.
    #[cfg(any(test, all(target_arch = "wasm32", feature = "browser")))]
    pub(crate) fn materialize_atomic_write_commands(
        commands: &[String],
    ) -> Result<MaterializedAtomicWrite> {
        if commands.is_empty() {
            bail!("executeAtomic requires at least one transact or retract command");
        }
        if commands.len() > BROWSER_ATOMIC_MAX_COMMANDS {
            bail!(
                "executeAtomic accepts at most {} commands (received {})",
                BROWSER_ATOMIC_MAX_COMMANDS,
                commands.len()
            );
        }

        let source_bytes = commands.iter().try_fold(0usize, |total, command| {
            total
                .checked_add(command.len())
                .ok_or_else(|| anyhow::anyhow!("executeAtomic input byte count overflow"))
        })?;
        if source_bytes > BROWSER_ATOMIC_MAX_SOURCE_BYTES {
            bail!(
                "executeAtomic accepts at most {} source bytes (received {})",
                BROWSER_ATOMIC_MAX_SOURCE_BYTES,
                source_bytes
            );
        }

        let mut facts = Vec::new();
        let mut transacted_fact_count = 0usize;
        let mut retracted_fact_count = 0usize;
        let mut preflight_fact_count = 0usize;
        let mut preflight_token_count = 0usize;
        let mut seen_fact_keys = std::collections::HashSet::new();

        for (index, source) in commands.iter().enumerate() {
            let (command_fact_count, command_token_count) = atomic_write_preflight(source)?;
            preflight_fact_count = preflight_fact_count
                .checked_add(command_fact_count)
                .ok_or_else(|| anyhow::anyhow!("executeAtomic fact count overflow"))?;
            if preflight_fact_count > BROWSER_ATOMIC_MAX_FACTS {
                bail!(
                    "executeAtomic accepts at most {} facts (command {} would raise the preflight count to {})",
                    BROWSER_ATOMIC_MAX_FACTS,
                    index,
                    preflight_fact_count
                );
            }
            preflight_token_count = preflight_token_count
                .checked_add(command_token_count)
                .ok_or_else(|| anyhow::anyhow!("executeAtomic token count overflow"))?;
            if preflight_token_count > BROWSER_ATOMIC_MAX_TOKENS {
                bail!(
                    "executeAtomic accepts at most {} lexical tokens",
                    BROWSER_ATOMIC_MAX_TOKENS
                );
            }
            let command = parse_datalog_command(source).map_err(|error| {
                anyhow::anyhow!("executeAtomic command {} failed to parse: {}", index, error)
            })?;
            let (materialized, is_retract) = match command {
                DatalogCommand::Transact(tx) => (
                    Self::materialize_transaction(&tx).map_err(|error| {
                        anyhow::anyhow!(
                            "executeAtomic command {} could not materialize: {}",
                            index,
                            error
                        )
                    })?,
                    false,
                ),
                DatalogCommand::Retract(tx) => (
                    Self::materialize_retraction(&tx).map_err(|error| {
                        anyhow::anyhow!(
                            "executeAtomic command {} could not materialize: {}",
                            index,
                            error
                        )
                    })?,
                    true,
                ),
                DatalogCommand::Query(_) => {
                    bail!(
                        "executeAtomic command {} is a query; only transact and retract are allowed",
                        index
                    )
                }
                DatalogCommand::Rule(_) => {
                    bail!(
                        "executeAtomic command {} is a rule; only transact and retract are allowed",
                        index
                    )
                }
                DatalogCommand::Forget(_) => {
                    bail!(
                        "executeAtomic command {} is a forget; only transact and retract are allowed",
                        index
                    )
                }
            };

            let next_fact_count = facts
                .len()
                .checked_add(materialized.len())
                .ok_or_else(|| anyhow::anyhow!("executeAtomic fact count overflow"))?;
            if next_fact_count > BROWSER_ATOMIC_MAX_FACTS {
                bail!(
                    "executeAtomic accepts at most {} facts (command {} would raise the batch to {})",
                    BROWSER_ATOMIC_MAX_FACTS,
                    index,
                    next_fact_count
                );
            }

            for fact in &materialized {
                let key = (fact.entity, fact.attribute.clone(), fact.value.clone());
                if !seen_fact_keys.insert(key) {
                    bail!(
                        "executeAtomic command {} repeats one entity/attribute/value fact; atomic fact order is intentionally undefined",
                        index
                    );
                }
            }

            if is_retract {
                retracted_fact_count = retracted_fact_count
                    .checked_add(materialized.len())
                    .ok_or_else(|| {
                        anyhow::anyhow!("executeAtomic retracted fact count overflow")
                    })?;
            } else {
                transacted_fact_count = transacted_fact_count
                    .checked_add(materialized.len())
                    .ok_or_else(|| {
                        anyhow::anyhow!("executeAtomic transacted fact count overflow")
                    })?;
            }
            facts.extend(materialized);
        }

        if facts.is_empty() {
            bail!("executeAtomic requires at least one fact");
        }

        Ok(MaterializedAtomicWrite {
            facts,
            command_count: commands.len(),
            transacted_fact_count,
            retracted_fact_count,
        })
    }

    /// Convert a `Transaction` into a list of assertion `Fact`s (tx_id and tx_count
    /// are set to 0 as placeholders; they are assigned at commit time).
    pub(crate) fn materialize_transaction(tx: &Transaction) -> Result<Vec<Fact>> {
        use crate::query::datalog::matcher::{edn_to_entity_id, edn_to_value};
        use crate::query::datalog::types::EdnValue;

        let tx_valid_from = tx.valid_from;
        let tx_valid_to = tx.valid_to;
        let mut facts = Vec::new();

        for pattern in &tx.facts {
            let entity = edn_to_entity_id(&pattern.entity)
                .map_err(|e| anyhow::anyhow!("invalid entity: {}", e))?;

            let attr = match &pattern.attribute {
                AttributeSpec::Real(EdnValue::Keyword(k)) => k.clone(),
                AttributeSpec::Real(_) => anyhow::bail!("attribute must be a keyword"),
                AttributeSpec::Pseudo(_) => anyhow::bail!("cannot transact a pseudo-attribute"),
            };

            let value = edn_to_value(&pattern.value)
                .map_err(|e| anyhow::anyhow!("invalid value: {}", e))?;

            let valid_from = pattern
                .valid_from
                .or(tx_valid_from)
                .unwrap_or(VALID_FROM_USE_TX_TIME);
            let valid_to = pattern
                .valid_to
                .or(tx_valid_to)
                .unwrap_or(VALID_TIME_FOREVER);

            facts.push(Fact::with_valid_time(
                entity, attr, value, 0, 0, valid_from, valid_to,
            ));
        }

        Ok(facts)
    }

    /// Convert a `Transaction` into a list of retraction `Fact`s.
    pub(crate) fn materialize_retraction(tx: &Transaction) -> Result<Vec<Fact>> {
        use crate::query::datalog::matcher::{edn_to_entity_id, edn_to_value};
        use crate::query::datalog::types::EdnValue;

        let tx_valid_from = tx.valid_from;
        let tx_valid_to = tx.valid_to;
        let mut facts = Vec::new();

        for pattern in &tx.facts {
            let entity = edn_to_entity_id(&pattern.entity)
                .map_err(|e| anyhow::anyhow!("invalid entity: {}", e))?;

            let attr = match &pattern.attribute {
                AttributeSpec::Real(EdnValue::Keyword(k)) => k.clone(),
                AttributeSpec::Real(_) => anyhow::bail!("attribute must be a keyword"),
                AttributeSpec::Pseudo(_) => anyhow::bail!("cannot transact a pseudo-attribute"),
            };

            let value = edn_to_value(&pattern.value)
                .map_err(|e| anyhow::anyhow!("invalid value: {}", e))?;

            let valid_from = pattern.valid_from.or(tx_valid_from);
            let valid_to = pattern.valid_to.or(tx_valid_to);

            if valid_from.is_some() || valid_to.is_some() {
                facts.push(Fact::retract_with_valid_time(
                    entity,
                    attr,
                    value,
                    0,
                    0,
                    valid_from.unwrap_or(VALID_FROM_USE_TX_TIME),
                    valid_to.unwrap_or(VALID_TIME_FOREVER),
                ));
            } else {
                let mut f = Fact::retract(entity, attr, value, 0);
                f.tx_count = 0;
                facts.push(f);
            }
        }

        Ok(facts)
    }

    /// Resolve the EAV triples a `(forget ...)` names — either by running the
    /// embedded query pinned to the closure time, or by converting the
    /// explicit fact list. Returns deduplicated triples.
    pub(crate) fn resolve_forget_triples(
        spec: &ForgetSpec,
        executor: &DatalogExecutor,
        closure_time: i64,
    ) -> Result<Vec<(crate::graph::types::EntityId, String, Value)>> {
        use crate::query::datalog::matcher::{edn_to_entity_id, edn_to_value};
        use crate::query::datalog::types::{EdnValue, ValidAt};
        use crate::storage::index::encode_value;
        use std::collections::HashSet;

        let mut seen: HashSet<(crate::graph::types::EntityId, String, Vec<u8>)> = HashSet::new();
        let mut triples = Vec::new();

        match &spec.source {
            ForgetSource::Query(query) => {
                // Evaluate at the closure time: matched facts are exactly
                // those whose valid-time windows contain T.
                let mut query = query.clone();
                query.valid_at = Some(ValidAt::Timestamp(closure_time));

                let QueryResult::QueryResults { results, .. } =
                    executor.execute(DatalogCommand::Query(query))?
                else {
                    bail!("internal error: forget query did not return query results");
                };

                for row in results {
                    let (Some(e), Some(a), Some(v)) = (row.first(), row.get(1), row.get(2)) else {
                        bail!("internal error: forget query row does not have 3 columns");
                    };
                    let Value::Ref(entity) = e else {
                        bail!(
                            "forget query first :find variable must bind entities \
                             (entity position in :where), got a non-entity value"
                        );
                    };
                    let Value::Keyword(attr) = a else {
                        bail!(
                            "forget query second :find variable must bind attributes \
                             (attribute position in :where), got a non-keyword value"
                        );
                    };
                    if seen.insert((*entity, attr.clone(), encode_value(v))) {
                        triples.push((*entity, attr.clone(), v.clone()));
                    }
                }
            }
            ForgetSource::Facts(patterns) => {
                for pattern in patterns {
                    let entity = edn_to_entity_id(&pattern.entity)
                        .map_err(|e| anyhow::anyhow!("invalid entity: {}", e))?;
                    let attr = match &pattern.attribute {
                        AttributeSpec::Real(EdnValue::Keyword(k)) => k.clone(),
                        AttributeSpec::Real(_) => bail!("attribute must be a keyword"),
                        AttributeSpec::Pseudo(_) => bail!("cannot forget a pseudo-attribute"),
                    };
                    let value = edn_to_value(&pattern.value)
                        .map_err(|e| anyhow::anyhow!("invalid value: {}", e))?;
                    if seen.insert((entity, attr.clone(), encode_value(&value))) {
                        triples.push((entity, attr, value));
                    }
                }
            }
        }

        Ok(triples)
    }

    /// Materialize the closure of every valid-time window containing
    /// `closure_time` for the given triples, as unstamped retract + truncated
    /// re-assert fact pairs. Returns the facts and the number of distinct
    /// triples that had at least one window closed.
    pub(crate) fn materialize_closure(
        storage: &FactStorage,
        triples: &[(crate::graph::types::EntityId, String, Value)],
        closure_time: i64,
    ) -> Result<(Vec<Fact>, usize)> {
        use crate::graph::storage::net_asserted_facts;

        let mut facts = Vec::new();
        let mut closed_triples = 0usize;

        for (entity, attr, value) in triples {
            let history = storage.facts_for_triple(entity, attr, value)?;
            let mut closed_any = false;
            // Overlapping windows that share a valid_from would truncate to
            // the same (valid_from, T) re-assert — emit it once.
            let mut reasserted_from: std::collections::HashSet<i64> =
                std::collections::HashSet::new();

            for window in net_asserted_facts(history) {
                if window.valid_from <= closure_time && closure_time < window.valid_to {
                    facts.push(Fact::retract_with_valid_time(
                        *entity,
                        attr.clone(),
                        value.clone(),
                        0,
                        0,
                        window.valid_from,
                        window.valid_to,
                    ));
                    // valid_from == closure_time would re-assert an empty
                    // (T, T) window no query can match — the retraction alone
                    // already expresses the full closure.
                    if window.valid_from < closure_time && reasserted_from.insert(window.valid_from)
                    {
                        facts.push(Fact::with_valid_time(
                            *entity,
                            attr.clone(),
                            value.clone(),
                            0,
                            0,
                            window.valid_from,
                            closure_time,
                        ));
                    }
                    closed_any = true;
                }
            }

            if closed_any {
                closed_triples = closed_triples.saturating_add(1);
            }
        }

        Ok((facts, closed_triples))
    }

    // ── UDF registration ─────────────────────────────────────────────────────

    /// Register a custom aggregate function.
    ///
    /// `Acc` is any `Send + 'static` type that serves as the accumulator.
    /// It is type-erased internally. The function is usable in both `:find`
    /// grouping position and `:over` (window) position.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` is already registered (built-in or UDF)
    /// or the function registry lock is poisoned.
    ///
    /// # Example
    /// ```
    /// # use minigraf::db::Minigraf;
    /// # use minigraf::Value;
    /// let db = Minigraf::in_memory().unwrap();
    /// db.register_aggregate(
    ///     "mysum",
    ///     || 0i64,
    ///     |acc: &mut i64, v: &Value| { if let Value::Integer(i) = v { *acc += i; } },
    ///     |acc: &i64, _n: usize| Value::Integer(*acc),
    /// ).unwrap();
    /// ```
    pub fn register_aggregate<Acc>(
        &self,
        name: &str,
        init: impl Fn() -> Acc + Send + Sync + 'static,
        step: impl Fn(&mut Acc, &Value) + Send + Sync + 'static,
        finalise: impl Fn(&Acc, usize) -> Value + Send + Sync + 'static,
    ) -> Result<()>
    where
        Acc: Any + Send + 'static,
    {
        let init_boxed: Arc<dyn Fn() -> Box<dyn Any + Send> + Send + Sync> =
            Arc::new(move || Box::new(init()) as Box<dyn Any + Send>);
        let step_boxed: UdfStepFn = Arc::new(move |acc, v| {
            // `init_boxed` always creates `Box<Acc>`, so downcast is infallible.
            if let Some(typed_acc) = acc.downcast_mut::<Acc>() {
                step(typed_acc, v);
            }
        });
        let finalise_boxed: UdfFinaliseFn = Arc::new(move |acc, n| {
            acc.downcast_ref::<Acc>()
                .map(|typed_acc| finalise(typed_acc, n))
                .unwrap_or(Value::Null)
        });

        let desc = AggregateDesc {
            impl_: AggImpl::Udf(UdfOps {
                init: init_boxed,
                step: step_boxed,
                finalise: finalise_boxed,
            }),
            is_builtin: false,
        };
        self.inner
            .functions
            .write()
            .map_err(|e| anyhow::anyhow!("function registry lock poisoned: {}", e))?
            .register_aggregate_desc(name.to_string(), desc)
    }

    /// Register a custom single-argument filter predicate.
    ///
    /// The predicate is usable in `[(name? ?var)]` `:where` clauses.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` is already registered (built-in or UDF)
    /// or the function registry lock is poisoned.
    ///
    /// # Example
    /// ```
    /// # use minigraf::db::Minigraf;
    /// # use minigraf::Value;
    /// let db = Minigraf::in_memory().unwrap();
    /// db.register_predicate(
    ///     "email?",
    ///     |v: &Value| matches!(v, Value::String(s) if s.contains('@')),
    /// ).unwrap();
    /// ```
    pub fn register_predicate(
        &self,
        name: &str,
        f: impl Fn(&Value) -> bool + Send + Sync + 'static,
    ) -> Result<()> {
        let desc = PredicateDesc {
            f: Arc::new(f),
            is_builtin: false,
        };
        self.inner
            .functions
            .write()
            .map_err(|e| anyhow::anyhow!("function registry lock poisoned: {}", e))?
            .register_predicate_desc(name.to_string(), desc)
    }
}

// ─── WriteTransaction ─────────────────────────────────────────────────────────

/// An explicit write transaction. Holds the write lock for its lifetime.
///
/// # Usage
/// ```no_run
/// use minigraf::db::Minigraf;
///
/// let db = Minigraf::in_memory().unwrap();
/// let mut tx = db.begin_write().unwrap();
/// tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#).unwrap();
/// tx.execute(r#"(transact [[:alice :person/age 30]])"#).unwrap();
/// tx.commit().unwrap();
/// ```
///
/// Dropping without committing performs an implicit rollback.
pub struct WriteTransaction<'a> {
    guard: MutexGuard<'a, WriteContext>,
    inner: &'a Inner,
    /// Facts buffered in this transaction (not yet committed to FactStorage).
    pending_facts: Vec<Fact>,
    /// Synthetic tx_count assigned to the next staged write batch.
    next_pending_tx_count: u64,
    /// Synthetic tx_id seed used to keep staged writes monotonic.
    next_pending_tx_id: TxId,
    /// Set to `true` after a successful `commit()` to suppress rollback in `Drop`.
    committed: bool,
}

impl<'a> WriteTransaction<'a> {
    /// Execute a Datalog command within this transaction.
    ///
    /// - **Writes** (transact / retract): buffered in-memory; not durable until `commit()`.
    ///   Returns `Ok(QueryResult::Ok)` immediately (not `Transacted`/`Retracted`).
    /// - **Reads** (query): see committed facts in `FactStorage` **plus** all facts
    ///   buffered in this transaction (read-your-own-writes).
    /// - **Rules**: registered immediately into the shared rule registry.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing or execution fails.
    pub fn execute(&mut self, input: &str) -> Result<QueryResult> {
        let cmd = parse_datalog_command(input).map_err(|e| anyhow::anyhow!("{}", e))?;

        match cmd {
            DatalogCommand::Transact(tx) => {
                self.stage_pending_facts(Minigraf::materialize_transaction(&tx)?);
                Ok(QueryResult::Ok)
            }
            DatalogCommand::Retract(tx) => {
                self.stage_pending_facts(Minigraf::materialize_retraction(&tx)?);
                Ok(QueryResult::Ok)
            }
            DatalogCommand::Query(_) => self.execute_read_command(cmd),
            DatalogCommand::Rule(rule) => self.execute_rule_command(rule),
            // Staged forget would compute closure windows against staged
            // valid_from sentinels that commit() re-stamps, silently missing
            // the exact-window retraction keys — reject rather than lose data.
            DatalogCommand::Forget(_) => Err(anyhow::anyhow!(
                "(forget ...) is not supported inside an explicit WriteTransaction; \
                 use Minigraf::execute"
            )),
        }
    }

    fn execute_read_command(&self, cmd: DatalogCommand) -> Result<QueryResult> {
        if self.pending_facts.is_empty() {
            let mut executor = DatalogExecutor::new_with_rules_and_functions(
                self.inner.fact_storage.clone(),
                self.inner.rules.clone(),
                self.inner.functions.clone(),
            );
            executor.set_limits(
                self.inner.options.max_derived_facts,
                self.inner.options.max_results,
            );
            return executor.execute(cmd);
        }

        let merged_facts = self.merged_query_facts()?;
        let mut executor = DatalogExecutor::new_from_facts_with_rules_and_functions(
            merged_facts,
            self.pending_read_now_floor(),
            self.inner.rules.clone(),
            self.inner.functions.clone(),
        );
        executor.set_limits(
            self.inner.options.max_derived_facts,
            self.inner.options.max_results,
        );
        executor.execute(cmd)
    }

    fn execute_rule_command(
        &self,
        rule: crate::query::datalog::types::Rule,
    ) -> Result<QueryResult> {
        let mut executor = DatalogExecutor::new_with_rules_and_functions(
            self.inner.fact_storage.clone(),
            self.inner.rules.clone(),
            self.inner.functions.clone(),
        );
        executor.set_limits(
            self.inner.options.max_derived_facts,
            self.inner.options.max_results,
        );
        executor.execute(DatalogCommand::Rule(rule))
    }

    /// Stage buffered facts with stable synthetic read metadata.
    fn stage_pending_facts(&mut self, facts: Vec<Fact>) {
        let staged_tx_id = std::cmp::max(crate::graph::types::tx_id_now(), self.next_pending_tx_id);
        let staged_tx_count = self.next_pending_tx_count;

        self.pending_facts.extend(facts.into_iter().map(|mut fact| {
            fact.tx_id = staged_tx_id;
            fact.tx_count = staged_tx_count;
            fact
        }));

        self.next_pending_tx_id = staged_tx_id.saturating_add(1);
        self.next_pending_tx_count = self.next_pending_tx_count.saturating_add(1);
    }

    /// Commit this transaction atomically.
    ///
    /// All buffered facts are stamped with a single `tx_count` / `tx_id`, then
    /// the WAL entry is written and fsynced **before** any fact is applied to the
    /// shared `FactStorage`.  This guarantees that if the WAL write fails the
    /// database is left completely unchanged (clean rollback — no cleanup needed).
    ///
    /// # Errors
    ///
    /// Returns an error if the WAL write or fact application fails.
    pub fn commit(mut self) -> Result<()> {
        let facts_to_commit = std::mem::take(&mut self.pending_facts);

        if !facts_to_commit.is_empty() {
            let tx_count = self.inner.fact_storage.allocate_tx_count();
            let tx_id = crate::graph::types::tx_id_now();

            // Stamp facts with tx_id and tx_count
            let stamped: Vec<Fact> = facts_to_commit
                .into_iter()
                .map(|mut f| {
                    f.tx_id = tx_id;
                    f.tx_count = tx_count;
                    // Fix valid_from if it was left as the sentinel (placeholder for "use tx time")
                    if f.valid_from == VALID_FROM_USE_TX_TIME {
                        f.valid_from = tx_id.cast_signed();
                    }
                    f
                })
                .collect();

            // WAL write includes size validation — no separate check needed.

            // Write WAL entry FIRST — if this fails, no facts have been applied
            // to shared FactStorage, so the database remains in a clean state.
            let should_checkpoint = Self::wal_write_stamped_batch(
                &mut self.guard,
                &self.inner.options,
                tx_count,
                &stamped,
            )?;

            // WAL succeeded — now apply facts to shared FactStorage.
            for fact in stamped {
                let _ = self.inner.fact_storage.load_fact(fact)?;
            }

            // Trigger auto-checkpoint AFTER facts are in FactStorage so the
            // checkpoint captures the newly written facts.
            if should_checkpoint {
                Minigraf::do_checkpoint(&self.inner.fact_storage, &mut self.guard)?;
            }
        }

        self.committed = true;
        set_write_tx_active(false);
        Ok(())
    }

    /// Write a pre-stamped batch of facts to the WAL.
    ///
    /// Accepts an already-computed `tx_count` and `facts` slice.
    /// Called while the write lock is held.
    ///
    /// Returns `true` if an auto-checkpoint should be triggered.  The caller is
    /// responsible for applying facts to `FactStorage` **before** triggering the
    /// checkpoint, so that the checkpoint captures the newly written facts.
    #[allow(unused_variables)]
    fn wal_write_stamped_batch(
        ctx: &mut WriteContext,
        opts: &OpenOptions,
        tx_count: u64,
        facts: &[Fact],
    ) -> Result<bool> {
        match ctx {
            WriteContext::Memory => Ok(false),
            #[cfg(not(target_arch = "wasm32"))]
            WriteContext::File {
                pfs,
                wal,
                db_path,
                wal_entry_count,
            } => {
                // Lazily open the WAL writer if not already open.
                if wal.is_none() {
                    let wal_path = Minigraf::wal_path_for(db_path);
                    *wal = Some(WalWriter::open_or_create(&wal_path)?);
                }

                let wal_writer = wal
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("WAL not initialized"))?;
                wal_writer.append_entry(tx_count, facts)?;
                pfs.mark_dirty();
                *wal_entry_count = wal_entry_count.saturating_add(1);

                Ok(*wal_entry_count >= opts.wal_checkpoint_threshold)
            }
        }
    }

    /// Explicitly roll back the transaction. Also happens implicitly on drop.
    pub fn rollback(mut self) {
        self.pending_facts.clear();
        self.committed = true; // Suppress rollback in Drop
        set_write_tx_active(false);
    }

    /// Build a merged fact snapshot for transactional reads.
    ///
    /// Pending facts still carry `VALID_FROM_USE_TX_TIME` where no
    /// explicit `valid_from` was supplied (the sentinel is left intact so `commit()`
    /// can stamp it with the real commit timestamp).  We resolve it here using each
    /// fact's own staged `tx_id` so transactional queries see a coherent valid-time.
    fn merged_query_facts(&self) -> Result<Arc<[Fact]>> {
        let committed = self.inner.fact_storage.get_all_facts()?;
        let mut merged =
            Vec::with_capacity(committed.len().saturating_add(self.pending_facts.len()));
        merged.extend(committed);
        merged.extend(self.pending_facts.iter().cloned().map(|mut f| {
            if f.valid_from == VALID_FROM_USE_TX_TIME {
                f.valid_from = f.tx_id.cast_signed();
            }
            f
        }));
        Ok(Arc::from(merged))
    }

    /// Return the read-time floor implied by pending facts only.
    fn pending_read_now_floor(&self) -> Option<i64> {
        self.pending_facts
            .iter()
            .map(|fact| fact.tx_id.cast_signed())
            .max()
    }
}

impl Drop for WriteTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Implicit rollback: pending facts are simply discarded.
            // No changes were made to the shared FactStorage during buffering,
            // so no cleanup is required there.
            self.pending_facts.clear();
            set_write_tx_active(false);
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    // ── repl factory ────────────────────────────────────────────────────────

    #[test]
    fn repl_constructed_from_db() {
        let db = Minigraf::in_memory().unwrap();
        let _repl = db.repl();
    }

    // ── in_memory basic ─────────────────────────────────────────────────────

    #[test]
    fn test_in_memory_no_wal_file() {
        let db = Minigraf::in_memory().unwrap();

        db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
            .unwrap();
        db.execute(r#"(transact [[:alice :person/age 30]])"#)
            .unwrap();

        let facts = db.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 2, "expected 2 facts after 2 transacts");
    }

    // ── begin_write / commit ─────────────────────────────────────────────────

    #[test]
    fn test_begin_write_commit_facts_visible() {
        let db = Minigraf::in_memory().unwrap();

        {
            let mut tx = db.begin_write().unwrap();
            tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();
            tx.execute(r#"(transact [[:alice :person/age 30]])"#)
                .unwrap();
            tx.commit().unwrap();
        }

        let facts = db.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 2, "committed facts must be visible");
    }

    // ── begin_write / rollback ────────────────────────────────────────────────

    // Atomic write preparation and shared core transaction identity.
    #[test]
    fn test_write_transaction_mixed_commit_uses_one_transaction_identity() {
        let db = Minigraf::in_memory().unwrap();
        db.execute("(transact [[:head :value :old]])").unwrap();

        let mut tx = db.begin_write().unwrap();
        tx.execute("(retract [[:head :value :old]])").unwrap();
        tx.execute("(transact [[:head :value :new] [:event :kind :replace]])")
            .unwrap();
        tx.commit().unwrap();

        let committed = db.inner.fact_storage.get_all_facts().unwrap();
        let batch: Vec<_> = committed.iter().filter(|fact| fact.tx_count == 2).collect();
        assert_eq!(batch.len(), 3, "mixed commit must retain all three facts");
        assert!(
            batch.iter().all(|fact| fact.tx_id == batch[0].tx_id),
            "mixed commit must stamp one tx_id"
        );
        assert!(
            batch.iter().all(|fact| fact.tx_count == 2),
            "mixed commit must stamp one tx_count"
        );
    }

    #[test]
    fn test_materialize_atomic_write_commands_is_write_only_and_bounded() {
        assert!(
            BROWSER_ATOMIC_MAX_FACTS >= 32_768usize.saturating_mul(4),
            "browser atomic fact budget must contain Vetch's maximum chunk envelope"
        );
        let commands = vec![
            "(retract [[:head :value :old]])".to_string(),
            "(transact [[:head :value :new] [:event :kind :replace]])".to_string(),
        ];
        let prepared = Minigraf::materialize_atomic_write_commands(&commands).unwrap();
        assert_eq!(prepared.command_count, 2);
        assert_eq!(prepared.transacted_fact_count, 2);
        assert_eq!(prepared.retracted_fact_count, 1);
        assert_eq!(prepared.facts.len(), 3);

        let empty = Minigraf::materialize_atomic_write_commands(&[]).unwrap_err();
        assert!(empty.to_string().contains("at least one"));

        let too_many = vec![
            "(transact [[:bounded :value true]])".to_string();
            BROWSER_ATOMIC_MAX_COMMANDS.saturating_add(1)
        ];
        let too_many = Minigraf::materialize_atomic_write_commands(&too_many).unwrap_err();
        assert!(too_many.to_string().contains("at most 256 commands"));

        for (kind, command) in [
            ("query", "(query [:find ?v :where [:head :value ?v]])"),
            ("rule", "(rule [(head ?v) [:head :value ?v]])"),
            ("forget", "(forget [[:head :value :old]])"),
        ] {
            let batch = vec![commands[1].clone(), command.to_string()];
            let error = Minigraf::materialize_atomic_write_commands(&batch).unwrap_err();
            assert!(error.to_string().contains(kind));
        }
    }

    #[test]
    fn test_materialize_atomic_write_commands_rejects_fact_overflow() {
        let tuples = (0..BROWSER_ATOMIC_MAX_FACTS.saturating_add(1))
            .map(|index| format!("[:batch/e{index} :value {index}]"))
            .collect::<Vec<_>>()
            .join(" ");
        let command = vec![format!("(transact [{tuples}])")];
        let error = Minigraf::materialize_atomic_write_commands(&command).unwrap_err();
        assert!(error.to_string().contains("at most 262144 facts"));
    }

    #[test]
    fn test_materialize_atomic_write_commands_rejects_same_fact_ordering() {
        for commands in [
            vec![
                "(retract [[:head :value :same]])".to_string(),
                "(transact [[:head :value :same]])".to_string(),
            ],
            vec![
                "(transact [[:head :value :same]])".to_string(),
                "(retract [[:head :value :same]])".to_string(),
            ],
            vec![
                "(transact [[:head :value :same]])".to_string(),
                "(transact [[:head :value :same]])".to_string(),
            ],
        ] {
            let error = Minigraf::materialize_atomic_write_commands(&commands).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("repeats one entity/attribute/value fact")
            );
        }

        let replacement = vec![
            "(retract [[:head :value :old]])".to_string(),
            "(transact [[:head :value :new]])".to_string(),
        ];
        assert!(Minigraf::materialize_atomic_write_commands(&replacement).is_ok());
    }

    #[test]
    fn test_atomic_write_preflight_rejects_token_bomb_before_parse() {
        let source = "x ".repeat(BROWSER_ATOMIC_MAX_TOKENS.saturating_add(1));
        let error = atomic_write_preflight(&source).unwrap_err();
        assert!(error.to_string().contains("lexical tokens"));
    }

    // begin_write / rollback
    #[test]
    fn test_begin_write_rollback_no_facts_visible() {
        let db = Minigraf::in_memory().unwrap();

        {
            let mut tx = db.begin_write().unwrap();
            tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();
            tx.rollback();
        }

        let facts = db.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 0, "rolled-back facts must not be visible");
    }

    // ── drop without commit = rollback ────────────────────────────────────────

    #[test]
    fn test_drop_without_commit_is_rollback() {
        let db = Minigraf::in_memory().unwrap();

        {
            let mut tx = db.begin_write().unwrap();
            tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();
            // tx dropped here without commit
        }

        let facts = db.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 0, "dropped transaction must act as rollback");
    }

    // ── read-your-own-writes ──────────────────────────────────────────────────

    #[test]
    fn test_write_transaction_read_your_own_writes() {
        let db = Minigraf::in_memory().unwrap();

        let mut tx = db.begin_write().unwrap();
        tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
            .unwrap();

        // Query within the same transaction should see the buffered fact.
        let result = tx
            .execute(r#"(query [:find ?name :where [?e :person/name ?name]])"#)
            .unwrap();

        match result {
            QueryResult::QueryResults { results, .. } => {
                assert_eq!(results.len(), 1, "buffered fact must be visible in query");
            }
            _ => panic!("expected QueryResults"),
        }

        tx.commit().unwrap();
    }

    #[test]
    fn test_write_transaction_query_with_pending_retraction_and_assertion() {
        let db = Minigraf::in_memory().unwrap();
        db.execute(r#"(transact [[:alice :person/name "Alice"] [:alice :person/age 30]])"#)
            .unwrap();

        let mut tx = db.begin_write().unwrap();
        tx.execute(r#"(retract [[:alice :person/age 30]])"#)
            .unwrap();
        tx.execute(r#"(transact [[:alice :person/age 31]])"#)
            .unwrap();

        let result = tx
            .execute(r#"(query [:find ?age :where [:alice :person/age ?age]])"#)
            .unwrap();

        match result {
            QueryResult::QueryResults { results, .. } => {
                assert_eq!(results.len(), 1, "should see only one visible age");
                assert_eq!(results[0][0], Value::Integer(31));
            }
            _ => panic!("expected QueryResults"),
        }
    }

    #[test]
    fn test_write_transaction_pending_retraction_only_hides_committed_fact() {
        let db = Minigraf::in_memory().unwrap();
        db.execute(r#"(transact [[:alice :person/age 30]])"#)
            .unwrap();

        let mut tx = db.begin_write().unwrap();
        tx.execute(r#"(retract [[:alice :person/age 30]])"#)
            .unwrap();

        let result = tx
            .execute(r#"(query [:find ?age :where [:alice :person/age ?age]])"#)
            .unwrap();

        match result {
            QueryResult::QueryResults { results, .. } => {
                assert_eq!(
                    results.len(),
                    0,
                    "retracted committed fact must not be visible"
                );
            }
            _ => panic!("expected QueryResults"),
        }
    }

    #[test]
    fn test_write_transaction_rule_query_with_pending_write() {
        let db = Minigraf::in_memory().unwrap();
        db.execute("(rule [(reachable ?x ?y) [?x :edge ?y]])")
            .unwrap();
        db.execute("(rule [(reachable ?x ?y) [?x :edge ?z] (reachable ?z ?y)])")
            .unwrap();
        db.execute("(transact [[:a :edge :b]])").unwrap();

        let mut tx = db.begin_write().unwrap();
        tx.execute("(transact [[:b :edge :c]])").unwrap();

        let result = tx
            .execute("(query [:find ?y :where (reachable :a ?y)])")
            .unwrap();

        match result {
            QueryResult::QueryResults { results, .. } => {
                assert!(
                    results
                        .iter()
                        .any(|row| row[0] == Value::Keyword(":c".to_string())),
                    "rule query should see pending edge"
                );
            }
            _ => panic!("expected QueryResults"),
        }
    }

    #[test]
    fn test_write_transaction_pending_metadata_is_stable_across_reads() {
        use chrono::{SecondsFormat, Utc};
        use std::time::Duration;

        let db = Minigraf::in_memory().unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.execute(r#"(transact [[:alice :person/age 30]])"#)
            .unwrap();

        let first = tx
            .execute(
                r#"(query [:find ?tx ?tc ?vf :any-valid-time :where [:alice :person/age ?age] [:alice :db/tx-id ?tx] [:alice :db/tx-count ?tc] [:alice :db/valid-from ?vf]])"#,
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(3));
        let second = tx
            .execute(
                r#"(query [:find ?tx ?tc ?vf :any-valid-time :where [:alice :person/age ?age] [:alice :db/tx-id ?tx] [:alice :db/tx-count ?tc] [:alice :db/valid-from ?vf]])"#,
            )
            .unwrap();

        let (first_row, tx_id) = match &first {
            QueryResult::QueryResults { results, .. } => {
                assert_eq!(results.len(), 1);
                let row = results[0].clone();
                let tx_id = match row[0] {
                    Value::Integer(i) => i,
                    _ => panic!("expected tx id integer"),
                };
                assert_eq!(row[1], Value::Integer(1));
                assert_eq!(row[2], Value::Integer(tx_id));
                (row, tx_id)
            }
            _ => panic!("expected QueryResults"),
        };

        match second {
            QueryResult::QueryResults {
                results: second_results,
                ..
            } => {
                assert_eq!(second_results.len(), 1);
                assert_eq!(first_row, second_results[0]);
            }
            _ => panic!("expected QueryResults"),
        }

        tx.execute(r#"(transact [[:alice :person/age 31]])"#)
            .unwrap();

        let future_tx_id = tx_id as u64 + 60_000;
        let future_fact = Fact::with_valid_time(
            uuid::Uuid::new_v4(),
            ":future/marker".to_string(),
            Value::Integer(99),
            future_tx_id,
            1,
            future_tx_id.cast_signed(),
            VALID_TIME_FOREVER,
        );
        db.inner.fact_storage.load_fact(future_fact).unwrap();

        let as_of_timestamp = chrono::DateTime::<Utc>::from_timestamp_millis(tx_id)
            .unwrap()
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let as_of_query = format!(
            r#"(query [:find ?age :as-of "{}" :where [:alice :person/age ?age]])"#,
            as_of_timestamp
        );
        let as_of_result = tx.execute(&as_of_query).unwrap();

        match as_of_result {
            QueryResult::QueryResults { results, .. } => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0][0], Value::Integer(30));
            }
            _ => panic!("expected QueryResults"),
        }

        let future_query = tx
            .execute(r#"(query [:find ?v :where [?e :future/marker ?v]])"#)
            .unwrap();
        match future_query {
            QueryResult::QueryResults { results, .. } => {
                assert_eq!(
                    results.len(),
                    0,
                    "future committed facts must not become visible from pending-only floor"
                );
            }
            _ => panic!("expected QueryResults"),
        }
    }

    // ── thread-local flag: same-thread reentrant error ────────────────────────

    #[test]
    fn test_same_thread_reentrant_write_returns_error() {
        let db = Minigraf::in_memory().unwrap();

        let _tx = db.begin_write().unwrap();

        // While _tx is active, db.execute() on the same thread should fail fast.
        let err = db
            .execute(r#"(transact [[:bob :person/name "Bob"]])"#)
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("WriteTransaction is already in progress"),
            "expected reentrant-write error, got: {}",
            err
        );
    }

    // ── thread-local flag cleared after commit ────────────────────────────────

    #[test]
    fn test_thread_local_flag_cleared_after_commit() {
        let db = Minigraf::in_memory().unwrap();

        {
            let mut tx = db.begin_write().unwrap();
            tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();
            tx.commit().unwrap();
        }

        // After commit, begin_write should succeed again on the same thread.
        let result = db.begin_write();
        assert!(
            result.is_ok(),
            "begin_write must succeed after commit clears the flag"
        );
        result.unwrap().rollback();
    }

    // ── thread-local flag cleared after rollback ──────────────────────────────

    #[test]
    fn test_thread_local_flag_cleared_after_rollback() {
        let db = Minigraf::in_memory().unwrap();

        {
            let tx = db.begin_write().unwrap();
            tx.rollback();
        }

        let result = db.begin_write();
        assert!(
            result.is_ok(),
            "begin_write must succeed after rollback clears the flag"
        );
        result.unwrap().rollback();
    }

    // ── thread-local flag cleared after drop ─────────────────────────────────

    #[test]
    fn test_thread_local_flag_cleared_after_drop() {
        let db = Minigraf::in_memory().unwrap();

        {
            let mut tx = db.begin_write().unwrap();
            tx.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();
            // dropped here without commit
        }

        let result = db.begin_write();
        assert!(
            result.is_ok(),
            "begin_write must succeed after drop clears the flag"
        );
        result.unwrap().rollback();
    }

    // ── in-memory checkpoint is a no-op ──────────────────────────────────────

    #[test]
    fn test_in_memory_checkpoint_is_noop() {
        let db = Minigraf::in_memory().unwrap();
        db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
            .unwrap();
        // Should not error
        db.checkpoint().unwrap();
        // Facts should still be present
        let facts = db.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn test_idle_maintenance_in_memory_is_noop() {
        let db = Minigraf::in_memory().unwrap();
        db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
            .unwrap();

        let outcome = db.run_idle_maintenance().unwrap();

        assert_eq!(outcome.checkpoint, MaintenanceCheckpointEffect::Noop);
        assert_eq!(outcome.delta, MaintenanceDeltaEffect::Noop);
        assert_eq!(outcome.advice, MaintenanceAdvice::None);
        let facts = db.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 1, "maintenance must preserve in-memory facts");
    }

    #[test]
    fn test_idle_maintenance_flushes_pending_file_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maintenance-flush.graph");
        let db = Minigraf::open_with_options(
            &path,
            OpenOptions {
                wal_checkpoint_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .unwrap();
        db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
            .unwrap();

        let outcome = db.run_idle_maintenance().unwrap();

        assert_eq!(outcome.checkpoint, MaintenanceCheckpointEffect::Published);
        assert_eq!(outcome.delta, MaintenanceDeltaEffect::Noop);
        assert_eq!(outcome.advice, MaintenanceAdvice::None);
        assert!(
            !Minigraf::wal_path_for(&path).exists(),
            "maintenance checkpoint must retire WAL after durable publish"
        );
        drop(db);

        let reopened = Minigraf::open(&path).unwrap();
        let records = reopened.export_fact_log().unwrap();
        assert_eq!(records.len(), 1, "maintenance must persist pending write");
    }

    #[test]
    fn test_idle_maintenance_checkpoints_then_recompacts_threshold_delta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maintenance-recompact.graph");
        let db = Minigraf::open_with_options(
            &path,
            OpenOptions {
                wal_checkpoint_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .unwrap();
        db.execute(r#"(transact [[:base :bench/name "base"]])"#)
            .unwrap();
        db.checkpoint().unwrap();

        for index in 0..1_023u64 {
            let command = format!(r#"(transact [[:delta-{index} :bench/value {index}]])"#);
            db.execute(&command).unwrap();
            db.checkpoint().unwrap();
        }
        db.execute(r#"(transact [[:delta-pending :bench/value 1023]])"#)
            .unwrap();

        let outcome = db.run_idle_maintenance().unwrap();

        assert_eq!(outcome.checkpoint, MaintenanceCheckpointEffect::Published);
        assert_eq!(outcome.delta, MaintenanceDeltaEffect::Recompacted);
        assert_eq!(outcome.advice, MaintenanceAdvice::ReduceCheckpointCadence);
        assert!(
            !Minigraf::wal_path_for(&path).exists(),
            "maintenance must retire WAL before returning"
        );
        let records = db.export_fact_log().unwrap();
        assert_eq!(
            records.len(),
            1_025,
            "maintenance must preserve base, checkpointed deltas, and pending write"
        );

        let second = db.run_idle_maintenance().unwrap();
        assert_eq!(second.checkpoint, MaintenanceCheckpointEffect::Noop);
        assert_eq!(second.delta, MaintenanceDeltaEffect::Noop);
        assert_eq!(second.advice, MaintenanceAdvice::None);
        drop(db);

        let reopened = Minigraf::open(&path).unwrap();
        let reopened_records = reopened.export_fact_log().unwrap();
        assert_eq!(
            reopened_records.len(),
            1_025,
            "reopened maintenance result must preserve fact log"
        );
    }

    #[test]
    fn test_checkpoint_does_not_run_delta_maintenance_on_threshold_delta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint-no-recompact.graph");
        let db = Minigraf::open_with_options(
            &path,
            OpenOptions {
                wal_checkpoint_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .unwrap();
        db.execute(r#"(transact [[:base :bench/name "base"]])"#)
            .unwrap();
        db.checkpoint().unwrap();

        for index in 0..1_023u64 {
            let command = format!(r#"(transact [[:delta-{index} :bench/value {index}]])"#);
            db.execute(&command).unwrap();
            db.checkpoint().unwrap();
        }
        db.execute(r#"(transact [[:delta-pending :bench/value 1023]])"#)
            .unwrap();

        db.checkpoint().unwrap();

        let decision = {
            let ctx = db.inner.write_lock.lock().unwrap();
            match &*ctx {
                WriteContext::Memory => DeltaMaintenanceDecision::ContinueDeltaAppend,
                #[cfg(not(target_arch = "wasm32"))]
                WriteContext::File { pfs, .. } => pfs.delta_maintenance_decision(),
            }
        };
        assert_eq!(
            decision,
            DeltaMaintenanceDecision::MaintenanceBackpressure,
            "foreground checkpoint must leave threshold delta for idle maintenance"
        );
        let records = db.export_fact_log().unwrap();
        assert_eq!(
            records.len(),
            1_025,
            "checkpoint must preserve base and accumulated deltas"
        );

        let outcome = db.run_idle_maintenance().unwrap();
        assert_eq!(outcome.checkpoint, MaintenanceCheckpointEffect::Noop);
        assert_eq!(outcome.delta, MaintenanceDeltaEffect::Recompacted);
    }

    #[test]
    fn test_idle_maintenance_rejects_same_thread_write_transaction() {
        let db = Minigraf::in_memory().unwrap();
        let _tx = db.begin_write().unwrap();

        let result = db.run_idle_maintenance();

        assert!(
            result.is_err(),
            "maintenance must not deadlock behind same-thread write transaction"
        );
    }

    #[test]
    fn test_backup_preserves_pending_full_history_and_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.graph");
        let backup_path = dir.path().join("backup.graph");
        let source = "00000000-0000-0000-0000-0000000000a1";
        let target = "00000000-0000-0000-0000-0000000000b2";
        let db = Minigraf::open_with_options(
            &source_path,
            OpenOptions {
                wal_checkpoint_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .unwrap();
        db.execute(&format!(
            r#"(transact {{:valid-from "2026-01-01"}} [[#uuid "{source}" :edge/to #uuid "{target}"] [#uuid "{target}" :name "target"]])"#
        ))
        .unwrap();
        db.execute(&format!(
            r#"(retract {{:valid-from "2026-01-01"}} [[#uuid "{source}" :edge/to #uuid "{target}"]])"#
        ))
        .unwrap();
        let expected = db.export_fact_log().unwrap();
        assert!(
            Minigraf::wal_path_for(&source_path).exists(),
            "fixture must exercise pending WAL checkpointing"
        );

        let outcome = db.backup_to(&backup_path).unwrap();

        assert_eq!(outcome.tx_count, 2);
        assert_eq!(
            outcome.bytes,
            std::fs::metadata(&backup_path).unwrap().len()
        );
        assert_eq!(outcome.bytes % crate::storage::PAGE_SIZE as u64, 0);
        assert!(
            !Minigraf::wal_path_for(&source_path).exists(),
            "backup must retire the source WAL after checkpoint publish"
        );
        assert!(
            !Minigraf::wal_path_for(&backup_path).exists(),
            "backup is an independent checkpointed graph without a WAL"
        );

        let backup = Minigraf::open(&backup_path).unwrap();
        assert_eq!(backup.current_tx_count(), outcome.tx_count);
        assert!(
            backup.export_fact_log().unwrap() == expected,
            "backup must preserve exact full-history fact identity"
        );
    }

    #[test]
    fn test_backup_rejects_unsafe_targets_without_overwrite_or_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.graph");
        let db = Minigraf::open(&source_path).unwrap();
        db.execute(r#"(transact [[:safe :value "source"]])"#)
            .unwrap();
        db.checkpoint().unwrap();

        let memory = Minigraf::in_memory().unwrap();
        assert!(
            memory.backup_to(dir.path().join("memory.graph")).is_err(),
            "in-memory backup must be explicit error"
        );

        let existing = dir.path().join("existing.graph");
        std::fs::write(&existing, b"sentinel").unwrap();
        assert!(db.backup_to(&existing).is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"sentinel");

        let stale_wal_target = dir.path().join("stale-wal.graph");
        let stale_wal = Minigraf::wal_path_for(&stale_wal_target);
        std::fs::write(&stale_wal, b"unrelated-wal").unwrap();
        assert!(db.backup_to(&stale_wal_target).is_err());
        assert!(!stale_wal_target.exists());
        assert_eq!(std::fs::read(&stale_wal).unwrap(), b"unrelated-wal");

        let stale_lock_target = dir.path().join("stale-lock.graph");
        let stale_lock = FileBackend::lock_path_for(&stale_lock_target);
        std::fs::write(&stale_lock, b"occupied").unwrap();
        assert!(db.backup_to(&stale_lock_target).is_err());
        assert!(!stale_lock_target.exists());
        assert_eq!(std::fs::read(&stale_lock).unwrap(), b"occupied");

        let source_wal = Minigraf::wal_path_for(&source_path);
        assert!(
            !source_wal.exists(),
            "fixture needs absent source WAL for alias check"
        );
        assert!(
            db.backup_to(&source_wal).is_err(),
            "source WAL pathname must never become a backup"
        );
        assert!(
            Minigraf::backup_paths_share_namespace(
                Path::new("C:/memory/DB.graph.wal"),
                Path::new("c:/MEMORY/db.GRAPH.WAL"),
                true,
            ),
            "Windows/Apple target validation must conservatively reject case-folded aliases"
        );
        assert!(
            !Minigraf::backup_paths_share_namespace(
                Path::new("/memory/DB.graph.wal"),
                Path::new("/memory/db.graph.wal"),
                false,
            ),
            "case-sensitive targets keep distinct path semantics"
        );

        let tx = db.begin_write().unwrap();
        assert!(
            db.backup_to(dir.path().join("during-tx.graph")).is_err(),
            "same-thread transaction must reject instead of deadlock"
        );
        tx.rollback();
    }

    #[test]
    fn test_backup_write_lock_spans_copy_and_atomic_publish() {
        use std::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("linear-source.graph");
        let backup_path = dir.path().join("linear-backup.graph");
        let db = Minigraf::open_with_options(
            &source_path,
            OpenOptions {
                wal_checkpoint_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .unwrap();
        db.execute(r#"(transact [[:before :value 1]])"#).unwrap();

        let writer_db = db.clone();
        let (start_tx, start_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            start_rx.recv().unwrap();
            attempt_tx.send(()).unwrap();
            let result = writer_db.execute(r#"(transact [[:after :value 2]])"#);
            done_tx.send(result.is_ok()).unwrap();
        });

        let outcome = db
            .backup_to_with_hook(&backup_path, || {
                assert!(
                    matches!(
                        db.inner.write_lock.try_lock(),
                        Err(std::sync::TryLockError::WouldBlock)
                    ),
                    "backup must still own the writer lock immediately before publish"
                );
                start_tx.send(()).unwrap();
                attempt_rx.recv().unwrap();
                assert!(
                    done_rx
                        .recv_timeout(std::time::Duration::from_millis(100))
                        .is_err(),
                    "writer must remain blocked after copy and before backup publish"
                );
            })
            .unwrap();
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            "writer must complete after backup releases the lock"
        );
        writer.join().unwrap();

        assert_eq!(outcome.tx_count, 1);
        assert_eq!(db.current_tx_count(), 2);
        let backup = Minigraf::open(&backup_path).unwrap();
        assert_eq!(backup.current_tx_count(), 1);
        let backup_log = backup.export_fact_log().unwrap();
        assert_eq!(backup_log.len(), 1);
        assert_eq!(backup_log[0].attribute, ":value");
        assert!(
            backup_log[0].value == Value::Integer(1),
            "backup must contain only the pre-linearization value"
        );
        assert_eq!(db.export_fact_log().unwrap().len(), 2);
    }

    #[test]
    fn test_backup_publish_conflict_cleans_candidate_and_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("conflict-source.graph");
        let backup_path = dir.path().join("conflict-backup.graph");
        let db = Minigraf::open(&source_path).unwrap();
        db.execute(r#"(transact [[:safe :value 1]])"#).unwrap();

        let result = db.backup_to_with_hook(&backup_path, || {
            std::fs::write(&backup_path, b"racer").unwrap();
        });

        assert!(
            result.is_err(),
            "publish race must reject without overwrite"
        );
        assert_eq!(std::fs::read(&backup_path).unwrap(), b"racer");
        assert_eq!(db.export_fact_log().unwrap().len(), 1);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".vicia-backup-")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "failed backup must clean temp candidate"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_backup_rejects_symlink_source_wal_alias() {
        let dir = tempfile::tempdir().unwrap();
        let real_path = dir.path().join("real.graph");
        let link_path = dir.path().join("link.graph");
        {
            let seed = Minigraf::open(&real_path).unwrap();
            seed.execute(r#"(transact [[:seed :value 1]])"#).unwrap();
            seed.checkpoint().unwrap();
        }
        std::os::unix::fs::symlink(&real_path, &link_path).unwrap();
        let db = Minigraf::open(&link_path).unwrap();
        let lexical_wal = Minigraf::wal_path_for(&link_path);
        assert!(!lexical_wal.exists());

        assert!(
            db.backup_to(&lexical_wal).is_err(),
            "backup must reject the WAL path actually used by a symlink-opened handle"
        );
        assert!(!lexical_wal.exists());
        db.execute(r#"(transact [[:after :value 2]])"#).unwrap();
        assert!(
            lexical_wal.exists(),
            "later source writes must still own their WAL path"
        );
    }

    #[test]
    fn test_backup_receipt_uses_published_watermark_after_failed_write() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("hole-source.graph");
        let backup_path = dir.path().join("hole-backup.graph");
        let db = Minigraf::open_with_options(
            &source_path,
            OpenOptions {
                wal_checkpoint_threshold: usize::MAX,
                ..Default::default()
            },
        )
        .unwrap();
        let oversized = "x".repeat(crate::storage::packed_pages::MAX_FACT_BYTES + 1);
        let failed = db.execute(&format!(
            r#"(transact [[:too-large :value "{oversized}"]])"#
        ));
        assert!(
            failed.is_err(),
            "oversized write must fail before WAL apply"
        );
        assert_eq!(
            db.current_tx_count(),
            1,
            "failed WAL validation currently leaves a counter hole"
        );

        let outcome = db.backup_to(&backup_path).unwrap();
        assert_eq!(
            outcome.tx_count, 0,
            "receipt must describe the published header, not the in-memory counter hole"
        );
        let backup = Minigraf::open(&backup_path).unwrap();
        assert_eq!(backup.current_tx_count(), outcome.tx_count);
        assert_eq!(backup.export_fact_log().unwrap().len(), 0);
    }

    // ── file-backed: open_with_options custom threshold ───────────────────────

    #[test]
    fn test_open_with_options_custom_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.graph");

        let opts = OpenOptions {
            wal_checkpoint_threshold: 5,
            page_cache_size: 256,
            max_derived_facts: 100_000,
            max_results: 1_000_000,
        };
        let db = Minigraf::open_with_options(&path, opts).unwrap();
        assert_eq!(db.inner.options.wal_checkpoint_threshold, 5);
    }

    // ── failed commit leaves database unchanged ───────────────────────────────

    #[test]
    #[cfg(unix)] // directory-as-WAL trick is Unix-specific; skipped on Windows
    fn test_failed_commit_leaves_database_unchanged() {
        fn count_results(result: QueryResult) -> usize {
            match result {
                QueryResult::QueryResults { results, .. } => results.len(),
                _ => 0,
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.graph");
        let wal_path = {
            let mut p = db_path.as_os_str().to_owned();
            p.push(".wal");
            std::path::PathBuf::from(p)
        };

        // Open file-backed db and commit one fact so the main file + WAL both exist
        let db = Minigraf::open(&db_path).unwrap();
        db.execute("(transact [[:alice :name \"Alice\"]])").unwrap();

        // Checkpoint: flushes Alice to the main file, closes and deletes the WAL.
        // After this, WriteContext::File { wal: None } so the next commit will
        // try to create a new WAL file at wal_path.
        db.checkpoint().unwrap();
        assert!(!wal_path.exists(), "WAL must be gone after checkpoint");

        // Place a directory at the WAL path so WalWriter::open_or_create() fails
        // with EISDIR when it tries to open the path for writing.
        std::fs::create_dir(&wal_path).unwrap();

        // Begin a transaction and buffer a fact
        let mut tx = db.begin_write().unwrap();
        tx.execute("(transact [[:bob :name \"Bob\"]])").unwrap();

        // Commit should fail because the WAL path is now a directory
        let result = tx.commit();

        // Restore the directory so tempdir cleanup works
        std::fs::remove_dir(&wal_path).unwrap();

        assert!(
            result.is_err(),
            "commit should fail when WAL path is a directory"
        );

        // Bob must NOT be visible (failed commit must not apply facts)
        let n = count_results(
            db.execute("(query [:find ?name :where [?e :name ?name]])")
                .unwrap(),
        );
        assert_eq!(
            n, 1,
            "only Alice should be visible; Bob's failed commit must be rolled back"
        );
    }

    // ── file-backed: checkpoint deletes WAL and updates main file ─────────────

    #[test]
    fn test_file_backed_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.graph");
        let wal_path = dir.path().join("test.graph.wal");

        {
            let db = Minigraf::open(&path).unwrap();
            db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();

            // WAL should exist before checkpoint
            assert!(wal_path.exists(), "WAL must exist after a write");

            db.checkpoint().unwrap();

            // WAL should be deleted after checkpoint
            assert!(!wal_path.exists(), "WAL must be deleted after checkpoint");
        }

        // Reopen after first handle is dropped (releases file lock)
        let db2 = Minigraf::open(&path).unwrap();
        let facts = db2.inner.fact_storage.get_asserted_facts().unwrap();
        assert_eq!(facts.len(), 1, "facts must survive checkpoint");
    }

    #[test]
    fn test_materialize_transaction_non_keyword_real_attr_error() {
        // Exercises db.rs line 576: Real(_) bail! in materialize_transaction (non-keyword Real attr)
        use crate::query::datalog::types::EdnValue;
        use crate::query::datalog::types::{Pattern, Transaction};
        let tx = Transaction {
            facts: vec![Pattern::new(
                EdnValue::Keyword(":alice".to_string()),
                EdnValue::Integer(42), // Real(Integer) — not a keyword
                EdnValue::Integer(0),
            )],
            valid_from: None,
            valid_to: None,
        };
        let r = Minigraf::materialize_transaction(&tx);
        assert!(
            r.is_err(),
            "materialize_transaction with non-keyword Real attr must fail"
        );
    }

    #[test]
    fn test_materialize_retraction_non_keyword_real_attr_error() {
        // Exercises db.rs line 613: Real(_) bail! in materialize_retraction (non-keyword Real attr)
        use crate::query::datalog::types::EdnValue;
        use crate::query::datalog::types::{Pattern, Transaction};
        let tx = Transaction {
            facts: vec![Pattern::new(
                EdnValue::Keyword(":alice".to_string()),
                EdnValue::String("not-a-keyword".to_string()), // Real(String) — not a keyword
                EdnValue::Integer(0),
            )],
            valid_from: None,
            valid_to: None,
        };
        let r = Minigraf::materialize_retraction(&tx);
        assert!(
            r.is_err(),
            "materialize_retraction with non-keyword Real attr must fail"
        );
    }

    #[test]
    fn test_materialize_transaction_pseudo_attr_error() {
        // Exercises db.rs line ~577: Pseudo(_) bail! in materialize_transaction
        use crate::query::datalog::types::EdnValue;
        use crate::query::datalog::types::{Pattern, PseudoAttr, Transaction};
        let tx = Transaction {
            facts: vec![Pattern::pseudo(
                EdnValue::Keyword(":alice".to_string()),
                PseudoAttr::ValidFrom,
                EdnValue::Integer(0),
            )],
            valid_from: None,
            valid_to: None,
        };
        let r = Minigraf::materialize_transaction(&tx);
        assert!(
            r.is_err(),
            "materialize_transaction with pseudo-attr must fail"
        );
    }

    #[test]
    fn test_materialize_retraction_pseudo_attr_error() {
        // Exercises db.rs line ~614: Pseudo(_) bail! in materialize_retraction
        use crate::query::datalog::types::EdnValue;
        use crate::query::datalog::types::{Pattern, PseudoAttr, Transaction};
        let tx = Transaction {
            facts: vec![Pattern::pseudo(
                EdnValue::Keyword(":alice".to_string()),
                PseudoAttr::TxCount,
                EdnValue::Integer(0),
            )],
            valid_from: None,
            valid_to: None,
        };
        let r = Minigraf::materialize_retraction(&tx);
        assert!(
            r.is_err(),
            "materialize_retraction with pseudo-attr must fail"
        );
    }

    // ── begin_write flag not leaked on lock failure ────────────────────────────────

    #[test]
    fn test_begin_write_flag_not_leaked_on_lock_failure() {
        // This test verifies that the thread-local flag is not set if lock acquisition fails.
        // We can't easily simulate lock failure in normal test, but we can verify the
        // flag is correctly managed: set after lock acquired, cleared on drop.

        let db = Minigraf::in_memory().unwrap();

        // Normal flow: begin_write succeeds, flag should be set
        {
            let _tx = db.begin_write().unwrap();
            assert!(
                is_write_tx_active(),
                "flag should be set during active transaction"
            );
        }
        // After drop, flag should be cleared
        assert!(
            !is_write_tx_active(),
            "flag should be cleared after transaction ends"
        );

        // Multiple sequential transactions should work
        {
            let _tx = db.begin_write().unwrap();
        }
        assert!(
            !is_write_tx_active(),
            "flag should be cleared after second transaction"
        );

        {
            let _tx = db.begin_write().unwrap();
        }
        assert!(
            !is_write_tx_active(),
            "flag should be cleared after third transaction"
        );
    }

    // ── query complexity limits ───────────────────────────────────────────────────

    #[test]
    fn test_max_derived_facts_limit_enforced() {
        // Recursive rule will derive many facts
        // Low limit should trigger error
        let low_opts = OpenOptions::default()
            .max_derived_facts(5)
            .max_results(1_000_000);
        let db_low = Minigraf::in_memory_with_options(low_opts).unwrap();

        // Add base edges in a chain
        db_low.execute("(transact [[:a :edge :b] [:b :edge :c] [:c :edge :d] [:d :edge :e] [:e :edge :f]])").unwrap();

        // Register recursive rule
        db_low
            .execute(r#"(rule [(reachable ?x ?y) [?x :edge ?y]])"#)
            .unwrap();
        db_low
            .execute(r#"(rule [(reachable ?x ?y) [?x :edge ?z] (reachable ?z ?y)])"#)
            .unwrap();

        let result = db_low.execute("(query [:find ?to :where (reachable :a ?to)])");
        assert!(
            result.is_err(),
            "Query should fail with max_derived_facts limit"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("derived") || err_msg.contains("limit"),
            "Error should mention derived facts limit, got: {}",
            err_msg
        );

        // Same query with higher limit should succeed
        let high_opts = OpenOptions::default()
            .max_derived_facts(100_000)
            .max_results(1_000_000);
        let db_high = Minigraf::in_memory_with_options(high_opts).unwrap();

        db_high.execute("(transact [[:a :edge :b] [:b :edge :c] [:c :edge :d] [:d :edge :e] [:e :edge :f]])").unwrap();
        db_high
            .execute(r#"(rule [(reachable ?x ?y) [?x :edge ?y]])"#)
            .unwrap();
        db_high
            .execute(r#"(rule [(reachable ?x ?y) [?x :edge ?z] (reachable ?z ?y)])"#)
            .unwrap();

        let result = db_high.execute("(query [:find ?to :where (reachable :a ?to)])");
        assert!(result.is_ok(), "Query should succeed with higher limit");
    }

    #[test]
    fn test_per_query_max_derived_facts_via_execute() {
        let db = OpenOptions::new()
            .max_derived_facts(1_000_000)
            .open_memory()
            .unwrap();

        db.execute("(rule [(reachable ?x ?y) [?x :edge ?y]])")
            .unwrap();
        db.execute("(rule [(reachable ?x ?z) [?x :edge ?y] (reachable ?y ?z)])")
            .unwrap();
        db.execute(r#"(transact [[:a :edge :b] [:b :edge :c]])"#)
            .unwrap();

        // Per-query limit of 1 — too tight, must fail
        let result =
            db.execute("(query [:find ?x ?y :where (reachable ?x ?y) :max-derived-facts 1])");
        assert!(result.is_err(), "per-query limit of 1 should fail");

        // Per-query limit of 1M — should succeed
        let result =
            db.execute("(query [:find ?x ?y :where (reachable ?x ?y) :max-derived-facts 1000000])");
        assert!(result.is_ok(), "per-query limit of 1M should succeed");

        // No per-query limit — should fall back to OpenOptions default (1M) and succeed
        let result = db.execute("(query [:find ?x ?y :where (reachable ?x ?y)])");
        assert!(
            result.is_ok(),
            "no per-query limit should use database default"
        );
    }

    #[test]
    fn test_per_query_max_results_via_execute() {
        let db = Minigraf::in_memory().unwrap();
        db.execute(r#"(transact [[:a :v 1] [:b :v 2] [:c :v 3]])"#)
            .unwrap();

        // Confirms the field parses cleanly and the query succeeds
        let result = db.execute("(query [:find ?e :where [?e :v ?v] :max-results 1])");
        assert!(
            result.is_ok(),
            "query with :max-results should parse and execute"
        );
    }

    // ── read-only handle drop must not modify the file ────────────────────────

    #[test]
    fn test_readonly_handle_drop_does_not_modify_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.graph");

        // Write a fact and checkpoint so the main file is clean and WAL is gone.
        {
            let db = Minigraf::open(&path).unwrap();
            db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
                .unwrap();
            db.checkpoint().unwrap();
        }

        // db2 is opened with the standard read-write constructor — there is no
        // dedicated read-only open API.  The fix being tested is behavioral:
        // Drop must not checkpoint when no writes were made on this handle.
        let meta_before = std::fs::metadata(&path).unwrap();
        let len_before = meta_before.len();

        // Open a second handle, do a read-only query, then drop it.
        {
            let db2 = Minigraf::open(&path).unwrap();
            let result = db2
                .execute(r#"(query [:find ?name :where [?e :person/name ?name]])"#)
                .unwrap();
            match result {
                QueryResult::QueryResults { results, .. } => {
                    assert_eq!(results.len(), 1, "Alice must be visible");
                }
                _ => panic!("expected QueryResults"),
            }
            // db2 dropped here — Drop must NOT write to the file
        }

        // File must be byte-for-byte identical (same size).
        let meta_after = std::fs::metadata(&path).unwrap();
        assert_eq!(
            meta_after.len(),
            len_before,
            "file size must not change after read-only handle drop"
        );
    }
}

// ─── WASI smoke test ─────────────────────────────────────────────────────────
// Gated to target_os = "wasi" only. Regular #[test] works here because
// cargo test --target wasm32-wasip1 uses Wasmtime as the runner
// (CARGO_TARGET_WASM32_WASIP1_RUNNER). Not gated on target_arch = "wasm32"
// because the browser target (wasm32-unknown-unknown) requires
// #[wasm_bindgen_test] instead, which is a separate harness.
#[cfg(all(target_os = "wasi", test))]
mod wasi_tests {
    use crate::db::Minigraf;
    use crate::query::datalog::executor::QueryResult;

    #[test]
    fn in_memory_smoke() {
        let db = Minigraf::in_memory().expect("open in-memory db");
        db.execute("(transact [[:e1 :name \"hello\"]])")
            .expect("transact");
        let r = db
            .execute("(query [:find ?e :where [?e :name _]])")
            .expect("query");
        match r {
            QueryResult::QueryResults { results, .. } => {
                assert!(!results.is_empty());
            }
            _ => panic!("expected QueryResults"),
        }
    }
}
