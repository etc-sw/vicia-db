#![cfg(not(target_arch = "wasm32"))]

//! Repeated receipt-checkpoint growth and recovery benchmark.
//!
//! Modes (argv[1] or MINIGRAF_DELTA_ACCUMULATION_MODE):
//!   full     — 1M-fact base, seven accumulated-delta shapes
//!   t8b-mini — 1M-fact base, bounded pre-optimization gate
//!   smoke    — 10K-fact base, two quick correctness/growth shapes

use anyhow::{Context, Result, bail};
use minigraf::{Minigraf, OpenOptions, QueryResult};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

#[path = "helpers/receipt.rs"]
mod receipt;

const FULL_BASE_FACTS: usize = 1_000_000;
const SMOKE_BASE_FACTS: usize = 10_000;
const BASE_BATCH_SIZE: usize = 1_000;
const PAGE_SIZE_BYTES: u64 = 4096;
const DELTA_SEGMENT_MAGIC: &[u8] = b"MGDSG001";
const CORRUPTION_SCAN_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
const SEGMENT_COUNT_SCAN_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_QUERY_PROBES_PER_SCENARIO: usize = 32;
const T8B_MINI_MODE: &str = "t8b-mini";
const SMOKE_MODE: &str = "smoke";
const FULL_SCENARIOS: &[AccumulationScenario] = &[
    AccumulationScenario {
        facts_per_checkpoint: 1,
        checkpoints: 10,
    },
    AccumulationScenario {
        facts_per_checkpoint: 1,
        checkpoints: 100,
    },
    AccumulationScenario {
        facts_per_checkpoint: 1,
        checkpoints: 1_000,
    },
    AccumulationScenario {
        facts_per_checkpoint: 1,
        checkpoints: 10_000,
    },
    AccumulationScenario {
        facts_per_checkpoint: 10,
        checkpoints: 100,
    },
    AccumulationScenario {
        facts_per_checkpoint: 10,
        checkpoints: 1_000,
    },
    AccumulationScenario {
        facts_per_checkpoint: 100,
        checkpoints: 100,
    },
];
const T8B_MINI_SCENARIOS: &[AccumulationScenario] = &[
    AccumulationScenario {
        facts_per_checkpoint: 1,
        checkpoints: 1_000,
    },
    AccumulationScenario {
        facts_per_checkpoint: 10,
        checkpoints: 100,
    },
];
const SMOKE_SCENARIOS: &[AccumulationScenario] = &[
    AccumulationScenario {
        facts_per_checkpoint: 1,
        checkpoints: 20,
    },
    AccumulationScenario {
        facts_per_checkpoint: 10,
        checkpoints: 10,
    },
];

#[derive(Clone, Copy, Debug)]
struct AccumulationScenario {
    facts_per_checkpoint: usize,
    checkpoints: usize,
}

#[derive(Clone, Copy, Debug)]
struct AccumulationConfig {
    mode: &'static str,
    base_facts: usize,
    scenarios: &'static [AccumulationScenario],
}

#[derive(Debug)]
struct AccumulationMeasurement {
    facts_per_checkpoint: usize,
    checkpoints: usize,
    accumulated_delta_facts: usize,
    query_probe_count: usize,
    flush: DurationStats,
    reopen: DurationStats,
    current_query: DurationStats,
    as_of_query: DurationStats,
    base_file_bytes: u64,
    final_file_bytes: u64,
    base_pages: u64,
    final_pages: u64,
    actual_delta_facts: usize,
    segment_count: usize,
    corrupt_latest_fallback: bool,
}

