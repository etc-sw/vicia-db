#![cfg(not(target_arch = "wasm32"))]

//! A0 Vetch cadence replay (docs/APP_ADOPTION_GAP_PLAN.md).
//!
//! Replays the interactive Vetch work unit on a checkpointed base:
//! capture (new card entity) → edit (retract + assert geometry) → receipt
//! batch (`:bench/ref` facts) → agent-brief reads (current + as-of point
//! queries) → per-slice checkpoint. Reports per-op p50/p95/max, checkpoint
//! stats, and file growth as CSV.
//!
//! Modes (argv[1] or MINIGRAF_VETCH_CADENCE_MODE):
//!   default  — 1M-fact base, 100 slices (local evidence run)
//!   smoke    — 10K-fact base, 20 slices (fast correctness pass)

use anyhow::{Result, bail};
use minigraf::{Minigraf, OpenOptions, QueryResult};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

#[path = "helpers/receipt.rs"]
mod receipt;

const BASE_BATCH_SIZE: usize = 1_000;
const PAGE_SIZE_BYTES: u64 = 4096;
const RECEIPT_FACTS_PER_SLICE: usize = 5;
const SMOKE_MODE: &str = "smoke";

#[derive(Clone, Copy)]
struct CadenceConfig {
    label: &'static str,
    base_facts: usize,
    slices: usize,
}

const FULL_CONFIG: CadenceConfig = CadenceConfig {
    label: "full",
    base_facts: 1_000_000,
    slices: 100,
};

const SMOKE_CONFIG: CadenceConfig = CadenceConfig {
    label: "smoke",
    base_facts: 10_000,
    slices: 20,
};

struct OpSamples {
    capture: Vec<Duration>,
    edit: Vec<Duration>,
    receipt: Vec<Duration>,
    brief_current: Vec<Duration>,
    brief_as_of: Vec<Duration>,
    checkpoint: Vec<Duration>,
}

#[derive(Clone, Copy)]
struct DurationStats {
    p50: Duration,
    p95: Duration,
    max: Duration,
}

fn main() -> Result<()> {
    let started_at = SystemTime::now();
    let config = selected_config();
    let root = tempfile::tempdir()?;
    let path = root.path().join("vetch-cadence.graph");

    let fixture_source = receipt::install_base_fixture_if_configured(&path)?;
    if fixture_source.is_some() {
        verify_base_fact_count(&path, config.base_facts)?;
    } else {
        build_checkpointed_base(&path, config.base_facts)?;
    }
    let base_file_bytes = file_len(&path)?;
    let base_fixture_sha256 = receipt::sha256_file(&path)?;

    let samples = run_cadence(&path, config)?;
    let final_file_bytes = file_len(&path)?;

    println!(
        "mode,base_facts,slices,capture_p50_ms,capture_p95_ms,capture_max_ms,edit_p50_ms,edit_p95_ms,edit_max_ms,receipt_p50_ms,receipt_p95_ms,receipt_max_ms,brief_current_p50_ms,brief_current_p95_ms,brief_current_max_ms,brief_as_of_p50_ms,brief_as_of_p95_ms,brief_as_of_max_ms,checkpoint_p50_ms,checkpoint_p95_ms,checkpoint_max_ms,base_file_bytes,final_file_bytes,file_growth_bytes,file_growth_pages"
    );
    print_row(config, &samples, base_file_bytes, final_file_bytes)?;
    write_receipt(
        started_at,
        config,
        &samples,
        base_file_bytes,
        final_file_bytes,
        &base_fixture_sha256,
        fixture_source.as_deref(),
    )?;
    Ok(())
}

fn selected_config() -> CadenceConfig {
    let arg_mode = std::env::args().nth(1);
    let env_mode = std::env::var("MINIGRAF_VETCH_CADENCE_MODE").ok();
    if arg_mode.as_deref() == Some(SMOKE_MODE) || env_mode.as_deref() == Some(SMOKE_MODE) {
        SMOKE_CONFIG
    } else {
        FULL_CONFIG
    }
}

