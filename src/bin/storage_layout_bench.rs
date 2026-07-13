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

const SCHEMA: &str = "vicia.storage-layout.v2";
const BATCH: usize = 1_000;
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
    checkpoint_order: Vec<Vec<u8>>,
    query_order: Vec<Vec<u8>>,
    candidates: Vec<Candidate>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    fill_percent: u8,
    checkpoint: CheckpointMeasurement,
    query: QueryMeasurement,
    stats: CandidateStats,
    gates: CandidateGates,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricSummary {
    p50: f64,
    p95: f64,
    max: f64,
    mad: f64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateStats {
    checkpoint_ms: MetricSummary,
    checkpoint_delta_rss_bytes: MetricSummary,
    point_ms: MetricSummary,
    aggregate_ms: MetricSummary,
    query_delta_rss_bytes: MetricSummary,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateGates {
    size: bool,
    checkpoint: bool,
    point: bool,
    aggregate: bool,
    rss: bool,
    passed: bool,
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
    baseline_rss_samples_bytes: Vec<u64>,
    peak_rss_samples_bytes: Vec<u64>,
    delta_rss_samples_bytes: Vec<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuerySample {
    point_ms: f64,
    aggregate_ms: f64,
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
        [command, path] if command == "measure" => {
            println!("{}", serde_json::to_string(&measure(Path::new(path))?)?);
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
    let checkpoint_order = rotated_orders(profile.repetitions());
    let query_order = rotated_orders(profile.repetitions());
    let mut checkpoint_samples: Vec<Vec<CheckpointSample>> = FILLS
        .iter()
        .map(|_| Vec::with_capacity(profile.repetitions()))
        .collect();
    for (repetition, order) in checkpoint_order.iter().enumerate() {
        for fill in order {
            let fill_index = fill_index(*fill)?;
            let path = output.join(format!("fill-{fill}.graph"));
            eprintln!(
                "storage-layout: checkpoint fill-{fill} {}/{}",
                repetition + 1,
                profile.repetitions()
            );
            checkpoint_samples
                .get_mut(fill_index)
                .context("missing checkpoint candidate")?
                .push(child::<CheckpointSample>(
                    &executable,
                    &[
                        "build",
                        path.to_str().context("non-UTF8 path")?,
                        &profile.facts().to_string(),
                        &fill.to_string(),
                    ],
                )?);
        }
    }
    let mut query_samples: Vec<Vec<QuerySample>> = FILLS
        .iter()
        .map(|_| Vec::with_capacity(profile.repetitions()))
        .collect();
    for (repetition, order) in query_order.iter().enumerate() {
        for fill in order {
            let fill_index = fill_index(*fill)?;
            let path = output.join(format!("fill-{fill}.graph"));
            eprintln!(
                "storage-layout: query fill-{fill} {}/{}",
                repetition + 1,
                profile.repetitions()
            );
            query_samples
                .get_mut(fill_index)
                .context("missing query candidate")?
                .push(child::<QuerySample>(
                    &executable,
                    &["measure", path.to_str().context("non-UTF8 path")?],
                )?);
        }
    }
    let mut candidates = Vec::with_capacity(FILLS.len());
    for fill in FILLS {
        let checkpoint = checkpoint_samples
            .get_mut(fill_index(*fill)?)
            .context("missing checkpoint candidate")?;
        let query = query_samples
            .get_mut(fill_index(*fill)?)
            .context("missing query candidate")?;
        candidates.push(Candidate {
            fill_percent: *fill,
            checkpoint: combine_checkpoint_samples(std::mem::take(checkpoint))?,
            query: combine_query_samples(std::mem::take(query))?,
            stats: empty_stats(),
            gates: empty_gates(),
        });
    }
    populate_stats_and_gates(&mut candidates)?;
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
        checkpoint_order,
        query_order,
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
    for start in (0..facts).step_by(BATCH) {
        let mut command = String::from("(transact [");
        for entity in start..(start + u64::try_from(BATCH)?).min(facts) {
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

fn measure(path: &Path) -> Result<QuerySample> {
    let db = Minigraf::open(path)?;
    let point = "(query [:find ?v :where [:layout/e5000 :layout/value ?v]])";
    db.execute(point)?;
    let started = Instant::now();
    db.execute(point)?;
    let point_ms = elapsed(started);
    aggregate(&db)?;
    let baseline = rss_bytes()?;
    let (running, peak, sampler) = start_sampler(baseline);
    let started = Instant::now();
    let pair = aggregate(&db)?;
    let aggregate_ms = elapsed(started);
    running.store(false, Ordering::SeqCst);
    sampler
        .join()
        .map_err(|_| anyhow::anyhow!("RSS sampler panicked"))?;
    Ok(QuerySample {
        point_ms,
        aggregate_ms,
        count: pair.0,
        checksum: pair.1,
        baseline_rss_bytes: baseline,
        peak_rss_bytes: peak.load(Ordering::SeqCst),
        delta_rss_bytes: peak.load(Ordering::SeqCst).saturating_sub(baseline),
    })
}

fn combine_query_samples(samples: Vec<QuerySample>) -> Result<QueryMeasurement> {
    let first = samples.first().context("missing query sample")?;
    if samples
        .iter()
        .any(|sample| sample.count != first.count || sample.checksum != first.checksum)
    {
        bail!("query samples produced different results")
    }
    Ok(QueryMeasurement {
        point_samples_ms: samples.iter().map(|sample| sample.point_ms).collect(),
        aggregate_samples_ms: samples.iter().map(|sample| sample.aggregate_ms).collect(),
        count: first.count,
        checksum: first.checksum,
        baseline_rss_samples_bytes: samples
            .iter()
            .map(|sample| sample.baseline_rss_bytes)
            .collect(),
        peak_rss_samples_bytes: samples.iter().map(|sample| sample.peak_rss_bytes).collect(),
        delta_rss_samples_bytes: samples
            .iter()
            .map(|sample| sample.delta_rss_bytes)
            .collect(),
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
    percentile(values, 50)
}
fn percentile(values: &[f64], percent: usize) -> f64 {
    if values.is_empty() || !(1..=100).contains(&percent) {
        return f64::NAN;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let index = values.len().saturating_mul(percent).saturating_add(99) / 100 - 1;
    values.get(index).copied().unwrap_or(f64::NAN)
}
fn summarize(values: &[f64]) -> MetricSummary {
    let p50 = percentile(values, 50);
    let deviations = values
        .iter()
        .map(|value| (value - p50).abs())
        .collect::<Vec<_>>();
    MetricSummary {
        p50,
        p95: percentile(values, 95),
        max: values.iter().copied().reduce(f64::max).unwrap_or(f64::NAN),
        mad: percentile(&deviations, 50),
    }
}
fn summarize_u64(values: &[u64]) -> MetricSummary {
    summarize(&values.iter().map(|value| *value as f64).collect::<Vec<_>>())
}
fn rotated_orders(repetitions: usize) -> Vec<Vec<u8>> {
    (0..repetitions)
        .map(|repetition| {
            (0..FILLS.len())
                .filter_map(|offset| FILLS.get((repetition + offset) % FILLS.len()).copied())
                .collect()
        })
        .collect()
}
fn fill_index(fill: u8) -> Result<usize> {
    FILLS
        .iter()
        .position(|candidate| *candidate == fill)
        .context("unknown fill candidate")
}
fn empty_stats() -> CandidateStats {
    let empty = summarize(&[]);
    CandidateStats {
        checkpoint_ms: empty,
        checkpoint_delta_rss_bytes: empty,
        point_ms: empty,
        aggregate_ms: empty,
        query_delta_rss_bytes: empty,
    }
}
const fn empty_gates() -> CandidateGates {
    CandidateGates {
        size: false,
        checkpoint: false,
        point: false,
        aggregate: false,
        rss: false,
        passed: false,
    }
}
fn populate_stats_and_gates(candidates: &mut [Candidate]) -> Result<()> {
    let baseline_index = candidates
        .iter()
        .position(|candidate| candidate.fill_percent == 75)
        .context("missing fill-75 baseline")?;
    for candidate in candidates.iter_mut() {
        candidate.stats = CandidateStats {
            checkpoint_ms: summarize(&candidate.checkpoint.elapsed_samples_ms),
            checkpoint_delta_rss_bytes: summarize_u64(
                &candidate.checkpoint.delta_rss_samples_bytes,
            ),
            point_ms: summarize(&candidate.query.point_samples_ms),
            aggregate_ms: summarize(&candidate.query.aggregate_samples_ms),
            query_delta_rss_bytes: summarize_u64(&candidate.query.delta_rss_samples_bytes),
        };
    }
    let baseline_candidate = candidates
        .get(baseline_index)
        .context("missing fill-75 baseline")?;
    let baseline = baseline_candidate.stats;
    let baseline_bytes = baseline_candidate.checkpoint.graph_bytes;
    for candidate in candidates.iter_mut() {
        let size =
            candidate.checkpoint.graph_bytes.saturating_mul(10) <= baseline_bytes.saturating_mul(9);
        let checkpoint =
            latency_summary_gate(candidate.stats.checkpoint_ms, baseline.checkpoint_ms);
        let point = point_summary_gate(candidate.stats.point_ms, baseline.point_ms);
        let aggregate = latency_summary_gate(candidate.stats.aggregate_ms, baseline.aggregate_ms);
        let rss = candidate.stats.checkpoint_delta_rss_bytes.p50
            <= baseline.checkpoint_delta_rss_bytes.p50 * 1.10 + 2.0 * 1024.0 * 1024.0
            && candidate.stats.query_delta_rss_bytes.p50
                <= baseline.query_delta_rss_bytes.p50 * 1.10 + 2.0 * 1024.0 * 1024.0;
        candidate.gates = CandidateGates {
            size,
            checkpoint,
            point,
            aggregate,
            rss,
            passed: size && checkpoint && point && aggregate && rss,
        };
    }
    Ok(())
}
fn select_fill(candidates: &[Candidate]) -> Option<u8> {
    candidates
        .iter()
        .filter(|candidate| candidate.fill_percent > 75 && candidate.gates.passed)
        .map(|candidate| candidate.fill_percent)
        .max()
}
fn latency_summary_gate(candidate: MetricSummary, baseline: MetricSummary) -> bool {
    candidate.p50 <= baseline.p50 * 1.10
        && candidate.p95 <= baseline.p95 * 1.10
        && candidate.p95 <= candidate.p50 * 1.15
}
fn point_summary_gate(candidate: MetricSummary, baseline: MetricSummary) -> bool {
    candidate.p50 <= baseline.p50 * 1.20 && candidate.p95 <= baseline.p95 * 1.20
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_p95_keeps_max_separate_at_twenty_samples() {
        let values = (1..=20).map(f64::from).collect::<Vec<_>>();
        let summary = summarize(&values);
        assert_eq!(summary.p50, 10.0);
        assert_eq!(summary.p95, 19.0);
        assert_eq!(summary.max, 20.0);
        assert_eq!(summary.mad, 5.0);
    }

    #[test]
    fn nearest_rank_handles_odd_even_equal_and_outlier_samples() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 50), 2.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 50), 2.0);
        assert_eq!(summarize(&[7.0, 7.0, 7.0]).mad, 0.0);
        let summary = summarize(&[1.0, 1.0, 1.0, 100.0]);
        assert_eq!(summary.p50, 1.0);
        assert_eq!(summary.max, 100.0);
    }

    #[test]
    fn percentile_rejects_empty_and_invalid_percent_inputs() {
        assert!(percentile(&[], 50).is_nan());
        assert!(percentile(&[1.0], 0).is_nan());
        assert!(percentile(&[1.0], 101).is_nan());
    }

    #[test]
    fn candidate_order_rotates_once_per_repetition() {
        let orders = rotated_orders(FILLS.len());
        assert_eq!(orders.len(), FILLS.len());
        for (repetition, order) in orders.iter().enumerate() {
            assert_eq!(order.len(), FILLS.len());
            assert_eq!(order[0], FILLS[repetition]);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, FILLS);
        }
    }
}
