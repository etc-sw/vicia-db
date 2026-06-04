//! Regression coverage for same entity+attribute multi-value batches.
//!
//! Public Datalog query paths must not collapse distinct values that share the
//! same entity, attribute, valid window, and tx_count.

#![cfg(not(target_arch = "wasm32"))]

use minigraf::{Minigraf, OpenOptions, QueryResult, Value};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn rows(result: QueryResult) -> TestResult<Vec<Vec<Value>>> {
    match result {
        QueryResult::QueryResults { results, .. } => Ok(results),
        _ => Err(std::io::Error::other("expected query results").into()),
    }
}

fn query_rows(db: &Minigraf, query: &str) -> TestResult<Vec<Vec<Value>>> {
    rows(db.execute(query)?)
}

fn count_rows(db: &Minigraf, query: &str) -> TestResult<usize> {
    Ok(query_rows(db, query)?.len())
}

fn contains_string(rows: &[Vec<Value>], value: &str) -> bool {
    rows.iter()
        .flatten()
        .any(|cell| matches!(cell, Value::String(s) if s == value))
}

fn contains_integer(rows: &[Vec<Value>], value: i64) -> bool {
    rows.iter()
        .flatten()
        .any(|cell| matches!(cell, Value::Integer(n) if *n == value))
}

fn contains_bool(rows: &[Vec<Value>], value: bool) -> bool {
    rows.iter()
        .flatten()
        .any(|cell| matches!(cell, Value::Boolean(b) if *b == value))
}

fn contains_keyword(rows: &[Vec<Value>], value: &str) -> bool {
    rows.iter()
        .flatten()
        .any(|cell| matches!(cell, Value::Keyword(k) if k == value))
}

fn contains_ref(rows: &[Vec<Value>], value: Uuid) -> bool {
    rows.iter()
        .flatten()
        .any(|cell| matches!(cell, Value::Ref(id) if *id == value))
}