#[derive(Clone, Debug)]
struct DurationStats {
    samples: Vec<Duration>,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

fn main() -> Result<()> {
    let started_at = SystemTime::now();
    let config = selected_config();
    let root = tempfile::tempdir()?;
    let base_path = root.path().join(format!("base-{}.graph", config.mode));
    let fixture_source = receipt::install_base_fixture_if_configured(&base_path)?;
    if fixture_source.is_some() {
        verify_base_fact_count(&base_path, config.base_facts)?;
    } else {
        build_checkpointed_base(&base_path, config.base_facts)?;
    }
    let base_file_bytes = file_len(&base_path)?;
    let base_pages = file_page_count(&base_path)?;
    let base_fixture_sha256 = receipt::sha256_file(&base_path)?;

    println!(
        "facts_per_checkpoint,checkpoints,accumulated_delta_facts,query_probe_count,flush_p50_ms,flush_p95_ms,flush_max_ms,reopen_p50_ms,reopen_p95_ms,reopen_max_ms,current_query_p50_ms,current_query_p95_ms,current_query_max_ms,as_of_query_p50_ms,as_of_query_p95_ms,as_of_query_max_ms,base_file_bytes,final_file_bytes,file_growth_bytes,base_pages,final_pages,page_growth,actual_delta_facts,segment_count,corrupt_latest_fallback"
    );

    let mut measurements = Vec::new();
    for &scenario in config.scenarios {
        let run_path = root.path().join(format!(
            "accum-{}x{}.graph",
            scenario.facts_per_checkpoint, scenario.checkpoints
        ));
        copy_checkpointed_base(&base_path, &run_path)?;
        let measurement = measure_accumulation(
            &run_path,
            config.base_facts,
            base_file_bytes,
            base_pages,
            scenario,
        )?;
        print_measurement(&measurement);
        measurements.push(measurement);
    }

    assert_matrix_complete(&measurements, config.scenarios)?;
    write_receipt(
        started_at,
        config,
        &measurements,
        &base_fixture_sha256,
        fixture_source.as_deref(),
    )?;
    Ok(())
}

fn selected_config() -> AccumulationConfig {
    let arg_mode = std::env::args().nth(1);
    let env_mode = std::env::var("MINIGRAF_DELTA_ACCUMULATION_MODE").ok();
    if arg_mode.as_deref() == Some(SMOKE_MODE) || env_mode.as_deref() == Some(SMOKE_MODE) {
        AccumulationConfig {
            mode: SMOKE_MODE,
            base_facts: SMOKE_BASE_FACTS,
            scenarios: SMOKE_SCENARIOS,
        }
    } else if arg_mode.as_deref() == Some(T8B_MINI_MODE)
        || env_mode.as_deref() == Some(T8B_MINI_MODE)
    {
        AccumulationConfig {
            mode: T8B_MINI_MODE,
            base_facts: FULL_BASE_FACTS,
            scenarios: T8B_MINI_SCENARIOS,
        }
    } else {
        AccumulationConfig {
            mode: "full",
            base_facts: FULL_BASE_FACTS,
            scenarios: FULL_SCENARIOS,
        }
    }
}

fn measure_accumulation(
    path: &Path,
    base_facts: usize,
    base_file_bytes: u64,
    base_pages: u64,
    scenario: AccumulationScenario,
) -> Result<AccumulationMeasurement> {
    let mut flush = Vec::with_capacity(scenario.checkpoints);
    let mut reopen = Vec::with_capacity(scenario.checkpoints);
    let mut current_query = Vec::with_capacity(scenario.checkpoints);
    let mut as_of_query = Vec::with_capacity(scenario.checkpoints);
    let probe_points = probe_points(scenario.checkpoints);
    let mut probe_cursor = 0usize;

    let mut db = open_no_auto_checkpoint(path)?;
    for checkpoint_index in 0..scenario.checkpoints {
        add_receipt_batch(&db, scenario, checkpoint_index)?;
        let tx_count = db.current_tx_count();
        let should_probe = probe_points
            .get(probe_cursor)
            .is_some_and(|probe| *probe == checkpoint_index);
        let current_entity = if should_probe {
            Some(receipt_entity(
                scenario,
                checkpoint_index,
                scenario.facts_per_checkpoint.saturating_sub(1),
            ))
        } else {
            None
        };

        let started = Instant::now();
        db.checkpoint().map_err(db_error)?;
        flush.push(started.elapsed());

        if let Some(current_entity) = current_entity.as_ref() {
            let started = Instant::now();
            assert_query_count(
                &db,
                &current_query_for(current_entity),
                1,
                "current receipt query should see the latest write",
            )?;
            current_query.push(started.elapsed());

            drop(db);
            let started = Instant::now();
            db = open_no_auto_checkpoint(path)?;
            reopen.push(started.elapsed());

            let started = Instant::now();
            assert_query_count(
                &db,
                &as_of_query_for(current_entity, tx_count),
                1,
                "as-of receipt query should replay the latest write",
            )?;
            as_of_query.push(started.elapsed());
            probe_cursor = probe_cursor.saturating_add(1);
        }
    }

    let records = db.export_fact_log().map_err(db_error)?;
    let actual_delta_facts = records
        .len()
        .checked_sub(base_facts)
        .ok_or_else(|| anyhow::anyhow!("exported fact log is smaller than base fixture"))?;
    drop(db);

    let final_file_bytes = file_len(path)?;
    let final_pages = file_page_count(path)?;
    let segment_count = count_delta_segments(path)?;
    let corrupt_latest_fallback = verify_corrupt_latest_segment_fallback(path, scenario)?;

    Ok(AccumulationMeasurement {
        facts_per_checkpoint: scenario.facts_per_checkpoint,
        checkpoints: scenario.checkpoints,
        accumulated_delta_facts: scenario.facts_per_checkpoint * scenario.checkpoints,
        query_probe_count: probe_points.len(),
        flush: duration_stats(&flush)?,
        reopen: duration_stats(&reopen)?,
        current_query: duration_stats(&current_query)?,
        as_of_query: duration_stats(&as_of_query)?,
        base_file_bytes,
        final_file_bytes,
        base_pages,
        final_pages,
        actual_delta_facts,
        segment_count,
        corrupt_latest_fallback,
    })
}

fn build_checkpointed_base(path: &Path, base_facts: usize) -> Result<()> {
    let db = open_no_auto_checkpoint(path)?;
    for batch_start in (0..base_facts).step_by(BASE_BATCH_SIZE) {
        let batch_end = (batch_start + BASE_BATCH_SIZE).min(base_facts);
        let mut command = String::from("(transact [");
        for index in batch_start..batch_end {
            push_base_fact(&mut command, index);
        }
        command.push_str("])");
        db.execute(&command).map_err(db_error)?;
    }
    db.checkpoint().map_err(db_error)?;
    drop(db);
    Ok(())
}

fn verify_base_fact_count(path: &Path, expected: usize) -> Result<()> {
    let db = open_no_auto_checkpoint(path)?;
    let actual = db.export_fact_log().map_err(db_error)?.len();
    if actual != expected {
        bail!("provided base fixture fact count does not match benchmark profile");
    }
    Ok(())
}

fn add_receipt_batch(
    db: &Minigraf,
    scenario: AccumulationScenario,
    checkpoint_index: usize,
) -> Result<()> {
    let mut command = String::from("(transact [");
    for fact_index in 0..scenario.facts_per_checkpoint {
        let entity = receipt_entity(scenario, checkpoint_index, fact_index);
        let target = deterministic_uuid(delta_fact_ordinal(scenario, checkpoint_index, fact_index));
        command.push_str(&format!(r#"[{entity} :bench/ref #uuid "{target}"]"#));
    }
    command.push_str("])");
    db.execute(&command).map_err(db_error)?;
    Ok(())
}

fn verify_corrupt_latest_segment_fallback(
    path: &Path,
    scenario: AccumulationScenario,
) -> Result<bool> {
    if scenario.checkpoints < 2 {
        return Ok(false);
    }

    corrupt_last_delta_segment(path)?;
    let db = open_no_auto_checkpoint(path)?;
    let previous_entity = receipt_entity(
        scenario,
        scenario.checkpoints - 2,
        scenario.facts_per_checkpoint.saturating_sub(1),
    );
    let latest_entity = receipt_entity(
        scenario,
        scenario.checkpoints - 1,
        scenario.facts_per_checkpoint.saturating_sub(1),
    );

    let previous_visible = query_count(&db, &current_query_for(&previous_entity))? == 1;
    let latest_hidden = query_count(&db, &current_query_for(&latest_entity))? == 0;
    Ok(previous_visible && latest_hidden)
}

fn corrupt_last_delta_segment(path: &Path) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let file_len = file.metadata()?.len();
    let offset = find_last_delta_segment_marker(&mut file, file_len)?;
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut byte)?;
    byte[0] ^= 0x01;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(&byte)?;
    file.sync_data()?;
    Ok(())
}

fn count_delta_segments(path: &Path) -> Result<usize> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; SEGMENT_COUNT_SCAN_CHUNK_BYTES];
    let mut carry = Vec::new();
    let mut count = 0usize;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        let mut chunk = Vec::with_capacity(carry.len().saturating_add(read));
        chunk.extend_from_slice(&carry);
        chunk.extend_from_slice(
            buffer
                .get(..read)
                .context("segment scan read exceeded buffer length")?,
        );
        count = count.saturating_add(
            chunk
                .windows(DELTA_SEGMENT_MAGIC.len())
                .filter(|window| *window == DELTA_SEGMENT_MAGIC)
                .count(),
        );

        let keep = DELTA_SEGMENT_MAGIC.len().saturating_sub(1).min(chunk.len());
        carry.clear();
        if keep > 0 {
            carry.extend_from_slice(
                chunk
                    .get(chunk.len().saturating_sub(keep)..)
                    .context("segment scan carry range is invalid")?,
            );
        }
    }

    Ok(count)
}

