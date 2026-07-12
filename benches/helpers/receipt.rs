//! Machine-readable evidence receipts for caller-shaped benchmark harnesses.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RECEIPT_PATH_ENV: &str = "VICIA_BENCH_RECEIPT";
pub const BASE_FIXTURE_ENV: &str = "VICIA_BENCH_BASE_FIXTURE";
const RECEIPT_SCHEMA: &str = "vicia.benchmark.receipt.v1";
const CATALOG_SCHEMA: &str = "vicia.benchmark.milestones.v1";
const MILESTONE_CATALOG: &str = include_str!("../../benchmarks/milestones.json");

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSeries {
    unit: String,
    count: usize,
    min: f64,
    p25: f64,
    p50: f64,
    p75: f64,
    p95: f64,
    p99: f64,
    max: f64,
    mean: f64,
    std_dev: f64,
    mad: f64,
    cv: f64,
    p95_sample_count_eligible: bool,
    samples: Vec<f64>,
}

impl MetricSeries {
    pub fn from_durations(samples: &[Duration]) -> Result<Self> {
        Self::from_values(
            "ms",
            samples
                .iter()
                .map(|sample| sample.as_secs_f64() * 1_000.0)
                .collect(),
        )
    }

    pub fn from_values(unit: &str, mut samples: Vec<f64>) -> Result<Self> {
        if samples.is_empty() {
            anyhow::bail!("benchmark metric must contain at least one sample");
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            anyhow::bail!("benchmark metric samples must be finite");
        }
        for sample in &mut samples {
            *sample = round3(*sample);
        }
        samples.sort_by(f64::total_cmp);
        let min = samples
            .first()
            .copied()
            .context("benchmark metric must contain at least one sample")?;
        let p25 = nearest_rank(&samples, 25);
        let p50 = nearest_rank(&samples, 50);
        let p75 = nearest_rank(&samples, 75);
        let p95 = nearest_rank(&samples, 95);
        let p99 = nearest_rank(&samples, 99);
        let max = samples
            .last()
            .copied()
            .context("benchmark metric must contain at least one sample")?;
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let variance = samples
            .iter()
            .map(|sample| (sample - mean).powi(2))
            .sum::<f64>()
            / samples.len() as f64;
        let std_dev = variance.sqrt();
        let mut absolute_deviations = samples
            .iter()
            .map(|sample| (sample - p50).abs())
            .collect::<Vec<_>>();
        absolute_deviations.sort_by(f64::total_cmp);
        let mad = nearest_rank(&absolute_deviations, 50);
        let cv = if mean == 0.0 { 0.0 } else { std_dev / mean };
        Ok(Self {
            unit: unit.to_owned(),
            count: samples.len(),
            min,
            p25,
            p50,
            p75,
            p95,
            p99,
            max,
            mean: round3(mean),
            std_dev: round3(std_dev),
            mad: round3(mad),
            cv: round3(cv),
            p95_sample_count_eligible: false,
            samples,
        })
    }

