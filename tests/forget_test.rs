//! A8 `(forget ...)` bulk valid-time closure tests.
//!
//! Semantic forgetting: every valid-time window of the matched EAV triples
//! that contains the closure time `T` is truncated to end at `T`, in one
//! atomic transaction (one `tx_count`, one WAL entry). History is fully
//! preserved — `:as-of` before the closure shows the open windows, and the
//! fact-log export shows the closure as a scoped-retract + truncated
//! re-assert pair.
//!
//! Crash atomicity of this write path is exercised by the A7 kill -9 harness
//! (`tests/kill9_durability_test.rs`); this file pins the semantics and the
//! 10k-result-set one-transaction gate.

#![cfg(not(target_arch = "wasm32"))]

use anyhow::Result;
use minigraf::{Minigraf, QueryResult};

fn rows(db: &Minigraf, query: &str) -> Result<usize> {
    match db.execute(query)? {
        QueryResult::QueryResults { results, .. } => Ok(results.len()),
        _ => anyhow::bail!("expected query results"),
    }
}

fn forget(db: &Minigraf, cmd: &str) -> Result<(Option<u64>, usize)> {
    match db.execute(cmd)? {
        QueryResult::Forgotten { tx_id, count } => Ok((tx_id, count)),
        _ => anyhow::bail!("expected a forgotten result"),
    }
}

// ─── History correctness ─────────────────────────────────────────────────────

#[test]
fn forget_closes_window_at_explicit_time() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact {:valid-from "2026-01-01"} [[:alice :status :active]])"#)?;

    let (tx_id, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [[:alice :status :active]])"#,
    )?;
    assert!(tx_id.is_some(), "closure must report its tx_id");
    assert_eq!(count, 1, "one triple closed");

    // Before T: visible. At/after T: closed (valid_to is exclusive).
    let q = |at: &str| format!(r#"(query [:find ?s :valid-at "{at}" :where [:alice :status ?s]])"#);
    assert_eq!(rows(&db, &q("2026-03-01"))?, 1, "valid before closure time");
    assert_eq!(
        rows(&db, &q("2026-05-31"))?,
        1,
        "valid just before closure time"
    );
    assert_eq!(
        rows(&db, &q("2026-06-01"))?,
        0,
        "closed exactly at T (exclusive)"
    );
    assert_eq!(rows(&db, &q("2026-08-01"))?, 0, "closed after T");
    Ok(())
}

#[test]
fn forget_default_closure_time_is_tx_time() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact {:valid-from "2020-01-01"} [[:alice :status :active]])"#)?;

    let (_, count) = forget(&db, r#"(forget [[:alice :status :active]])"#)?;
    assert_eq!(count, 1);

    // Now (default valid-time) no longer sees the fact; history still does.
    assert_eq!(
        rows(&db, "(query [:find ?s :where [:alice :status ?s]])")?,
        0,
        "fact must be forgotten at the current time"
    );
    assert_eq!(
        rows(
            &db,
            r#"(query [:find ?s :valid-at "2020-06-01" :where [:alice :status ?s]])"#
        )?,
        1,
        "history before the closure must survive"
    );
    Ok(())
}

#[test]
fn forget_as_of_time_travel_shows_open_window() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact {:valid-from "2020-01-01"} [[:alice :status :active]])"#)?;
    let pre_closure = db.current_tx_count();

    forget(&db, r#"(forget [[:alice :status :active]])"#)?;

    assert_eq!(
        rows(
            &db,
            &format!("(query [:find ?s :as-of {pre_closure} :where [:alice :status ?s]])")
        )?,
        1,
        ":as-of before the closure must show the open window"
    );
    assert_eq!(
        rows(&db, "(query [:find ?s :where [:alice :status ?s]])")?,
        0,
        "current state must show the closure"
    );
    Ok(())
}

