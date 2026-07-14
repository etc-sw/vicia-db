#![cfg(not(target_arch = "wasm32"))]

mod bench_support;

use anyhow::{Context, Result, bail};
use bench_support::process_memory::{
    MemoryBreakdown, current_rss_bytes, memory_breakdown, peak_rss_bytes, trim_allocator,
};
use minigraf::{CurrentAttributeCursorDiagnostics, Minigraf, OpenOptions, QueryResult, Value};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const SCHEMA: &str = "vicia.aggregate-retention.v1";
const ATTRIBUTE: &str = ":retention/value";
const WRITE_BATCH: u64 = 1_000;
const DEFAULT_FILL_PERCENT: u8 = 90;
const ONE_ITERATION: usize = 1;
const REPEATED_ITERATIONS: usize = 20;

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

    fn facts(self) -> u64 {
        match self {
            Self::Smoke => 10_000,
            Self::Full => 1_000_000,
        }
    }

    fn pairs(self) -> usize {
        match self {
            Self::Smoke => 1,
            Self::Full => 5,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: String,
    profile: String,
    facts: u64,
    source_commit: String,
    tracked_clean: bool,
    host: Option<String>,
    cpu_model: Option<String>,
    fixture: Fixture,
    pairs: Vec<MeasurementPair>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    path: String,
    bytes: u64,
    sha256: String,
    format_version: u32,
    fill_percent: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeasurementPair {
    pair_index: usize,
    order: Vec<usize>,
    one: Measurement,
    twenty: Measurement,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    iterations: usize,
    tx_count_before: u64,
    tx_count_after: u64,
    graph_bytes_before: u64,
    graph_bytes_after: u64,
    wal_exists_before: bool,
    wal_exists_after: bool,
    baseline_rss_bytes: u64,
    baseline_breakdown: MemoryBreakdown,
    samples: Vec<AggregateSample>,
    process_peak_rss_bytes: u64,
    rss_before_trim_bytes: u64,
    breakdown_before_trim: MemoryBreakdown,
    breakdown_delta_before_trim: MemoryBreakdown,
    allocator_trim_supported: bool,
    allocator_trim_released: bool,
    rss_after_live_trim_bytes: u64,
    breakdown_after_live_trim: MemoryBreakdown,
    breakdown_delta_after_live_trim: MemoryBreakdown,
    retained_delta_before_trim_bytes: u64,
    retained_delta_after_live_trim_bytes: u64,
    rss_after_drop_trim_bytes: u64,
    breakdown_after_drop_trim: MemoryBreakdown,
    live_database_rss_bytes: u64,
    count: u64,
    checksum: i128,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregateSample {
    iteration: usize,
    elapsed_ms: f64,
    rss_bytes: u64,
    peak_rss_bytes: u64,
    cursor_diagnostics: CurrentAttributeCursorDiagnostics,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context("missing command")?;
    match command.as_str() {
        "run" => {
            let profile = Profile::parse(&args.next().context("missing profile")?)?;
            let output_dir = PathBuf::from(args.next().context("missing output directory")?);
            if args.next().is_some() {
                bail!("run accepts only <smoke|full> <output-directory>");
            }
            run_profile(profile, &output_dir)
        }
        "measure" => {
            let path = PathBuf::from(args.next().context("missing graph path")?);
            let facts = args.next().context("missing fact count")?.parse::<u64>()?;
            let iterations = args
                .next()
                .context("missing iterations")?
                .parse::<usize>()?;
            if args.next().is_some() || ![ONE_ITERATION, REPEATED_ITERATIONS].contains(&iterations)
            {
                bail!("measure accepts <graph-path> <facts> <1|20>");
            }
            println!(
                "{}",
                serde_json::to_string(&measure(&path, facts, iterations)?)?
            );
            Ok(())
        }
        _ => bail!(
            "usage: aggregate-retention-bench run <smoke|full> <output-directory> | measure <graph-path> <facts> <1|20>"
        ),
    }
}

fn run_profile(profile: Profile, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)?;
    let data_dir = output_dir.join("data");
    if data_dir.exists() {
        fs::remove_dir_all(&data_dir)?;
    }
    fs::create_dir_all(&data_dir)?;
    let fixture_path = data_dir.join("aggregate-retention.graph");
    build_fixture(&fixture_path, profile.facts())?;
    let executable = std::env::current_exe()?;
    let mut pairs = Vec::with_capacity(profile.pairs());
    for pair_index in 0..profile.pairs() {
        let order = if pair_index % 2 == 0 {
            [ONE_ITERATION, REPEATED_ITERATIONS]
        } else {
            [REPEATED_ITERATIONS, ONE_ITERATION]
        };
        eprintln!(
            "aggregate-retention: pair {}/{} order {} -> {}",
            pair_index + 1,
            profile.pairs(),
            order[0],
            order[1]
        );
        let first = child_measure(&executable, &fixture_path, profile.facts(), order[0])?;
        let second = child_measure(&executable, &fixture_path, profile.facts(), order[1])?;
        let (one, twenty) = if order[0] == ONE_ITERATION {
            (first, second)
        } else {
            (second, first)
        };
        pairs.push(MeasurementPair {
            pair_index,
            order: order.to_vec(),
            one,
            twenty,
        });
    }
    let receipt = Receipt {
        schema: SCHEMA.to_owned(),
        profile: profile.name().to_owned(),
        facts: profile.facts(),
        source_commit: command_text("git", &["rev-parse", "HEAD"])?,
        tracked_clean: command_text("git", &["status", "--porcelain", "--untracked-files=no"])?
            .is_empty(),
        host: command_text("hostname", &[]).ok(),
        cpu_model: cpu_model(),
        fixture: Fixture {
            path: fixture_path.to_string_lossy().into_owned(),
            bytes: fs::metadata(&fixture_path)?.len(),
            sha256: sha256_file(&fixture_path)?,
            format_version: fixture_format_version(&fixture_path)?,
            fill_percent: DEFAULT_FILL_PERCENT,
        },
        pairs,
    };
    let receipt_path = output_dir.join("receipt.json");
    fs::write(
        &receipt_path,
        format!("{}\n", serde_json::to_string_pretty(&receipt)?),
    )?;
    eprintln!("aggregate-retention: wrote {}", receipt_path.display());
    Ok(())
}

fn build_fixture(path: &Path, facts: u64) -> Result<()> {
    remove_database_files(path)?;
    let mut options = OpenOptions::new().benchmark_btree_fill_percent(DEFAULT_FILL_PERCENT);
    options.wal_checkpoint_threshold = usize::MAX;
    let db = options.path(path).open()?;
    for start in (0..facts).step_by(usize::try_from(WRITE_BATCH)?) {
        let end = start.saturating_add(WRITE_BATCH).min(facts);
        let mut command = String::from("(transact [");
        for value in start..end {
            command.push_str(&format!("[:retention/e{value} {ATTRIBUTE} {value}]"));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    drop(db);
    if sidecar_path(path, ".wal").exists() {
        bail!("checkpointed retention fixture left a WAL sidecar");
    }
    Ok(())
}

fn measure(path: &Path, facts: u64, iterations: usize) -> Result<Measurement> {
    let graph_bytes_before = fs::metadata(path)?.len();
    let wal = sidecar_path(path, ".wal");
    let wal_exists_before = wal.exists();
    let mut options = OpenOptions::new();
    options.wal_checkpoint_threshold = usize::MAX;
    let db = options.path(path).open()?;
    let tx_count_before = db.current_tx_count();
    let baseline_rss_bytes = current_rss_bytes().context("read RSS after open")?;
    let baseline_breakdown = memory_breakdown(path)?;
    let expected = (facts, expected_checksum(facts));
    let mut samples = Vec::with_capacity(iterations);
    for iteration in 1..=iterations {
        let (sample, actual) = aggregate_sample(&db, iteration)?;
        if actual != expected {
            bail!("aggregate correctness mismatch at iteration {iteration}");
        }
        samples.push(sample);
    }
    let rss_before_trim_bytes = current_rss_bytes().context("read RSS before trim")?;
    let breakdown_before_trim = memory_breakdown(path)?;
    let process_peak_rss_bytes = peak_rss_bytes().context("read process peak RSS")?;
    let (allocator_trim_supported, allocator_trim_released) = trim_allocator();
    let rss_after_live_trim_bytes = current_rss_bytes().context("read RSS after live trim")?;
    let breakdown_after_live_trim = memory_breakdown(path)?;
    let tx_count_after = db.current_tx_count();
    drop(db);
    let _ = trim_allocator();
    let rss_after_drop_trim_bytes = current_rss_bytes().context("read RSS after drop trim")?;
    let breakdown_after_drop_trim = memory_breakdown(path)?;
    let graph_bytes_after = fs::metadata(path)?.len();
    let wal_exists_after = wal.exists();
    Ok(Measurement {
        iterations,
        tx_count_before,
        tx_count_after,
        graph_bytes_before,
        graph_bytes_after,
        wal_exists_before,
        wal_exists_after,
        baseline_rss_bytes,
        baseline_breakdown,
        samples,
        process_peak_rss_bytes,
        rss_before_trim_bytes,
        breakdown_before_trim,
        breakdown_delta_before_trim: breakdown_before_trim.saturating_sub(baseline_breakdown),
        allocator_trim_supported,
        allocator_trim_released,
        rss_after_live_trim_bytes,
        breakdown_after_live_trim,
        breakdown_delta_after_live_trim: breakdown_after_live_trim
            .saturating_sub(baseline_breakdown),
        retained_delta_before_trim_bytes: rss_before_trim_bytes.saturating_sub(baseline_rss_bytes),
        retained_delta_after_live_trim_bytes: rss_after_live_trim_bytes
            .saturating_sub(baseline_rss_bytes),
        rss_after_drop_trim_bytes,
        breakdown_after_drop_trim,
        live_database_rss_bytes: rss_after_live_trim_bytes
            .saturating_sub(rss_after_drop_trim_bytes),
        count: expected.0,
        checksum: expected.1,
    })
}

fn aggregate_sample(db: &Minigraf, iteration: usize) -> Result<(AggregateSample, (u64, i128))> {
    let started = Instant::now();
    let result =
        db.execute("(query [:find (count ?v) (sum ?v) :where [?e :retention/value ?v]])")?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
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
        .context("aggregate checksum missing")?;
    let cursor_diagnostics = db
        .last_current_attribute_cursor_diagnostics()
        .context("aggregate cursor diagnostics missing")?;
    Ok((
        AggregateSample {
            iteration,
            elapsed_ms,
            rss_bytes: current_rss_bytes().context("read aggregate RSS")?,
            peak_rss_bytes: peak_rss_bytes().context("read aggregate peak RSS")?,
            cursor_diagnostics,
        },
        (u64::try_from(count)?, i128::from(checksum)),
    ))
}

fn child_measure(
    executable: &Path,
    path: &Path,
    facts: u64,
    iterations: usize,
) -> Result<Measurement> {
    let output = Command::new(executable)
        .arg("measure")
        .arg(path)
        .arg(facts.to_string())
        .arg(iterations.to_string())
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!(
            "measurement child failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode measurement child output")
}

fn expected_checksum(facts: u64) -> i128 {
    i128::from(facts) * i128::from(facts.saturating_sub(1)) / 2
}

fn fixture_format_version(path: &Path) -> Result<u32> {
    let mut bytes = [0_u8; 8];
    fs::File::open(path)?.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes[4..8].try_into()?))
}

fn sha256_file(path: &Path) -> Result<String> {
    let output = Command::new("sha256sum").arg(path).output()?;
    if !output.status.success() {
        bail!("sha256sum failed with {}", output.status);
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum returned no digest")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_checksum_matches_fixture_contract() {
        assert_eq!(expected_checksum(1_000_000), 499_999_500_000);
    }

    #[test]
    fn full_profile_rotates_endpoint_order() {
        let orders = (0..Profile::Full.pairs())
            .map(|index| {
                if index % 2 == 0 {
                    [ONE_ITERATION, REPEATED_ITERATIONS]
                } else {
                    [REPEATED_ITERATIONS, ONE_ITERATION]
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(orders[0], [1, 20]);
        assert_eq!(orders[1], [20, 1]);
        assert_eq!(orders.len(), 5);
    }
}
