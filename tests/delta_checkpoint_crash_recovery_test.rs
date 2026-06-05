#![cfg(not(target_arch = "wasm32"))]

use minigraf::{Minigraf, OpenOptions, QueryResult};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const PAGE_SIZE: usize = 4096;
const DELTA_SEGMENT_MAGIC: &[u8] = b"MGDSG001";
const DELTA_MANIFEST_MAGIC: &[u8] = b"MGDMF001";

struct DeltaCheckpointImage {
    base_page0: Vec<u8>,
    wal_backup: Vec<u8>,
}

fn open_no_auto_checkpoint(path: &Path) -> Minigraf {
    Minigraf::open_with_options(
        path,
        OpenOptions {
            wal_checkpoint_threshold: usize::MAX,
            ..Default::default()
        },
    )
    .expect("database should open")
}

fn wal_path_for(db_path: &Path) -> PathBuf {
    let mut wal_path = db_path.as_os_str().to_owned();
    wal_path.push(".wal");
    PathBuf::from(wal_path)
}

fn query_count(db: &Minigraf, query: &str) -> usize {
    match db.execute(query).expect("query should execute") {
        QueryResult::QueryResults { results, .. } => results.len(),
        _ => panic!("expected query results"),
    }
}

fn read_page0(path: &Path) -> Vec<u8> {
    let mut page = vec![0; PAGE_SIZE];
    let mut file = std::fs::File::open(path).expect("database file should open");
    file.read_exact(&mut page)
        .expect("page 0 should read exactly");
    page
}

fn write_page0(path: &Path, page: &[u8]) {
    assert_eq!(page.len(), PAGE_SIZE, "page 0 image must be one page");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("database file should open for page 0 restore");
    file.seek(SeekFrom::Start(0))
        .expect("database file should seek to page 0");
    file.write_all(page)
        .expect("page 0 restore should write exactly");
    file.sync_all().expect("page 0 restore should sync");
}

fn find_marker_offset(path: &Path, marker: &[u8]) -> usize {
    let bytes = std::fs::read(path).expect("database file should read");
    bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("marker should exist in database file")
}

fn assert_marker_present(path: &Path, marker: &[u8]) {
    let bytes = std::fs::read(path).expect("database file should read");
    assert!(
        bytes.windows(marker.len()).any(|window| window == marker),
        "expected marker should be present"
    );
}

fn corrupt_marker(path: &Path, marker: &[u8]) {
    let mut bytes = std::fs::read(path).expect("database file should read");
    let offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("marker should exist before corruption");
    bytes[offset] ^= 0x01;
    std::fs::write(path, bytes).expect("database file should write");
}

fn restore_wal(path: &Path, wal_backup: &[u8]) {
    let wal_path = wal_path_for(path);
    std::fs::write(wal_path, wal_backup).expect("WAL backup should restore");
}

fn truncate_inside_marker(path: &Path, marker: &[u8]) {
    let offset = find_marker_offset(path, marker);
    let truncated_len = offset.saturating_add(marker.len() / 2);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("database file should open for truncate");
    file.set_len(u64::try_from(truncated_len).expect("truncate length should fit u64"))
        .expect("database file should truncate");
}

fn prepare_delta_checkpoint_image(path: &Path) -> DeltaCheckpointImage {
    let wal_path = wal_path_for(path);
    let db = open_no_auto_checkpoint(path);

    db.execute(r#"(transact [[:base :name "Base"]])"#)
        .expect("base transact should execute");
    db.checkpoint().expect("base checkpoint should succeed");
    let base_page0 = read_page0(path);

    db.execute(r#"(transact [[:delta :name "Delta"]])"#)
        .expect("delta transact should execute");
    let wal_backup = std::fs::read(&wal_path).expect("WAL should exist before delta checkpoint");
    db.checkpoint().expect("delta checkpoint should succeed");
    assert!(
        !wal_path.exists(),
        "WAL must be retired after delta checkpoint"
    );
    drop(db);

    assert_marker_present(path, DELTA_SEGMENT_MAGIC);
    assert_marker_present(path, DELTA_MANIFEST_MAGIC);
    DeltaCheckpointImage {
        base_page0,
        wal_backup,
    }
}

fn assert_base_and_delta_visible_once(db: &Minigraf) {
    let base_count = query_count(db, r#"(query [:find ?name :where [:base :name ?name]])"#);
    let delta_count = query_count(db, r#"(query [:find ?name :where [:delta :name ?name]])"#);
    assert_eq!(base_count, 1, "base fact must be visible once");
    assert_eq!(delta_count, 1, "delta fact must be visible once");

    let records = db.export_fact_log().expect("fact log should export");
    assert_eq!(records.len(), 2, "fact log must not duplicate WAL entries");
}

#[test]
fn pre_header_crash_replays_wal_and_ignores_unpublished_delta() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("pre-header.graph");
    let image = prepare_delta_checkpoint_image(&path);

    write_page0(&path, &image.base_page0);
    restore_wal(&path, &image.wal_backup);

    let db = open_no_auto_checkpoint(&path);
    assert_base_and_delta_visible_once(&db);
}

#[test]
fn post_header_pre_wal_delete_crash_skips_stale_wal() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("post-header.graph");
    let image = prepare_delta_checkpoint_image(&path);

    restore_wal(&path, &image.wal_backup);

    let db = open_no_auto_checkpoint(&path);
    assert_base_and_delta_visible_once(&db);
}

#[test]
fn unpublished_corrupt_delta_pages_do_not_block_wal_recovery() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("unpublished-corrupt.graph");
    let image = prepare_delta_checkpoint_image(&path);

    write_page0(&path, &image.base_page0);
    corrupt_marker(&path, DELTA_SEGMENT_MAGIC);
    restore_wal(&path, &image.wal_backup);

    let db = open_no_auto_checkpoint(&path);
    assert_base_and_delta_visible_once(&db);
}

#[test]
fn selected_corrupt_delta_errors_even_if_wal_exists() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("selected-corrupt.graph");
    let image = prepare_delta_checkpoint_image(&path);

    corrupt_marker(&path, DELTA_SEGMENT_MAGIC);
    restore_wal(&path, &image.wal_backup);

    let reopened = Minigraf::open_with_options(
        &path,
        OpenOptions {
            wal_checkpoint_threshold: usize::MAX,
            ..Default::default()
        },
    );
    assert!(
        reopened.is_err(),
        "selected corrupt delta must not silently recover from WAL"
    );
}

#[test]
fn selected_truncated_delta_errors_even_if_wal_exists() {
    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("selected-truncated.graph");
    let image = prepare_delta_checkpoint_image(&path);

    truncate_inside_marker(&path, DELTA_SEGMENT_MAGIC);
    restore_wal(&path, &image.wal_backup);

    let reopened = Minigraf::open_with_options(
        &path,
        OpenOptions {
            wal_checkpoint_threshold: usize::MAX,
            ..Default::default()
        },
    );
    assert!(
        reopened.is_err(),
        "selected truncated delta must not silently recover from WAL"
    );
}
