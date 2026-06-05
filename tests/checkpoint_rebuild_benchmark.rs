#![cfg(not(target_arch = "wasm32"))]

use anyhow::Result;
use minigraf::{Minigraf, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

const COMMITTED_FACT_COUNTS: &[usize] = &[10_000, 100_000, 1_000_000];
const PENDING_FACT_COUNTS: &[usize] = &[1, 10, 100, 1_000];
const BATCH_SIZE: usize = 1_000;
const PAGE_SIZE_BYTES: u64 = 4096;

fn db_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{}", error)
}

#[derive(Debug)]
struct CheckpointMeasurement {
    committed_facts: usize,
    pending_facts: usize,
    pending_assertions: usize,
    pending_retractions: usize,
    delta_flush: Duration,
    reopen_delta: Duration,
    second_delta_flush: Duration,
    base_file_bytes: u64,
    delta_file_bytes: u64,
    second_delta_file_bytes: u64,
    base_pages: u64,
    delta_pages: u64,
    second_delta_pages: u64,
    wal_bytes_before_delta_flush: u64,
    wal_bytes_before_second_delta_flush: u64,
}

#[test]
#[ignore = "benchmark fixture; run with --ignored --nocapture"]
fn delta_checkpoint_cost_after_small_pending_writes() -> Result<()> {
    let root = tempfile::tempdir()?;
    println!(
        "committed_facts,pending_facts,pending_assertions,pending_retractions,delta_flush_ms,reopen_delta_ms,second_delta_flush_ms,base_file_bytes,delta_file_bytes,second_delta_file_bytes,base_pages,delta_pages,second_delta_pages,wal_bytes_before_delta_flush,wal_bytes_before_second_delta_flush"
    );

    let mut measurements = Vec::new();
    for &committed_facts in COMMITTED_FACT_COUNTS {
        let base_path = root.path().join(format!("base-{committed_facts}.graph"));
        build_checkpointed_base(&base_path, committed_facts)?;
        let base_file_bytes = file_len(&base_path)?;
        let base_pages = file_page_count(&base_path)?;

        for &pending_facts in PENDING_FACT_COUNTS {
            let run_path = root
                .path()
                .join(format!("run-{committed_facts}-{pending_facts}.graph"));
            copy_checkpointed_base(&base_path, &run_path)?;

            let db = open_no_auto_checkpoint(&run_path)?;
            let (pending_assertions, pending_retractions) =
                add_pending_fact_mix(&db, committed_facts, pending_facts)?;
            let wal_bytes_before_delta_flush = file_len_optional(&wal_path_for(&run_path))?;

            let started = Instant::now();
            db.checkpoint().map_err(db_error)?;
            let delta_flush = started.elapsed();
            let delta_file_bytes = file_len(&run_path)?;
            let delta_pages = file_page_count(&run_path)?;
            drop(db);

            let started = Instant::now();
            let db = open_no_auto_checkpoint(&run_path)?;
            let reopen_delta = started.elapsed();

            add_second_delta_fact(&db, committed_facts, pending_facts)?;
            let wal_bytes_before_second_delta_flush = file_len_optional(&wal_path_for(&run_path))?;
            let started = Instant::now();
            db.checkpoint().map_err(db_error)?;
            let second_delta_flush = started.elapsed();
            let second_delta_file_bytes = file_len(&run_path)?;
            let second_delta_pages = file_page_count(&run_path)?;
            drop(db);

            let measurement = CheckpointMeasurement {
                committed_facts,
                pending_facts,
                pending_assertions,
                pending_retractions,
                delta_flush,
                reopen_delta,
                second_delta_flush,
                base_file_bytes,
                delta_file_bytes,
                second_delta_file_bytes,
                base_pages,
                delta_pages,
                second_delta_pages,
                wal_bytes_before_delta_flush,
                wal_bytes_before_second_delta_flush,
            };
            print_measurement(&measurement);
            measurements.push(measurement);
        }
    }

    assert!(
        measurements.iter().any(|m| m.committed_facts == 1_000_000),
        "benchmark matrix should include 1M committed facts"
    );
    assert_eq!(
        measurements.len(),
        COMMITTED_FACT_COUNTS.len() * PENDING_FACT_COUNTS.len(),
        "benchmark matrix should cover every committed x pending combination"
    );
    assert!(
        measurements
            .iter()
            .any(|m| m.committed_facts == 1_000_000 && m.pending_facts == 1),
        "benchmark matrix should include the 1M base plus one pending fact gate"
    );

    Ok(())
}

