use minigraf::{Minigraf, QueryResult, Value};
use std::path::Path;
use uuid::Uuid;

const PAGE_SIZE: usize = 4096;
const PAGE_COUNT_OFFSET: usize = 8;
const EAVT_ROOT_OFFSET: usize = 32;
const INDEX_CHECKSUM_OFFSET: usize = 64;
const HEADER_CHECKSUM_OFFSET: usize = 80;

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

fn read_page0(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("database file should read");
    bytes
        .get(..PAGE_SIZE)
        .expect("database file should include page 0")
        .to_vec()
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .expect("u32 field should exist")
            .try_into()
            .expect("u32 field should be four bytes"),
    )
}

fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .expect("u64 field should exist")
            .try_into()
            .expect("u64 field should be eight bytes"),
    )
}

fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
    bytes
        .get_mut(offset..offset + 8)
        .expect("u64 field should exist")
        .copy_from_slice(&value.to_le_bytes());
}

fn rewrite_page0(path: &Path, mut page0: Vec<u8>) {
    let checksum = crc32fast::hash(
        page0
            .get(..HEADER_CHECKSUM_OFFSET)
            .expect("header checksum input should exist"),
    );
    page0
        .get_mut(HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4)
        .expect("header checksum field should exist")
        .copy_from_slice(&checksum.to_le_bytes());

    let mut bytes = std::fs::read(path).expect("database file should read");
    bytes
        .get_mut(..PAGE_SIZE)
        .expect("database file should include page 0")
        .copy_from_slice(&page0);
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
fn delta_checkpoint_preserves_base_checksum_in_page0() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("base-checksum.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
    }
    let base_page0 = read_page0(&path);
    let base_checksum = read_u32_le(&base_page0, INDEX_CHECKSUM_OFFSET);
    let base_page_count = read_u64_le(&base_page0, PAGE_COUNT_OFFSET);

    {
        let db = Minigraf::open(&path).expect("database should reopen");
        db.execute(r#"(transact [[:delta :name "Delta"]])"#)
            .expect("delta transact should execute");
        db.checkpoint().expect("delta checkpoint should succeed");
    }

    let delta_page0 = read_page0(&path);
    assert_eq!(
        read_u32_le(&delta_page0, INDEX_CHECKSUM_OFFSET),
        base_checksum,
        "delta checkpoint must keep the base checksum instead of checksumming delta pages"
    );
    assert!(
        read_u64_le(&delta_page0, PAGE_COUNT_OFFSET) > base_page_count,
        "delta checkpoint should still publish appended delta pages"
    );
}

#[test]
fn delta_manifest_base_root_mismatch_rejected_on_reopen() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("base-root-mismatch.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta :name "Delta"]])"#)
            .expect("delta transact should execute");
        db.checkpoint().expect("delta checkpoint should succeed");
    }

    let mut page0 = read_page0(&path);
    let page_count = read_u64_le(&page0, PAGE_COUNT_OFFSET);
    let wrong_root = if page_count > 2 { 1 } else { 0 };
    write_u64_le(&mut page0, EAVT_ROOT_OFFSET, wrong_root);
    rewrite_page0(&path, page0);

    let reopened = Minigraf::open(&path);
    let message = match reopened {
        Ok(_) => panic!("delta manifest base root mismatch must reject reopen"),
        Err(err) => err.to_string(),
    };
    assert!(
        message.contains("base roots"),
        "error should mention base roots"
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