#[test]
fn same_entity_attr_values_survive_in_memory_indexed_public_queries() -> TestResult {
    let db = Minigraf::in_memory()?;

    db.execute(
        r#"(transact [
            [:packet-1 :support "a"]
            [:packet-1 :support "b"]
            [:packet-1 :support "c"]
        ])"#,
    )?;

    let entity_bound = query_rows(&db, r#"(query [:find ?v :where [:packet-1 :support ?v]])"#)?;
    assert_eq!(
        entity_bound.len(),
        3,
        "entity-bound query must keep all values"
    );
    assert!(contains_string(&entity_bound, "a"), "must include value a");
    assert!(contains_string(&entity_bound, "b"), "must include value b");
    assert!(contains_string(&entity_bound, "c"), "must include value c");

    let attr_bound = query_rows(&db, r#"(query [:find ?p ?v :where [?p :support ?v]])"#)?;
    assert_eq!(
        attr_bound.len(),
        3,
        "attribute-bound query must keep all values"
    );

    assert_eq!(
        count_rows(&db, r#"(query [:find ?p :where [?p :support "a"]])"#)?,
        1,
        "attribute+value query must find value a"
    );
    assert_eq!(
        count_rows(&db, r#"(query [:find ?p :where [?p :support "b"]])"#)?,
        1,
        "attribute+value query must find value b"
    );
    assert_eq!(
        count_rows(&db, r#"(query [:find ?p :where [?p :support "c"]])"#)?,
        1,
        "attribute+value query must find value c"
    );
    Ok(())
}

#[test]
fn ten_same_entity_attr_values_survive_in_one_transaction() -> TestResult {
    let db = Minigraf::in_memory()?;

    db.execute(
        r#"(transact [
            [:packet-10 :support 0]
            [:packet-10 :support 1]
            [:packet-10 :support 2]
            [:packet-10 :support 3]
            [:packet-10 :support 4]
            [:packet-10 :support 5]
            [:packet-10 :support 6]
            [:packet-10 :support 7]
            [:packet-10 :support 8]
            [:packet-10 :support 9]
        ])"#,
    )?;

    let entity_bound = query_rows(&db, r#"(query [:find ?v :where [:packet-10 :support ?v]])"#)?;
    assert_eq!(
        entity_bound.len(),
        10,
        "entity-bound query must keep ten values"
    );
    for n in 0..10 {
        assert!(
            contains_integer(&entity_bound, n),
            "must include expected integer"
        );
    }
    Ok(())
}

#[test]
fn mixed_value_types_survive_same_entity_attr_batch() -> TestResult {
    let db = Minigraf::in_memory()?;

    db.execute(
        r#"(transact [
            [:mixed :value "text"]
            [:mixed :value 42]
            [:mixed :value true]
            [:mixed :value :status/active]
        ])"#,
    )?;

    let rows = query_rows(&db, r#"(query [:find ?v :where [:mixed :value ?v]])"#)?;
    assert_eq!(rows.len(), 4, "mixed same-attribute values must survive");
    assert!(contains_string(&rows, "text"), "must include string value");
    assert!(contains_integer(&rows, 42), "must include integer value");
    assert!(contains_bool(&rows, true), "must include boolean value");
    assert!(
        contains_keyword(&rows, ":status/active"),
        "must include keyword value"
    );
    Ok(())
}

#[test]
fn ref_values_survive_same_entity_attr_edge_batch() -> TestResult {
    let db = Minigraf::in_memory()?;
    let source = Uuid::parse_str("00000000-0000-0000-0000-000000000101")?;
    let target_a = Uuid::parse_str("00000000-0000-0000-0000-000000000201")?;
    let target_b = Uuid::parse_str("00000000-0000-0000-0000-000000000202")?;
    let target_c = Uuid::parse_str("00000000-0000-0000-0000-000000000203")?;

    db.execute(&format!(
        r#"(transact [
            [#uuid "{source}" :edge/to #uuid "{target_a}"]
            [#uuid "{source}" :edge/to #uuid "{target_b}"]
            [#uuid "{source}" :edge/to #uuid "{target_c}"]
        ])"#
    ))?;

    let entity_bound = query_rows(
        &db,
        &format!(r#"(query [:find ?to :where [#uuid "{source}" :edge/to ?to]])"#),
    )?;
    assert_eq!(
        entity_bound.len(),
        3,
        "entity-bound ref edge query must keep all targets"
    );
    assert!(
        contains_ref(&entity_bound, target_a),
        "must include ref target a"
    );
    assert!(
        contains_ref(&entity_bound, target_b),
        "must include ref target b"
    );
    assert!(
        contains_ref(&entity_bound, target_c),
        "must include ref target c"
    );

    let attr_bound = query_rows(
        &db,
        r#"(query [:find ?from ?to :where [?from :edge/to ?to]])"#,
    )?;
    assert_eq!(
        attr_bound.len(),
        3,
        "attribute-bound ref edge query must keep all targets"
    );

    assert_eq!(
        count_rows(
            &db,
            &format!(r#"(query [:find ?from :where [?from :edge/to #uuid "{target_a}"]])"#)
        )?,
        1,
        "attribute+ref-value query must find target a"
    );

    Ok(())
}

#[test]
fn same_entity_attr_values_survive_as_of_and_retract_all() -> TestResult {
    let db = Minigraf::in_memory()?;

    db.execute(
        r#"(transact [
            [:packet-hist :support "a"]
            [:packet-hist :support "b"]
            [:packet-hist :support "c"]
        ])"#,
    )?;

    db.execute(
        r#"(retract [
            [:packet-hist :support "a"]
            [:packet-hist :support "b"]
            [:packet-hist :support "c"]
        ])"#,
    )?;

    let past = query_rows(
        &db,
        r#"(query [:find ?v :as-of 1 :valid-at :any-valid-time :where [:packet-hist :support ?v]])"#,
    )?;
    assert_eq!(
        past.len(),
        3,
        "as-of before retraction must replay all values"
    );
    assert!(
        contains_string(&past, "a"),
        "past replay must include value a"
    );
    assert!(
        contains_string(&past, "b"),
        "past replay must include value b"
    );
    assert!(
        contains_string(&past, "c"),
        "past replay must include value c"
    );

    let current = query_rows(
        &db,
        r#"(query [:find ?v :valid-at :any-valid-time :where [:packet-hist :support ?v]])"#,
    )?;
    assert_eq!(
        current.len(),
        0,
        "current view must hide every retracted value"
    );
    Ok(())
}

#[test]
fn per_fact_valid_windows_survive_same_entity_attr_batch() -> TestResult {
    let db = Minigraf::in_memory()?;

    db.execute(
        r#"(transact [
            [:packet-window :state "draft" {:valid-from "2023-01-01" :valid-to "2023-02-01"}]
            [:packet-window :state "review" {:valid-from "2023-02-01" :valid-to "2023-03-01"}]
            [:packet-window :state "final" {:valid-from "2023-03-01"}]
        ])"#,
    )?;

    let january = query_rows(
        &db,
        r#"(query [:find ?state :valid-at "2023-01-15" :where [:packet-window :state ?state]])"#,
    )?;
    assert_eq!(
        january.len(),
        1,
        "January valid-time query must return one state"
    );
    assert!(
        contains_string(&january, "draft"),
        "January state must be draft"
    );

    let february = query_rows(
        &db,
        r#"(query [:find ?state :valid-at "2023-02-15" :where [:packet-window :state ?state]])"#,
    )?;
    assert_eq!(
        february.len(),
        1,
        "February valid-time query must return one state"
    );
    assert!(
        contains_string(&february, "review"),
        "February state must be review"
    );

    let march = query_rows(
        &db,
        r#"(query [:find ?state :valid-at "2023-03-15" :where [:packet-window :state ?state]])"#,
    )?;
    assert_eq!(
        march.len(),
        1,
        "March valid-time query must return one state"
    );
    assert!(
        contains_string(&march, "final"),
        "March state must be final"
    );
    Ok(())
}

