#![cfg(not(target_arch = "wasm32"))]

use anyhow::{Result, bail};
use minigraf::{BindValue, Minigraf, OpenOptions, QueryResult};
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

const FULL_BASE_FACTS: usize = 1_000_000;
const SMOKE_BASE_FACTS: usize = 10_000;
const BASE_BATCH_SIZE: usize = 1_000;
const PAGE_SIZE_BYTES: u64 = 4096;
const MAX_FULL_PROBES_PER_SCENARIO: usize = 32;
const MAX_SMOKE_PROBES_PER_SCENARIO: usize = 5;
const SMOKE_MODE: &str = "smoke";
const FULL_SCENARIOS: &[BriefScenario] = &[
    BriefScenario {
        label: "single_receipt",
        facts_per_checkpoint: 1,
        checkpoints: 1,
    },
    BriefScenario {
        label: "receipt_stream_100",
        facts_per_checkpoint: 1,
        checkpoints: 100,
    },
    BriefScenario {
        label: "batched_receipts_1000",
        facts_per_checkpoint: 10,
        checkpoints: 100,
    },
];
const SMOKE_SCENARIOS: &[BriefScenario] = &[
    BriefScenario {
        label: "smoke_single_receipt",
        facts_per_checkpoint: 1,
        checkpoints: 1,
    },
    BriefScenario {
        label: "smoke_receipt_stream_10",
        facts_per_checkpoint: 1,
        checkpoints: 10,
    },
];

#[derive(Clone, Copy, Debug)]
struct BriefScenario {
    label: &'static str,
    facts_per_checkpoint: usize,
    checkpoints: usize,
}

#[derive(Clone, Copy, Debug)]
struct BenchConfig {
    mode: &'static str,
    base_facts: usize,
    max_probes: usize,
    scenarios: &'static [BriefScenario],
}

#[derive(Debug)]
struct BriefMeasurement {
    mode: &'static str,
    scenario: BriefScenario,
    base_facts: usize,
    probe_count: usize,
    current_query: DurationStats,
    as_of_query: DurationStats,
    prepared_as_of_query: DurationStats,
    export_recent_filter: DurationStats,
    base_file_bytes: u64,
    final_file_bytes: u64,
    base_pages: u64,
    final_pages: u64,
    base_tx_count: u64,
    final_tx_count: u64,
}

#[derive(Clone, Copy, Debug)]
struct DurationStats {
    p50: Duration,
    p95: Duration,
    max: Duration,
}

fn main() -> Result<()> {
    let config = selected_config();
    let root = tempfile::tempdir()?;
    let base_path = root
        .path()
        .join(format!("agent-brief-base-{}.graph", config.mode));
    let base_tx_count = build_checkpointed_base(&base_path, config.base_facts)?;
    let base_file_bytes = file_len(&base_path)?;
    let base_pages = file_page_count(&base_path)?;

    println!(
        "mode,scenario,base_facts,facts_per_checkpoint,checkpoints,delta_facts,probe_count,current_query_p50_ms,current_query_p95_ms,current_query_max_ms,as_of_query_p50_ms,as_of_query_p95_ms,as_of_query_max_ms,prepared_as_of_query_p50_ms,prepared_as_of_query_p95_ms,prepared_as_of_query_max_ms,export_recent_filter_p50_ms,export_recent_filter_p95_ms,export_recent_filter_max_ms,base_file_bytes,final_file_bytes,file_growth_bytes,base_pages,final_pages,page_growth,base_tx_count,final_tx_count"
    );

    for &scenario in config.scenarios {
        let run_path = root.path().join(format!(
            "agent-brief-{}-{}.graph",
            config.mode, scenario.label
        ));
        copy_checkpointed_base(&base_path, &run_path)?;
        let measurement = measure_brief_reads(
            &run_path,
            config,
            scenario,
            base_tx_count,
            base_file_bytes,
            base_pages,
        )?;
        print_measurement(&measurement);
    }

    Ok(())
}

