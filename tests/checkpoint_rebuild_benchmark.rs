#![cfg(not(target_arch = "wasm32"))]

use anyhow::Result;
use minigraf::{Minigraf, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

const COMMITTED_FACT_COUNTS: &[usize] = &[10_000, 100_000, 1_000_000];
const PENDING_FACT_COUNTS: &[usize] = &[1, 10, 100, 1_000];
const BATCH_SIZE: usize = 1_000;

#[derive(Debug)]
struct CheckpointMeasurement {
    committed_facts: usize,
    pending_facts: usize,
    pending_assertions: usize,
    pending_retractions: usize,
    checkpoint: Duration,
    base_file_bytes: u64,
    post_checkpoint_file_bytes: u64,
    wal_bytes_before_checkpoint: u64,
}

#[test]
#[ignore = "benchmark fixture; run with --ignored --nocapture"]
fn checkpoint_rebuild_cost_after_small_pending_writes() -> Result<()> {
    let root = tempfile::tempdir()?;
    println!(
        "committed_facts,pending_facts,pending_assertions,pending_retractions,checkpoint_ms,base_file_bytes,post_checkpoint_file_bytes,wal_bytes_before_checkpoint"
    );

    let mut measurements = Vec::new();
    for &committed_facts in COMMITTED_FACT_COUNTS {
        let base_path = root.path().join(format!("base-{committed_facts}.graph"));
        build_checkpointed_base(&base_path, committed_facts)?;
        let base_file_bytes = file_len(&base_path)?;

        for &pending_facts in PENDING_FACT_COUNTS {
            let run_path = root
                .path()
                .join(format!("run-{committed_facts}-{pending_facts}.graph"));
            std::fs::copy(&base_path, &run_path)?;

            let db = open_no_auto_checkpoint(&run_path)?;
            let (pending_assertions, pending_retractions) =
                add_pending_fact_mix(&db, committed_facts, pending_facts)?;
            let wal_bytes_before_checkpoint = file_len_optional(&wal_path_for(&run_path))?;

            let started = Instant::now();
            db.checkpoint()?;
            let checkpoint = started.elapsed();
            let post_checkpoint_file_bytes = file_len(&run_path)?;
            drop(db);

            let measurement = CheckpointMeasurement {
                committed_facts,
                pending_facts,
                pending_assertions,
                pending_retractions,
                checkpoint,
                base_file_bytes,
                post_checkpoint_file_bytes,
                wal_bytes_before_checkpoint,
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

    Ok(())
}

fn build_checkpointed_base(path: &Path, fact_count: usize) -> Result<()> {
    let db = open_no_auto_checkpoint(path)?;
    insert_committed_mix(&db, fact_count)?;
    db.checkpoint()?;
    drop(db);
    Ok(())
}

fn open_no_auto_checkpoint(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..Default::default()
    }
    .path(path)
    .open()
}

fn insert_committed_mix(db: &Minigraf, fact_count: usize) -> Result<()> {
    for batch_start in (0..fact_count).step_by(BATCH_SIZE) {
        let batch_end = (batch_start + BATCH_SIZE).min(fact_count);
        let mut command = String::from("(transact [");
        for i in batch_start..batch_end {
            push_fact(&mut command, i, "bench/e", false);
        }
        command.push_str("])");
        db.execute(&command)?;
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

    let mut tx = db.begin_write()?;

    if pending_retractions > 0 {
        let mut command = String::from("(retract [");
        for i in 0..pending_retractions {
            push_fact(&mut command, i, "bench/e", false);
        }
        command.push_str("])");
        tx.execute(&command)?;
    }

    if pending_assertions > 0 {
        let mut command = String::from("(transact [");
        let start = committed_facts;
        for i in start..start + pending_assertions {
            push_fact(&mut command, i, "bench/pending", true);
        }
        command.push_str("])");
        tx.execute(&command)?;
    }

    tx.commit()?;
    Ok((pending_assertions, pending_retractions))
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

fn print_measurement(m: &CheckpointMeasurement) {
    println!(
        "{},{},{},{},{:.3},{},{},{}",
        m.committed_facts,
        m.pending_facts,
        m.pending_assertions,
        m.pending_retractions,
        m.checkpoint.as_secs_f64() * 1000.0,
        m.base_file_bytes,
        m.post_checkpoint_file_bytes,
        m.wal_bytes_before_checkpoint
    );
}