fn find_last_delta_segment_marker(file: &mut std::fs::File, file_len: u64) -> Result<u64> {
    if DELTA_SEGMENT_MAGIC.is_empty() {
        bail!("delta segment marker must not be empty");
    }

    let overlap = DELTA_SEGMENT_MAGIC.len().saturating_sub(1) as u64;
    let mut search_end = file_len;
    while search_end > 0 {
        let chunk_start = search_end.saturating_sub(CORRUPTION_SCAN_CHUNK_BYTES);
        let read_end = if search_end == file_len {
            search_end
        } else {
            search_end.saturating_add(overlap).min(file_len)
        };
        let read_len = usize::try_from(read_end.saturating_sub(chunk_start))
            .context("delta marker scan chunk does not fit usize")?;
        let mut buffer = vec![0u8; read_len];
        file.seek(SeekFrom::Start(chunk_start))?;
        file.read_exact(&mut buffer)?;
        if let Some(offset) = buffer
            .windows(DELTA_SEGMENT_MAGIC.len())
            .rposition(|window| window == DELTA_SEGMENT_MAGIC)
        {
            return Ok(chunk_start + offset as u64);
        }
        search_end = chunk_start;
    }

    bail!("delta segment marker not found")
}

fn open_no_auto_checkpoint(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..Default::default()
    }
    .path(path)
    .open()
    .map_err(db_error)
}