#[test]
fn forget_is_reversible_by_reassert() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact [[:alice :status :active]])"#)?;
    forget(&db, r#"(forget [[:alice :status :active]])"#)?;
    assert_eq!(
        rows(&db, "(query [:find ?s :where [:alice :status ?s]])")?,
        0
    );

    db.execute(r#"(transact [[:alice :status :active]])"#)?;
    assert_eq!(
        rows(&db, "(query [:find ?s :where [:alice :status ?s]])")?,
        1,
        "re-asserting after forget must restore the fact"
    );
    Ok(())
}

#[test]
fn forget_is_idempotent_and_noop_consumes_no_tx() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact {:valid-from "2026-01-01"} [[:alice :status :active]])"#)?;

    let (tx_id, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [[:alice :status :active]])"#,
    )?;
    assert!(tx_id.is_some());
    assert_eq!(count, 1);
    let after_first = db.current_tx_count();

    // Same closure again: the window no longer contains T — nothing matches.
    let (tx_id, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [[:alice :status :active]])"#,
    )?;
    assert!(tx_id.is_none(), "no-op forget must not report a tx_id");
    assert_eq!(count, 0);
    assert_eq!(
        db.current_tx_count(),
        after_first,
        "no-op forget must not consume a tx_count"
    );
    Ok(())
}

#[test]
fn forget_retruncates_to_earlier_time() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact {:valid-from "2026-01-01"} [[:r :status :active]])"#)?;

    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-12-01"} [[:r :status :active]])"#,
    )?;
    assert_eq!(count, 1);
    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-03-01"} [[:r :status :active]])"#,
    )?;
    assert_eq!(count, 1, "re-truncating the closed window to an earlier T");

    let q = |at: &str| format!(r#"(query [:find ?s :valid-at "{at}" :where [:r :status ?s]])"#);
    assert_eq!(rows(&db, &q("2026-02-01"))?, 1, "before the earlier T");
    assert_eq!(
        rows(&db, &q("2026-06-01"))?,
        0,
        "between the two closure times"
    );
    assert_eq!(
        rows(&db, &q("2026-12-15"))?,
        0,
        "after the original closure time"
    );
    Ok(())
}

#[test]
fn forget_after_legacy_unscoped_retract_is_noop() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact [[:l :status :active]])"#)?;
    db.execute(r#"(retract [[:l :status :active]])"#)?;

    let (tx_id, count) = forget(&db, r#"(forget [[:l :status :active]])"#)?;
    assert!(
        tx_id.is_none(),
        "fully retracted triple has no windows to close"
    );
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn forget_at_window_start_emits_retract_only() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact [[:z :status :active {:valid-from "2026-06-01"}]])"#)?;

    // T == valid_from: an empty (T, T) re-assert would be unmatchable — the
    // closure must degrade to a plain scoped retraction.
    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [[:z :status :active]])"#,
    )?;
    assert_eq!(count, 1);

    let records = db.export_fact_log()?;
    let z_records: Vec<_> = records
        .iter()
        .filter(|r| r.attribute == ":status")
        .collect();
    assert_eq!(
        z_records.len(),
        2,
        "original assert + closure retract, no empty-window re-assert"
    );
    assert_eq!(z_records.iter().filter(|r| !r.asserted).count(), 1);
    assert_eq!(
        rows(
            &db,
            r#"(query [:find ?s :valid-at "2026-07-01" :where [:z :status ?s]])"#
        )?,
        0
    );
    Ok(())
}

