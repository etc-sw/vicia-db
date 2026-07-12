#![cfg(not(target_arch = "wasm32"))]

use anyhow::{Context, Result, bail};
use minigraf::{
    Minigraf, OpenOptions, QueryResult, StorageLayoutDiagnostics, inspect_storage_layout,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "vicia.storage-layout.v1";
const BATCH: u64 = 1_000;
const FILLS: &[u8] = &[75, 85, 90, 95, 100];

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
    fn facts(self) -> u64 {
        match self {
            Self::Smoke => 10_000,
            Self::Full => 1_000_000,
        }
    }
    fn repetitions(self) -> usize {
        match self {
            Self::Smoke => 5,
            Self::Full => 20,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Full => "full",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    profile: &'static str,
    facts: u64,
    repetitions: usize,
    generated_at_unix_ms: u128,
    source_commit: String,
    tracked_clean: bool,
    selected_fill_percent: Option<u8>,
    candidates: Vec<Candidate>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    fill_percent: u8,
    checkpoint: CheckpointMeasurement,
    query: QueryMeasurement,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointMeasurement {
    elapsed_samples_ms: Vec<f64>,
    baseline_rss_samples_bytes: Vec<u64>,
    peak_rss_samples_bytes: Vec<u64>,
    delta_rss_samples_bytes: Vec<u64>,
    graph_bytes: u64,
    layout: StorageLayoutDiagnostics,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryMeasurement {
    point_samples_ms: Vec<f64>,
    aggregate_samples_ms: Vec<f64>,
    count: u64,
    checksum: i128,
    baseline_rss_bytes: u64,
    peak_rss_bytes: u64,
    delta_rss_bytes: u64,
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, profile, output] if command == "run" => {
            run(Profile::parse(profile)?, Path::new(output))
        }
        [command, path, facts, fill] if command == "build" => {
            println!(
                "{}",
                serde_json::to_string(&build(Path::new(path), facts.parse()?, fill.parse()?)?)?
            );
            Ok(())
        }
        [command, path, repetitions] if command == "measure" => {
            println!(
                "{}",
                serde_json::to_string(&measure(Path::new(path), repetitions.parse()?)?)?
            );
            Ok(())
        }
        _ => bail!("usage: storage-layout-bench run <smoke|full> <output-dir>"),
    }
}

fn run(profile: Profile, output: &Path) -> Result<()> {
    if output.exists() {
        fs::remove_dir_all(output)?;
    }
    fs::create_dir_all(output)?;
    let executable = std::env::current_exe()?;
    let mut candidates = Vec::new();
    for fill in FILLS {
        eprintln!("storage-layout: build fill-{fill}");
        let path = output.join(format!("fill-{fill}.graph"));
        let mut checkpoint_samples = Vec::with_capacity(profile.repetitions());
        for repetition in 0..profile.repetitions() {
            eprintln!(
                "storage-layout: checkpoint fill-{fill} {}/{}",
                repetition + 1,
                profile.repetitions()
            );
            checkpoint_samples.push(child::<CheckpointSample>(
                &executable,
                &[
                    "build",
                    path.to_str().context("non-UTF8 path")?,
                    &profile.facts().to_string(),
                    &fill.to_string(),
                ],
            )?);
        }
        let checkpoint = combine_checkpoint_samples(checkpoint_samples)?;
        eprintln!("storage-layout: measure fill-{fill}");
        let query = child::<QueryMeasurement>(
            &executable,
            &[
                "measure",
                path.to_str().context("non-UTF8 path")?,
                &profile.repetitions().to_string(),
            ],
        )?;
        candidates.push(Candidate {
            fill_percent: *fill,
            checkpoint,
            query,
        });
    }
    let source_commit = command_text("git", &["rev-parse", "HEAD"])?;
    let tracked_clean =
        command_text("git", &["status", "--short", "--untracked-files=no"])?.is_empty();
    let selected_fill_percent = select_fill(&candidates);
    let receipt = Receipt {
        schema: SCHEMA,
        profile: profile.name(),
        facts: profile.facts(),
        repetitions: profile.repetitions(),
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        source_commit,
        tracked_clean,
        selected_fill_percent,
        candidates,
    };
    fs::write(
        output.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    for candidate in &receipt.candidates {
        println!(
            "fill-{}: file={:.3} MiB checkpoint={:.1} ms point={} aggregate={} index-unused={:.3} MiB",
            candidate.fill_percent,
            candidate.checkpoint.graph_bytes as f64 / 1048576.0,
            median(&candidate.checkpoint.elapsed_samples_ms),
            median(&candidate.query.point_samples_ms),
            median(&candidate.query.aggregate_samples_ms),
            index_unused(&candidate.checkpoint.layout) as f64 / 1048576.0
        );
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointSample {
    elapsed_ms: f64,
    baseline_rss_bytes: u64,
    peak_rss_bytes: u64,
    delta_rss_bytes: u64,
    graph_bytes: u64,
    layout: StorageLayoutDiagnostics,
}

fn build(path: &Path, facts: u64, fill: u8) -> Result<CheckpointSample> {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(format!("{}.wal", path.display()));
    let db = Minigraf::open_with_options(
        path,
        OpenOptions {
            wal_checkpoint_threshold: usize::MAX,
            ..OpenOptions::default()
        }
        .benchmark_btree_fill_percent(fill),
    )?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut command = String::from("(transact [");
        for entity in start..(start + BATCH).min(facts) {
            command.push_str(&format!("[:layout/e{entity} :layout/value {entity}]"));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    let baseline = rss_bytes()?;
    let (running, peak, sampler) = start_sampler(baseline);
    let started = Instant::now();
    db.checkpoint()?;
    let elapsed_ms = elapsed(started);
    running.store(false, Ordering::SeqCst);
    sampler
        .join()
        .map_err(|_| anyhow::anyhow!("RSS sampler panicked"))?;
    drop(db);
    Ok(CheckpointSample {
        elapsed_ms,
        baseline_rss_bytes: baseline,
        peak_rss_bytes: peak.load(Ordering::SeqCst),
        delta_rss_bytes: peak.load(Ordering::SeqCst).saturating_sub(baseline),
        graph_bytes: fs::metadata(path)?.len(),
        layout: inspect_storage_layout(path)?,
    })
}

fn combine_checkpoint_samples(mut samples: Vec<CheckpointSample>) -> Result<CheckpointMeasurement> {
    let final_sample = samples.pop().context("missing checkpoint sample")?;
    let mut elapsed_samples_ms = Vec::with_capacity(samples.len() + 1);
    let mut baseline_rss_samples_bytes = Vec::with_capacity(samples.len() + 1);
    let mut peak_rss_samples_bytes = Vec::with_capacity(samples.len() + 1);
    let mut delta_rss_samples_bytes = Vec::with_capacity(samples.len() + 1);
    for sample in samples.iter().chain(std::iter::once(&final_sample)) {
        if sample.graph_bytes != final_sample.graph_bytes {
            bail!("checkpoint samples produced different graph sizes")
        }
        elapsed_samples_ms.push(sample.elapsed_ms);
        baseline_rss_samples_bytes.push(sample.baseline_rss_bytes);
        peak_rss_samples_bytes.push(sample.peak_rss_bytes);
        delta_rss_samples_bytes.push(sample.delta_rss_bytes);
    }
    Ok(CheckpointMeasurement {
        elapsed_samples_ms,
        baseline_rss_samples_bytes,
        peak_rss_samples_bytes,
        delta_rss_samples_bytes,
        graph_bytes: final_sample.graph_bytes,
        layout: final_sample.layout,
    })
}

fn measure(path: &Path, repetitions: usize) -> Result<QueryMeasurement> {
    let db = Minigraf::open(path)?;
    let point = "(query [:find ?v :where [:layout/e5000 :layout/value ?v]])";
    db.execute(point)?;
    let mut point_samples_ms = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let started = Instant::now();
        db.execute(point)?;
        point_samples_ms.push(elapsed(started));
    }
    aggregate(&db)?;
    let baseline = rss_bytes()?;
    let (running, peak, sampler) = start_sampler(baseline);
    let mut aggregate_samples_ms = Vec::with_capacity(repetitions);
    let mut pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
        pair = aggregate(&db)?;
        aggregate_samples_ms.push(elapsed(started));
    }
    running.store(false, Ordering::SeqCst);
    sampler
        .join()
        .map_err(|_| anyhow::anyhow!("RSS sampler panicked"))?;
    Ok(QueryMeasurement {
        point_samples_ms,
        aggregate_samples_ms,
        count: pair.0,
        checksum: pair.1,
        baseline_rss_bytes: baseline,
        peak_rss_bytes: peak.load(Ordering::SeqCst),
        delta_rss_bytes: peak.load(Ordering::SeqCst).saturating_sub(baseline),
    })
}

fn aggregate(db: &Minigraf) -> Result<(u64, i128)> {
    let QueryResult::QueryResults { results, .. } =
        db.execute("(query [:find (count ?v) (sum ?v) :where [?e :layout/value ?v]])")?
    else {
        bail!("aggregate returned non-query result")
    };
    let row = results.first().context("aggregate returned no row")?;
    Ok((
        u64::try_from(
            row.first()
                .and_then(|v| v.as_integer())
                .context("missing count")?,
        )?,
        i128::from(
            row.get(1)
                .and_then(|v| v.as_integer())
                .context("missing sum")?,
        ),
    ))
}

fn child<T: for<'de> Deserialize<'de>>(executable: &Path, args: &[&str]) -> Result<T> {
    let output = Command::new(executable).args(args).output()?;
    if !output.status.success() {
        bail!("child failed: {}", String::from_utf8_lossy(&output.stderr))
    }
    serde_json::from_slice(&output.stdout).context("decode child JSON")
}
fn command_text(command: &str, args: &[&str]) -> Result<String> {
    Ok(
        String::from_utf8(Command::new(command).args(args).output()?.stdout)?
            .trim()
            .to_owned(),
    )
}
fn elapsed(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
}
fn percentile(values: &[f64], quantile: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let index = ((values.len() - 1) as f64 * quantile).ceil() as usize;
    values[index]
}
fn median_u64(values: &[u64]) -> u64 {
    let mut values = values.to_vec();
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}
fn select_fill(candidates: &[Candidate]) -> Option<u8> {
    let baseline = candidates
        .iter()
        .find(|candidate| candidate.fill_percent == 75)?;
    candidates
        .iter()
        .filter(|candidate| candidate.fill_percent > 75)
        .filter(|candidate| {
            candidate.checkpoint.graph_bytes * 10 <= baseline.checkpoint.graph_bytes * 9
        })
        .filter(|candidate| {
            latency_gate(
                &candidate.checkpoint.elapsed_samples_ms,
                &baseline.checkpoint.elapsed_samples_ms,
            )
        })
        .filter(|candidate| {
            latency_gate(
                &candidate.query.point_samples_ms,
                &baseline.query.point_samples_ms,
            )
        })
        .filter(|candidate| {
            latency_gate(
                &candidate.query.aggregate_samples_ms,
                &baseline.query.aggregate_samples_ms,
            )
        })
        .filter(|candidate| {
            median_u64(&candidate.checkpoint.delta_rss_samples_bytes)
                <= median_u64(&baseline.checkpoint.delta_rss_samples_bytes).saturating_mul(110)
                    / 100
                && candidate.query.delta_rss_bytes
                    <= baseline.query.delta_rss_bytes.saturating_mul(110) / 100
        })
        .map(|candidate| candidate.fill_percent)
        .max()
}
fn latency_gate(candidate: &[f64], baseline: &[f64]) -> bool {
    let candidate_p50 = percentile(candidate, 0.5);
    let candidate_p95 = percentile(candidate, 0.95);
    candidate_p50 <= percentile(baseline, 0.5) * 1.10
        && candidate_p95 <= percentile(baseline, 0.95) * 1.10
        && candidate_p95 <= candidate_p50 * 1.15
}
fn index_unused(layout: &StorageLayoutDiagnostics) -> u64 {
    [&layout.eavt, &layout.aevt, &layout.avet, &layout.vaet]
        .iter()
        .map(|index| {
            index
                .leaf
                .unused_bytes
                .saturating_add(index.internal.unused_bytes)
        })
        .sum()
}
fn rss_bytes() -> Result<u64> {
    let status = fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .context("VmRSS missing")?;
    Ok(line
        .split_whitespace()
        .nth(1)
        .context("VmRSS value missing")?
        .parse::<u64>()?
        .saturating_mul(1024))
}
fn start_sampler(initial: u64) -> (Arc<AtomicBool>, Arc<AtomicU64>, std::thread::JoinHandle<()>) {
    let running = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicU64::new(initial));
    let r = running.clone();
    let p = peak.clone();
    let handle = std::thread::spawn(move || {
        while r.load(Ordering::Relaxed) {
            if let Ok(value) = rss_bytes() {
                p.fetch_max(value, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    (running, peak, handle)
}