fn selected_config() -> BenchConfig {
    let arg_mode = std::env::args().nth(1);
    let env_mode = std::env::var("MINIGRAF_AGENT_BRIEF_BENCH_MODE").ok();
    if arg_mode.as_deref() == Some(SMOKE_MODE) || env_mode.as_deref() == Some(SMOKE_MODE) {
        BenchConfig {
            mode: SMOKE_MODE,
            base_facts: SMOKE_BASE_FACTS,
            max_probes: MAX_SMOKE_PROBES_PER_SCENARIO,
            scenarios: SMOKE_SCENARIOS,
        }
    } else {
        BenchConfig {
            mode: "full",
            base_facts: FULL_BASE_FACTS,
            max_probes: MAX_FULL_PROBES_PER_SCENARIO,
            scenarios: FULL_SCENARIOS,
        }
    }
}

fn measure_brief_reads(
    path: &Path,
    config: BenchConfig,
    scenario: BriefScenario,
    base_tx_count: u64,
    base_file_bytes: u64,
    base_pages: u64,
) -> Result<BriefMeasurement> {
    let mut current_query = Vec::new();
    let mut as_of_query = Vec::new();
    let mut prepared_as_of_query = Vec::new();
    let mut export_recent_filter = Vec::new();
    let probe_points = probe_points(scenario.checkpoints, config.max_probes);
    let mut probe_cursor = 0usize;

    let db = open_no_auto_checkpoint(path)?;
    let prepared = db
        .prepare("(query [:find ?v :as-of $tx :valid-at $valid_at :where [$entity :bench/ref ?v]])")
        .map_err(db_error)?;

    for checkpoint_index in 0..scenario.checkpoints {
        add_receipt_batch(&db, scenario, checkpoint_index)?;
        let tx_count = db.current_tx_count();
        db.checkpoint().map_err(db_error)?;

        let should_probe = probe_points
            .get(probe_cursor)
            .is_some_and(|probe| *probe == checkpoint_index);
        if !should_probe {
            continue;
        }

        let entity = receipt_entity(
            scenario,
            checkpoint_index,
            scenario.facts_per_checkpoint.saturating_sub(1),
        );

        let started = Instant::now();
        assert_query_count(
            &db,
            &current_query_for(entity),
            1,
            "current brief point query should see the latest receipt",
        )?;
        current_query.push(started.elapsed());

        let started = Instant::now();
        assert_query_count(
            &db,
            &as_of_query_for(entity, tx_count),
            1,
            "as-of brief point query should see the latest receipt",
        )?;
        as_of_query.push(started.elapsed());

        let started = Instant::now();
        assert_query_result_count(
            prepared
                .execute(&[
                    ("entity", BindValue::Entity(entity)),
                    ("tx", BindValue::TxCount(tx_count)),
                    ("valid_at", BindValue::AnyValidTime),
                ])
                .map_err(db_error)?,
            1,
            "prepared as-of brief point query should see the latest receipt",
        )?;
        prepared_as_of_query.push(started.elapsed());

        let started = Instant::now();
        let records = db.export_fact_log().map_err(db_error)?;
        let recent_ref_count = records
            .iter()
            .filter(|record| record.tx_count == tx_count && record.attribute == ":bench/ref")
            .count();
        if recent_ref_count != scenario.facts_per_checkpoint {
            bail!("recent export filter should return the receipt batch");
        }
        export_recent_filter.push(started.elapsed());

        probe_cursor = probe_cursor.saturating_add(1);
    }

    let final_tx_count = db.current_tx_count();
    drop(db);

    let final_file_bytes = file_len(path)?;
    let final_pages = file_page_count(path)?;

    Ok(BriefMeasurement {
        mode: config.mode,
        scenario,
        base_facts: config.base_facts,
        probe_count: probe_points.len(),
        current_query: duration_stats(&current_query)?,
        as_of_query: duration_stats(&as_of_query)?,
        prepared_as_of_query: duration_stats(&prepared_as_of_query)?,
        export_recent_filter: duration_stats(&export_recent_filter)?,
        base_file_bytes,
        final_file_bytes,
        base_pages,
        final_pages,
        base_tx_count,
        final_tx_count,
    })
}

