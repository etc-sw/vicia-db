use minigraf::{Minigraf, QueryResult, Value};
use std::path::Path;
use uuid::Uuid;

const PAGE_SIZE: usize = 4096;
const PAGE_COUNT_OFFSET: usize = 8;
const EAVT_ROOT_OFFSET: usize = 32;
const INDEX_CHECKSUM_OFFSET: usize = 64;
const HEADER_CHECKSUM_OFFSET: usize = 80;
const HEADER_EXTENSION_OFFSET: usize = 84;
const HEADER_EXTENSION_PREFIX_LEN: usize = 12;
const HEADER_MANIFEST_SLOT_LEN: usize = 40;
const HEADER_MANIFEST_SLOT_GENERATION_OFFSET: usize = 0;
const HEADER_MANIFEST_SLOT_PAGE_START_OFFSET: usize = 8;
const HEADER_MANIFEST_SLOT_CHECKSUM_OFFSET: usize = 36;

#[derive(Clone, Copy)]
enum ManifestSlot {
    Primary,
    Secondary,
}

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

fn corrupt_last_delta_segment(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("database file should read");
    let marker = b"MGDSG001";
    let mut segment_offset = None;
    for (offset, window) in bytes.windows(marker.len()).enumerate() {
        if window == marker {
            segment_offset = Some(offset);
        }
    }
    let segment_offset = segment_offset.expect("delta segment payload should exist");
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

fn slot_offset(slot: ManifestSlot) -> usize {
    let slot_index = match slot {
        ManifestSlot::Primary => 0,
        ManifestSlot::Secondary => 1,
    };
    HEADER_EXTENSION_OFFSET + HEADER_EXTENSION_PREFIX_LEN + (slot_index * HEADER_MANIFEST_SLOT_LEN)
}

fn read_slot_generation(page0: &[u8], slot: ManifestSlot) -> u64 {
    read_u64_le(
        page0,
        slot_offset(slot) + HEADER_MANIFEST_SLOT_GENERATION_OFFSET,
    )
}

fn read_slot_manifest_page_start(page0: &[u8], slot: ManifestSlot) -> u64 {
    read_u64_le(
        page0,
        slot_offset(slot) + HEADER_MANIFEST_SLOT_PAGE_START_OFFSET,
    )
}

fn newest_manifest_slot(page0: &[u8]) -> ManifestSlot {
    let primary_generation = read_slot_generation(page0, ManifestSlot::Primary);
    let secondary_generation = read_slot_generation(page0, ManifestSlot::Secondary);
    assert!(
        primary_generation > 0 && secondary_generation > 0,
        "both manifest slots should be populated"
    );
    if primary_generation >= secondary_generation {
        ManifestSlot::Primary
    } else {
        ManifestSlot::Secondary
    }
}

fn corrupt_slot_checksum(path: &Path, slot: ManifestSlot) {
    let mut page0 = read_page0(path);
    page0[slot_offset(slot) + HEADER_MANIFEST_SLOT_CHECKSUM_OFFSET] ^= 0x55;
    rewrite_page0(path, page0);
}

fn corrupt_manifest_payload(path: &Path, slot: ManifestSlot) {
    let page0 = read_page0(path);
    let manifest_page_start = read_slot_manifest_page_start(&page0, slot);
    let manifest_offset = usize::try_from(manifest_page_start)
        .expect("manifest page start should fit usize")
        * PAGE_SIZE;
    let mut bytes = std::fs::read(path).expect("database file should read");
    bytes[manifest_offset] ^= 0x55;
    std::fs::write(path, bytes).expect("database file should write");
}

fn query_count(db: &Minigraf, query: &str) -> usize {
    query_rows(db, query).len()
}

fn transact_many_ref_facts(db: &Minigraf, entity_prefix: &str, count: usize) {
    let mut command = String::from("(transact [");
    for index in 0..count {
        let target = Uuid::from_u128(index as u128 + 10_000);
        command.push_str(&format!(
            r#"[:{entity_prefix}-{index} :edge/to #uuid "{target}"]"#
        ));
    }
    command.push_str("])");
    db.execute(&command)
        .expect("bulk ref transact should execute");
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
fn second_delta_checkpoint_uses_inactive_slot_and_newest_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("slot-rotation.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");

        let first_page0 = read_page0(&path);
        assert!(
            read_slot_generation(&first_page0, ManifestSlot::Primary) > 0,
            "first delta publish should populate primary slot"
        );
        assert_eq!(
            read_slot_generation(&first_page0, ManifestSlot::Secondary),
            0,
            "first delta publish should leave secondary slot empty"
        );

        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    let second_page0 = read_page0(&path);
    let primary_generation = read_slot_generation(&second_page0, ManifestSlot::Primary);
    let secondary_generation = read_slot_generation(&second_page0, ManifestSlot::Secondary);
    assert!(
        primary_generation > 0 && secondary_generation > primary_generation,
        "second delta publish should rotate to the inactive secondary slot"
    );

    let db = Minigraf::open(&path).expect("database should reopen");
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta1 :name ?name]])"#),
        1,
        "first delta fact must remain visible after second delta"
    );
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta2 :name ?name]])"#),
        1,
        "newest delta fact must be visible after reopen"
    );
}

