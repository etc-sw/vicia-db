//! Public-API corruption gates for the v11 generation-bound page catalog.
#![cfg(not(target_arch = "wasm32"))]

use minigraf::db::Minigraf;

const PAGE_SIZE: usize = 4096;
const HEADER_CHECKSUM_OFFSET: usize = 80;
const EAVT_ROOT_OFFSET: usize = 32;
const HEADER_EXTENSION_OFFSET: usize = 84;
const HEADER_EXTENSION_PREFIX_LEN: usize = 12;
const MANIFEST_SLOTS_LEN: usize = 80;
const BASE_FACT_PAGE_START_OFFSET: usize =
    HEADER_EXTENSION_OFFSET + HEADER_EXTENSION_PREFIX_LEN + MANIFEST_SLOTS_LEN;
const LEGACY_HEADER_EXTENSION_LEN: usize = HEADER_EXTENSION_PREFIX_LEN + MANIFEST_SLOTS_LEN + 12;
const BASE_INTEGRITY_DESCRIPTOR_OFFSET: usize =
    HEADER_EXTENSION_OFFSET + LEGACY_HEADER_EXTENSION_LEN;
const CATALOG_PAGE_START_OFFSET: usize = BASE_INTEGRITY_DESCRIPTOR_OFFSET + 24;

fn build_valid_db(path: &std::path::Path) {
    let db = Minigraf::open(path).unwrap();
    db.execute(r#"(transact [[:alice :idx "source-a"]])"#)
        .unwrap();
    db.checkpoint().unwrap();
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn flip_page_padding(path: &std::path::Path, page_id: u64) {
    let mut bytes = std::fs::read(path).unwrap();
    let offset = usize::try_from(page_id).unwrap() * PAGE_SIZE + PAGE_SIZE - 1;
    bytes[offset] ^= 0x01;
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn corrupted_header_checksum_is_rejected_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt-header.graph");
    build_valid_db(&path);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[HEADER_CHECKSUM_OFFSET] ^= 0x01;
    std::fs::write(&path, bytes).unwrap();

    assert!(
        Minigraf::open(&path).is_err(),
        "corrupt page-0 checksum must reject open"
    );
}

#[test]
fn corrupted_fact_page_fails_on_first_public_query() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt-fact.graph");
    build_valid_db(&path);
    let page0 = std::fs::read(&path).unwrap();
    let fact_page = read_u64(&page0, BASE_FACT_PAGE_START_OFFSET);
    flip_page_padding(&path, fact_page);

    let db = Minigraf::open(&path).expect("v11 open must not scan base fact pages");
    assert!(
        db.execute("(query [:find ?v :where [:alice :idx ?v]])")
            .is_err(),
        "selective query must propagate fact-page checksum failure"
    );
}

#[test]
fn corrupted_index_root_fails_on_first_public_query() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt-index.graph");
    build_valid_db(&path);
    let page0 = std::fs::read(&path).unwrap();
    let eavt_root = read_u64(&page0, EAVT_ROOT_OFFSET);
    flip_page_padding(&path, eavt_root);

    let db = Minigraf::open(&path).expect("v11 open must not scan base index pages");
    assert!(
        db.execute("(query [:find ?v :where [:alice :idx ?v]])")
            .is_err(),
        "selective query must propagate index-page checksum failure"
    );
}

#[test]
fn corrupted_catalog_payload_is_rejected_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt-catalog.graph");
    build_valid_db(&path);
    let page0 = std::fs::read(&path).unwrap();
    let catalog_page = read_u64(&page0, CATALOG_PAGE_START_OFFSET);
    flip_page_padding(&path, catalog_page);

    assert!(
        Minigraf::open(&path).is_err(),
        "catalog corruption must reject open before a handle is exposed"
    );
}