fn build_checkpointed_base(path: &Path, fact_count: usize) -> Result<()> {
    let db = open_no_auto_checkpoint(path)?;
    insert_committed_mix(&db, fact_count)?;
    db.checkpoint().map_err(db_error)?;
    drop(db);
    Ok(())
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

fn open_no_auto_checkpoint(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..Default::default()
    }
    .path(path)
    .open()
    .map_err(db_error)
}

fn insert_committed_mix(db: &Minigraf, fact_count: usize) -> Result<()> {
    for batch_start in (0..fact_count).step_by(BATCH_SIZE) {
        let batch_end = (batch_start + BATCH_SIZE).min(fact_count);
        let mut command = String::from("(transact [");
        for i in batch_start..batch_end {
            push_fact(&mut command, i, "bench/e", false);
        }
        command.push_str("])");
        db.execute(&command).map_err(db_error)?;
    }
    Ok(())
}

fn add_pending_fact_mix(
    db: &Minigraf,
    committed_facts: usize,
    pending_facts: usize,
) -> Result<(usize, usize)> {
    let pending_retractions = pending_facts.saturating_div(4).min(committed_facts);
    let pending_assertions = pending_facts.saturating_sub(pending_retractions);

    let mut tx = db.begin_write().map_err(db_error)?;

    if pending_retractions > 0 {
        let mut command = String::from("(retract [");
        for i in 0..pending_retractions {
            push_fact(&mut command, i, "bench/e", false);
        }
        command.push_str("])");
        tx.execute(&command).map_err(db_error)?;
    }

    if pending_assertions > 0 {
        let mut command = String::from("(transact [");
        let start = committed_facts;
        for i in start..start + pending_assertions {
            push_fact(&mut command, i, "bench/pending", true);
        }
        command.push_str("])");
        tx.execute(&command).map_err(db_error)?;
    }

    tx.commit().map_err(db_error)?;
    Ok((pending_assertions, pending_retractions))
}

fn add_second_delta_fact(
    db: &Minigraf,
    committed_facts: usize,
    pending_facts: usize,
) -> Result<()> {
    let index = committed_facts
        .checked_add(pending_facts)
        .and_then(|n| n.checked_add(1_000_000_000))
        .ok_or_else(|| anyhow::anyhow!("second delta fact index overflow"))?;
    let mut command = String::from("(transact [");
    push_fact(&mut command, index, "bench/second-delta", true);
    command.push_str("])");
    db.execute(&command).map_err(db_error)?;
    Ok(())
}

fn push_fact(command: &mut String, index: usize, entity_prefix: &str, force_ref: bool) {
    let entity = format!(":{entity_prefix}{index}");
    if force_ref || index.is_multiple_of(4) {
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

fn deterministic_uuid(index: usize) -> Uuid {
    Uuid::from_u128(index as u128 + 1)
}

fn wal_path_for(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    let name = wal
        .file_name()
        .map(|file_name| {
            let mut name = file_name.to_os_string();
            name.push(".wal");
            name
        })
        .unwrap_or_else(|| std::ffi::OsString::from("db.graph.wal"));
    wal.set_file_name(name);
    wal
}

fn file_len(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

fn file_len_optional(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0))
}

fn file_page_count(path: &Path) -> Result<u64> {
    Ok(file_len(path)?.div_ceil(PAGE_SIZE_BYTES))
}

fn print_measurement(m: &CheckpointMeasurement) {
    println!(
        "{},{},{},{},{:.3},{:.3},{:.3},{},{},{},{},{},{},{},{}",
        m.committed_facts,
        m.pending_facts,
        m.pending_assertions,
        m.pending_retractions,
        m.delta_flush.as_secs_f64() * 1000.0,
        m.reopen_delta.as_secs_f64() * 1000.0,
        m.second_delta_flush.as_secs_f64() * 1000.0,
        m.base_file_bytes,
        m.delta_file_bytes,
        m.second_delta_file_bytes,
        m.base_pages,
        m.delta_pages,
        m.second_delta_pages,
        m.wal_bytes_before_delta_flush,
        m.wal_bytes_before_second_delta_flush
    );
}
