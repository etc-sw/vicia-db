use anyhow::{Result, bail};
use minigraf::{
    CurrentEntitiesRequest, CurrentRefsRequest, Minigraf, OpenOptions, ReadViewOptions, Value,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

const BATCH_SIZE: usize = 1_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadReceipt {
    rows: usize,
    p50_ms: f64,
    p95_ms: f64,
    diagnostics: minigraf::LeafReadDiagnostics,
}

fn main() -> Result<()> {
    let profile = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "smoke".to_owned());
    let facts = match profile.as_str() {
        "smoke" => 10_000,
        "full" => 1_000_000,
        _ => bail!("profile must be smoke or full"),
    };
    let samples = if profile == "full" { 20 } else { 5 };
    let path = benchmark_graph_path(&profile);
    remove_benchmark_graph(&path)?;
    build_fixture(&path, facts)?;
    let db = open(&path)?;
    db.set_leaf_read_diagnostics_enabled(true);
    let selected = facts / 2;
    let source = source_id(selected);
    let target = target_id(selected);

    let mut entity_samples = Vec::with_capacity(samples);
    let mut entity_rows = Vec::new();
    for _ in 0..samples {
        let view = db.read_view(ReadViewOptions::default())?;
        let started = Instant::now();
        entity_rows = view.current_entities(CurrentEntitiesRequest {
            ids: &[source],
            attributes: &[":bench/ref"],
            limit: 4,
        })?;
        entity_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    if !matches!(entity_rows.as_slice(), [row] if row.value == Value::Ref(target)) {
        bail!("typed entity read mismatch")
    }
    let entity_diagnostics = db.last_leaf_read_diagnostics();

    let mut ref_samples = Vec::with_capacity(samples);
    let mut ref_rows = Vec::new();
    for _ in 0..samples {
        let view = db.read_view(ReadViewOptions::default())?;
        let started = Instant::now();
        ref_rows = view.refs_to(CurrentRefsRequest {
            attribute: ":bench/ref",
            value: target,
            limit: 4,
        })?;
        ref_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    if ref_rows != vec![source] {
        bail!("typed reverse-reference read mismatch")
    }
    let ref_diagnostics = db.last_leaf_read_diagnostics();
    db.set_leaf_read_diagnostics_enabled(false);

    entity_samples.sort_by(f64::total_cmp);
    ref_samples.sort_by(f64::total_cmp);
    let reads = serde_json::json!({
        "entities": ReadReceipt {
            rows: entity_rows.len(),
            p50_ms: percentile(&entity_samples, 50),
            p95_ms: percentile(&entity_samples, 95),
            diagnostics: entity_diagnostics,
        },
        "refsTo": ReadReceipt {
            rows: ref_rows.len(),
            p50_ms: percentile(&ref_samples, 50),
            p95_ms: percentile(&ref_samples, 95),
            diagnostics: ref_diagnostics,
        }
    });
    let receipt = serde_json::json!({
        "schema": "vicia.current-reader.v1",
        "profile": profile,
        "facts": facts,
        "samples": samples,
        "passed": true,
        "reads": reads,
        "provenance": provenance(),
    });
    let output = std::env::var("VICIA_CURRENT_READER_RECEIPT")
        .unwrap_or_else(|_| "target/current-reader/receipt.json".to_owned());
    if let Some(parent) = Path::new(&output).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &output,
        format!("{}\n", serde_json::to_string_pretty(&receipt)?),
    )?;
    drop(db);
    remove_benchmark_graph(&path)?;
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

fn benchmark_graph_path(profile: &str) -> PathBuf {
    Path::new("target/current-reader").join(format!("work-{profile}-{}.graph", std::process::id()))
}

fn remove_benchmark_graph(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        path.with_extension("graph.wal"),
        path.with_extension("graph.lock"),
    ] {
        match std::fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn build_fixture(path: &Path, facts: usize) -> Result<()> {
    let db = open(path)?;
    for start in (0..facts).step_by(BATCH_SIZE) {
        let end = start.saturating_add(BATCH_SIZE).min(facts);
        let mut command = String::from("(transact [");
        for index in start..end {
            command.push_str(&format!(
                "[#uuid \"{}\" :bench/ref #uuid \"{}\"]",
                source_id(index),
                target_id(index)
            ));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    Ok(())
}

fn open(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..Default::default()
    }
    .path(path)
    .open()
}

fn source_id(index: usize) -> Uuid {
    Uuid::from_u128(index as u128 + 1)
}

fn target_id(index: usize) -> Uuid {
    Uuid::from_u128((1_u128 << 127) | (index as u128 + 1))
}

fn percentile(samples: &[f64], percentile: usize) -> f64 {
    let index = (samples.len().saturating_sub(1) * percentile).div_ceil(100);
    samples.get(index).copied().unwrap_or_default()
}

fn provenance() -> serde_json::Value {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|output| !output.stdout.is_empty());
    serde_json::json!({ "sourceCommit": commit, "sourceDirty": dirty })
}