    fn statistic(&self, statistic: &str) -> Result<f64> {
        match statistic {
            "p50" => Ok(self.p50),
            "p95" => Ok(self.p95),
            "p99" => Ok(self.p99),
            "max" => Ok(self.max),
            _ => anyhow::bail!("unsupported benchmark statistic {statistic}"),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    suite: String,
    passed: bool,
    acceptance_eligible: bool,
    started_at_unix_ms: u128,
    completed_at_unix_ms: u128,
    total_ms: f64,
    milestone: MilestoneEvidence,
    provenance: Provenance,
    configuration: Value,
    measurements: Measurements,
    correctness: Correctness,
    budgets: Budgets,
    failures: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MilestoneEvidence {
    id: String,
    decision: String,
    kind: String,
    owner: String,
    profile: String,
    tier: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Provenance {
    source_commit: Option<String>,
    source_dirty: Option<bool>,
    executable: PathBuf,
    executable_digest: Option<ArtifactDigest>,
    testbed: String,
    os: &'static str,
    arch: &'static str,
    host: Host,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Host {
    kernel: Option<String>,
    cpu_model: Option<String>,
    logical_cpus: usize,
    memory_bytes: Option<u64>,
    rustc: Option<String>,
    cargo: Option<String>,
}

#[derive(Serialize)]
struct ArtifactDigest {
    algorithm: &'static str,
    value: String,
}

#[derive(Serialize)]
struct Measurements {
    metrics: BTreeMap<String, MetricSeries>,
    files: Value,
}

#[derive(Serialize)]
struct Correctness {
    checks: Vec<CorrectnessCheck>,
}

#[derive(Serialize)]
pub struct CorrectnessCheck {
    pub name: String,
    pub passed: bool,
    pub expected: Value,
    pub actual: Value,
}

impl CorrectnessCheck {
    pub fn equal(name: &str, expected: Value, actual: Value) -> Self {
        Self {
            name: name.to_owned(),
            passed: expected == actual,
            expected,
            actual,
        }
    }
}

#[derive(Serialize)]
struct Budgets {
    profile: String,
    limits: Vec<BudgetSpec>,
    checks: Vec<BudgetCheck>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BudgetSpec {
    name: String,
    metrics: Vec<String>,
    statistic: String,
    comparator: String,
    limit: f64,
    unit: String,
}

#[derive(Serialize)]
struct BudgetCheck {
    name: String,
    metric: String,
    statistic: String,
    actual: f64,
    limit: f64,
    unit: String,
    comparator: String,
    passed: bool,
}

#[derive(Deserialize)]
struct Catalog {
    schema: String,
    methodology: CatalogMethodology,
    suites: Vec<CatalogSuite>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogMethodology {
    p95_minimum_observations: usize,
}

#[derive(Deserialize)]
struct CatalogSuite {
    id: String,
    milestone: String,
    kind: String,
    owner: String,
    decision: String,
    profiles: Vec<CatalogProfile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogProfile {
    id: String,
    tier: String,
    acceptance_eligible: bool,
    budgets: Vec<BudgetSpec>,
}

pub struct ReceiptInput {
    pub suite: String,
    pub profile: String,
    pub started_at: SystemTime,
    pub configuration: Value,
    pub metrics: BTreeMap<String, MetricSeries>,
    pub files: Value,
    pub correctness_checks: Vec<CorrectnessCheck>,
}

/// Writes a receipt only when `VICIA_BENCH_RECEIPT` names an output path.
///
/// CSV stdout remains stable for existing callers. The receipt path is reported
/// on stderr so shell pipelines can keep treating stdout as CSV.
pub fn write_if_requested(mut input: ReceiptInput) -> Result<Option<PathBuf>> {
    let Some(path) = std::env::var_os(RECEIPT_PATH_ENV).map(PathBuf::from) else {
        return Ok(None);
    };
    let completed_at = SystemTime::now();
    let (suite, profile, p95_minimum_observations) = catalog_profile(&input.suite, &input.profile)?;
    for metric in input.metrics.values_mut() {
        metric.p95_sample_count_eligible = metric.count >= p95_minimum_observations;
    }
    let correctness_passed = input.correctness_checks.iter().all(|check| check.passed);
    let budget_checks = evaluate_budgets(&input.metrics, &profile.budgets)?;
    let budgets_passed = budget_checks.iter().all(|check| check.passed);
    let provenance = provenance()?;
    let p95_evidence_eligible = profile
        .budgets
        .iter()
        .filter(|budget| budget.statistic == "p95")
        .flat_map(|budget| &budget.metrics)
        .all(|metric| {
            input
                .metrics
                .get(metric)
                .is_some_and(|series| series.p95_sample_count_eligible)
        });
    let passed = correctness_passed && budgets_passed;
    let acceptance_eligible = profile.acceptance_eligible
        && provenance.source_dirty == Some(false)
        && provenance
            .source_commit
            .as_deref()
            .is_some_and(|commit| !commit.is_empty())
        && p95_evidence_eligible
        && passed;
    let failures = input
        .correctness_checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.name.clone())
        .chain(
            budget_checks
                .iter()
                .filter(|check| !check.passed)
                .map(|check| format!("{}: {}", check.name, check.metric)),
        )
        .collect::<Vec<_>>();
    let receipt = Receipt {
        schema: RECEIPT_SCHEMA,
        suite: input.suite,
        passed,
        acceptance_eligible,
        started_at_unix_ms: unix_ms(input.started_at)?,
        completed_at_unix_ms: unix_ms(completed_at)?,
        total_ms: round3(completed_at.duration_since(input.started_at)?.as_secs_f64() * 1_000.0),
        milestone: MilestoneEvidence {
            id: suite.milestone.clone(),
            decision: suite.decision.clone(),
            kind: suite.kind.clone(),
            owner: suite.owner.clone(),
            profile: profile.id.clone(),
            tier: profile.tier.clone(),
        },
        provenance,
        configuration: input.configuration,
        measurements: Measurements {
            metrics: input.metrics,
            files: input.files,
        },
        correctness: Correctness {
            checks: input.correctness_checks,
        },
        budgets: Budgets {
            profile: profile.id.clone(),
            limits: profile.budgets.clone(),
            checks: budget_checks,
        },
        failures,
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create receipt directory {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&receipt)?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("write benchmark receipt {}", path.display()))?;
    eprintln!("benchmark receipt: {}", path.display());
    Ok(Some(path))
}

fn catalog_profile(
    suite_id: &str,
    profile_id: &str,
) -> Result<(CatalogSuite, CatalogProfile, usize)> {
    let mut catalog: Catalog = serde_json::from_str(MILESTONE_CATALOG)?;
    if catalog.schema != CATALOG_SCHEMA {
        anyhow::bail!("unsupported benchmark milestone catalog schema");
    }
    let p95_minimum_observations = catalog.methodology.p95_minimum_observations;
    let suite_index = catalog
        .suites
        .iter()
        .position(|suite| suite.id == suite_id)
        .with_context(|| format!("benchmark suite {suite_id} is absent from milestone catalog"))?;
    let mut suite = catalog.suites.swap_remove(suite_index);
    let profile_index = suite
        .profiles
        .iter()
        .position(|profile| profile.id == profile_id)
        .with_context(|| {
            format!("benchmark profile {suite_id}/{profile_id} is absent from milestone catalog")
        })?;
    let profile = suite.profiles.swap_remove(profile_index);
    Ok((suite, profile, p95_minimum_observations))
}

fn evaluate_budgets(
    metrics: &BTreeMap<String, MetricSeries>,
    budgets: &[BudgetSpec],
) -> Result<Vec<BudgetCheck>> {
    let mut checks = Vec::new();
    for budget in budgets {
        if budget.comparator != "<=" {
            anyhow::bail!("unsupported benchmark comparator {}", budget.comparator);
        }
        for metric_name in &budget.metrics {
            let metric = metrics.get(metric_name).with_context(|| {
                format!("catalog budget references missing metric {metric_name}")
            })?;
            if metric.unit != budget.unit {
                anyhow::bail!(
                    "catalog budget unit {} does not match metric {} unit {}",
                    budget.unit,
                    metric_name,
                    metric.unit
                );
            }
            let actual = metric.statistic(&budget.statistic)?;
            checks.push(BudgetCheck {
                name: budget.name.clone(),
                metric: metric_name.clone(),
                statistic: budget.statistic.clone(),
                actual,
                limit: budget.limit,
                unit: budget.unit.clone(),
                comparator: budget.comparator.clone(),
                passed: actual <= budget.limit,
            });
        }
    }
    Ok(checks)
}

fn provenance() -> Result<Provenance> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let source_commit = std::env::var("VICIA_BENCH_SOURCE_COMMIT")
        .ok()
        .or_else(|| git_output(&["rev-parse", "HEAD"]));
    let source_dirty = match std::env::var("VICIA_BENCH_SOURCE_DIRTY") {
        Ok(value) => Some(parse_bool(&value)?),
        Err(_) => git_output(&["status", "--porcelain", "--untracked-files=no"])
            .map(|output| !output.is_empty()),
    };
    Ok(Provenance {
        source_commit,
        source_dirty,
        executable_digest: Some(ArtifactDigest {
            algorithm: "sha256",
            value: sha256_file(&executable)?,
        }),
        executable,
        testbed: std::env::var("VICIA_BENCH_TESTBED").unwrap_or_else(|_| "local".to_owned()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        host: host(),
    })
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let bytes = buffer
            .get(..read)
            .context("executable digest read exceeded buffer length")?;
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn install_base_fixture_if_configured(destination: &Path) -> Result<Option<PathBuf>> {
    let Some(source) = std::env::var_os(BASE_FIXTURE_ENV).map(PathBuf::from) else {
        return Ok(None);
    };
    let source = source
        .canonicalize()
        .with_context(|| format!("resolve base fixture {}", source.display()))?;
    std::fs::copy(&source, destination).with_context(|| {
        format!(
            "copy base fixture {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    let destination_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)?;
    destination_file.sync_all()?;
    Ok(Some(source))
}

fn host() -> Host {
    Host {
        kernel: command_output("uname", &["-sr"]),
        cpu_model: cpu_model(),
        logical_cpus: std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1),
        memory_bytes: memory_bytes(),
        rustc: command_output("rustc", &["--version", "--verbose"]),
        cargo: command_output("cargo", &["--version"]),
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

fn cpu_model() -> Option<String> {
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        return cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_owned())
        });
    }
    std::env::var("PROCESSOR_IDENTIFIER").ok()
}

fn memory_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let value_kib = meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })?;
    value_kib.checked_mul(1024)
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => anyhow::bail!("VICIA_BENCH_SOURCE_DIRTY must be true, false, 1, or 0"),
    }
}

fn unix_ms(time: SystemTime) -> Result<u128> {
    Ok(time.duration_since(UNIX_EPOCH)?.as_millis())
}

fn nearest_rank(sorted: &[f64], percentile: usize) -> f64 {
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    sorted
        .get(rank.saturating_sub(1).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}

fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}