fn copy_checkpointed_base(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dst)?;
    file.sync_all()?;
    Ok(())
}

fn push_base_fact(command: &mut String, index: usize) {
    let entity = format!(":bench/base-{index}");
    if index.is_multiple_of(4) {
        let target = deterministic_uuid(index);
        command.push_str(&format!(r#"[{entity} :bench/ref #uuid "{target}"]"#));
    } else if index % 4 == 1 {
        command.push_str(&format!("[{entity} :bench/value {index}]"));
    } else if index % 4 == 2 {
        command.push_str(&format!("[{entity} :bench/state :bench/state-{index}]"));
    } else {
        command.push_str(&format!("[{entity} :bench/flag true]"));
    }
}

fn receipt_entity(
    scenario: AccumulationScenario,
    checkpoint_index: usize,
    fact_index: usize,
) -> String {
    format!(
        ":bench/receipt-{}-{}-{}",
        scenario.facts_per_checkpoint, checkpoint_index, fact_index
    )
}

fn current_query_for(entity: &str) -> String {
    format!("(query [:find ?v :where [{entity} :bench/ref ?v]])")
}

fn as_of_query_for(entity: &str, tx_count: u64) -> String {
    format!(
        "(query [:find ?v :as-of {tx_count} :valid-at :any-valid-time :where [{entity} :bench/ref ?v]])"
    )
}

fn delta_fact_ordinal(
    scenario: AccumulationScenario,
    checkpoint_index: usize,
    fact_index: usize,
) -> usize {
    scenario
        .facts_per_checkpoint
        .checked_mul(checkpoint_index)
        .and_then(|offset| offset.checked_add(fact_index))
        .and_then(|offset| offset.checked_add(1_000_000_000))
        .unwrap_or(usize::MAX)
}

fn deterministic_uuid(index: usize) -> Uuid {
    Uuid::from_u128(index as u128 + 1)
}

fn assert_query_count(db: &Minigraf, query: &str, expected: usize, label: &str) -> Result<()> {
    let count = query_count(db, query)?;
    if count != expected {
        bail!("{label}");
    }
    Ok(())
}

fn query_count(db: &Minigraf, query: &str) -> Result<usize> {
    match db.execute(query).map_err(db_error)? {
        QueryResult::QueryResults { results, .. } => Ok(results.len()),
        _ => bail!("expected query results"),
    }
}

fn duration_stats(samples: &[Duration]) -> Result<DurationStats> {
    if samples.is_empty() {
        bail!("duration sample set must not be empty");
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    Ok(DurationStats {
        samples: sorted.clone(),
        p50: percentile(&sorted, 50),
        p95: percentile(&sorted, 95),
        max: *sorted
            .last()
            .ok_or_else(|| anyhow::anyhow!("duration sample set must not be empty"))?,
    })
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    sorted
        .get(rank.saturating_sub(1).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}

fn probe_points(checkpoints: usize) -> Vec<usize> {
    let probe_count = checkpoints.min(MAX_QUERY_PROBES_PER_SCENARIO);
    if probe_count == 0 {
        return Vec::new();
    }
    if probe_count == 1 {
        return vec![checkpoints.saturating_sub(1)];
    }

    let mut points = Vec::with_capacity(probe_count);
    let last = checkpoints.saturating_sub(1);
    for sample_index in 0..probe_count {
        points.push(sample_index.saturating_mul(last) / probe_count.saturating_sub(1));
    }
    points.dedup();
    points
}

fn file_len(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

fn file_page_count(path: &Path) -> Result<u64> {
    Ok(file_len(path)?.div_ceil(PAGE_SIZE_BYTES))
}

fn db_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{}", error)
}

fn print_measurement(measurement: &AccumulationMeasurement) {
    println!(
        "{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{},{},{},{},{},{},{}",
        measurement.facts_per_checkpoint,
        measurement.checkpoints,
        measurement.accumulated_delta_facts,
        measurement.query_probe_count,
        ms(measurement.flush.p50),
        ms(measurement.flush.p95),
        ms(measurement.flush.max),
        ms(measurement.reopen.p50),
        ms(measurement.reopen.p95),
        ms(measurement.reopen.max),
        ms(measurement.current_query.p50),
        ms(measurement.current_query.p95),
        ms(measurement.current_query.max),
        ms(measurement.as_of_query.p50),
        ms(measurement.as_of_query.p95),
        ms(measurement.as_of_query.max),
        measurement.base_file_bytes,
        measurement.final_file_bytes,
        measurement
            .final_file_bytes
            .saturating_sub(measurement.base_file_bytes),
        measurement.base_pages,
        measurement.final_pages,
        measurement
            .final_pages
            .saturating_sub(measurement.base_pages),
        measurement.actual_delta_facts,
        measurement.segment_count,
        measurement.corrupt_latest_fallback
    );
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn assert_matrix_complete(
    measurements: &[AccumulationMeasurement],
    scenarios: &[AccumulationScenario],
) -> Result<()> {
    if measurements.len() != scenarios.len() {
        bail!("accumulation benchmark did not cover every scenario");
    }
    for scenario in scenarios {
        let found = measurements.iter().any(|measurement| {
            measurement.facts_per_checkpoint == scenario.facts_per_checkpoint
                && measurement.checkpoints == scenario.checkpoints
        });
        if !found {
            bail!("accumulation benchmark scenario is missing");
        }
    }
    Ok(())
}

fn write_receipt(
    started_at: SystemTime,
    config: AccumulationConfig,
    measurements: &[AccumulationMeasurement],
    base_fixture_sha256: &str,
    fixture_source: Option<&Path>,
) -> Result<()> {
    let mut metrics = BTreeMap::new();
    let mut files = Vec::new();
    let mut correctness_checks = vec![receipt::CorrectnessCheck::equal(
        "scenario matrix cardinality",
        json!(config.scenarios.len()),
        json!(measurements.len()),
    )];

    for measurement in measurements {
        let prefix = format!(
            "{}x{}",
            measurement.facts_per_checkpoint, measurement.checkpoints
        );
        for (name, stats) in [
            ("flush", &measurement.flush),
            ("reopen", &measurement.reopen),
            ("current_query", &measurement.current_query),
            ("as_of_query", &measurement.as_of_query),
        ] {
            metrics.insert(
                format!("{prefix}.{name}"),
                receipt::MetricSeries::from_durations(&stats.samples)?,
            );
        }
        let growth_bytes = measurement
            .final_file_bytes
            .saturating_sub(measurement.base_file_bytes);
        metrics.insert(
            format!("{prefix}.file_growth_mib"),
            receipt::MetricSeries::from_values(
                "MiB",
                vec![growth_bytes as f64 / (1024.0 * 1024.0)],
            )?,
        );
        files.push(json!({
            "scenario": prefix,
            "baseBytes": measurement.base_file_bytes,
            "finalBytes": measurement.final_file_bytes,
            "growthBytes": growth_bytes,
            "basePages": measurement.base_pages,
            "finalPages": measurement.final_pages,
            "segmentCount": measurement.segment_count
        }));
        correctness_checks.push(receipt::CorrectnessCheck::equal(
            &format!("{prefix} exported delta fact count"),
            json!(measurement.accumulated_delta_facts),
            json!(measurement.actual_delta_facts),
        ));
        correctness_checks.push(receipt::CorrectnessCheck::equal(
            &format!("{prefix} visible segment count"),
            json!(measurement.checkpoints),
            json!(measurement.segment_count),
        ));
        correctness_checks.push(receipt::CorrectnessCheck::equal(
            &format!("{prefix} corrupt-newest fallback"),
            json!(true),
            json!(measurement.corrupt_latest_fallback),
        ));
        correctness_checks.push(receipt::CorrectnessCheck::equal(
            &format!("{prefix} query probe count"),
            json!(measurement.query_probe_count),
            json!(measurement.current_query.samples.len()),
        ));
    }

    receipt::write_if_requested(receipt::ReceiptInput {
        suite: "delta-accumulation".to_owned(),
        profile: config.mode.to_owned(),
        started_at,
        configuration: json!({
            "mode": config.mode,
            "baseFacts": config.base_facts,
            "maxQueryProbesPerScenario": MAX_QUERY_PROBES_PER_SCENARIO,
            "scenarioCount": config.scenarios.len(),
            "coldWarmPolicy": "checkpointed-base-copy-per-scenario-reopen-at-probes",
            "fixtureOrigin": if fixture_source.is_some() { "provided" } else { "generated" },
            "fixtureSource": fixture_source.map(|path| path.display().to_string())
        }),
        metrics,
        files: json!({
            "baseFixtureSha256": base_fixture_sha256,
            "scenarios": files
        }),
        correctness_checks,
    })?;
    Ok(())
}
