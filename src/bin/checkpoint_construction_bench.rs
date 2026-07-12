#![cfg(not(target_arch = "wasm32"))]

use anyhow::{Context, Result, bail};
use minigraf::{CheckpointConstructionDiagnostics, Minigraf, OpenOptions, QueryResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "vicia.checkpoint-construction.v1";
const PENDING: &[u64] = &[1, 10, 100, 1_000];

#[derive(Clone, Copy)]
enum Profile {
    Smoke,
    Full,
}
impl Profile {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "full" => Ok(Self::Full),
            _ => bail!("profile must be smoke or full"),
        }
    }
    fn base_facts(self) -> u64 {
        match self {
            Self::Smoke => 10_000,
            Self::Full => 1_000_000,
        }
    }
    fn repetitions(self) -> usize {
        match self {
            Self::Smoke => 5,
            Self::Full => 20,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Full => "full",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    profile: &'static str,
    base_facts: u64,
    repetitions: usize,
    generated_at_unix_ms: u128,
    source_commit: String,
    tracked_clean: bool,
    variants: Vec<Variant>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Variant {
    pending_facts: u64,
    samples: Vec<Sample>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    checkpoint_elapsed_ms: f64,
    checkpoint_baseline_rss_bytes: u64,
    checkpoint_peak_rss_bytes: u64,
    checkpoint_delta_rss_bytes: u64,
    recompact_elapsed_ms: f64,
    recompact_baseline_rss_bytes: u64,
    recompact_peak_rss_bytes: u64,
    recompact_delta_rss_bytes: u64,
    graph_bytes: u64,
    count: u64,
    checksum: i128,
    diagnostics: CheckpointConstructionDiagnostics,
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, profile, output] if command == "run" => {
            run(Profile::parse(profile)?, Path::new(output))
        }
        [command, path, facts] if command == "base" => build_base(Path::new(path), facts.parse()?),
        [command, base, sample, base_facts, pending] if command == "sample" => {
            println!(
                "{}",
                serde_json::to_string(&measure_sample(
                    Path::new(base),
                    Path::new(sample),
                    base_facts.parse()?,
                    pending.parse()?
                )?)?
            );
            Ok(())
        }
        _ => bail!("usage: checkpoint-construction-bench run <smoke|full> <output-dir>"),
    }
}

