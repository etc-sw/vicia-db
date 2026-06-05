use minigraf::{Minigraf, QueryResult, Value};
use uuid::Uuid;

fn rows(result: QueryResult) -> Vec<Vec<Value>> {
    match result {
        QueryResult::QueryResults { results, .. } => results,
        _ => panic!("expected query results"),
    }
}

fn query_rows(db: &Minigraf, query: &str) -> Vec<Vec<Value>> {
    rows(db.execute(query).expect("query should execute"))
}

fn assert_delta_manifest_payload_present(path: &std::path::Path) {
    let bytes = std::fs::read(path).expect("database file should read");
    assert!(
        bytes
            .windows(b"MGDMF001".len())
            .any(|window| window == b"MGDMF001"),
        "delta checkpoint should write a manifest payload"
    );
}

fn corrupt_first_delta_segment(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("database file should read");
    let segment_offset = bytes
        .windows(b"MGDSG001".len())
        .position(|window| window == b"MGDSG001")
        .expect("delta segment payload should exist");
    bytes[segment_offset] ^= 0x01;
    std::fs::write(path, bytes).expect("database file should write");
}

#[test]
fn delta_checkpoint_reopen_sees_delta_only_fact_and_export_log() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("delta.graph");
    let wal_path = dir.path().join("delta.graph.wal");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");

        db.execute(r#"(transact [[:delta :name "Delta"]])"#)
            .expect("delta transact should execute");
        assert!(wal_path.exists(), "WAL must exist before delta checkpoint");
        db.checkpoint().expect("delta checkpoint should succeed");
        assert!(
            !wal_path.exists(),
            "WAL must be retired after durable delta checkpoint"
        );
    }

    assert_delta_manifest_payload_present(&path);

    let db = Minigraf::open(&path).expect("database should reopen");
    let name_rows = query_rows(&db, r#"(query [:find ?name :where [:delta :name ?name]])"#);
    assert_eq!(name_rows.len(), 1, "delta-only fact must survive reopen");
    match &name_rows[0][0] {
        Value::String(name) => assert_eq!(name, "Delta"),
        _ => panic!("delta name should be a string"),
    }

    let records = db.export_fact_log().expect("fact log should export");
    assert_eq!(
        records.len(),
        2,
        "export log must include base and delta facts"
    );
}

#[test]
fn delta_checkpoint_reopen_sees_base_to_delta_ref_edge() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("ref-edge.graph");
    let source = Uuid::from_u128(0x100);
    let target = Uuid::from_u128(0x200);

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(&format!(
            r#"(transact [[#uuid "{source}" :name "source"]])"#
        ))
        .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");

        db.execute(&format!(
            r#"(transact [[#uuid "{source}" :edge/to #uuid "{target}"]
                          [#uuid "{target}" :name "target"]])"#
        ))
        .expect("delta transact should execute");
        db.checkpoint().expect("delta checkpoint should succeed");
    }

    let db = Minigraf::open(&path).expect("database should reopen");
    let edge_rows = query_rows(
        &db,
        &format!(
            r#"(query [:find ?name
                      :where [#uuid "{source}" :edge/to ?target]
                             [?target :name ?name]])"#
        ),
    );
    assert_eq!(edge_rows.len(), 1, "base-to-delta ref edge must resolve");
    match &edge_rows[0][0] {
        Value::String(name) => assert_eq!(name, "target"),
        _ => panic!("target name should be a string"),
    }
}

#[test]
fn delta_retraction_hides_base_assertion_after_reopen() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("retraction.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:item :status :active]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");

        db.execute(r#"(retract [[:item :status :active]])"#)
            .expect("delta retract should execute");
        db.checkpoint().expect("delta checkpoint should succeed");
    }

    let db = Minigraf::open(&path).expect("database should reopen");
    let current = query_rows(&db, r#"(query [:find ?s :where [:item :status ?s]])"#);
    assert_eq!(
        current.len(),
        0,
        "delta retraction must hide base assertion"
    );

    let past = query_rows(
        &db,
        r#"(query [:find ?s :as-of 1 :valid-at :any-valid-time :where [:item :status ?s]])"#,
    );
    assert_eq!(past.len(), 1, "base assertion must remain in history");
}

#[test]
fn delta_checkpoint_corrupt_segment_errors_instead_of_silent_loss() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("corrupt.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta :name "Delta"]])"#)
            .expect("delta transact should execute");
        db.checkpoint().expect("delta checkpoint should succeed");
    }

    corrupt_first_delta_segment(&path);
    let reopened = Minigraf::open(&path);
    assert!(
        reopened.is_err(),
        "corrupt delta segment must not open cleanly"
    );
}

#[test]
fn full_rebuild_fallback_after_visible_delta_preserves_results() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("fallback.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta :name "Delta"]])"#)
            .expect("delta transact should execute");
        db.checkpoint().expect("delta checkpoint should succeed");

        let before = query_rows(&db, r#"(query [:find ?name :where [:delta :name ?name]])"#);
        assert_eq!(
            before.len(),
            1,
            "delta fact must be visible before fallback"
        );

        db.execute(r#"(transact [[:after :name "After"]])"#)
            .expect("post-delta transact should execute");
        db.checkpoint()
            .expect("full rebuild fallback checkpoint should succeed");
    }

    let db = Minigraf::open(&path).expect("database should reopen");
    let delta = query_rows(&db, r#"(query [:find ?name :where [:delta :name ?name]])"#);
    let after = query_rows(&db, r#"(query [:find ?name :where [:after :name ?name]])"#);
    assert_eq!(delta.len(), 1, "delta fact must survive fallback rebuild");
    assert_eq!(after.len(), 1, "new fact must survive fallback rebuild");
    let records = db.export_fact_log().expect("fact log should export");
    assert_eq!(records.len(), 3, "fallback rebuild must preserve full log");
}
