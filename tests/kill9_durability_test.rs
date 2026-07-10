//! A7 kill -9 durability harness (harrekki P0 #3).
//!
//! Spawns real `minigraf --session --file` child processes, pipelines framed
//! NDJSON requests at them, and SIGKILLs the child at randomized instants —
//! including mid-checkpoint — over many kill cycles on the same growing
//! `.graph` lineage. After every kill the parent reaps the child, reopens the
//! file, and audits it against the model of acknowledged transactions.
//!
//! Acknowledged = the parent read a complete (`\n`-terminated) response line
//! with `ok:true` and `result.type == "transacted"`. Per the A6 session
//! protocol that frame is only written after the WAL entry is fsynced
//! (`durability: "applied"`), so every acknowledged transaction must survive
//! SIGKILL.
//!
//! Gate (docs/APP_ADOPTION_GAP_PLAN.md A7): zero lost acknowledged
//! transactions, zero unopenable files.
//!
//! Division of labor: `tests/delta_checkpoint_crash_recovery_test.rs` pins
//! each named checkpoint crash window deterministically via file surgery;
//! this harness contributes randomized end-to-end composition (real SIGKILL,
//! WAL replay, auto-checkpoints, growing base) under the resident-daemon
//! profile.
//!
//! Scope caveats, by design:
//! - SIGKILL validates process-death durability, not power loss (the kernel
//!   page cache survives the kill).
//! - `maintenance` ops exercise the maintenance checkpoint path only; delta
//!   recompact thresholds (1024 segments / ratio pages) are unreachable at
//!   this scale.
//!
//! Determinism: the seed reproduces the schedule (op sequence, burst sizes,
//! sampled kill delays), not the exact kill instant relative to child
//! progress. Reproduction is statistical; on failure the lineage directory
//! is preserved and its path printed for autopsy.
//!
//! Env overrides: `VICIA_A7_SEED` (both tests), `VICIA_A7_CYCLES` (nightly
//! gate only, so a stray env var cannot inflate the default suite).
//!
//! Full gate run:
//! `cargo test --release --test kill9_durability_test -- --ignored --nocapture`
//!
//! A8 extension point: add new write-path ops as one arm in
//! `gen_stream_op`'s weight roll plus one `OpKind` variant plus one model
//! rule in `verify_cycle`.

#![cfg(all(unix, not(target_arch = "wasm32")))]

use minigraf::{EntityId, OpenOptions, QueryResult, Value};
use serde_json::Value as JVal;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_SEED: u64 = 0xA7A7_2026_0711;
const ATTR_SEQ: &str = ":h/seq";
const ATTR_CYC: &str = ":h/cyc";
const ATTR_MODE: &str = ":h/mode";
const CALIBRATION_WRITES: u32 = 128;
const MAX_LINEAGE_CYCLES: u32 = 500;

// ─── Deterministic PRNG (no new dependency) ─────────────────────────────────

struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, n)`; modulo bias is irrelevant at harness precision.
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// Uniform in `[lo, hi)`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo).max(1))
    }
}

// ─── Config ──────────────────────────────────────────────────────────────────

struct HarnessConfig {
    cycles: u32,
    seed: u64,
    /// Mode A ops per cycle, `[lo, hi)`.
    stream_len: (u64, u64),
    /// Mode B/C transact burst before the checkpoint/maintenance op, `[lo, hi)`.
    burst_len: (u64, u64),
    /// Rotate to a fresh lineage file once the audit export exceeds this.
    rotate_at_records: usize,
    /// Every N cycles the parent checkpoints after verification (bounds WAL
    /// growth and proves checkpoint-after-recovery works).
    fold_every: u32,
    /// Watchdog for a wedged child; breach is an infra signal, not a gate failure.
    cycle_deadline: Duration,
}

fn seed_from_env() -> u64 {
    std::env::var("VICIA_A7_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SEED)
}

impl HarnessConfig {
    fn smoke() -> Self {
        HarnessConfig {
            cycles: 24,
            seed: seed_from_env(),
            stream_len: (8, 24),
            burst_len: (32, 64),
            rotate_at_records: 512,
            fold_every: 8,
            cycle_deadline: Duration::from_secs(30),
        }
    }

    fn gate() -> Self {
        let cycles = std::env::var("VICIA_A7_CYCLES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2_400);
        HarnessConfig {
            cycles,
            stream_len: (10, 60),
            burst_len: (64, 256),
            rotate_at_records: 8_192,
            fold_every: 25,
            ..Self::smoke()
        }
    }
}

// ─── Workload ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum KillMode {
    RandomInstant,
    MidCheckpoint,
    MidMaintenance,
}

impl KillMode {
    fn as_str(self) -> &'static str {
        match self {
            KillMode::RandomInstant => "random-instant",
            KillMode::MidCheckpoint => "mid-checkpoint",
            KillMode::MidMaintenance => "mid-maintenance",
        }
    }
}

enum OpKind {
    Transact { seq: u64, fact_count: u8 },
    Query { target: u64 },
    Checkpoint,
    Status,
    Maintenance,
}

struct Op {
    index: usize,
    kind: OpKind,
    line: String,
}

enum KillPlan {
    /// Fire SIGKILL this long after the writer thread starts.
    AfterDelay(Duration),
    /// Fire SIGKILL `delay` after the `count`-th complete response line.
    AfterResponses { count: usize, delay: Duration },
}

struct Workload {
    ops: Vec<Op>,
    kill: KillPlan,
    mode: KillMode,
}

fn transact_line(seq: u64, cycle: u32, mode_ix: u32, fact_count: u8, id: usize) -> String {
    if fact_count == 1 {
        format!(
            "{{\"op\":\"execute\",\"datalog\":\"(transact [[:k{seq} :h/seq {seq}]])\",\"id\":{id}}}"
        )
    } else {
        format!(
            "{{\"op\":\"execute\",\"datalog\":\"(transact [[:k{seq} :h/seq {seq}] [:k{seq} :h/cyc {cycle}] [:k{seq} :h/mode {mode_ix}]])\",\"id\":{id}}}"
        )
    }
}

fn query_line(target: u64, id: usize) -> String {
    format!(
        "{{\"op\":\"execute\",\"datalog\":\"(query [:find ?v :where [:k{target} :h/seq ?v]])\",\"id\":{id}}}"
    )
}

fn bare_op_line(op: &str, id: usize) -> String {
    format!("{{\"op\":\"{op}\",\"id\":{id}}}")
}

fn random_expected_seq(lineage: &Lineage, rng: &mut SplitMix64) -> Option<u64> {
    if lineage.expected.is_empty() {
        return None;
    }
    let n = rng.below(lineage.expected.len() as u64) as usize;
    lineage.expected.keys().nth(n).copied()
}

fn gen_stream_op(
    lineage: &Lineage,
    rng: &mut SplitMix64,
    cycle: u32,
    next_seq: &mut u64,
    index: usize,
) -> Op {
    let roll = rng.below(100);
    // Weight table — the A8 extension point.
    if roll < 70 {
        let seq = bump(next_seq);
        Op { index, kind: OpKind::Transact { seq, fact_count: 1 }, line: transact_line(seq, cycle, 0, 1, index) }
    } else if roll < 85 {
        let seq = bump(next_seq);
        Op { index, kind: OpKind::Transact { seq, fact_count: 3 }, line: transact_line(seq, cycle, 0, 3, index) }
    } else if roll < 93 {
        match random_expected_seq(lineage, rng) {
            Some(target) => Op { index, kind: OpKind::Query { target }, line: query_line(target, index) },
            None => {
                let seq = bump(next_seq);
                Op { index, kind: OpKind::Transact { seq, fact_count: 1 }, line: transact_line(seq, cycle, 0, 1, index) }
            }
        }
    } else if roll < 97 {
        Op { index, kind: OpKind::Checkpoint, line: bare_op_line("checkpoint", index) }
    } else if roll < 99 {
        Op { index, kind: OpKind::Status, line: bare_op_line("status", index) }
    } else {
        Op { index, kind: OpKind::Maintenance, line: bare_op_line("maintenance", index) }
    }
}

fn bump(next_seq: &mut u64) -> u64 {
    let s = *next_seq;
    *next_seq += 1;
    s
}

fn generate_workload(
    lineage: &Lineage,
    cfg: &HarnessConfig,
    rng: &mut SplitMix64,
    mode: KillMode,
    cycle: u32,
    next_seq: &mut u64,
) -> Workload {
    match mode {
        KillMode::RandomInstant => {
            let len = rng.range(cfg.stream_len.0, cfg.stream_len.1);
            let ops: Vec<Op> = (0..len as usize)
                .map(|i| gen_stream_op(lineage, rng, cycle, next_seq, i))
                .collect();
            let est_micros = (lineage.per_op_est.as_micros() as u64).saturating_mul(len);
            let delay = Duration::from_micros(rng.below((est_micros * 9 / 10).max(1)));
            Workload { ops, kill: KillPlan::AfterDelay(delay), mode }
        }
        KillMode::MidCheckpoint | KillMode::MidMaintenance => {
            let burst = rng.range(cfg.burst_len.0, cfg.burst_len.1) as usize;
            let mut ops: Vec<Op> = (0..burst)
                .map(|i| {
                    let fact_count = if rng.below(100) < 80 { 1 } else { 3 };
                    let seq = bump(next_seq);
                    Op { index: i, kind: OpKind::Transact { seq, fact_count }, line: transact_line(seq, cycle, 1, fact_count, i) }
                })
                .collect();
            let (final_op, final_kind) = if mode == KillMode::MidCheckpoint {
                ("checkpoint", OpKind::Checkpoint)
            } else {
                ("maintenance", OpKind::Maintenance)
            };
            ops.push(Op { index: burst, kind: final_kind, line: bare_op_line(final_op, burst) });
            let max_delay = (lineage.ckpt_est.as_micros() as u64 * 5 / 4).max(1);
            let delay = Duration::from_micros(rng.below(max_delay));
            Workload { ops, kill: KillPlan::AfterResponses { count: burst, delay }, mode }
        }
    }
}

// ─── Child session lifecycle ─────────────────────────────────────────────────

fn spawn_session_child(db_path: &Path, stderr: Stdio) -> Child {
    Command::new(env!("CARGO_BIN_EXE_minigraf"))
        .arg("--session")
        .arg("--file")
        .arg(db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .expect("spawn minigraf --session child")
}

fn sync_round_trip(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>, req: &str) -> JVal {
    writeln!(stdin, "{req}").expect("write calibration request");
    stdin.flush().expect("flush calibration request");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read calibration response");
    serde_json::from_str(line.trim()).expect("calibration response must be valid JSON")
}

/// Sleep with sub-millisecond precision (WSL2/CI timer granularity is coarse).
fn precise_sleep(d: Duration) {
    let start = Instant::now();
    if let Some(coarse) = d.checked_sub(Duration::from_millis(1)) {
        thread::sleep(coarse);
    }
    while start.elapsed() < d {
        std::hint::spin_loop();
    }
}

/// Belt-and-braces against PID reuse: `FileLock` already removes stale locks
/// via `/proc`, but only delete the sidecar ourselves when its content is
/// exactly the PID we killed. Never masks a foreign live lock.
fn clear_stale_lock_if_ours(db_path: &Path, killed_pid: u32) {
    let lock_path = db_path.with_extension("graph.lock");
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if content.trim() == killed_pid.to_string() {
            let _ = std::fs::remove_file(&lock_path);
        }
    }
}

struct RawCycle {
    responses: Vec<String>,
    signal: Option<i32>,
    stderr: String,
    deadline_hit: bool,
}

fn run_cycle(db_path: &Path, workload: &Workload, deadline: Duration) -> RawCycle {
    let mut child = spawn_session_child(db_path, Stdio::piped());
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();

    // Drain stderr so a chatty child can never block on a full pipe; keep a
    // bounded capture for diagnostics.
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stderr_pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < 65_536 {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    // Reader: only a complete `\n`-terminated line counts as a response.
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let resp_count = Arc::new(AtomicUsize::new(0));
    let (reader_lines, reader_count) = (lines.clone(), resp_count.clone());
    let reader = thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line.ends_with('\n') {
                        reader_lines.lock().unwrap().push(line);
                        reader_count.fetch_add(1, Ordering::SeqCst);
                    } else {
                        // Partial line at pipe EOF after the kill: not a response.
                        break;
                    }
                }
            }
        }
    });

    // Writer: pipeline every request without waiting for responses. Returns
    // the stdin handle instead of dropping it — EOF would let the child exit
    // gracefully (and Drop-checkpoint), silently voiding the kill cycle.
    let op_lines: Vec<String> = workload.ops.iter().map(|o| o.line.clone()).collect();
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        for l in &op_lines {
            // BrokenPipe after the kill is the expected end of stream.
            if stdin.write_all(l.as_bytes()).is_err() {
                break;
            }
            if stdin.write_all(b"\n").is_err() {
                break;
            }
            if stdin.flush().is_err() {
                break;
            }
        }
        stdin
    });

    let started = Instant::now();
    let mut deadline_hit = false;
    match workload.kill {
        KillPlan::AfterDelay(d) => precise_sleep(d),
        KillPlan::AfterResponses { count, delay } => {
            while resp_count.load(Ordering::SeqCst) < count {
                if started.elapsed() > deadline {
                    deadline_hit = true;
                    break;
                }
                thread::sleep(Duration::from_micros(200));
            }
            if !deadline_hit {
                precise_sleep(delay);
            }
        }
    }

    // Teardown order is load-bearing: kill, then reap (a zombie still owns
    // /proc/<pid>, which would defeat FileLock's stale-lock detection), then
    // join the pipe threads, then drop stdin.
    let _ = child.kill();
    let status = child.wait().expect("reap killed child");
    reader.join().expect("reader thread");
    let retained_stdin = writer.join().expect("writer thread");
    drop(retained_stdin);
    let stderr_text = stderr_handle.join().expect("stderr thread");

    clear_stale_lock_if_ours(db_path, pid);

    let responses = Arc::try_unwrap(lines)
        .expect("reader thread done")
        .into_inner()
        .unwrap();
    RawCycle { responses, signal: status.signal(), stderr: stderr_text, deadline_hit }
}

// ─── Analysis: responses → acknowledged set ──────────────────────────────────

struct CycleOutcome {
    /// Transactions with a complete `ok:true` transacted frame: must survive.
    acked: Vec<(u64, u8)>,
    /// Transactions without a complete ack: may survive (all-or-nothing).
    maybe: Vec<(u64, u8)>,
    /// Kill confirmed to have landed in the trailing checkpoint/maintenance
    /// gap (every burst write acked, final op unanswered). Upper-bound
    /// approximation: the request may still have been unread.
    mid_ckpt: bool,
}

fn analyze(workload: &Workload, raw: &RawCycle) -> Result<CycleOutcome, String> {
    let mut by_id: HashMap<usize, JVal> = HashMap::new();
    for line in &raw.responses {
        let v: JVal = serde_json::from_str(line.trim())
            .map_err(|_| "complete response line is not valid JSON (framing corruption)".to_string())?;
        let id = v
            .get("id")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| "response frame missing the echoed numeric id".to_string())?;
        by_id.insert(id as usize, v);
    }

    let mut acked = Vec::new();
    let mut maybe = Vec::new();
    for op in &workload.ops {
        match by_id.get(&op.index) {
            Some(resp) => {
                if resp["ok"] != JVal::Bool(true) {
                    let kind = resp["error"]["kind"].as_str().unwrap_or("?");
                    let msg = resp["error"]["message"].as_str().unwrap_or("?");
                    return Err(format!("child rejected op {}: kind={kind} message={msg}", op.index));
                }
                match &op.kind {
                    OpKind::Transact { seq, fact_count } => {
                        if resp["result"]["type"] != "transacted" {
                            return Err(format!("transact op {} answered with a non-transacted frame", op.index));
                        }
                        acked.push((*seq, *fact_count));
                    }
                    OpKind::Query { target } => {
                        let rows = resp["result"]["results"].as_array().map(|a| a.len()).unwrap_or(0);
                        if rows != 1 {
                            return Err(format!("query for durable seq {target} returned {rows} rows"));
                        }
                    }
                    OpKind::Checkpoint | OpKind::Status | OpKind::Maintenance => {}
                }
            }
            None => {
                if let OpKind::Transact { seq, fact_count } = &op.kind {
                    maybe.push((*seq, *fact_count));
                }
            }
        }
    }

    let mid_ckpt = matches!(workload.mode, KillMode::MidCheckpoint | KillMode::MidMaintenance)
        && !raw.deadline_hit
        && workload.ops.last().is_some_and(|last| !by_id.contains_key(&last.index))
        && workload
            .ops
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Transact { .. }))
            .all(|o| by_id.contains_key(&o.index));

    Ok(CycleOutcome { acked, maybe, mid_ckpt })
}

// ─── Lineage: one .graph file across many kill cycles ────────────────────────

struct Lineage {
    dir: Option<tempfile::TempDir>,
    db_path: PathBuf,
    /// seq → fact_count for every transaction that must be present.
    expected: BTreeMap<u64, u8>,
    cycles_on_file: u32,
    last_export_len: usize,
    ckpt_est: Duration,
    per_op_est: Duration,
}

impl Lineage {
    fn fresh(next_seq: &mut u64) -> Self {
        let dir = tempfile::tempdir().expect("create lineage tempdir");
        let db_path = dir.path().join("kill.graph");
        let mut lineage = Lineage {
            dir: Some(dir),
            db_path,
            expected: BTreeMap::new(),
            cycles_on_file: 0,
            last_export_len: 0,
            ckpt_est: Duration::from_millis(5),
            per_op_est: Duration::from_micros(500),
        };
        lineage.calibrate(next_seq);
        lineage
    }

    /// One un-killed session: seeds the file, and measures per-op and
    /// checkpoint durations that the kill-delay distributions sample from.
    fn calibrate(&mut self, next_seq: &mut u64) {
        let mut child = spawn_session_child(&self.db_path, Stdio::inherit());
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());

        let t0 = Instant::now();
        for _ in 0..CALIBRATION_WRITES {
            let seq = bump(next_seq);
            let resp = sync_round_trip(&mut stdin, &mut stdout, &transact_line(seq, 0, 9, 1, 0));
            assert_eq!(resp["ok"], true, "calibration write must succeed");
            self.expected.insert(seq, 1);
        }
        self.per_op_est = (t0.elapsed() / CALIBRATION_WRITES).max(Duration::from_micros(200));

        let t1 = Instant::now();
        let resp = sync_round_trip(&mut stdin, &mut stdout, "{\"op\":\"checkpoint\",\"id\":0}");
        assert_eq!(resp["ok"], true, "calibration checkpoint must succeed");
        self.ckpt_est = t1
            .elapsed()
            .clamp(Duration::from_millis(2), Duration::from_millis(500));

        drop(stdin);
        let status = child.wait().expect("calibration child exit");
        assert!(status.success(), "calibration child must exit 0 on EOF");
    }
}

// ─── Verification ────────────────────────────────────────────────────────────

type SeqLocations = BTreeMap<u64, Vec<(EntityId, u64)>>;

struct VerifyStats {
    export_len: usize,
    promoted: u32,
}

fn verify_cycle(
    lineage: &mut Lineage,
    outcome: &CycleOutcome,
    next_seq: &mut u64,
    cycle: u32,
    cfg: &HarnessConfig,
) -> Result<VerifyStats, String> {
    lineage.expected.extend(outcome.acked.iter().copied());

    // Observer open: wal_checkpoint_threshold = usize::MAX is load-bearing —
    // without the sentinel, Drop would checkpoint and fold the WAL after
    // every cycle, gutting the replay coverage this harness exists for.
    let db = OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..OpenOptions::default()
    }
    .path(&lineage.db_path)
    .open()
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("locked by another process") {
            // The child is dead and reaped, and the parent heals its own
            // leaked locks — a residual lock here means a kill left the
            // database unopenable without manual intervention.
            format!("GATE unopenable file (lock survived the kill): {msg}")
        } else {
            format!("GATE unopenable file: {msg}")
        }
    })?;

    let export = db
        .export_fact_log()
        .map_err(|e| format!("GATE unopenable file (export failed): {e}"))?;

    let mut seq_locs: SeqLocations = BTreeMap::new();
    let mut groups: HashMap<EntityId, Vec<(&str, u64)>> = HashMap::new();
    let mut max_tx = 0u64;
    for r in &export {
        if !r.asserted {
            return Err(format!("phantom retraction record at tx_count {}", r.tx_count));
        }
        max_tx = max_tx.max(r.tx_count);
        let attr = r.attribute.as_str();
        if attr == ATTR_SEQ {
            let seq = match &r.value {
                Value::Integer(i) if *i >= 0 => *i as u64,
                _ => return Err(format!("non-integer :h/seq value at tx_count {}", r.tx_count)),
            };
            seq_locs.entry(seq).or_default().push((r.entity, r.tx_count));
        }
        groups.entry(r.entity).or_default().push((attr, r.tx_count));
    }

    // Transaction atomicity: every entity carries exactly one whole
    // transaction (1 fact, or 3 facts under a single tx_count).
    for recs in groups.values() {
        match recs.len() {
            1 => {
                if recs[0].0 != ATTR_SEQ {
                    return Err("orphan sibling fact without :h/seq (partial transaction applied)".to_string());
                }
            }
            3 => {
                let tx = recs[0].1;
                if !recs.iter().all(|(_, t)| *t == tx) {
                    return Err("multi-fact transaction split across tx_counts".to_string());
                }
                let mut attrs: Vec<&str> = recs.iter().map(|(a, _)| *a).collect();
                attrs.sort_unstable();
                if attrs != [ATTR_CYC, ATTR_MODE, ATTR_SEQ] {
                    return Err("multi-fact transaction with unexpected attribute shape".to_string());
                }
            }
            n => return Err(format!("entity group with {n} records (expected 1 or 3)")),
        }
    }

    // The gate: every acknowledged transaction present exactly once.
    for (&seq, &fc) in &lineage.expected {
        match seq_locs.get(&seq) {
            None => return Err(format!("GATE lost acknowledged transaction seq={seq}")),
            Some(v) if v.len() != 1 => {
                return Err(format!(
                    "GATE acknowledged seq={seq} appears {} times (duplicate replay)",
                    v.len()
                ));
            }
            Some(v) => {
                let group_len = groups.get(&v[0].0).map(|g| g.len()).unwrap_or(0);
                if group_len != fc as usize {
                    return Err(format!("seq={seq} has {group_len} facts, expected {fc}"));
                }
            }
        }
    }

    // In-flight transactions: all-or-nothing. Present ones were WAL-fsynced
    // before the kill; the dead child's WAL is immutable, so present-now
    // means present-forever — promote them into the expected model.
    let mut promoted = 0u32;
    for &(seq, fc) in &outcome.maybe {
        if let Some(v) = seq_locs.get(&seq) {
            if v.len() != 1 {
                return Err(format!("in-flight seq={seq} appears {} times", v.len()));
            }
            let group_len = groups.get(&v[0].0).map(|g| g.len()).unwrap_or(0);
            if group_len != fc as usize {
                return Err(format!(
                    "in-flight seq={seq} partially applied ({group_len} of {fc} facts)"
                ));
            }
            lineage.expected.insert(seq, fc);
            promoted += 1;
        }
    }

    // No phantoms: everything present was either acknowledged or in-flight.
    for &seq in seq_locs.keys() {
        if !lineage.expected.contains_key(&seq) {
            return Err(format!("phantom seq={seq} present but never accounted for"));
        }
    }

    if db.current_tx_count() < max_tx {
        return Err(format!(
            "current_tx_count {} below max exported tx_count {max_tx}",
            db.current_tx_count()
        ));
    }

    // Functional after recovery: the file must accept a write and answer a
    // query — openable-but-wedged also counts as a dead resident.
    let probe_seq = bump(next_seq);
    match db.execute(&format!("(transact [[:k{probe_seq} :h/seq {probe_seq}]])")) {
        Ok(QueryResult::Transacted(_)) => {}
        Ok(_) => return Err("probe transact returned an unexpected result kind".to_string()),
        Err(e) => return Err(format!("post-recovery write failed: {e}")),
    }
    match db.execute(&format!("(query [:find ?v :where [:k{probe_seq} :h/seq ?v]])")) {
        Ok(QueryResult::QueryResults { results, .. }) if results.len() == 1 => {}
        Ok(_) => return Err("probe query did not return exactly one row".to_string()),
        Err(e) => return Err(format!("post-recovery query failed: {e}")),
    }
    lineage.expected.insert(probe_seq, 1);

    if (cycle + 1) % cfg.fold_every == 0 {
        db.checkpoint()
            .map_err(|e| format!("post-recovery checkpoint failed: {e}"))?;
    }

    Ok(VerifyStats { export_len: export.len(), promoted })
    // db drops here, releasing the parent's file lock before the next spawn.
}

// ─── Harness runner ──────────────────────────────────────────────────────────

#[derive(Default)]
struct Telemetry {
    acked_total: u64,
    promoted: u64,
    mid_ckpt_confirmed: u32,
    rotations: u32,
    deadline_hits: u32,
}

fn pick_mode(rng: &mut SplitMix64) -> KillMode {
    match rng.below(10) {
        0..=5 => KillMode::RandomInstant,
        6..=8 => KillMode::MidCheckpoint,
        _ => KillMode::MidMaintenance,
    }
}

fn fail(lineage: &mut Lineage, cfg: &HarnessConfig, cycle: u32, mode: KillMode, msg: &str) -> ! {
    let kept = lineage
        .dir
        .take()
        .map(|d| d.keep())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<tempdir already released>".to_string());
    panic!(
        "A7 failure: {msg} | seed={:#x} cycle={cycle} mode={} | lineage kept at {kept}",
        cfg.seed,
        mode.as_str()
    );
}

fn run_harness(cfg: HarnessConfig) {
    eprintln!(
        "A7 kill -9 harness: seed={:#x} cycles={} stream={:?} burst={:?}",
        cfg.seed, cfg.cycles, cfg.stream_len, cfg.burst_len
    );
    let started = Instant::now();
    let mut rng = SplitMix64::new(cfg.seed);
    let mut next_seq: u64 = 0;
    let mut t = Telemetry::default();
    let mut lineage = Lineage::fresh(&mut next_seq);

    for cycle in 0..cfg.cycles {
        if lineage.last_export_len > cfg.rotate_at_records
            || lineage.cycles_on_file >= MAX_LINEAGE_CYCLES
        {
            lineage = Lineage::fresh(&mut next_seq);
            t.rotations += 1;
        }

        let mode = pick_mode(&mut rng);
        let workload = generate_workload(&lineage, &cfg, &mut rng, mode, cycle, &mut next_seq);
        let raw = run_cycle(&lineage.db_path, &workload, cfg.cycle_deadline);
        if raw.deadline_hit {
            t.deadline_hits += 1;
        }
        if raw.signal != Some(9) {
            let stderr = raw.stderr.trim().to_string();
            fail(
                &mut lineage,
                &cfg,
                cycle,
                mode,
                &format!("child did not die by SIGKILL (stderr: {stderr})"),
            );
        }

        let outcome = match analyze(&workload, &raw) {
            Ok(o) => o,
            Err(e) => fail(&mut lineage, &cfg, cycle, mode, &e),
        };
        t.acked_total += outcome.acked.len() as u64;
        if outcome.mid_ckpt {
            t.mid_ckpt_confirmed += 1;
        }

        match verify_cycle(&mut lineage, &outcome, &mut next_seq, cycle, &cfg) {
            Ok(stats) => {
                lineage.last_export_len = stats.export_len;
                t.promoted += stats.promoted as u64;
            }
            Err(e) => fail(&mut lineage, &cfg, cycle, mode, &e),
        }
        lineage.cycles_on_file += 1;
    }

    eprintln!(
        "A7 summary: cycles={} acked={} promoted={} mid_checkpoint_confirmed={} rotations={} deadline_hits={} wall={:.1}s",
        cfg.cycles,
        t.acked_total,
        t.promoted,
        t.mid_ckpt_confirmed,
        t.rotations,
        t.deadline_hits,
        started.elapsed().as_secs_f64()
    );
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// Default-suite smoke: a short kill loop proving the harness machinery
/// end-to-end (spawn, pipeline, SIGKILL, reap, reopen, audit, rotate, fold).
#[test]
fn kill9_smoke_short_lineage() {
    run_harness(HarnessConfig::smoke());
}

/// The A7 gate: thousands of kill cycles, tens of thousands of acknowledged
/// transactions, randomized and checkpoint-biased SIGKILL. Zero lost
/// acknowledged transactions, zero unopenable files.
#[test]
#[ignore = "A7 full gate (~10 min): cargo test --release --test kill9_durability_test -- --ignored --nocapture"]
fn gate_kill9_durability_nightly() {
    run_harness(HarnessConfig::gate());
}