#[test]
fn forget_truncates_finite_window_and_leaves_disjoint_window() -> Result<()> {
    let db = Minigraf::in_memory()?;
    // Two disjoint windows of the same triple: only the one containing T closes.
    db.execute(
        r#"(transact [[:f :status :active {:valid-from "2026-01-01" :valid-to "2026-04-01"}]])"#,
    )?;
    db.execute(r#"(transact [[:f :status :active {:valid-from "2026-07-01"}]])"#)?;

    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-02-01"} [[:f :status :active]])"#,
    )?;
    assert_eq!(count, 1);

    let q = |at: &str| format!(r#"(query [:find ?s :valid-at "{at}" :where [:f :status ?s]])"#);
    assert_eq!(
        rows(&db, &q("2026-01-15"))?,
        1,
        "before T inside the closed window"
    );
    assert_eq!(
        rows(&db, &q("2026-03-01"))?,
        0,
        "truncated tail of the finite window"
    );
    assert_eq!(
        rows(&db, &q("2026-08-01"))?,
        1,
        "disjoint later window untouched"
    );
    Ok(())
}

#[test]
fn forget_dedupes_reasserts_for_overlapping_windows_sharing_start() -> Result<()> {
    let db = Minigraf::in_memory()?;
    // Two overlapping windows with the same valid_from, both containing T.
    db.execute(r#"(transact {:valid-from "2026-01-01"} [[:m :status :active]])"#)?;
    db.execute(
        r#"(transact [[:m :status :active {:valid-from "2026-01-01" :valid-to "2026-12-01"}]])"#,
    )?;

    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [[:m :status :active]])"#,
    )?;
    assert_eq!(count, 1);

    let closure_tx = db.current_tx_count();
    let closure_records: Vec<_> = db
        .export_fact_log()?
        .into_iter()
        .filter(|r| r.tx_count == closure_tx)
        .collect();
    assert_eq!(
        closure_records.len(),
        3,
        "two window retracts + one deduplicated re-assert"
    );
    assert_eq!(closure_records.iter().filter(|r| !r.asserted).count(), 2);
    assert_eq!(closure_records.iter().filter(|r| r.asserted).count(), 1);

    let q = |at: &str| format!(r#"(query [:find ?s :valid-at "{at}" :where [:m :status ?s]])"#);
    assert_eq!(rows(&db, &q("2026-03-01"))?, 1);
    assert_eq!(rows(&db, &q("2026-08-01"))?, 0);
    Ok(())
}

// ─── Input forms ─────────────────────────────────────────────────────────────

#[test]
fn forget_query_form_closes_matched_subset() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(
        r#"(transact {:valid-from "2026-01-01"} [[:s1 :session/expired true] [:s1 :session/data 1] [:s2 :session/expired false] [:s2 :session/data 2]])"#,
    )?;

    // Close every fact of expired sessions, including the marker itself.
    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [:find ?e ?a ?v :where [?e :session/expired true] [?e ?a ?v]])"#,
    )?;
    assert_eq!(count, 2, ":s1's two facts closed, :s2 untouched");

    let q = |at: &str, attr: &str| {
        format!(r#"(query [:find ?e ?v :valid-at "{at}" :where [?e {attr} ?v]])"#)
    };
    assert_eq!(
        rows(&db, &q("2026-08-01", ":session/data"))?,
        1,
        "only :s2 remains"
    );
    assert_eq!(
        rows(&db, &q("2026-03-01", ":session/data"))?,
        2,
        "history intact"
    );
    Ok(())
}

#[test]
fn forget_fact_list_dedupes_and_skips_unknown_triples() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact {:valid-from "2026-01-01"} [[:d :s 1] [:d2 :s 2]])"#)?;

    let (_, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-06-01"} [[:d :s 1] [:d :s 1] [:d2 :s 999]])"#,
    )?;
    assert_eq!(
        count, 1,
        "duplicate triple deduplicated; unknown triple skipped"
    );

    let closure_tx = db.current_tx_count();
    let closure_records = db
        .export_fact_log()?
        .into_iter()
        .filter(|r| r.tx_count == closure_tx)
        .count();
    assert_eq!(closure_records, 2, "exactly one retract + re-assert pair");
    Ok(())
}

#[test]
fn forget_empty_result_set_is_a_noop() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact [[:x :s 1]])"#)?;
    let before = db.current_tx_count();
    let export_len = db.export_fact_log()?.len();

    let (tx_id, count) = forget(
        &db,
        r#"(forget [:find ?e ?a ?v :where [?e :nothing/here ?v] [?e ?a ?v]])"#,
    )?;
    assert!(tx_id.is_none());
    assert_eq!(count, 0);
    assert_eq!(db.current_tx_count(), before, "no tx_count consumed");
    assert_eq!(
        db.export_fact_log()?.len(),
        export_len,
        "no records written"
    );
    Ok(())
}

// ─── Error surfaces ──────────────────────────────────────────────────────────

