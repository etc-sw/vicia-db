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