#[test]
fn second_delta_checkpoint_appends_only_pending_segment_pages() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("append-only-segment.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
    }
    let base_page_count = read_u64_le(&read_page0(&path), PAGE_COUNT_OFFSET);

    {
        let db = Minigraf::open(&path).expect("database should reopen");
        transact_many_ref_facts(&db, "bulk-delta", 1_000);
        db.checkpoint()
            .expect("large first delta checkpoint should succeed");
    }
    let first_delta_page_count = read_u64_le(&read_page0(&path), PAGE_COUNT_OFFSET);
    let first_delta_growth = first_delta_page_count.saturating_sub(base_page_count);

    {
        let db = Minigraf::open(&path).expect("database should reopen");
        db.execute(r#"(transact [[:tiny-delta :edge/to :target]])"#)
            .expect("tiny second delta transact should execute");
        db.checkpoint()
            .expect("tiny second delta checkpoint should succeed");
    }
    let second_delta_page_count = read_u64_le(&read_page0(&path), PAGE_COUNT_OFFSET);
    let second_delta_growth = second_delta_page_count.saturating_sub(first_delta_page_count);

    assert!(
        first_delta_growth > 20,
        "first large delta should occupy enough pages to prove replacement cost"
    );
    assert!(
        second_delta_growth.saturating_mul(10) < first_delta_growth,
        "second delta checkpoint should append only pending pages, not rewrite the accumulated delta"
    );
}

#[test]
fn multi_segment_checkpoint_reopen_sees_segment_to_segment_ref_edge() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("segment-ref-edge.graph");
    let source = Uuid::from_u128(0x300);
    let target = Uuid::from_u128(0x400);

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");

        db.execute(&format!(
            r#"(transact [[#uuid "{source}" :edge/to #uuid "{target}"]])"#
        ))
        .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");

        db.execute(&format!(
            r#"(transact [[#uuid "{target}" :name "target"]])"#
        ))
        .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
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
    assert_eq!(
        edge_rows.len(),
        1,
        "segment-to-segment ref edge must resolve after reopen"
    );
    match &edge_rows[0][0] {
        Value::String(name) => assert_eq!(name, "target"),
        _ => panic!("target name should be a string"),
    }
}

