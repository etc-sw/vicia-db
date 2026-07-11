//! A2 gate benchmark: `export_fact_log_since` at a 1M-fact committed base.
//!
//! Gate (docs/APP_ADOPTION_GAP_PLAN.md A2): a since-tail of ≤100 records
//! returns without a committed full scan; latency recorded in BENCHMARKS.md.
//! The structural no-`stream_all` proof lives in the unit and integration
//! tests; this fixture produces the numeric evidence at gate scale.
//!
//! Run with: `cargo test --release --test fact_log_since_benchmark -- --ignored --nocapture`

#![cfg(not(target_arch = "wasm32"))]

use anyhow::Result;
use minigraf::{Minigraf, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

const BASE_FACTS: usize = 1_000_000;
const BATCH_SIZE: usize = 1_000;
/// Trailing single-fact transactions inside the committed base, so the
/// base-tail scenario has a ≤100-record tail addressable by tx_count cursor.
const BASE_TAIL_TXS: usize = 100;
const PENDING_TAIL_TXS: usize = 50;

fn db_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{}", error)
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

fn insert_batch(db: &Minigraf, start: usize, end: usize) -> Result<()> {
    let mut command = String::from("(transact [");
    for i in start..end {
        command.push_str(&format!("[:bench/e{i} :bench/value {i}]"));
    }
    command.push_str("])");
    db.execute(&command).map_err(db_error)?;
    Ok(())
}

fn timed_since_tail(db: &Minigraf, since: u64) -> Result<(Duration, usize)> {
    let start = Instant::now();
    let tail = db.export_fact_log_since(since).map_err(db_error)?;
    Ok((start.elapsed(), tail.len()))
}

#[test]
#[ignore = "benchmark fixture; run with --release --ignored --nocapture"]
fn gate_since_tail_at_1m_base() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fact-log-since-1m.graph");

    // Build the base: 1M - 100 facts in 1000-fact batches, then 100
    // single-fact transactions so the base tail is cursor-addressable.
    let bulk = BASE_FACTS - BASE_TAIL_TXS;
    {
        let db = open_no_auto_checkpoint(&path)?;
        for batch_start in (0..bulk).step_by(BATCH_SIZE) {
            insert_batch(&db, batch_start, (batch_start + BATCH_SIZE).min(bulk))?;
        }
        for i in bulk..BASE_FACTS {
            insert_batch(&db, i, i + 1)?;
        }
        let checkpoint_start = Instant::now();
        db.checkpoint().map_err(db_error)?;
        println!(
            "setup: checkpoint of {BASE_FACTS} facts took {:?}",
            checkpoint_start.elapsed()
        );
    }

    // Reopen cold so the page cache starts empty.
    let db = open_no_auto_checkpoint(&path)?;
    let head = db.current_tx_count();
    let base_tail_cursor = head - BASE_TAIL_TXS as u64;

    // Scenario A — ≤100-record tail that lives inside the 1M committed base
    // (the post-recompact daemon-tick shape): served by the tx-ordered page
    // probe, no full scan.
    let (base_tail_cold, base_tail_len) = timed_since_tail(&db, base_tail_cursor)?;
    assert_eq!(
        base_tail_len, BASE_TAIL_TXS,
        "base tail must be 100 records"
    );
    let (base_tail_warm, _) = timed_since_tail(&db, base_tail_cursor)?;

    // Scenario B — tail in the pending (uncheckpointed) layer.
    for i in 0..PENDING_TAIL_TXS {
        insert_batch(&db, BASE_FACTS + i, BASE_FACTS + i + 1)?;
    }
    let (pending_tail, pending_len) = timed_since_tail(&db, head)?;
    assert_eq!(
        pending_len, PENDING_TAIL_TXS,
        "pending tail must be 50 records"
    );

    // Scenario C — same tail after checkpoint moves it into a delta segment.
    db.checkpoint().map_err(db_error)?;
    let (delta_tail, delta_len) = timed_since_tail(&db, head)?;
    assert_eq!(delta_len, PENDING_TAIL_TXS, "delta tail must be 50 records");

    // Empty tail at head — the steady-state poll.
    let new_head = db.current_tx_count();
    let (empty_tail, empty_len) = timed_since_tail(&db, new_head)?;
    assert_eq!(empty_len, 0, "cursor at head must yield an empty tail");

    // Contrast: the full export the since path must beat by construction.
    let full_start = Instant::now();
    let full = db.export_fact_log().map_err(db_error)?;
    let full_elapsed = full_start.elapsed();
    assert_eq!(full.len(), BASE_FACTS + PENDING_TAIL_TXS);

    println!("A2 gate @ {BASE_FACTS} committed facts (head tx_count {head}):");
    println!(
        "  base-tail   since={base_tail_cursor} -> {base_tail_len:>3} records: cold {base_tail_cold:?}, warm {base_tail_warm:?}"
    );
    println!("  pending-tail since={head} -> {pending_len:>3} records: {pending_tail:?}");
    println!("  delta-tail   since={head} -> {delta_len:>3} records: {delta_tail:?}");
    println!("  empty-tail   since={new_head} -> {empty_len:>3} records: {empty_tail:?}");
    println!("  full export  {} records: {full_elapsed:?}", full.len());

    // Self-checking gate: a ≤100-record tail must be far cheaper than the
    // full export. 10x is deliberately loose; measured gap is orders larger.
    assert!(
        base_tail_cold < full_elapsed / 10,
        "base tail must not degrade toward a committed full scan"
    );
    Ok(())
}
