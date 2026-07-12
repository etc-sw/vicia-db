#![cfg(not(target_arch = "wasm32"))]

use anyhow::{Context, Result, bail};
use minigraf::{CurrentAttributeCursorDiagnostics, Minigraf, OpenOptions, QueryResult, Value};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "vicia.pending-isolation.v1";
const BASE_FACTS: u64 = 1_000_000;
const SELECTED_CONTROL_FACTS: u64 = 10_000;
const WRITE_BATCH: u64 = 1_000;
const SELECTED_ATTRIBUTE: &str = ":bench/selected";
const UNRELATED_ATTRIBUTE: &str = ":bench/unrelated";

#[derive(Clone, Copy)]
enum Profile {
    Smoke,
    Full,
}

impl Profile {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "full" => Ok(Self::Full),
            _ => bail!("profile must be smoke or full"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Full => "full",
        }
    }

    fn repetitions(self) -> usize {
        match self {
            Self::Smoke => 5,
            Self::Full => 20,
        }
    }

    fn unrelated_counts(self) -> &'static [u64] {
        match self {
            Self::Smoke => &[0, 100, 1_000, 10_000],
            Self::Full => &[0, 10_000, 100_000, 1_000_000],
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum PendingKind {
    Unrelated,
    SelectedControl,
}

impl PendingKind {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "unrelated" => Ok(Self::Unrelated),
            "selected-control" => Ok(Self::SelectedControl),
            _ => bail!("pending kind must be unrelated or selected-control"),
        }
    }

    fn cli_name(self) -> &'static str {
        match self {
            Self::Unrelated => "unrelated",
            Self::SelectedControl => "selected-control",
        }
    }

    fn label(self, count: u64) -> String {
        match self {
            Self::Unrelated => format!("unrelated-{count}"),
            Self::SelectedControl => format!("selected-control-{count}"),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    suite: &'static str,
    profile: &'static str,
    generated_at_unix_ms: u128,
    base_facts: u64,
    repetitions: usize,
    warmup_repetitions: usize,
    provenance: Provenance,
    acceptance_policy: AcceptancePolicy,
    base_build_ms: f64,
    variants: Vec<VariantReceipt>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Provenance {
    source_commit: String,
    tracked_clean: bool,
    clean_state_eligible: bool,
    os: &'static str,
    arch: &'static str,
    host: Option<String>,
    cpu_model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptancePolicy {
    unrelated_count: u64,
    unrelated_checksum: i128,
    rss_delta_tolerance_bytes: u64,
    p50_regression_ratio: f64,
    p95_to_p50_ratio: f64,
    diagnostic_equality: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VariantReceipt {
    label: String,
    pending_kind: PendingKind,
    pending_facts: u64,
    expected_count: u64,
    expected_checksum: i128,
    graph_bytes: u64,
    wal_bytes: u64,
    measurement: Measurement,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildResult {
    elapsed_ms: f64,
    graph_bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    open_elapsed_ms: f64,
    open_baseline_rss_bytes: u64,
    baseline_breakdown: MemoryBreakdown,
    warmup: AggregateSample,
    samples: Vec<AggregateSample>,
    elapsed_summary_ms: ElapsedSummary,
    /// Maximum sampled VmRSS after warmup/measured queries (open excluded).
    workload_peak_rss_bytes: u64,
    workload_delta_rss_bytes: u64,
    /// Raw process VmHWM, retained to expose pre-baseline open/replay peaks.
    process_peak_rss_bytes: u64,
    retained_rss_bytes: u64,
    retained_delta_rss_bytes: u64,
    retained_breakdown: MemoryBreakdown,
    retained_delta_breakdown: MemoryBreakdown,
    count: u64,
    checksum: i128,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregateSample {
    elapsed_ms: f64,
    rss_bytes: u64,
    peak_rss_bytes: u64,
    cursor_diagnostics: CurrentAttributeCursorDiagnostics,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ElapsedSummary {
    p50: f64,
    p95: f64,
    max: f64,
    mad: f64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryBreakdown {
    anonymous_rss_bytes: u64,
    file_backed_rss_bytes: u64,
    heap_mapping_rss_bytes: u64,
    database_mapped_rss_bytes: u64,
}

impl MemoryBreakdown {
    fn saturating_sub(self, baseline: Self) -> Self {
        Self {
            anonymous_rss_bytes: self
                .anonymous_rss_bytes
                .saturating_sub(baseline.anonymous_rss_bytes),
            file_backed_rss_bytes: self
                .file_backed_rss_bytes
                .saturating_sub(baseline.file_backed_rss_bytes),
            heap_mapping_rss_bytes: self
                .heap_mapping_rss_bytes
                .saturating_sub(baseline.heap_mapping_rss_bytes),
            database_mapped_rss_bytes: self
                .database_mapped_rss_bytes
                .saturating_sub(baseline.database_mapped_rss_bytes),
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context("missing command")?;
    match command.as_str() {
        "run" => {
            let profile = Profile::parse(&args.next().context("missing profile")?)?;
            let output_dir = PathBuf::from(args.next().context("missing output directory")?);
            if args.next().is_some() {
                bail!("run accepts only <profile> <output-directory>");
            }
            run_profile(profile, &output_dir)
        }
        "build-base" => {
            let path = PathBuf::from(args.next().context("missing base path")?);
            if args.next().is_some() {
                bail!("build-base accepts only <graph-path>");
            }
            let result = build_base(&path)?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        "prepare" => {
            let path = PathBuf::from(args.next().context("missing graph path")?);
            let kind = PendingKind::parse(&args.next().context("missing pending kind")?)?;
            let facts = args
                .next()
                .context("missing pending fact count")?
                .parse::<u64>()?;
            if args.next().is_some() {
                bail!("prepare accepts only <graph-path> <pending-kind> <facts>");
            }
            append_pending(&path, kind, facts)
        }
        "measure" => {
            let path = PathBuf::from(args.next().context("missing graph path")?);
            let repetitions = args
                .next()
                .context("missing repetitions")?
                .parse::<usize>()?;
            if args.next().is_some() || repetitions == 0 {
                bail!("measure accepts <graph-path> <positive-repetitions>");
            }
            let measurement = measure(&path, repetitions)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        _ => bail!("usage: pending-isolation-bench run <smoke|full> <output-directory>"),
    }
}

fn run_profile(profile: Profile, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)?;
    let data_dir = output_dir.join("data");
    if data_dir.exists() {
        fs::remove_dir_all(&data_dir)?;
    }
    fs::create_dir_all(&data_dir)?;

    let provenance = provenance()?;
    let executable = std::env::current_exe()?;
    let base_path = data_dir.join("base.graph");
    eprintln!("pending-isolation: building common {BASE_FACTS}-fact committed base");
    let build: BuildResult = child_json(&executable, &["build-base"], &[&base_path])?;

    let mut specs = profile
        .unrelated_counts()
        .iter()
        .copied()
        .map(|facts| (PendingKind::Unrelated, facts))
        .collect::<Vec<_>>();
    specs.push((PendingKind::SelectedControl, SELECTED_CONTROL_FACTS));

    let mut variants = Vec::with_capacity(specs.len());
    for (kind, pending_facts) in specs {
        let label = kind.label(pending_facts);
        let variant_path = data_dir.join(format!("{label}.graph"));
        eprintln!("pending-isolation: preparing {label}");
        fs::copy(&base_path, &variant_path).with_context(|| {
            format!(
                "copy common base to variant {}",
                variant_path.to_string_lossy()
            )
        })?;
        if pending_facts > 0 {
            child_status(
                &executable,
                &["prepare", kind.cli_name(), &pending_facts.to_string()],
                &[&variant_path],
            )?;
        }
        eprintln!(
            "pending-isolation: measuring {label} ({} repetitions after warmup)",
            profile.repetitions()
        );
        let measurement: Measurement = child_json(
            &executable,
            &["measure", &profile.repetitions().to_string()],
            &[&variant_path],
        )?;
        let (expected_count, expected_checksum) = expected(kind, pending_facts);
        if measurement.count != expected_count || measurement.checksum != expected_checksum {
            bail!(
                "{label}: correctness mismatch: count {}/{expected_count}, checksum {}/{expected_checksum}",
                measurement.count,
                measurement.checksum
            );
        }
        variants.push(VariantReceipt {
            label,
            pending_kind: kind,
            pending_facts,
            expected_count,
            expected_checksum,
            graph_bytes: fs::metadata(&variant_path)?.len(),
            wal_bytes: fs::metadata(sidecar_path(&variant_path, ".wal"))
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            measurement,
        });
    }

    let receipt = Receipt {
        schema: SCHEMA,
        suite: "unrelated pending aggregate isolation",
        profile: profile.name(),
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        base_facts: BASE_FACTS,
        repetitions: profile.repetitions(),
        warmup_repetitions: 1,
        provenance,
        acceptance_policy: AcceptancePolicy {
            unrelated_count: BASE_FACTS,
            unrelated_checksum: arithmetic_sum(BASE_FACTS),
            rss_delta_tolerance_bytes: 2 * 1024 * 1024,
            p50_regression_ratio: 1.10,
            p95_to_p50_ratio: 1.15,
            diagnostic_equality: "all unrelated variants exactly equal unrelated-0",
        },
        base_build_ms: build.elapsed_ms,
        variants,
    };
    let receipt_path = output_dir.join("receipt.json");
    fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt)?)?;
    eprintln!(
        "pending-isolation: wrote {} (base {:.3} MiB)",
        receipt_path.display(),
        build.graph_bytes as f64 / 1024.0 / 1024.0
    );
    for variant in &receipt.variants {
        println!(
            "{}: p50={:.3} ms p95={:.3} ms rss-delta={:.3} MiB count={} checksum={}",
            variant.label,
            variant.measurement.elapsed_summary_ms.p50,
            variant.measurement.elapsed_summary_ms.p95,
            variant.measurement.workload_delta_rss_bytes as f64 / 1024.0 / 1024.0,
            variant.measurement.count,
            variant.measurement.checksum
        );
    }
    Ok(())
}

fn child_json<T: for<'de> Deserialize<'de>>(
    executable: &Path,
    args: &[&str],
    paths: &[&Path],
) -> Result<T> {
    let mut command = Command::new(executable);
    if let Some(first) = args.first() {
        command.arg(first);
    }
    for path in paths {
        command.arg(path);
    }
    for arg in args.iter().skip(1) {
        command.arg(arg);
    }
    let output = command.output().context("launch benchmark child")?;
    if !output.status.success() {
        bail!(
            "benchmark child failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode benchmark child output")
}

fn child_status(executable: &Path, args: &[&str], paths: &[&Path]) -> Result<()> {
    let mut command = Command::new(executable);
    if let Some(first) = args.first() {
        command.arg(first);
    }
    for path in paths {
        command.arg(path);
    }
    for arg in args.iter().skip(1) {
        command.arg(arg);
    }
    let status = command
        .stdin(Stdio::null())
        .status()
        .context("launch benchmark preparation child")?;
    if !status.success() {
        bail!("benchmark preparation child failed with {status}");
    }
    Ok(())
}

fn build_base(path: &Path) -> Result<BuildResult> {
    remove_database_files(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let started = Instant::now();
    let db = open_without_auto_checkpoint(path)?;
    write_facts(&db, PendingKind::SelectedControl, 0, BASE_FACTS, "base")?;
    db.checkpoint()?;
    drop(db);
    let graph_bytes = fs::metadata(path)?.len();
    if sidecar_path(path, ".wal").exists() {
        bail!("base checkpoint left a WAL sidecar");
    }
    Ok(BuildResult {
        elapsed_ms: elapsed_ms(started),
        graph_bytes,
    })
}

fn append_pending(path: &Path, kind: PendingKind, facts: u64) -> Result<()> {
    if facts == 0 {
        return Ok(());
    }
    let db = open_without_auto_checkpoint(path)?;
    write_facts(&db, kind, 0, facts, "pending")?;
    drop(db);
    let wal = sidecar_path(path, ".wal");
    if !wal.exists() {
        bail!("pending preparation did not leave a WAL sidecar");
    }
    Ok(())
}

fn write_facts(
    db: &Minigraf,
    kind: PendingKind,
    start: u64,
    facts: u64,
    entity_namespace: &str,
) -> Result<()> {
    let attribute = match kind {
        PendingKind::Unrelated => UNRELATED_ATTRIBUTE,
        PendingKind::SelectedControl => SELECTED_ATTRIBUTE,
    };
    let end = start.saturating_add(facts);
    for batch_start in (start..end).step_by(usize::try_from(WRITE_BATCH)?) {
        let batch_end = batch_start.saturating_add(WRITE_BATCH).min(end);
        let mut command = String::from("(transact [");
        for value in batch_start..batch_end {
            command.push_str(&format!(
                "[:iso/{entity_namespace}{value} {attribute} {value}]"
            ));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    Ok(())
}

fn open_without_auto_checkpoint(path: &Path) -> Result<Minigraf> {
    let mut options = OpenOptions::new();
    options.wal_checkpoint_threshold = usize::MAX;
    options.path(path).open()
}

fn measure(path: &Path, repetitions: usize) -> Result<Measurement> {
    let open_started = Instant::now();
    let db = open_without_auto_checkpoint(path)?;
    let open_elapsed_ms = elapsed_ms(open_started);
    let baseline = current_rss_bytes().context("read RSS after open")?;
    let baseline_breakdown = memory_breakdown(path)?;

    let (warmup, expected) = aggregate_sample(&db)?;
    let mut samples = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let (sample, pair) = aggregate_sample(&db)?;
        if pair != expected {
            bail!("aggregate correctness changed between repetitions");
        }
        samples.push(sample);
    }
    let elapsed_summary_ms = elapsed_summary(
        &samples
            .iter()
            .map(|sample| sample.elapsed_ms)
            .collect::<Vec<_>>(),
    )?;
    let retained = current_rss_bytes().context("read retained RSS")?;
    let workload_peak = samples
        .iter()
        .map(|sample| sample.rss_bytes)
        .chain(std::iter::once(warmup.rss_bytes))
        .max()
        .context("query RSS samples missing")?;
    let process_peak = peak_rss_bytes().context("read process peak RSS")?;
    let retained_breakdown = memory_breakdown(path)?;
    Ok(Measurement {
        open_elapsed_ms,
        open_baseline_rss_bytes: baseline,
        baseline_breakdown,
        warmup,
        samples,
        elapsed_summary_ms,
        workload_peak_rss_bytes: workload_peak,
        workload_delta_rss_bytes: workload_peak.saturating_sub(baseline),
        process_peak_rss_bytes: process_peak,
        retained_rss_bytes: retained,
        retained_delta_rss_bytes: retained.saturating_sub(baseline),
        retained_breakdown,
        retained_delta_breakdown: retained_breakdown.saturating_sub(baseline_breakdown),
        count: expected.0,
        checksum: expected.1,
    })
}

fn aggregate_sample(db: &Minigraf) -> Result<(AggregateSample, (u64, i128))> {
    let started = Instant::now();
    let result =
        db.execute("(query [:find (count ?v) (sum ?v) :where [?e :bench/selected ?v]])")?;
    let elapsed_ms = elapsed_ms(started);
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("aggregate returned a non-query result");
    };
    let row = results.first().context("aggregate returned no row")?;
    let count = row
        .first()
        .and_then(Value::as_integer)
        .context("aggregate count missing")?;
    let checksum = row
        .get(1)
        .and_then(Value::as_integer)
        .context("aggregate sum missing")?;
    let cursor_diagnostics = db
        .last_current_attribute_cursor_diagnostics()
        .context("selected aggregate did not expose cursor diagnostics")?;
    Ok((
        AggregateSample {
            elapsed_ms,
            rss_bytes: current_rss_bytes().context("read sample RSS")?,
            peak_rss_bytes: peak_rss_bytes().context("read sample peak RSS")?,
            cursor_diagnostics,
        },
        (u64::try_from(count)?, i128::from(checksum)),
    ))
}

fn expected(kind: PendingKind, pending_facts: u64) -> (u64, i128) {
    match kind {
        PendingKind::Unrelated => (BASE_FACTS, arithmetic_sum(BASE_FACTS)),
        PendingKind::SelectedControl => (
            BASE_FACTS.saturating_add(pending_facts),
            arithmetic_sum(BASE_FACTS).saturating_add(arithmetic_sum(pending_facts)),
        ),
    }
}

fn arithmetic_sum(facts: u64) -> i128 {
    i128::from(facts) * i128::from(facts.saturating_sub(1)) / 2
}

fn elapsed_summary(samples: &[f64]) -> Result<ElapsedSummary> {
    if samples.is_empty() {
        bail!("elapsed summary requires samples");
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let p50 = nearest_rank(&sorted, 50)?;
    let p95 = nearest_rank(&sorted, 95)?;
    let max = sorted
        .last()
        .copied()
        .context("elapsed summary requires samples")?;
    let mut deviations = sorted
        .iter()
        .map(|sample| (sample - p50).abs())
        .collect::<Vec<_>>();
    deviations.sort_by(f64::total_cmp);
    Ok(ElapsedSummary {
        p50,
        p95,
        max,
        mad: nearest_rank(&deviations, 50)?,
    })
}

fn nearest_rank(sorted: &[f64], percentile: usize) -> Result<f64> {
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    sorted
        .get(rank.saturating_sub(1))
        .copied()
        .context("percentile requires samples")
}

fn provenance() -> Result<Provenance> {
    let source_commit = command_text("git", &["rev-parse", "HEAD"])?;
    let status = command_text("git", &["status", "--porcelain", "--untracked-files=no"])?;
    let tracked_clean = status.is_empty();
    Ok(Provenance {
        source_commit,
        tracked_clean,
        clean_state_eligible: tracked_clean,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        host: command_text("hostname", &[]).ok(),
        cpu_model: cpu_model(),
    })
}

fn command_text(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        bail!("{program} failed with {}", output.status);
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn cpu_model() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_owned))
}

fn current_rss_bytes() -> Option<u64> {
    proc_status_kib("VmRSS:")?.checked_mul(1024)
}

fn peak_rss_bytes() -> Option<u64> {
    proc_status_kib("VmHWM:")?.checked_mul(1024)
}

fn proc_status_kib(field: &str) -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with(field))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn memory_breakdown(database_path: &Path) -> Result<MemoryBreakdown> {
    let smaps = fs::read_to_string("/proc/self/smaps").context("read /proc/self/smaps")?;
    let database_path = database_path.canonicalize()?;
    let database_path = database_path.to_string_lossy();
    let mut current_heap = false;
    let mut current_database = false;
    let mut total_rss = 0_u64;
    let mut anonymous_rss = 0_u64;
    let mut heap_rss = 0_u64;
    let mut database_rss = 0_u64;

    for line in smaps.lines() {
        if is_smaps_header(line) {
            let path = line.split_whitespace().nth(5).unwrap_or("");
            current_heap = path == "[heap]";
            current_database = path == database_path.as_ref();
            continue;
        }
        if let Some(bytes) = smaps_kib_value(line, "Rss:") {
            total_rss = total_rss.saturating_add(bytes);
            if current_heap {
                heap_rss = heap_rss.saturating_add(bytes);
            }
            if current_database {
                database_rss = database_rss.saturating_add(bytes);
            }
        } else if let Some(bytes) = smaps_kib_value(line, "Anonymous:") {
            anonymous_rss = anonymous_rss.saturating_add(bytes);
        }
    }
    Ok(MemoryBreakdown {
        anonymous_rss_bytes: anonymous_rss,
        file_backed_rss_bytes: total_rss.saturating_sub(anonymous_rss),
        heap_mapping_rss_bytes: heap_rss,
        database_mapped_rss_bytes: database_rss,
    })
}

fn is_smaps_header(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|range| {
        range.contains('-')
            && range
                .bytes()
                .all(|byte| byte == b'-' || byte.is_ascii_hexdigit())
    })
}

fn smaps_kib_value(line: &str, field: &str) -> Option<u64> {
    line.strip_prefix(field)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn remove_database_files(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        sidecar_path(path, ".wal"),
        sidecar_path(path, ".lock"),
    ] {
        if candidate.exists() {
            fs::remove_file(candidate)?;
        }
    }
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}