fn build_checkpointed_base(path: &Path, base_facts: usize) -> Result<u64> {
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
    let tx_count = db.current_tx_count();
    drop(db);
    Ok(tx_count)
}

fn add_receipt_batch(
    db: &Minigraf,
    scenario: BriefScenario,
    checkpoint_index: usize,
) -> Result<()> {
    let mut command = String::from("(transact [");
    for fact_index in 0..scenario.facts_per_checkpoint {
        let entity = receipt_entity(scenario, checkpoint_index, fact_index);
        let target = deterministic_uuid(delta_fact_ordinal(scenario, checkpoint_index, fact_index));
        command.push_str(&format!(
            r#"[#uuid "{entity}" :bench/ref #uuid "{target}"]"#
        ));
    }
    command.push_str("])");
    db.execute(&command).map_err(db_error)?;
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

fn receipt_entity(scenario: BriefScenario, checkpoint_index: usize, fact_index: usize) -> Uuid {
    deterministic_uuid(
        scenario
            .facts_per_checkpoint
            .checked_mul(checkpoint_index)
            .and_then(|offset| offset.checked_add(fact_index))
            .and_then(|offset| offset.checked_add(2_000_000_000))
            .unwrap_or(usize::MAX),
    )
}

fn delta_fact_ordinal(
    scenario: BriefScenario,
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

fn current_query_for(entity: Uuid) -> String {
    format!(r#"(query [:find ?v :where [#uuid "{entity}" :bench/ref ?v]])"#)
}

fn as_of_query_for(entity: Uuid, tx_count: u64) -> String {
    format!(
        r#"(query [:find ?v :as-of {tx_count} :valid-at :any-valid-time :where [#uuid "{entity}" :bench/ref ?v]])"#
    )
}

fn assert_query_count(db: &Minigraf, query: &str, expected: usize, label: &str) -> Result<()> {
    assert_query_result_count(db.execute(query).map_err(db_error)?, expected, label)
}

fn assert_query_result_count(result: QueryResult, expected: usize, label: &str) -> Result<()> {
    let count = match result {
        QueryResult::QueryResults { results, .. } => results.len(),
        _ => bail!("expected query results"),
    };
    if count != expected {
        bail!("{label}");
    }
    Ok(())
}

fn duration_stats(samples: &[Duration]) -> Result<DurationStats> {
    if samples.is_empty() {
        bail!("duration sample set must not be empty");
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    Ok(DurationStats {
        p50: percentile(&sorted, 50),
        p95: percentile(&sorted, 95),
        max: *sorted
            .last()
            .ok_or_else(|| anyhow::anyhow!("duration sample set must not be empty"))?,
    })
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    let index = rank.saturating_sub(1).min(sorted.len().saturating_sub(1));
    if let Some(duration) = sorted.get(index) {
        *duration
    } else {
        Duration::ZERO
    }
}

fn probe_points(checkpoints: usize, max_probes: usize) -> Vec<usize> {
    let probe_count = checkpoints.min(max_probes);
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

fn print_measurement(measurement: &BriefMeasurement) {
    println!(
        "{},{},{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{},{},{},{},{},{}",
        measurement.mode,
        measurement.scenario.label,
        measurement.base_facts,
        measurement.scenario.facts_per_checkpoint,
        measurement.scenario.checkpoints,
        measurement
            .scenario
            .facts_per_checkpoint
            .saturating_mul(measurement.scenario.checkpoints),
        measurement.probe_count,
        ms(measurement.current_query.p50),
        ms(measurement.current_query.p95),
        ms(measurement.current_query.max),
        ms(measurement.as_of_query.p50),
        ms(measurement.as_of_query.p95),
        ms(measurement.as_of_query.max),
        ms(measurement.prepared_as_of_query.p50),
        ms(measurement.prepared_as_of_query.p95),
        ms(measurement.prepared_as_of_query.max),
        ms(measurement.export_recent_filter.p50),
        ms(measurement.export_recent_filter.p95),
        ms(measurement.export_recent_filter.max),
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
        measurement.base_tx_count,
        measurement.final_tx_count
    );
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
