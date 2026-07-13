use anyhow::{Context, Result, bail};
use minigraf::{LeafReadDiagnostics, OpenOptions, QueryResult};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;

const BATCH: u64 = 1_000;
const POINT_BATCH: usize = 200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PointMeasurement {
    samples_ms_per_operation: Vec<f64>,
    raw_single_query_samples_ms: Vec<f64>,
    diagnostics: LeafReadDiagnostics,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregateMeasurement {
    samples_ms: Vec<f64>,
    count: u64,
    checksum: i128,
    open_baseline_rss_bytes: u64,
    workload_peak_rss_bytes: u64,
    workload_delta_rss_bytes: u64,
    diagnostics: LeafReadDiagnostics,
}

fn main() -> Result<()> {
    let args = env::args().collect::<Vec<_>>();
    match args.as_slice() {
        [_, command, path, facts] if command == "build" => {
            build_fixture(Path::new(path), facts.parse()?)
        }
        [_, command, path, facts, samples] if command == "point" => {
            let measurement = measure_point(Path::new(path), facts.parse()?, samples.parse()?)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        [_, command, path, facts, samples] if command == "aggregate" => {
            let measurement = measure_aggregate(Path::new(path), facts.parse()?, samples.parse()?)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        _ => bail!(
            "usage: leaf-read-path-bench build <graph> <facts> | point|aggregate <graph> <facts> <samples>"
        ),
    }
}

fn build_fixture(path: &Path, facts: u64) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let db = OpenOptions::new().path(path).open()?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut command = String::from("(transact [");
        for entity in start..(start + BATCH).min(facts) {
            command.push_str(&format!("[:leaf/e{entity} :leaf/value {entity}]"));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    Ok(())
}

fn measure_point(path: &Path, facts: u64, samples: usize) -> Result<PointMeasurement> {
    let db = OpenOptions::new().path(path).open()?;
    let expected = facts / 2;
    let query = format!("(query [:find ?v :where [:leaf/e{expected} :leaf/value ?v]])");
    for _ in 0..100 {
        validate_point(db.execute(&query)?, expected)?;
    }
    let mut samples_ms_per_operation = Vec::with_capacity(samples);
    let mut raw_single_query_samples_ms = Vec::with_capacity(samples);
    for _ in 0..samples {
        let raw_started = Instant::now();
        validate_point(db.execute(&query)?, expected)?;
        raw_single_query_samples_ms.push(elapsed_ms(raw_started));

        let started = Instant::now();
        for _ in 0..POINT_BATCH {
            validate_point(db.execute(&query)?, expected)?;
        }
        samples_ms_per_operation.push(elapsed_ms(started) / POINT_BATCH as f64);
    }
    db.set_leaf_read_diagnostics_enabled(true);
    validate_point(db.execute(&query)?, expected)?;
    let diagnostics = db.last_leaf_read_diagnostics();
    db.set_leaf_read_diagnostics_enabled(false);
    Ok(PointMeasurement {
        samples_ms_per_operation,
        raw_single_query_samples_ms,
        diagnostics,
    })
}

fn validate_point(result: QueryResult, expected: u64) -> Result<()> {
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("point query returned a non-query result")
    };
    let actual = results
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_integer())
        .context("point query returned no integer")?;
    if actual != i64::try_from(expected)? {
        bail!("point query mismatch")
    }
    Ok(())
}

fn measure_aggregate(path: &Path, facts: u64, samples: usize) -> Result<AggregateMeasurement> {
    let db = OpenOptions::new().path(path).open()?;
    let query = "(query [:find (count ?v) (sum ?v) :where [?e :leaf/value ?v]])";
    let expected_checksum = i128::from(facts) * i128::from(facts.saturating_sub(1)) / 2;
    let baseline = current_rss_bytes().context("read open baseline RSS")?;
    let mut samples_ms = Vec::with_capacity(samples);
    let mut pair = (0, 0);
    for _ in 0..samples {
        let started = Instant::now();
        pair = aggregate_pair(db.execute(query)?)?;
        samples_ms.push(elapsed_ms(started));
    }
    if pair != (facts, expected_checksum) {
        bail!("aggregate correctness mismatch")
    }
    db.set_leaf_read_diagnostics_enabled(true);
    pair = aggregate_pair(db.execute(query)?)?;
    let diagnostics = db.last_leaf_read_diagnostics();
    db.set_leaf_read_diagnostics_enabled(false);
    let peak = peak_rss_bytes().context("read workload peak RSS")?;
    Ok(AggregateMeasurement {
        samples_ms,
        count: pair.0,
        checksum: pair.1,
        open_baseline_rss_bytes: baseline,
        workload_peak_rss_bytes: peak,
        workload_delta_rss_bytes: peak.saturating_sub(baseline),
        diagnostics,
    })
}

fn aggregate_pair(result: QueryResult) -> Result<(u64, i128)> {
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("aggregate returned a non-query result")
    };
    let row = results.first().context("aggregate returned no row")?;
    let count = row
        .first()
        .and_then(|value| value.as_integer())
        .context("aggregate count missing")?;
    let checksum = row
        .get(1)
        .and_then(|value| value.as_integer())
        .context("aggregate checksum missing")?;
    Ok((u64::try_from(count)?, i128::from(checksum)))
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn status_kib(name: &str) -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with(name))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn current_rss_bytes() -> Option<u64> {
    status_kib("VmRSS:")
}

fn peak_rss_bytes() -> Option<u64> {
    status_kib("VmHWM:")
}
