//! Generates a checkpointed `.graph` fixture of N facts for the A0
//! browser open-at-scale runner (docs/APP_ADOPTION_GAP_PLAN.md).
//!
//!   cargo run --release --example generate_bench_fixture -- <facts> <out.graph>
//!
//! Fact shape matches the delta/cadence benchmark base (`:bench/base-{i}`
//! cycling ref/value/keyword/flag) so browser numbers are comparable with
//! the native suites. Output is fully checkpointed with no WAL sidecar.

// wasm-pack compiles examples for the browser target; provide a no-op entry
// point so the example compiles cleanly.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    use uuid::Uuid;

    let mut args = std::env::args().skip(1);
    let facts: usize = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: generate_bench_fixture <facts> <out.graph>"))?
        .parse()?;
    let out = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: generate_bench_fixture <facts> <out.graph>"))?;

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(format!("{out}.wal"));

    let db = minigraf::OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..Default::default()
    }
    .path(&out)
    .open()?;

    const BATCH: usize = 1_000;
    for batch_start in (0..facts).step_by(BATCH) {
        let batch_end = (batch_start + BATCH).min(facts);
        let mut command = String::from("(transact [");
        for index in batch_start..batch_end {
            let entity = format!(":bench/base-{index}");
            if index % 4 == 0 {
                let target = Uuid::from_u128(index as u128 + 1);
                command.push_str(&format!(r#"[{entity} :bench/ref #uuid "{target}"]"#));
            } else if index % 4 == 1 {
                command.push_str(&format!("[{entity} :bench/value {index}]"));
            } else if index % 4 == 2 {
                command.push_str(&format!("[{entity} :bench/state :bench/state-{index}]"));
            } else {
                command.push_str(&format!("[{entity} :bench/flag true]"));
            }
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    drop(db);

    let _ = std::fs::remove_file(format!("{out}.wal"));
    let len = std::fs::metadata(&out)?.len();
    println!("Written: {out} ({facts} facts, {len} bytes)");
    Ok(())
}