#[test]
fn same_entity_attr_values_survive_checkpoint_and_reopen() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("same-eav.graph");

    {
        let db = OpenOptions::new().path(&path).open()?;
        db.execute(
            r#"(transact [
                [:packet-file :support "a"]
                [:packet-file :support "b"]
                [:packet-file :support "c"]
            ])"#,
        )?;
        db.checkpoint()?;
    }

    let db = OpenOptions::new().path(&path).open()?;

    let entity_bound = query_rows(
        &db,
        r#"(query [:find ?v :where [:packet-file :support ?v]])"#,
    )?;
    assert_eq!(
        entity_bound.len(),
        3,
        "reopened entity-bound query must keep all values"
    );
    assert!(
        contains_string(&entity_bound, "a"),
        "must include value a after reopen"
    );
    assert!(
        contains_string(&entity_bound, "b"),
        "must include value b after reopen"
    );
    assert!(
        contains_string(&entity_bound, "c"),
        "must include value c after reopen"
    );

    let attr_bound = query_rows(&db, r#"(query [:find ?p ?v :where [?p :support ?v]])"#)?;
    assert_eq!(
        attr_bound.len(),
        3,
        "reopened attribute-bound query must keep all values"
    );

    assert_eq!(
        count_rows(&db, r#"(query [:find ?p :where [?p :support "a"]])"#)?,
        1,
        "reopened attribute+value query must find value a"
    );
    assert_eq!(
        count_rows(&db, r#"(query [:find ?p :where [?p :support "b"]])"#)?,
        1,
        "reopened attribute+value query must find value b"
    );
    assert_eq!(
        count_rows(&db, r#"(query [:find ?p :where [?p :support "c"]])"#)?,
        1,
        "reopened attribute+value query must find value c"
    );
    Ok(())
}