fn run_cadence(path: &Path, config: CadenceConfig) -> Result<OpSamples> {
    let mut samples = OpSamples {
        capture: Vec::with_capacity(config.slices),
        edit: Vec::with_capacity(config.slices),
        receipt: Vec::with_capacity(config.slices),
        brief_current: Vec::with_capacity(config.slices),
        brief_as_of: Vec::with_capacity(config.slices),
        checkpoint: Vec::with_capacity(config.slices),
    };

    let db = open_no_auto_checkpoint(path)?;
    for slice in 0..config.slices {
        // capture: one new card, 4 facts in one transact
        let space = deterministic_uuid(slice);
        let (x0, y0) = (slice as f64 * 10.0 + 0.5, slice as f64 * 20.0 + 0.5);
        let started = Instant::now();
        db.execute(&format!(
            r#"(transact [[:card-{slice} :card/title "card {slice}"] [:card-{slice} :card/x {x0:.1}] [:card-{slice} :card/y {y0:.1}] [:card-{slice} :card/space #uuid "{space}"]])"#
        ))
        .map_err(db_error)?;
        samples.capture.push(started.elapsed());

        // edit: move the card just captured — retract old geometry, assert new
        let pre_edit_tx = db.current_tx_count();
        let (x1, y1) = (x0 + 100.0, y0 + 100.0);
        let started = Instant::now();
        db.execute(&format!(
            "(retract [[:card-{slice} :card/x {x0:.1}] [:card-{slice} :card/y {y0:.1}]])"
        ))
        .map_err(db_error)?;
        db.execute(&format!(
            "(transact [[:card-{slice} :card/x {x1:.1}] [:card-{slice} :card/y {y1:.1}]])"
        ))
        .map_err(db_error)?;
        samples.edit.push(started.elapsed());

        // receipt batch: RECEIPT_FACTS_PER_SLICE ref facts in one transact
        let started = Instant::now();
        let mut command = String::from("(transact [");
        for fact_index in 0..RECEIPT_FACTS_PER_SLICE {
            let target =
                deterministic_uuid(1_000_000_000 + slice * RECEIPT_FACTS_PER_SLICE + fact_index);
            command.push_str(&format!(
                r#"[:receipt-{slice}-{fact_index} :bench/ref #uuid "{target}"]"#
            ));
        }
        command.push_str("])");
        db.execute(&command).map_err(db_error)?;
        samples.receipt.push(started.elapsed());

        // brief reads: current geometry, then pre-edit geometry via as-of
        let started = Instant::now();
        assert_query_count(
            &db,
            &format!("(query [:find ?x :where [:card-{slice} :card/x ?x]])"),
            1,
            "current brief read should see exactly the post-edit x",
        )?;
        samples.brief_current.push(started.elapsed());

        let started = Instant::now();
        assert_query_count(
            &db,
            &format!(
                "(query [:find ?x :as-of {pre_edit_tx} :valid-at :any-valid-time :where [:card-{slice} :card/x ?x]])"
            ),
            1,
            "as-of brief read should see exactly the pre-edit x",
        )?;
        samples.brief_as_of.push(started.elapsed());

        // per-slice checkpoint (Vetch receipt/slice batching policy)
        let started = Instant::now();
        db.checkpoint().map_err(db_error)?;
        samples.checkpoint.push(started.elapsed());
    }

    Ok(samples)
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

fn deterministic_uuid(index: usize) -> Uuid {
    Uuid::from_u128(index as u128 + 1)
}

fn assert_query_count(db: &Minigraf, query: &str, expected: usize, label: &str) -> Result<()> {
    match db.execute(query).map_err(db_error)? {
        QueryResult::QueryResults { results, .. } => {
            if results.len() != expected {
                bail!("{label}");
            }
            Ok(())
        }
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

fn file_len(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

fn db_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{}", error)
}

fn print_row(
    config: CadenceConfig,
    samples: &OpSamples,
    base_file_bytes: u64,
    final_file_bytes: u64,
) -> Result<()> {
    let capture = duration_stats(&samples.capture)?;
    let edit = duration_stats(&samples.edit)?;
    let receipt = duration_stats(&samples.receipt)?;
    let brief_current = duration_stats(&samples.brief_current)?;
    let brief_as_of = duration_stats(&samples.brief_as_of)?;
    let checkpoint = duration_stats(&samples.checkpoint)?;
    let growth = final_file_bytes.saturating_sub(base_file_bytes);
    println!(
        "{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{},{}",
        config.label,
        config.base_facts,
        config.slices,
        ms(capture.p50),
        ms(capture.p95),
        ms(capture.max),
        ms(edit.p50),
        ms(edit.p95),
        ms(edit.max),
        ms(receipt.p50),
        ms(receipt.p95),
        ms(receipt.max),
        ms(brief_current.p50),
        ms(brief_current.p95),
        ms(brief_current.max),
        ms(brief_as_of.p50),
        ms(brief_as_of.p95),
        ms(brief_as_of.max),
        ms(checkpoint.p50),
        ms(checkpoint.p95),
        ms(checkpoint.max),
        base_file_bytes,
        final_file_bytes,
        growth,
        growth.div_ceil(PAGE_SIZE_BYTES)
    );
    Ok(())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn write_receipt(
    started_at: SystemTime,
    config: CadenceConfig,
    samples: &OpSamples,
    base_file_bytes: u64,
    final_file_bytes: u64,
    base_fixture_sha256: &str,
    fixture_source: Option<&Path>,
) -> Result<()> {
    let metrics = [
        ("capture", &samples.capture),
        ("edit", &samples.edit),
        ("receipt", &samples.receipt),
        ("brief_current", &samples.brief_current),
        ("brief_as_of", &samples.brief_as_of),
        ("checkpoint", &samples.checkpoint),
    ]
    .into_iter()
    .map(|(name, values)| {
        Ok((
            name.to_owned(),
            receipt::MetricSeries::from_durations(values)?,
        ))
    })
    .collect::<Result<BTreeMap<_, _>>>()?;
    let mut metrics = metrics;
    metrics.insert(
        "file_growth_mib".to_owned(),
        receipt::MetricSeries::from_values(
            "MiB",
            vec![final_file_bytes.saturating_sub(base_file_bytes) as f64 / (1024.0 * 1024.0)],
        )?,
    );

    receipt::write_if_requested(receipt::ReceiptInput {
        suite: "vetch-cadence".to_owned(),
        profile: config.label.to_owned(),
        started_at,
        configuration: json!({
            "mode": config.label,
            "baseFacts": config.base_facts,
            "slices": config.slices,
            "receiptFactsPerSlice": RECEIPT_FACTS_PER_SLICE,
            "coldWarmPolicy": "checkpointed-base-single-process-cadence",
            "fixtureOrigin": if fixture_source.is_some() { "provided" } else { "generated" },
            "fixtureSource": fixture_source.map(|path| path.display().to_string())
        }),
        metrics,
        files: json!({
            "baseFixtureSha256": base_fixture_sha256,
            "baseBytes": base_file_bytes,
            "finalBytes": final_file_bytes,
            "growthBytes": final_file_bytes.saturating_sub(base_file_bytes),
            "growthPages": final_file_bytes
                .saturating_sub(base_file_bytes)
                .div_ceil(PAGE_SIZE_BYTES)
        }),
        correctness_checks: vec![
            receipt::CorrectnessCheck::equal(
                "current brief read cardinality",
                json!(config.slices),
                json!(samples.brief_current.len()),
            ),
            receipt::CorrectnessCheck::equal(
                "historical brief read cardinality",
                json!(config.slices),
                json!(samples.brief_as_of.len()),
            ),
            receipt::CorrectnessCheck::equal(
                "checkpoint sample cardinality",
                json!(config.slices),
                json!(samples.checkpoint.len()),
            ),
        ],
    })?;
    Ok(())
}
