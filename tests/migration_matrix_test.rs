//! Migration matrix tests (#215).
#![cfg(not(target_arch = "wasm32"))]

use minigraf::QueryResult;
use minigraf::db::Minigraf;
use std::io::Write;
use std::ops::Range;

const PAGE_SIZE: usize = 4096;
const MAGIC_NUMBER: [u8; 4] = *b"MGRF";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn count_results(r: QueryResult) -> TestResult<usize> {
    match r {
        QueryResult::QueryResults { results, .. } => Ok(results.len()),
        _ => Err(std::io::Error::other("expected query results").into()),
    }
}

fn write_range(page: &mut [u8], range: Range<usize>, bytes: &[u8]) -> TestResult {
    let slot = page
        .get_mut(range)
        .ok_or_else(|| std::io::Error::other("test page range must exist"))?;
    if slot.len() != bytes.len() {
        return Err(std::io::Error::other("test page range length mismatch").into());
    }
    slot.copy_from_slice(bytes);
    Ok(())
}

fn read_u32_le(bytes: &[u8], range: Range<usize>) -> TestResult<u32> {
    let slot = bytes
        .get(range)
        .ok_or_else(|| std::io::Error::other("test header range must exist"))?;
    Ok(u32::from_le_bytes(slot.try_into()?))
}

#[test]
fn current_format_round_trip_is_idempotent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.graph");
    {
        let db = Minigraf::open(&path)?;
        db.execute(r#"(transact [[:e1 :name "Alice"]])"#)?;
        db.checkpoint()?;
    }
    let db2 = Minigraf::open(&path)?;
    let n = count_results(db2.execute("(query [:find ?n :where [?e :name ?n]])")?)?;
    assert_eq!(n, 1, "round-trip: Alice must survive close/reopen");
    Ok(())
}

#[test]
fn v7_fixture_migrates_to_current_index_format() -> TestResult {
    let fixture: &[u8] = include_bytes!("fixtures/compat.graph");
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fixture-v7.graph");
    std::fs::write(&path, fixture)?;

    let db = Minigraf::open(&path)?;
    let n = count_results(db.execute("(query [:find ?name :where [?e :name ?name]])")?)?;
    assert_eq!(n, 1, "v7 fixture fact must remain queryable");
    drop(db);

    let raw = std::fs::read(&path)?;
    let version = read_u32_le(&raw, 4..8)?;
    assert_eq!(version, 11, "v7 fixture must migrate to current format");
    Ok(())
}

#[test]
fn v3_empty_migrates_without_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("v3.graph");
    // Build a minimally valid v3 header: magic + version=3 + page_count=1
    // All other fields zero (roots=0 means empty index, fact_count=0).
    let mut page = vec![0u8; PAGE_SIZE];
    write_range(&mut page, 0..4, &MAGIC_NUMBER)?;
    write_range(&mut page, 4..8, &3u32.to_le_bytes())?; // version
    write_range(&mut page, 8..16, &1u64.to_le_bytes())?; // page_count = 1
    let mut f = std::fs::File::create(&path)?;
    f.write_all(&page)?;
    drop(f);
    Minigraf::open(&path)?;
    Ok(())
}

#[test]
fn corrupt_magic_fails_loudly() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("corrupt.graph");
    let mut page = vec![0u8; PAGE_SIZE];
    write_range(&mut page, 0..4, b"XXXX")?;
    write_range(&mut page, 4..8, &7u32.to_le_bytes())?;
    let mut f = std::fs::File::create(&path)?;
    f.write_all(&page)?;
    let result = Minigraf::open(&path);
    let msg = match result {
        Ok(_) => return Err(std::io::Error::other("corrupt magic must produce an error").into()),
        Err(err) => err.to_string(),
    };
    assert!(
        msg.contains("magic") || msg.contains("invalid") || msg.contains("not a"),
        "error message must describe the corrupt magic"
    );
    Ok(())
}

#[test]
fn unsupported_version_fails_loudly() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("future.graph");
    let mut page = vec![0u8; PAGE_SIZE];
    write_range(&mut page, 0..4, &MAGIC_NUMBER)?;
    write_range(&mut page, 4..8, &99u32.to_le_bytes())?;
    let mut f = std::fs::File::create(&path)?;
    f.write_all(&page)?;
    let result = Minigraf::open(&path);
    if result.is_ok() {
        return Err(std::io::Error::other("unsupported version must produce an error").into());
    }
    Ok(())
}

#[test]
fn wal_replay_after_migration_is_idempotent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("replay.graph");
    {
        let db = Minigraf::open(&path)?;
        db.execute(r#"(transact [[:e1 :color "red"]])"#)?;
        db.checkpoint()?;
    }
    {
        let db = Minigraf::open(&path)?;
        db.execute(r#"(transact [[:e2 :color "blue"]])"#)?;
        std::mem::forget(db);
    }
    let db3 = Minigraf::open(&path)?;
    let n = count_results(db3.execute("(query [:find ?c :where [?e :color ?c]])")?)?;
    assert_eq!(n, 2, "WAL replay after checkpoint must be idempotent");
    Ok(())
}
