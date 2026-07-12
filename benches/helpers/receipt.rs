//! Machine-readable evidence receipts for caller-shaped benchmark harnesses.

use anyhow::{Context, Result};
use crc32fast::Hasher;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RECEIPT_PATH_ENV: &str = "VICIA_BENCH_RECEIPT";
const RECEIPT_SCHEMA: &str = "vicia.benchmark.receipt.v1";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSeries {
    unit: &'static str,
    count: usize,
    p50: f64,
    p95: f64,
    max: f64,
    p95_sample_count_eligible: bool,
    samples: Vec<f64>,
}

impl MetricSeries {
    pub fn from_durations(samples: &[Duration]) -> Result<Self> {
        if samples.is_empty() {
            anyhow::bail!("benchmark metric must contain at least one sample");
        }
        let mut values = samples
            .iter()
            .map(|sample| round3(sample.as_secs_f64() * 1000.0))
            .collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let p50 = nearest_rank(&values, 50);
        let p95 = nearest_rank(&values, 95);
        let max = values
            .last()
            .copied()
            .context("benchmark metric must contain at least one sample")?;
        Ok(Self {
            unit: "ms",
            count: values.len(),
            p50,
            p95,
            max,
            p95_sample_count_eligible: values.len() >= 20,
            samples: values,
        })
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
    provenance: Provenance,
    configuration: Value,
    measurements: Measurements,
    correctness: Correctness,
    budgets: Budgets,
    failures: Vec<String>,
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
}

#[derive(Serialize)]
struct Budgets {
    profile: &'static str,
    checks: Vec<Value>,
}

pub struct ReceiptInput {
    pub suite: String,
    pub acceptance_eligible: bool,
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
pub fn write_if_requested(input: ReceiptInput) -> Result<Option<PathBuf>> {
    let Some(path) = std::env::var_os(RECEIPT_PATH_ENV).map(PathBuf::from) else {
        return Ok(None);
    };
    let completed_at = SystemTime::now();
    let correctness_passed = input.correctness_checks.iter().all(|check| check.passed);
    let failures = input
        .correctness_checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.name.clone())
        .collect::<Vec<_>>();
    let provenance = provenance()?;
    let acceptance_eligible = input.acceptance_eligible
        && provenance.source_dirty == Some(false)
        && input
            .metrics
            .values()
            .all(|metric| metric.p95_sample_count_eligible);
    let receipt = Receipt {
        schema: RECEIPT_SCHEMA,
        suite: input.suite,
        passed: correctness_passed,
        acceptance_eligible,
        started_at_unix_ms: unix_ms(input.started_at)?,
        completed_at_unix_ms: unix_ms(completed_at)?,
        total_ms: round3(completed_at.duration_since(input.started_at)?.as_secs_f64() * 1000.0),
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
            profile: "none",
            checks: Vec::new(),
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
        executable_digest: crc32_digest(&executable)?,
        executable,
        testbed: std::env::var("VICIA_BENCH_TESTBED").unwrap_or_else(|_| "local".to_owned()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    })
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

fn crc32_digest(path: &Path) -> Result<Option<ArtifactDigest>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let mut hasher = Hasher::new();
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
    Ok(Some(ArtifactDigest {
        algorithm: "crc32",
        value: format!("{:08x}", hasher.finalize()),
    }))
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
