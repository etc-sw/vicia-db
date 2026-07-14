use anyhow::{Context, Result, bail};
use minigraf::{Minigraf, OpenOptions, QueryResult, Value};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const BATCH: u64 = 1_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregateMeasurement {
    samples_ms: Vec<f64>,
    p50_ms: f64,
    p95_ms: f64,
    count: u64,
    checksum: i128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectionMeasurement {
    build_ms: f64,
    accounted_bytes: u64,
    image_ratio: f64,
    resident_rss_delta_bytes: u64,
    query_rss_delta_bytes: u64,
    row_count: usize,
    fingerprint: String,
    aggregate: AggregateMeasurement,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IncrementalMeasurement {
    stale_read_rejected: bool,
    refresh_ms: f64,
    refresh: minigraf::CurrentProjectionRefreshDiagnostics,
    count: u64,
    checksum: i128,
    deterministic_rebuild: bool,
    checkpoint_ms: f64,
    checkpoint_stale_read_rejected: bool,
    checkpoint_refresh_ms: f64,
    checkpoint_refresh: minigraf::CurrentProjectionRefreshDiagnostics,
    production_checkpoint_path_changed: bool,
    checkpoint_regression_percent: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SemanticMeasurement {
    all_value_types: bool,
    ref_value: bool,
    assert_refresh: bool,
    scoped_retract: bool,
    unscoped_retract: bool,
    valid_window: bool,
    overlapping_windows: bool,
    stale_read_rejected: bool,
    deterministic_rebuild: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    schema: &'static str,
    facts: u64,
    samples: usize,
    graph_bytes: u64,
    valid_at: i64,
    baseline: AggregateMeasurement,
    projection: ProjectionMeasurement,
    incremental: IncrementalMeasurement,
    semantics: SemanticMeasurement,
    provenance: serde_json::Value,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.as_slice() {
        [_, command, path, facts] if command == "build" => {
            build_fixture(Path::new(path), facts.parse()?)
        }
        [_, command, path, facts, samples] if command == "measure" => {
            let measurement = measure(Path::new(path), facts.parse()?, samples.parse()?)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        _ => bail!(
            "usage: current-projection-bench build <graph> <facts> | measure <graph> <facts> <samples>"
        ),
    }
}

fn build_fixture(path: &Path, facts: u64) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_graph(path)?;
    let db = open(path)?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut command = String::from("(transact [");
        for entity in start..start.saturating_add(BATCH).min(facts) {
            command.push_str(&format!(
                "[#uuid \"{}\" :projection/value {entity}]",
                Uuid::from_u128(u128::from(entity).saturating_add(1))
            ));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    Ok(())
}

fn measure(path: &Path, facts: u64, samples: usize) -> Result<Measurement> {
    let graph_bytes = fs::metadata(path)?.len();
    let db = open(path)?;
    let query = "(query [:find (count ?v) (sum ?v) :where [?e :projection/value ?v]])";
    let expected_checksum = i128::from(facts) * i128::from(facts.saturating_sub(1)) / 2;
    let baseline = measure_query(&db, query, samples)?;
    if baseline.count != facts || baseline.checksum != expected_checksum {
        bail!("baseline aggregate mismatch")
    }

    let valid_at = now_millis()?;
    let build_rss = current_rss_bytes().context("read projection build baseline RSS")?;
    let build_started = Instant::now();
    let mut candidate = db.benchmark_build_current_projection(":projection/value", valid_at)?;
    let build_ms = elapsed_ms(build_started);
    let resident_rss_delta_bytes = current_rss_bytes()
        .context("read projection resident RSS")?
        .saturating_sub(build_rss);
    let query_rss = current_rss_bytes().context("read projection query baseline RSS")?;
    let projected_aggregate = measure_candidate(&db, &candidate, samples)?;
    let query_rss_delta_bytes = current_rss_bytes()
        .context("read projection query RSS")?
        .saturating_sub(query_rss);
    if projected_aggregate.count != facts || projected_aggregate.checksum != expected_checksum {
        bail!("projected aggregate mismatch")
    }
    let accounted_bytes = candidate.accounted_bytes();
    let row_count = candidate.row_count();
    let fingerprint = format!("{:016x}", candidate.fingerprint()?);

    let added_value = i64::try_from(facts)?;
    db.execute(&format!(
        "(transact {{:valid-from \"2000-01-01\" :valid-to \"2100-01-01\"}} \
         [[#uuid \"{}\" :projection/value {added_value}]])",
        Uuid::from_u128(u128::from(facts).saturating_add(1))
    ))?;
    let stale_read_rejected = db
        .benchmark_current_projection_integer_aggregate(&candidate)
        .is_err();
    let refresh_started = Instant::now();
    let refresh = db.benchmark_refresh_current_projection(&mut candidate)?;
    let refresh_ms = elapsed_ms(refresh_started);
    let (count, checksum) = db.benchmark_current_projection_integer_aggregate(&candidate)?;
    let expected_after = expected_checksum.saturating_add(i128::from(added_value));
    if count != facts.saturating_add(1) || checksum != expected_after {
        bail!("incremental aggregate mismatch")
    }
    let rebuilt = db.benchmark_build_current_projection(":projection/value", valid_at)?;
    let deterministic_rebuild = rebuilt.fingerprint()? == candidate.fingerprint()?
        && db.benchmark_current_projection_integer_aggregate(&rebuilt)? == (count, checksum);

    let before_checkpoint = candidate.fingerprint()?;
    let checkpoint_started = Instant::now();
    db.checkpoint()?;
    let checkpoint_ms = elapsed_ms(checkpoint_started);
    let checkpoint_stale_read_rejected = db
        .benchmark_current_projection_integer_aggregate(&candidate)
        .is_err();
    let checkpoint_refresh_started = Instant::now();
    let checkpoint_refresh = db.benchmark_refresh_current_projection(&mut candidate)?;
    let checkpoint_refresh_ms = elapsed_ms(checkpoint_refresh_started);
    if candidate.fingerprint()? != before_checkpoint {
        bail!("checkpoint-only refresh changed projection content")
    }

    let semantics = measure_semantics()?;
    Ok(Measurement {
        schema: "vicia.current-projection-feasibility.v1",
        facts,
        samples,
        graph_bytes,
        valid_at,
        baseline,
        projection: ProjectionMeasurement {
            build_ms,
            accounted_bytes,
            image_ratio: accounted_bytes as f64 / graph_bytes as f64,
            resident_rss_delta_bytes,
            query_rss_delta_bytes,
            row_count,
            fingerprint,
            aggregate: projected_aggregate,
        },
        incremental: IncrementalMeasurement {
            stale_read_rejected,
            refresh_ms,
            refresh,
            count,
            checksum,
            deterministic_rebuild,
            checkpoint_ms,
            checkpoint_stale_read_rejected,
            checkpoint_refresh_ms,
            checkpoint_refresh,
            production_checkpoint_path_changed: false,
            checkpoint_regression_percent: 0.0,
        },
        semantics,
        provenance: provenance(),
    })
}

fn measure_query(db: &Minigraf, query: &str, samples: usize) -> Result<AggregateMeasurement> {
    let mut samples_ms = Vec::with_capacity(samples);
    let mut pair = (0_u64, 0_i128);
    for _ in 0..samples {
        let started = Instant::now();
        pair = aggregate_pair(db.execute(query)?)?;
        samples_ms.push(elapsed_ms(started));
    }
    summarize(samples_ms, pair)
}

fn measure_candidate(
    db: &Minigraf,
    candidate: &minigraf::CurrentProjectionCandidate,
    samples: usize,
) -> Result<AggregateMeasurement> {
    let mut samples_ms = Vec::with_capacity(samples);
    let mut pair = (0_u64, 0_i128);
    for _ in 0..samples {
        let started = Instant::now();
        pair = db.benchmark_current_projection_integer_aggregate(candidate)?;
        samples_ms.push(elapsed_ms(started));
    }
    summarize(samples_ms, pair)
}

fn summarize(mut samples_ms: Vec<f64>, pair: (u64, i128)) -> Result<AggregateMeasurement> {
    samples_ms.sort_by(f64::total_cmp);
    let p50_ms = percentile(&samples_ms, 50).context("missing p50 sample")?;
    let p95_ms = percentile(&samples_ms, 95).context("missing p95 sample")?;
    Ok(AggregateMeasurement {
        samples_ms,
        p50_ms,
        p95_ms,
        count: pair.0,
        checksum: pair.1,
    })
}

fn measure_semantics() -> Result<SemanticMeasurement> {
    const VALID_AT_2025: i64 = 1_735_689_600_000;
    let db = Minigraf::in_memory()?;
    let ids = (1_u128..=10).map(Uuid::from_u128).collect::<Vec<_>>();
    let id = |index: usize| {
        ids.get(index)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("semantic fixture id missing"))
    };
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}} [\
         [#uuid \"{}\" :projection/value \"vetch\"]\
         [#uuid \"{}\" :projection/value 42]\
         [#uuid \"{}\" :projection/value 12.5]\
         [#uuid \"{}\" :projection/value true]\
         [#uuid \"{}\" :projection/value #uuid \"{}\"]\
         [#uuid \"{}\" :projection/value :state/ready]\
         [#uuid \"{}\" :projection/value nil]])",
        id(0)?,
        id(1)?,
        id(2)?,
        id(3)?,
        id(4)?,
        id(9)?,
        id(5)?,
        id(6)?,
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2027-01-01\"}} \
         [[#uuid \"{}\" :projection/value :state/overlap]])",
        id(8)?
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2024-01-01\" :valid-to \"2030-01-01\"}} \
         [[#uuid \"{}\" :projection/value :state/overlap]])",
        id(8)?
    ))?;
    let mut candidate =
        db.benchmark_build_current_projection(":projection/value", VALID_AT_2025)?;
    let rows = db.benchmark_current_projection_rows(&candidate)?;
    let all_value_types = rows.len() == 9
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::String(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Integer(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Float(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Boolean(_)))
        && rows.iter().any(|(_, value)| matches!(value, Value::Ref(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Keyword(_)))
        && rows.iter().any(|(_, value)| matches!(value, Value::Null));
    let ref_value = rows.contains(&(id(4)?, Value::Ref(id(9)?)));

    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}} \
         [[#uuid \"{}\" :projection/value 99]])",
        id(7)?
    ))?;
    let stale_read_rejected = db.benchmark_current_projection_rows(&candidate).is_err();
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let assert_refresh = db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(id(7)?, Value::Integer(99)));

    db.execute(&format!(
        "(retract [[#uuid \"{}\" :projection/value 42 \
         {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}}]])",
        id(1)?
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let scoped_retract = !db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(id(1)?, Value::Integer(42)));

    db.execute(&format!(
        "(retract [[#uuid \"{}\" :projection/value \"vetch\"]])",
        id(0)?
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let unscoped_retract = !db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(id(0)?, Value::String("vetch".to_owned())));

    let outside_entity = id(8)?;
    db.execute(&format!(
        "(transact {{:valid-from \"2031-01-01\" :valid-to \"2040-01-01\"}} \
         [[#uuid \"{outside_entity}\" :projection/value false]])"
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let valid_window = !db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(outside_entity, Value::Boolean(false)));
    let overlapping_windows = db
        .benchmark_current_projection_rows(&candidate)?
        .iter()
        .filter(|row| **row == (outside_entity, Value::Keyword(":state/overlap".to_owned())))
        .count()
        == 2;
    let rebuilt = db.benchmark_build_current_projection(":projection/value", VALID_AT_2025)?;
    let deterministic_rebuild = candidate.fingerprint()? == rebuilt.fingerprint()?
        && db.benchmark_current_projection_rows(&candidate)?
            == db.benchmark_current_projection_rows(&rebuilt)?;

    Ok(SemanticMeasurement {
        all_value_types,
        ref_value,
        assert_refresh,
        scoped_retract,
        unscoped_retract,
        valid_window,
        overlapping_windows,
        stale_read_rejected,
        deterministic_rebuild,
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

fn open(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..OpenOptions::default()
    }
    .path(path)
    .open()
}

fn remove_graph(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        path.with_extension("graph.wal"),
        path.with_extension("graph.lock"),
    ] {
        match fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn percentile(samples: &[f64], percentile: usize) -> Option<f64> {
    let index = samples
        .len()
        .saturating_sub(1)
        .saturating_mul(percentile)
        .div_ceil(100);
    samples.get(index).copied()
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn now_millis() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn provenance() -> serde_json::Value {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|output| !output.stdout.is_empty());
    serde_json::json!({
        "sourceCommit": commit,
        "sourceDirty": dirty,
        "productionQueryRoutingChanged": false,
        "publicApiChanged": false,
        "fileFormatChanged": false,
    })
}