fn run(profile: Profile, output: &Path) -> Result<()> {
    if output.exists() {
        fs::remove_dir_all(output)?;
    }
    fs::create_dir_all(output)?;
    let executable = std::env::current_exe()?;
    let base = output.join("base.graph");
    child_status(
        &executable,
        &["base", text_path(&base)?, &profile.base_facts().to_string()],
    )?;
    let mut variants = Vec::new();
    for pending in PENDING {
        let mut samples = Vec::new();
        for repetition in 0..profile.repetitions() {
            eprintln!(
                "checkpoint-construction: pending-{pending} {}/{}",
                repetition + 1,
                profile.repetitions()
            );
            let sample_path = output.join(format!("pending-{pending}.graph"));
            samples.push(child_json(
                &executable,
                &[
                    "sample",
                    text_path(&base)?,
                    text_path(&sample_path)?,
                    &profile.base_facts().to_string(),
                    &pending.to_string(),
                ],
            )?);
        }
        variants.push(Variant {
            pending_facts: *pending,
            samples,
        });
    }
    let receipt = Receipt {
        schema: SCHEMA,
        profile: profile.name(),
        base_facts: profile.base_facts(),
        repetitions: profile.repetitions(),
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        source_commit: command_text("git", &["rev-parse", "HEAD"])?,
        tracked_clean: command_text("git", &["status", "--short", "--untracked-files=no"])?
            .is_empty(),
        variants,
    };
    fs::write(
        output.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(())
}

fn build_base(path: &Path, facts: u64) -> Result<()> {
    remove_graph(path);
    let db = Minigraf::open_with_options(
        path,
        OpenOptions {
            wal_checkpoint_threshold: usize::MAX,
            ..OpenOptions::default()
        },
    )?;
    for start in (0..facts).step_by(1_000) {
        let mut command = String::from("(transact [");
        for entity in start..(start + 1_000).min(facts) {
            command.push_str(&format!(
                "[:checkpoint/e{entity} :checkpoint/value {entity}]"
            ));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    Ok(())
}

fn measure_sample(base: &Path, path: &Path, base_facts: u64, pending: u64) -> Result<Sample> {
    remove_graph(path);
    fs::copy(base, path)?;
    fs::OpenOptions::new().write(true).open(path)?.sync_all()?;
    let db = Minigraf::open_with_options(
        path,
        OpenOptions {
            wal_checkpoint_threshold: usize::MAX,
            ..OpenOptions::default()
        },
    )?;
    let mut command = String::from("(transact [");
    for index in 0..pending {
        command.push_str(&format!(
            "[:checkpoint/p{index} :checkpoint/pending {index}]"
        ));
    }
    command.push_str("])");
    db.execute(&command)?;
    let checkpoint = sampled(|| db.checkpoint())?;
    let recompact = sampled(|| db.benchmark_recompact_visible_delta())?;
    let diagnostics = db.checkpoint_construction_diagnostics();
    let (count, checksum) = aggregate(&db)?;
    if count != base_facts {
        bail!("base count mismatch")
    }
    Ok(Sample {
        checkpoint_elapsed_ms: checkpoint.0,
        checkpoint_baseline_rss_bytes: checkpoint.1,
        checkpoint_peak_rss_bytes: checkpoint.2,
        checkpoint_delta_rss_bytes: checkpoint.2.saturating_sub(checkpoint.1),
        recompact_elapsed_ms: recompact.0,
        recompact_baseline_rss_bytes: recompact.1,
        recompact_peak_rss_bytes: recompact.2,
        recompact_delta_rss_bytes: recompact.2.saturating_sub(recompact.1),
        graph_bytes: fs::metadata(path)?.len(),
        count,
        checksum,
        diagnostics,
    })
}

fn aggregate(db: &Minigraf) -> Result<(u64, i128)> {
    let QueryResult::QueryResults { results, .. } =
        db.execute("(query [:find (count ?v) (sum ?v) :where [?e :checkpoint/value ?v]])")?
    else {
        bail!("aggregate returned non-query result")
    };
    let row = results.first().context("aggregate returned no row")?;
    Ok((
        u64::try_from(
            row.first()
                .and_then(|v| v.as_integer())
                .context("count missing")?,
        )?,
        i128::from(
            row.get(1)
                .and_then(|v| v.as_integer())
                .context("sum missing")?,
        ),
    ))
}

fn sampled<T>(operation: impl FnOnce() -> Result<T>) -> Result<(f64, u64, u64)> {
    let baseline = rss_bytes()?;
    let running = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicU64::new(baseline));
    let r = running.clone();
    let p = peak.clone();
    let sampler = std::thread::spawn(move || {
        while r.load(Ordering::Relaxed) {
            if let Ok(value) = rss_bytes() {
                p.fetch_max(value, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    let started = Instant::now();
    operation()?;
    let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
    running.store(false, Ordering::SeqCst);
    sampler
        .join()
        .map_err(|_| anyhow::anyhow!("RSS sampler panicked"))?;
    Ok((elapsed, baseline, peak.load(Ordering::SeqCst)))
}

fn remove_graph(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(format!("{}.wal", path.display()));
}
fn text_path(path: &Path) -> Result<&str> {
    path.to_str().context("non-UTF8 path")
}
fn child_status(executable: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(executable).args(args).output()?;
    if !output.status.success() {
        bail!("child failed: {}", String::from_utf8_lossy(&output.stderr))
    }
    Ok(())
}
fn child_json<T: for<'de> Deserialize<'de>>(executable: &Path, args: &[&str]) -> Result<T> {
    let output = Command::new(executable).args(args).output()?;
    if !output.status.success() {
        bail!("child failed: {}", String::from_utf8_lossy(&output.stderr))
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn command_text(command: &str, args: &[&str]) -> Result<String> {
    Ok(
        String::from_utf8(Command::new(command).args(args).output()?.stdout)?
            .trim()
            .to_owned(),
    )
}
fn rss_bytes() -> Result<u64> {
    let status = fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .context("VmRSS missing")?;
    Ok(line
        .split_whitespace()
        .nth(1)
        .context("VmRSS value missing")?
        .parse::<u64>()?
        .saturating_mul(1024))
}
