use anyhow::Result;
use minigraf::{FactRecord, FactValidTime, Minigraf, Value};
use uuid::Uuid;

const TARGET_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
const VALID_FROM_2020_01_01: i64 = 1_577_836_800_000;
const VALID_TO_2021_01_01: i64 = 1_609_459_200_000;
const VALID_TO_2022_01_01: i64 = 1_640_995_200_000;

fn target_uuid() -> Result<Uuid> {
    Ok(Uuid::parse_str(TARGET_UUID)?)
}

fn find_record<'a>(
    records: &'a [FactRecord],
    attribute: &str,
    asserted: bool,
) -> Result<&'a FactRecord> {
    records
        .iter()
        .find(|record| record.attribute == attribute && record.asserted == asserted)
        .ok_or_else(|| anyhow::anyhow!("expected matching fact-log record"))
}

fn insert_two_ref_windows(db: &Minigraf) -> Result<()> {
    db.execute(
        r#"(transact {:valid-from "2020-01-01" :valid-to "2021-01-01"} [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#,
    )?;
    db.execute(
        r#"(transact {:valid-from "2021-01-01" :valid-to "2022-01-01"} [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#,
    )?;
    Ok(())
}

#[test]
fn export_fact_log_includes_assertions_and_legacy_retractions() -> Result<()> {
    let db = Minigraf::in_memory()?;
    db.execute(r#"(transact [[:alice :role "writer"]])"#)?;
    db.execute(r#"(retract [[:alice :role "writer"]])"#)?;

    let records = db.export_fact_log()?;
    assert_eq!(
        records.len(),
        2,
        "expected assertion and retraction records"
    );

    let assertion = find_record(&records, ":role", true)?;
    assert_eq!(
        assertion.value,
        Value::String("writer".to_string()),
        "assertion value should be exported"
    );
    assert_eq!(assertion.tx_count, 1, "assertion tx_count should be stable");
    let expected_valid_from = i64::try_from(assertion.tx_id)?;
    match assertion.valid_time {
        FactValidTime::Window {
            valid_from,
            valid_to,
        } => {
            assert_eq!(
                valid_from, expected_valid_from,
                "default valid_from should be the assertion tx_id"
            );
            assert_eq!(valid_to, i64::MAX, "default valid_to should be open-ended");
        }
        FactValidTime::AllValidTime => anyhow::bail!("assertion should have a valid-time window"),
    }

    let retraction = find_record(&records, ":role", false)?;
    assert_eq!(
        retraction.value,
        Value::String("writer".to_string()),
        "retraction value should be exported"
    );
    assert_eq!(
        retraction.tx_count, 2,
        "retraction tx_count should be stable"
    );
    assert!(
        retraction.tx_id >= assertion.tx_id,
        "retraction tx_id should not precede assertion tx_id"
    );
    assert_eq!(
        retraction.valid_time,
        FactValidTime::AllValidTime,
        "legacy retract should export as all-valid-time scope"
    );
    Ok(())
}

#[test]
fn export_fact_log_distinguishes_scoped_retract_window_for_ref_edge() -> Result<()> {
    let db = Minigraf::in_memory()?;
    insert_two_ref_windows(&db)?;
    db.execute(
        r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000" {:valid-from "2020-01-01" :valid-to "2021-01-01"}]])"#,
    )?;

    let records = db.export_fact_log()?;
    assert_eq!(
        records.len(),
        3,
        "expected two assertions and one scoped retraction"
    );

    let target = target_uuid()?;
    let retraction = find_record(&records, ":edge/to", false)?;
    assert_eq!(
        retraction.value,
        Value::Ref(target),
        "scoped retraction should preserve the Ref edge value"
    );
    assert_eq!(
        retraction.tx_count, 3,
        "scoped retraction tx_count should be exported"
    );
    assert_eq!(
        retraction.valid_time,
        FactValidTime::Window {
            valid_from: VALID_FROM_2020_01_01,
            valid_to: VALID_TO_2021_01_01,
        },
        "scoped retraction should export its exact valid-time window"
    );
    Ok(())
}

#[test]
fn export_fact_log_preserves_same_ref_eav_retract_and_assert_in_one_write_tx() -> Result<()> {
    let db = Minigraf::in_memory()?;
    let target = target_uuid()?;

    db.execute(r#"(transact [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#)?;

    let mut tx = db.begin_write()?;
    tx.execute(r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#)?;
    tx.execute(r#"(transact [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#)?;
    tx.commit()?;

    let records = db.export_fact_log()?;
    assert_eq!(
        records.len(),
        3,
        "expected initial assertion plus same-write retract and assert"
    );
    assert!(
        records
            .iter()
            .all(|record| record.attribute == ":edge/to" && record.value == Value::Ref(target)),
        "all exported records should preserve the same Ref EAV identity"
    );

    let initial_assertion = records
        .iter()
        .find(|record| record.tx_count == 1 && record.asserted)
        .ok_or_else(|| anyhow::anyhow!("expected initial assertion record"))?;
    assert_eq!(
        initial_assertion.value,
        Value::Ref(target),
        "initial assertion should preserve Ref value"
    );

    let same_write_records: Vec<&FactRecord> = records
        .iter()
        .filter(|record| record.tx_count == 2)
        .collect();
    assert_eq!(
        same_write_records.len(),
        2,
        "write transaction should export both same-tx records"
    );

    let same_write_tx_id = same_write_records
        .first()
        .ok_or_else(|| anyhow::anyhow!("expected same write records"))?
        .tx_id;
    assert!(
        same_write_records
            .iter()
            .all(|record| record.tx_id == same_write_tx_id),
        "same write transaction records should share tx_id"
    );
    assert!(
        same_write_records.iter().any(|record| !record.asserted),
        "same write transaction should include the retraction"
    );
    assert!(
        same_write_records.iter().any(|record| record.asserted),
        "same write transaction should include the reassertion"
    );
    assert!(
        same_write_records
            .iter()
            .all(|record| record.value == Value::Ref(target)),
        "same write transaction should preserve Ref value identity"
    );
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn export_fact_log_preserves_ref_values_after_checkpoint_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fact-log-export.graph");
    let target = target_uuid()?;

    {
        let db = Minigraf::open(&path)?;
        insert_two_ref_windows(&db)?;
        db.checkpoint()?;
    }

    let reopened = Minigraf::open(&path)?;
    let records = reopened.export_fact_log()?;
    assert_eq!(
        records.len(),
        2,
        "checkpointed fact log should include both ref windows"
    );
    assert!(
        records
            .iter()
            .all(|record| record.value == Value::Ref(target)),
        "all exported values should remain Ref values after reopen"
    );
    assert!(
        records.iter().any(|record| record.valid_time
            == (FactValidTime::Window {
                valid_from: VALID_FROM_2020_01_01,
                valid_to: VALID_TO_2021_01_01,
            })),
        "first valid-time window should survive checkpoint/reopen"
    );
    assert!(
        records.iter().any(|record| record.valid_time
            == (FactValidTime::Window {
                valid_from: VALID_TO_2021_01_01,
                valid_to: VALID_TO_2022_01_01,
            })),
        "second valid-time window should survive checkpoint/reopen"
    );
    Ok(())
}

// ── A2: export_fact_log_since ────────────────────────────────────────────────

/// Assert `export_fact_log_since(since)` returns exactly the ordered
/// subsequence of the full export with `tx_count > since`.
fn assert_since_matches_filtered_full(db: &Minigraf, since: u64) -> Result<()> {
    let full = db.export_fact_log()?;
    let expected: Vec<&FactRecord> = full
        .iter()
        .filter(|record| record.tx_count > since)
        .collect();
    let got = db.export_fact_log_since(since)?;
    assert_eq!(
        got.len(),
        expected.len(),
        "since-tail length must match filtered full export"
    );
    let matching = got
        .iter()
        .zip(expected.iter())
        .filter(|(a, b)| *a == **b)
        .count();
    assert_eq!(
        matching,
        expected.len(),
        "since-tail must be the exact ordered subsequence of the full export"
    );
    Ok(())
}

#[test]
fn export_fact_log_since_filters_asserted_and_retracted_with_valid_time() -> Result<()> {
    let db = Minigraf::in_memory()?;
    insert_two_ref_windows(&db)?; // tx 1, 2
    db.execute(
        r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000" {:valid-from "2020-01-01" :valid-to "2021-01-01"}]])"#,
    )?; // tx 3
    db.execute(r#"(transact [[:alice :role "writer"]])"#)?; // tx 4

    for since in 0..=5 {
        assert_since_matches_filtered_full(&db, since)?;
    }

    // The scoped retraction must survive the since filter with its window.
    let tail = db.export_fact_log_since(2)?;
    assert_eq!(tail.len(), 2, "tail past tx 2 holds retraction + assertion");
    let retraction = find_record(&tail, ":edge/to", false)?;
    assert_eq!(
        retraction.valid_time,
        FactValidTime::Window {
            valid_from: VALID_FROM_2020_01_01,
            valid_to: VALID_TO_2021_01_01,
        },
        "scoped retraction keeps its valid-time window through the since path"
    );

    assert!(
        db.export_fact_log_since(4)?.is_empty(),
        "since at the head must return an empty tail"
    );
    assert_eq!(
        db.export_fact_log_since(0)?.len(),
        db.export_fact_log()?.len(),
        "since zero must equal the full export"
    );
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn export_fact_log_since_spans_base_delta_and_pending_layers() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fact-log-since-layers.graph");
    let db = Minigraf::open(&path)?;

    // Base layer: first checkpoint full-rebuilds.
    for i in 0..10 {
        db.execute(&format!(r#"(transact [[:alice :seq/base {i}]])"#))?;
    }
    db.checkpoint()?;
    // Delta layer: second checkpoint appends a delta segment.
    for i in 0..10 {
        db.execute(&format!(r#"(transact [[:alice :seq/delta {i}]])"#))?;
    }
    db.checkpoint()?;
    // Pending layer: uncheckpointed in-memory facts.
    for i in 0..5 {
        db.execute(&format!(r#"(transact [[:alice :seq/pending {i}]])"#))?;
    }

    for since in [0, 1, 9, 10, 11, 19, 20, 22, 25, 30] {
        assert_since_matches_filtered_full(&db, since)?;
    }

    let tail = db.export_fact_log_since(20)?;
    assert_eq!(tail.len(), 5, "tail past the delta layer is the pending set");
    assert!(
        tail.iter().all(|record| record.attribute == ":seq/pending"),
        "tail past tx 20 must only hold pending-layer records"
    );
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn export_fact_log_since_serves_stored_cursor_across_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fact-log-since-cursor.graph");

    // Tick 1: write, checkpoint, remember the cursor.
    let cursor = {
        let db = Minigraf::open(&path)?;
        for i in 0..8 {
            db.execute(&format!(r#"(transact [[:alice :tick/one {i}]])"#))?;
        }
        db.checkpoint()?;
        db.current_tx_count()
    };

    // Tick 2 (after reopen): new writes land past the stored cursor.
    let db = Minigraf::open(&path)?;
    db.execute(r#"(transact [[:alice :tick/two 1]])"#)?;
    db.execute(r#"(retract [[:alice :tick/two 1]])"#)?;

    let tail = db.export_fact_log_since(cursor)?;
    assert_eq!(
        tail.len(),
        2,
        "reopened cursor poll must see exactly the new assertion + retraction"
    );
    assert!(
        tail.iter().all(|record| record.tx_count > cursor),
        "every polled record must sit past the stored cursor"
    );
    assert_eq!(
        tail.iter().filter(|record| record.asserted).count(),
        1,
        "poll must include the assertion"
    );
    assert_eq!(
        tail.iter().filter(|record| !record.asserted).count(),
        1,
        "poll must include the retraction"
    );
    assert_since_matches_filtered_full(&db, cursor)?;
    Ok(())
}
