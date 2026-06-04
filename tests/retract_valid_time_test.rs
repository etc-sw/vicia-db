use anyhow::Result;
use minigraf::{Minigraf, QueryResult};

fn row_count(db: &Minigraf, query: &str) -> Result<usize> {
    match db.execute(query)? {
        QueryResult::QueryResults { results, .. } => Ok(results.len()),
        _ => anyhow::bail!("expected query results"),
    }
}

fn assert_person_edge_windows(db: &Minigraf, w1_count: usize, w2_count: usize) -> Result<()> {
    assert_eq!(
        row_count(
            db,
            r#"(query [:find ?target :valid-at "2020-06-01" :where [:alice :edge/to ?target]])"#,
        )?,
        w1_count,
        "unexpected row count for first valid-time window"
    );
    assert_eq!(
        row_count(
            db,
            r#"(query [:find ?target :valid-at "2021-06-01" :where [:alice :edge/to ?target]])"#,
        )?,
        w2_count,
        "unexpected row count for second valid-time window"
    );
    Ok(())
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
fn scoped_retract_only_removes_matching_valid_time_window() -> Result<()> {
    let db = Minigraf::in_memory()?;
    insert_two_ref_windows(&db)?;

    db.execute(
        r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000" {:valid-from "2020-01-01" :valid-to "2021-01-01"}]])"#,
    )?;

    assert_person_edge_windows(&db, 0, 1)?;
    assert_eq!(
        row_count(
            &db,
            r#"(query [:find ?target :valid-at :any-valid-time :where [:alice :edge/to ?target]])"#,
        )?,
        1,
        "only the second window should survive in any-valid-time view"
    );
    Ok(())
}

#[test]
fn tx_level_scoped_retract_options_apply_to_ref_edge() -> Result<()> {
    let db = Minigraf::in_memory()?;
    insert_two_ref_windows(&db)?;

    db.execute(
        r#"(retract {:valid-from "2020-01-01" :valid-to "2021-01-01"} [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#,
    )?;

    assert_person_edge_windows(&db, 0, 1)
}

#[test]
fn legacy_retract_still_removes_all_valid_time_windows() -> Result<()> {
    let db = Minigraf::in_memory()?;
    insert_two_ref_windows(&db)?;

    db.execute(r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000"]])"#)?;

    assert_person_edge_windows(&db, 0, 0)?;
    assert_eq!(
        row_count(
            &db,
            r#"(query [:find ?target :valid-at :any-valid-time :where [:alice :edge/to ?target]])"#,
        )?,
        0,
        "legacy retraction should still wipe every valid-time window"
    );
    Ok(())
}

#[test]
fn write_transaction_scoped_retract_matches_implicit_execute() -> Result<()> {
    let db = Minigraf::in_memory()?;
    insert_two_ref_windows(&db)?;

    let mut tx = db.begin_write()?;
    tx.execute(
        r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000" {:valid-from "2020-01-01" :valid-to "2021-01-01"}]])"#,
    )?;
    match tx.execute(
        r#"(query [:find ?target :valid-at "2020-06-01" :where [:alice :edge/to ?target]])"#,
    )? {
        QueryResult::QueryResults { results, .. } => {
            assert_eq!(
                results.len(),
                0,
                "transactional read should hide the scoped-retracted window"
            );
        }
        _ => anyhow::bail!("expected query results"),
    }
    tx.commit()?;

    assert_person_edge_windows(&db, 0, 1)
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scoped_retract_survives_checkpoint_and_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("scoped-retract.graph");

    {
        let db = Minigraf::open(&path)?;
        insert_two_ref_windows(&db)?;
        db.execute(
            r#"(retract [[:alice :edge/to #uuid "550e8400-e29b-41d4-a716-446655440000" {:valid-from "2020-01-01" :valid-to "2021-01-01"}]])"#,
        )?;
        db.checkpoint()?;
    }

    let reopened = Minigraf::open(&path)?;
    assert_person_edge_windows(&reopened, 0, 1)
}