#[test]
fn forget_rejected_inside_write_transaction() -> Result<()> {
    let db = Minigraf::in_memory()?;
    let mut tx = db.begin_write()?;
    let err = tx
        .execute(r#"(forget [[:a :s 1]])"#)
        .expect_err("forget must be rejected inside an explicit transaction");
    assert!(
        err.to_string().contains("WriteTransaction"),
        "error must name the rejection context"
    );
    tx.rollback();
    Ok(())
}

#[test]
fn forget_query_row_must_bind_entity_first() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact [[:alice :age 30]])"#)?;
    let err = db
        .execute(r#"(forget [:find ?v ?a ?e :where [?e ?a ?v]])"#)
        .expect_err("misordered find variables must be rejected");
    assert!(
        err.to_string().contains("entities"),
        "error must explain the entity binding requirement"
    );
    Ok(())
}

#[test]
fn forget_not_preparable() -> Result<()> {
    let db = Minigraf::in_memory()?;
    let err = db
        .prepare(r#"(forget [[:a :s 1]])"#)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("forget"), "prepare must reject forget by name");
    Ok(())
}

// ─── Durability across checkpoint/reopen ─────────────────────────────────────

#[test]
fn forget_survives_checkpoint_and_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("forget.graph");

    {
        let db = Minigraf::open(&path)?;
        db.execute(r#"(transact {:valid-from "2026-01-01"} [[:p :status :active]])"#)?;
        forget(
            &db,
            r#"(forget {:valid-to "2026-06-01"} [[:p :status :active]])"#,
        )?;
        db.checkpoint()?;
    }

    let db = Minigraf::open(&path)?;
    let q = |at: &str| format!(r#"(query [:find ?s :valid-at "{at}" :where [:p :status ?s]])"#);
    assert_eq!(rows(&db, &q("2026-03-01"))?, 1, "history survives reopen");
    assert_eq!(rows(&db, &q("2026-08-01"))?, 0, "closure survives reopen");
    Ok(())
}

// ─── The A8 gate: 10k-fact result set, one transaction ───────────────────────

#[test]
fn forget_10k_result_set_closes_in_one_transaction() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("forget-10k.graph");
    let db = Minigraf::open(&path)?;

    const TOTAL: usize = 10_000;
    const BATCH: usize = 1_000;
    for batch in 0..(TOTAL / BATCH) {
        let mut cmd = String::from(r#"(transact {:valid-from "2020-01-01"} ["#);
        for i in (batch * BATCH)..((batch + 1) * BATCH) {
            cmd.push_str(&format!("[:k{i} :bulk/val {i}] "));
        }
        cmd.push_str("])");
        db.execute(&cmd)?;
    }
    let before = db.current_tx_count();

    let started = std::time::Instant::now();
    let (tx_id, count) = forget(
        &db,
        r#"(forget {:valid-to "2026-01-01"} [:find ?e ?a ?v :where [?e ?a ?v]])"#,
    )?;
    let elapsed = started.elapsed();

    assert!(tx_id.is_some());
    assert_eq!(count, TOTAL, "every triple closed");
    assert_eq!(
        db.current_tx_count(),
        before + 1,
        "GATE: the whole closure is exactly one transaction"
    );

    // Every closure record shares the single tx_count: 10k retracts + 10k
    // truncated re-asserts.
    let closure_tx = db.current_tx_count();
    let closure_records: Vec<_> = db
        .export_fact_log()?
        .into_iter()
        .filter(|r| r.tx_count == closure_tx)
        .collect();
    assert_eq!(closure_records.len(), 2 * TOTAL);
    assert_eq!(
        closure_records.iter().filter(|r| !r.asserted).count(),
        TOTAL,
        "one scoped retract per closed window"
    );

    // History shows the closed window edge correctly.
    assert_eq!(
        rows(
            &db,
            r#"(query [:find ?e ?v :valid-at "2025-06-01" :where [?e :bulk/val ?v]])"#
        )?,
        TOTAL,
        "all facts visible before the closure time"
    );
    assert_eq!(
        rows(
            &db,
            r#"(query [:find ?e ?v :valid-at "2026-06-01" :where [?e :bulk/val ?v]])"#
        )?,
        0,
        "no facts visible after the closure time"
    );

    println!(
        "A8 gate: closed {TOTAL} facts in one tx ({} records) in {:.1} ms",
        2 * TOTAL,
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}
