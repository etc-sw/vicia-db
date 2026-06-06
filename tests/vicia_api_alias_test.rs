use minigraf::{Minigraf, QueryResult, Value, ViciaDb};

fn assert_single_string(result: QueryResult, expected: &str) {
    match result {
        QueryResult::QueryResults { results, .. } => {
            assert_eq!(results.len(), 1, "expected one query row");
            let row = results.first().expect("expected query row");
            let value = row.first().expect("expected first query column");
            assert_eq!(
                value,
                &Value::String(expected.to_owned()),
                "expected query value"
            );
        }
        _ => panic!("expected query results"),
    }
}

#[test]
fn vicia_alias_uses_the_minigraf_api_in_memory() {
    let db = ViciaDb::in_memory().expect("in-memory db should open");
    let legacy: Minigraf = db.clone();

    db.execute(r#"(transact [[:alice :person/name "Alice"]])"#)
        .expect("transact should succeed");

    let result = legacy
        .execute(r#"(query [:find ?name :where [:alice :person/name ?name]])"#)
        .expect("query should succeed");

    assert_single_string(result, "Alice");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn vicia_alias_file_checkpoint_reopens_through_minigraf() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("vicia-alias.graph");

    {
        let db = ViciaDb::open(&path).expect("file db should open");
        db.execute(r#"(transact [[:bob :person/name "Bob"]])"#)
            .expect("transact should succeed");
        db.checkpoint().expect("checkpoint should succeed");
    }

    let reopened = Minigraf::open(&path).expect("file db should reopen");
    let result = reopened
        .execute(r#"(query [:find ?name :where [:bob :person/name ?name]])"#)
        .expect("query should succeed");

    assert_single_string(result, "Bob");
}