#[test]
fn later_delta_segment_retraction_hides_earlier_delta_assertion() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("segment-retraction.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");

        db.execute(r#"(transact [[:item :status :active]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");

        db.execute(r#"(retract [[:item :status :active]])"#)
            .expect("second delta retract should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    let db = Minigraf::open(&path).expect("database should reopen");
    assert_eq!(
        query_count(&db, r#"(query [:find ?s :where [:item :status ?s]])"#),
        0,
        "later delta retraction must hide earlier delta assertion"
    );
    assert_eq!(
        query_count(
            &db,
            r#"(query [:find ?s :as-of 2 :valid-at :any-valid-time :where [:item :status ?s]])"#,
        ),
        1,
        "earlier delta assertion must remain visible in history"
    );
}

#[test]
fn export_fact_log_preserves_multiple_delta_segments_in_tx_order() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("multi-delta-export.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");

        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");

        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    let db = Minigraf::open(&path).expect("database should reopen");
    let records = db.export_fact_log().expect("fact log should export");
    assert_eq!(
        records.len(),
        3,
        "export log must include base and both delta segments"
    );
    let tx_counts: Vec<u64> = records.iter().map(|record| record.tx_count).collect();
    assert_eq!(
        tx_counts,
        vec![1, 2, 3],
        "export log must preserve deterministic tx order across segments"
    );
    assert!(
        records.iter().all(|record| record.asserted),
        "all records in this fixture should be assertions"
    );
}

#[test]
fn corrupt_newer_header_slot_falls_back_to_previous_manifest() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("corrupt-newer-slot.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");
        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    let newest_slot = newest_manifest_slot(&read_page0(&path));
    corrupt_slot_checksum(&path, newest_slot);

    let db = Minigraf::open(&path).expect("database should reopen through older slot");
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta1 :name ?name]])"#),
        1,
        "older manifest should preserve first delta"
    );
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta2 :name ?name]])"#),
        0,
        "corrupt newer slot should not expose second delta"
    );
}

#[test]
fn corrupt_newer_manifest_payload_falls_back_to_previous_manifest() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("corrupt-newer-manifest.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");
        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    let newest_slot = newest_manifest_slot(&read_page0(&path));
    corrupt_manifest_payload(&path, newest_slot);

    let db = Minigraf::open(&path).expect("database should reopen through older manifest payload");
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta1 :name ?name]])"#),
        1,
        "older manifest should preserve first delta"
    );
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta2 :name ?name]])"#),
        0,
        "corrupt newer manifest should not expose second delta"
    );
}

#[test]
fn corrupt_newer_delta_segment_falls_back_to_previous_manifest() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("corrupt-newer-segment.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");
        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    corrupt_last_delta_segment(&path);

    let db = Minigraf::open(&path).expect("database should reopen through older delta segment");
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta1 :name ?name]])"#),
        1,
        "older segment should preserve first delta"
    );
    assert_eq!(
        query_count(&db, r#"(query [:find ?name :where [:delta2 :name ?name]])"#),
        0,
        "corrupt newer segment should not expose second delta"
    );
}

#[test]
fn corrupt_older_segment_in_selected_multi_segment_manifest_errors() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("corrupt-selected-older-segment.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");
        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    corrupt_first_delta_segment(&path);
    let reopened = Minigraf::open(&path);
    assert!(
        reopened.is_err(),
        "corrupt older segment referenced by the selected manifest must not open cleanly"
    );
}

#[test]
fn both_manifest_slots_invalid_with_committed_delta_errors() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("both-slots-invalid.graph");

    {
        let db = Minigraf::open(&path).expect("database should open");
        db.execute(r#"(transact [[:base :kind :root]])"#)
            .expect("base transact should execute");
        db.checkpoint().expect("base checkpoint should succeed");
        db.execute(r#"(transact [[:delta1 :name "Delta 1"]])"#)
            .expect("first delta transact should execute");
        db.checkpoint()
            .expect("first delta checkpoint should succeed");
        db.execute(r#"(transact [[:delta2 :name "Delta 2"]])"#)
            .expect("second delta transact should execute");
        db.checkpoint()
            .expect("second delta checkpoint should succeed");
    }

    corrupt_slot_checksum(&path, ManifestSlot::Primary);
    corrupt_slot_checksum(&path, ManifestSlot::Secondary);

    let reopened = Minigraf::open(&path);
    assert!(
        reopened.is_err(),
        "both invalid manifest slots must not silently open base-only"
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
