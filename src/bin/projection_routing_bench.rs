use anyhow::{Context, Result, bail};
use minigraf::{
    InteractiveLedger, MaintenanceLedger, ProjectionReadDiagnostics, QueryResult, ReadViewOptions,
    Value,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

const ATTRIBUTE: &str = ":projection/value";
const QUERY: &str =
    "(query [:find (count ?value) (sum ?value) :where [?entity :projection/value ?value]])";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureReceipt {
    schema: String,
    facts: u64,
    fill_percent: u8,
    bytes: u64,
    sha256: String,
    format_version: u32,
    builder_source_commit: String,
    builder_tracked_clean: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimingSummary {
    samples_ms: Vec<f64>,
    p50_ms: f64,
    p95_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Gates {
    exact: bool,
    routed: bool,
    page_backed: bool,
    route_p50: bool,
    improvement: bool,
    tail: bool,
    query_rss: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    profile: String,
    facts: u64,
    trials: usize,
    admission_eligible: bool,
    source_commit: String,
    tracked_clean: bool,
    fixture: FixtureReceipt,
    expected_count: u64,
    expected_checksum: i128,
    ledger_count: u64,
    ledger_checksum: i128,
    projected_count: u64,
    projected_checksum: i128,
    maintenance_elapsed_ms: f64,
    ledger: TimingSummary,
    projected: TimingSummary,
    projection_diagnostics: ProjectionReadDiagnostics,
    baseline_rss_bytes: u64,
    peak_query_rss_delta_bytes: u64,
    gates: Gates,
    admitted: bool,
    default_write_format_changed: bool,
    arbitrary_datalog_routing_changed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TailGates {
    exact: bool,
    routed: bool,
    page_backed: bool,
    p50: bool,
    no_tail_relative: bool,
    tail: bool,
    query_rss: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TailReceipt {
    schema: &'static str,
    profile: String,
    facts: u64,
    tail_facts: u64,
    trials: usize,
    admission_eligible: bool,
    source_commit: String,
    tracked_clean: bool,
    fixture: FixtureReceipt,
    expected_count: u64,
    expected_checksum: i128,
    observed_count: u64,
    observed_checksum: i128,
    refresh_elapsed_ms: f64,
    no_tail: TimingSummary,
    resident_tail: TimingSummary,
    refresh_diagnostics: ProjectionReadDiagnostics,
    cached_diagnostics: ProjectionReadDiagnostics,
    peak_query_rss_delta_bytes: u64,
    gates: TailGates,
    admitted: bool,
    file_format_changed: bool,
    public_api_changed: bool,
    arbitrary_datalog_routing_changed: bool,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if let [_, mode, source, published, fixture, facts, profile] = args.as_slice()
        && mode == "tail"
    {
        return run_tail(
            Path::new(source),
            Path::new(published),
            Path::new(fixture),
            facts.parse()?,
            profile,
        );
    }
    let [_, source, published, fixture, facts, profile] = args.as_slice() else {
        bail!(
            "usage: projection-routing-bench [tail] <source> <published> <fixture> <facts> <smoke|full>"
        )
    };
    run(
        Path::new(source),
        Path::new(published),
        Path::new(fixture),
        facts.parse()?,
        profile,
    )
}

fn run_tail(
    source: &Path,
    published: &Path,
    fixture_path: &Path,
    facts: u64,
    profile: &str,
) -> Result<()> {
    const TAIL_FACTS: u64 = 1_024;
    let trials = match profile {
        "smoke" => 3,
        "full" => 10,
        _ => bail!("profile must be smoke or full"),
    };
    let fixture: FixtureReceipt = serde_json::from_slice(&fs::read(fixture_path)?)?;
    if fixture.facts != facts || fixture.fill_percent != 90 {
        bail!("fixture provenance does not match the tail benchmark")
    }
    let source_commit = command_text("git", &["rev-parse", "HEAD"])?;
    if fixture.builder_source_commit != source_commit {
        bail!("fixture was not built by the current source commit")
    }
    let tracked_clean =
        command_text("git", &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
    if published.exists() {
        fs::remove_file(published)?;
    }
    fs::copy(source, published)?;
    let maintenance = MaintenanceLedger::open(published)?;
    maintenance.rebuild_current_projections(&[ATTRIBUTE.to_owned()])?;
    drop(maintenance);

    let ledger = InteractiveLedger::open(published)?;
    let max_work = usize::try_from(facts)?;
    let (no_tail, base_count, base_checksum, _) = measure(&ledger, max_work, trials, true)?;
    let mut command = String::from("(transact [");
    for offset in 0..TAIL_FACTS {
        let entity =
            uuid::Uuid::from_u128(u128::from(facts.saturating_add(offset).saturating_add(1)));
        let value = facts.saturating_add(offset);
        command.push_str(&format!("[#uuid \"{entity}\" {ATTRIBUTE} {value}]"));
    }
    command.push_str("])");
    ledger.execute_write(&command)?;

    let peak_before = peak_rss_bytes().context("read resident-tail peak RSS baseline")?;
    let refresh_started = Instant::now();
    let refresh_view = ledger.read_view(ReadViewOptions::default())?;
    let refresh_pair = aggregate_pair(refresh_view.query(QUERY, max_work)?)?;
    let refresh_elapsed_ms = refresh_started.elapsed().as_secs_f64() * 1_000.0;
    let refresh_diagnostics = refresh_view.last_projection_read_diagnostics();
    let (resident_tail, observed_count, observed_checksum, cached_diagnostics) =
        measure(&ledger, max_work, trials, true)?;
    let peak_query_rss_delta_bytes = peak_rss_bytes()
        .context("read resident-tail peak RSS")?
        .saturating_sub(peak_before);

    let tail_checksum = (facts..facts.saturating_add(TAIL_FACTS))
        .map(i128::from)
        .sum::<i128>();
    let expected_count = base_count.saturating_add(TAIL_FACTS);
    let expected_checksum = base_checksum.saturating_add(tail_checksum);
    let exact = refresh_pair == (expected_count, expected_checksum)
        && (observed_count, observed_checksum) == (expected_count, expected_checksum);
    let routed = refresh_diagnostics.completed_scans == 1
        && refresh_diagnostics.ledger_fallbacks == 0
        && refresh_diagnostics.tail_refreshes == 1
        && refresh_diagnostics.tail_facts_visited == TAIL_FACTS
        && cached_diagnostics.tail_cache_hits == 1;
    let page_backed = refresh_diagnostics.pages_read > 0
        && refresh_diagnostics.full_image_decodes == 0
        && cached_diagnostics.full_image_decodes == 0;
    let p50 = resident_tail.p50_ms <= 230.0;
    let no_tail_relative = resident_tail.p50_ms <= no_tail.p50_ms * 1.15;
    let tail = resident_tail.p95_ms <= resident_tail.p50_ms * 1.15;
    let query_rss = peak_query_rss_delta_bytes <= 2 * 1024 * 1024;
    let gates = TailGates {
        exact,
        routed,
        page_backed,
        p50,
        no_tail_relative,
        tail,
        query_rss,
    };
    let admission_eligible = profile == "full";
    let admitted = admission_eligible
        && gates.exact
        && gates.routed
        && gates.page_backed
        && gates.p50
        && gates.no_tail_relative
        && gates.tail
        && gates.query_rss;
    println!(
        "{}",
        serde_json::to_string_pretty(&TailReceipt {
            schema: "vicia.projection-tail-overlay.v1",
            profile: profile.to_owned(),
            facts,
            tail_facts: TAIL_FACTS,
            trials,
            admission_eligible,
            source_commit,
            tracked_clean,
            fixture,
            expected_count,
            expected_checksum,
            observed_count,
            observed_checksum,
            refresh_elapsed_ms,
            no_tail,
            resident_tail,
            refresh_diagnostics,
            cached_diagnostics,
            peak_query_rss_delta_bytes,
            gates,
            admitted,
            file_format_changed: false,
            public_api_changed: false,
            arbitrary_datalog_routing_changed: false,
        })?
    );
    Ok(())
}

fn run(
    source: &Path,
    published: &Path,
    fixture_path: &Path,
    facts: u64,
    profile: &str,
) -> Result<()> {
    let trials = match profile {
        "smoke" => 3,
        "full" => 10,
        _ => bail!("profile must be smoke or full"),
    };
    let fixture: FixtureReceipt = serde_json::from_slice(&fs::read(fixture_path)?)?;
    if fixture.facts != facts {
        bail!("fixture fact count does not match benchmark request")
    }
    let source_commit = command_text("git", &["rev-parse", "HEAD"])?;
    if fixture.builder_source_commit != source_commit {
        bail!("fixture was not built by the current source commit")
    }
    let tracked_clean =
        command_text("git", &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
    if published.exists() {
        fs::remove_file(published)?;
    }

    let max_work = usize::try_from(facts)?;
    let source_ledger = InteractiveLedger::open(source)?;
    let (ledger, ledger_count, ledger_checksum, _) =
        measure(&source_ledger, max_work, trials, false)?;
    drop(source_ledger);

    fs::copy(source, published)?;
    let maintenance = MaintenanceLedger::open(published)?;
    let maintenance_started = Instant::now();
    maintenance.rebuild_current_projections(&[ATTRIBUTE.to_owned()])?;
    let maintenance_elapsed_ms = maintenance_started.elapsed().as_secs_f64() * 1_000.0;
    drop(maintenance);

    let projected_ledger = InteractiveLedger::open(published)?;
    let baseline_rss_bytes = current_rss_bytes().context("read query RSS baseline")?;
    let peak_before = peak_rss_bytes().context("read query peak RSS baseline")?;
    let (projected, projected_count, projected_checksum, projection_diagnostics) =
        measure(&projected_ledger, max_work, trials, true)?;
    let peak_query_rss_delta_bytes = peak_rss_bytes()
        .context("read query peak RSS")?
        .saturating_sub(peak_before.max(baseline_rss_bytes));

    let (expected_count, expected_checksum) = expected_pair(facts);
    let exact = (ledger_count, ledger_checksum) == (expected_count, expected_checksum)
        && (projected_count, projected_checksum) == (expected_count, expected_checksum);
    let routed = projection_diagnostics.route_attempts == 1
        && projection_diagnostics.completed_scans == 1
        && projection_diagnostics.ledger_fallbacks == 0;
    let page_backed =
        projection_diagnostics.pages_read > 0 && projection_diagnostics.full_image_decodes == 0;
    let route_p50 = projected.p50_ms <= 230.0;
    let improvement = projected.p50_ms <= ledger.p50_ms * 0.9;
    let tail = projected.p95_ms <= projected.p50_ms * 1.15;
    let query_rss = peak_query_rss_delta_bytes <= 2 * 1024 * 1024;
    let gates = Gates {
        exact,
        routed,
        page_backed,
        route_p50,
        improvement,
        tail,
        query_rss,
    };
    let admission_eligible = profile == "full";
    let admitted = admission_eligible
        && gates.exact
        && gates.routed
        && gates.page_backed
        && gates.route_p50
        && gates.improvement
        && gates.tail
        && gates.query_rss;
    println!(
        "{}",
        serde_json::to_string_pretty(&Receipt {
            schema: "vicia.projection-routing.v1",
            profile: profile.to_owned(),
            facts,
            trials,
            admission_eligible,
            source_commit,
            tracked_clean,
            fixture,
            expected_count,
            expected_checksum,
            ledger_count,
            ledger_checksum,
            projected_count,
            projected_checksum,
            maintenance_elapsed_ms,
            ledger,
            projected,
            projection_diagnostics,
            baseline_rss_bytes,
            peak_query_rss_delta_bytes,
            gates,
            admitted,
            default_write_format_changed: false,
            arbitrary_datalog_routing_changed: false,
        })?
    );
    Ok(())
}

fn measure(
    ledger: &InteractiveLedger,
    max_work: usize,
    trials: usize,
    require_projection: bool,
) -> Result<(TimingSummary, u64, i128, ProjectionReadDiagnostics)> {
    let mut samples = Vec::with_capacity(trials);
    let mut expected = None;
    let mut last_diagnostics = ProjectionReadDiagnostics::default();
    for _ in 0..trials {
        let view = ledger.read_view(ReadViewOptions::default())?;
        let started = Instant::now();
        let result = view.query(QUERY, max_work)?;
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        let pair = aggregate_pair(result)?;
        if expected.is_some_and(|previous| previous != pair) {
            bail!("aggregate changed across benchmark trials")
        }
        expected = Some(pair);
        last_diagnostics = view.last_projection_read_diagnostics();
        if require_projection && last_diagnostics.completed_scans != 1 {
            bail!("eligible benchmark query did not complete through the projection")
        }
    }
    samples.sort_by(f64::total_cmp);
    let p50_ms = percentile(&samples, 0.50);
    let p95_ms = percentile(&samples, 0.95);
    let (count, checksum) = expected.context("benchmark produced no samples")?;
    Ok((
        TimingSummary {
            samples_ms: samples,
            p50_ms,
            p95_ms,
        },
        count,
        checksum,
        last_diagnostics,
    ))
}

fn aggregate_pair(result: QueryResult) -> Result<(u64, i128)> {
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("expected aggregate query result")
    };
    let row = results.first().context("aggregate result is empty")?;
    let [Value::Integer(count), Value::Integer(checksum)] = row.as_slice() else {
        bail!("aggregate result has unexpected values")
    };
    Ok((u64::try_from(*count)?, i128::from(*checksum)))
}

fn expected_pair(facts: u64) -> (u64, i128) {
    let mut count = 0_u64;
    let mut checksum = 0_i128;
    for value in 0..facts {
        if value % 4 == 0 || value % 4 == 1 {
            count = count.saturating_add(1);
            checksum += i128::from(value);
        }
    }
    (count, checksum)
}

fn percentile(samples: &[f64], quantile: f64) -> f64 {
    let index = ((samples.len().saturating_sub(1)) as f64 * quantile).ceil() as usize;
    samples[index.min(samples.len().saturating_sub(1))]
}

fn command_text(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        bail!("{program} command failed")
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn current_rss_bytes() -> Result<u64> {
    let mut statm = String::new();
    fs::File::open("/proc/self/statm")?.read_to_string(&mut statm)?;
    let resident_pages = statm
        .split_whitespace()
        .nth(1)
        .context("missing resident page count")?
        .parse::<u64>()?;
    Ok(resident_pages.saturating_mul(4_096))
}

fn peak_rss_bytes() -> Result<u64> {
    let status = fs::read_to_string("/proc/self/status")?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .context("missing VmHWM")?
        .split_whitespace()
        .next()
        .context("missing VmHWM value")?
        .parse::<u64>()?;
    Ok(kib.saturating_mul(1_024))
}
