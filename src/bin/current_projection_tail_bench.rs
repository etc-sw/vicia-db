use anyhow::{Context, Result, bail};
use minigraf::{CurrentProjectionCandidate, Minigraf, OpenOptions};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

const ATTRIBUTE: &str = ":projection/value";
const TEMPORAL_BOUNDARY: i64 = 1_735_689_600_000;
const TEMPORAL_BEFORE: i64 = TEMPORAL_BOUNDARY - 1;
const TEMPORAL_AFTER: i64 = TEMPORAL_BOUNDARY + 2;
const FULL_TRIALS: usize = 20;
const SMOKE_TRIALS: usize = 6;
const EXPECTED_FILL_PERCENT: u8 = 90;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum CandidateKind {
    Source,
    Decoded,
}

#[derive(Clone, Copy)]
struct Probe {
    name: &'static str,
    valid_at: i64,
}

const PROBES: [Probe; 3] = [
    Probe {
        name: "beforeBoundary",
        valid_at: TEMPORAL_BEFORE,
    },
    Probe {
        name: "atBoundary",
        valid_at: TEMPORAL_BOUNDARY,
    },
    Probe {
        name: "afterBoundary",
        valid_at: TEMPORAL_AFTER,
    },
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimedAggregate {
    elapsed_ms: f64,
    count: u64,
    checksum: i128,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeTrial {
    name: String,
    valid_at: i64,
    order: Vec<CandidateKind>,
    source: TimedAggregate,
    decoded: TimedAggregate,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageIdentity {
    base_generation: u64,
    manifest_generation: u64,
    tx_count: u64,
    fingerprint: String,
    row_count: u64,
    padded_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Trial {
    trial_index: usize,
    probe_order: Vec<String>,
    image: ImageIdentity,
    probes: Vec<ProbeTrial>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
struct SeriesSummary {
    samples: Vec<f64>,
    p50: f64,
    p95: f64,
    max: f64,
    mad: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeGates {
    exact: bool,
    decoded_latency: bool,
    decoded_tail: bool,
    decoded_p50_relative: bool,
    decoded_p95_relative: bool,
    admitted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeSummary {
    name: String,
    valid_at: i64,
    source_ms: SeriesSummary,
    decoded_ms: SeriesSummary,
    decoded_source_ratio: SeriesSummary,
    decoded_wins: usize,
    gates: ProbeGates,
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
    host: Option<String>,
    fixture: FixtureReceipt,
    projection_identity: ImageIdentity,
    measurements: Vec<Trial>,
    probes: Vec<ProbeSummary>,
    admitted: bool,
    production_query_routing_changed: bool,
    public_api_changed: bool,
    file_format_changed: bool,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.as_slice() {
        [_, command, graph, fixture, facts, profile] if command == "run" => run(
            Path::new(graph),
            Path::new(fixture),
            facts.parse()?,
            profile,
        ),
        [_, command, graph, facts, trial_index] if command == "trial" => {
            let trial = measure_trial(Path::new(graph), facts.parse()?, trial_index.parse()?)?;
            println!("{}", serde_json::to_string(&trial)?);
            Ok(())
        }
        _ => bail!(
            "usage: current-projection-tail-bench \
             run <graph> <fixture-metadata> <facts> <smoke|full> | \
             trial <graph> <facts> <trial-index>"
        ),
    }
}

fn run(graph: &Path, fixture_path: &Path, facts: u64, profile: &str) -> Result<()> {
    let trials = match profile {
        "smoke" => SMOKE_TRIALS,
        "full" => FULL_TRIALS,
        _ => bail!("profile must be smoke or full"),
    };
    let source_commit = command_text("git", &["rev-parse", "HEAD"])?;
    let tracked_clean =
        command_text("git", &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
    let fixture = load_fixture_metadata(
        graph,
        fixture_path,
        facts,
        &source_commit,
        profile == "full",
    )?;
    let executable = std::env::current_exe()?;
    let mut measurements = Vec::with_capacity(trials);
    for trial_index in 0..trials {
        eprintln!("projection-page-tail: trial {}/{}", trial_index + 1, trials);
        measurements.push(child_trial(&executable, graph, facts, trial_index)?);
    }
    validate_trial_set(&measurements, trials, facts)?;
    let projection_identity = common_identity(&measurements)?;
    let probes = summarize_probes(&measurements, facts)?;
    let admission_eligible = profile == "full";
    let admitted = admission_eligible && probes.iter().all(|probe| probe.gates.admitted);
    let receipt = Receipt {
        schema: "vicia.current-projection-tail.v2",
        profile: profile.to_owned(),
        facts,
        trials,
        admission_eligible,
        source_commit,
        tracked_clean,
        host: command_text("hostname", &[]).ok(),
        fixture,
        projection_identity,
        measurements,
        probes,
        admitted,
        production_query_routing_changed: false,
        public_api_changed: false,
        file_format_changed: false,
    };
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}

fn load_fixture_metadata(
    graph: &Path,
    fixture_path: &Path,
    facts: u64,
    source_commit: &str,
    require_clean: bool,
) -> Result<FixtureReceipt> {
    let fixture: FixtureReceipt = serde_json::from_slice(&fs::read(fixture_path)?)
        .context("decode temporal fixture metadata")?;
    validate_fixture_metadata(
        &fixture,
        fs::metadata(graph)?.len(),
        &sha256_file(graph)?,
        fixture_format_version(graph)?,
        facts,
        source_commit,
        require_clean,
    )?;
    Ok(fixture)
}

fn validate_fixture_metadata(
    fixture: &FixtureReceipt,
    graph_bytes: u64,
    graph_sha256: &str,
    graph_format_version: u32,
    facts: u64,
    source_commit: &str,
    require_clean: bool,
) -> Result<()> {
    if fixture.schema != "vicia.temporal-projection-fixture.v1" {
        bail!("temporal fixture metadata schema mismatch")
    }
    if fixture.facts != facts {
        bail!("temporal fixture fact count mismatch")
    }
    if fixture.fill_percent != EXPECTED_FILL_PERCENT {
        bail!("temporal fixture fill percent mismatch")
    }
    if fixture.bytes != graph_bytes
        || fixture.sha256 != graph_sha256
        || fixture.format_version != graph_format_version
    {
        bail!("temporal fixture graph identity mismatch")
    }
    if fixture.builder_source_commit != source_commit {
        bail!("temporal fixture builder source mismatch")
    }
    if require_clean && !fixture.builder_tracked_clean {
        bail!("full temporal fixture builder source must be clean")
    }
    Ok(())
}

fn measure_trial(graph: &Path, facts: u64, trial_index: usize) -> Result<Trial> {
    let db = open(graph)?;
    let source = db.benchmark_build_current_projection(ATTRIBUTE, TEMPORAL_BEFORE)?;
    if source.row_count() != usize::try_from(facts)? {
        bail!("source projection row count mismatch")
    }
    let image = db.benchmark_encode_current_projection_page_image(&source)?;
    let decoded =
        db.benchmark_decode_current_projection_page_image(&image, ATTRIBUTE, TEMPORAL_BEFORE)?;
    let source_fingerprint = source.fingerprint()?;
    if decoded.fingerprint()? != source_fingerprint || decoded.row_count() != source.row_count() {
        bail!("source and decoded projections differ")
    }
    let identity = image.identity();
    let image_identity = ImageIdentity {
        base_generation: identity.base_generation(),
        manifest_generation: identity.manifest_generation(),
        tx_count: identity.tx_count(),
        fingerprint: format!("{source_fingerprint:016x}"),
        row_count: u64::try_from(source.row_count())?,
        padded_bytes: image.padded_bytes(),
    };

    let probe_indices = rotated_probe_indices(trial_index);
    let mut probe_order = Vec::with_capacity(PROBES.len());
    let mut probes = Vec::with_capacity(PROBES.len());
    for probe_index in probe_indices {
        let probe = PROBES
            .get(probe_index)
            .copied()
            .context("probe rotation index")?;
        probe_order.push(probe.name.to_owned());
        let order = candidate_order(trial_index, probe_index);
        for candidate in order {
            let pair = aggregate(
                &db,
                select_candidate(candidate, &source, &decoded),
                probe.valid_at,
            )?;
            ensure_expected(facts, probe.valid_at, pair)?;
        }
        let mut source_measurement = None;
        let mut decoded_measurement = None;
        for candidate in order {
            let measurement = timed_aggregate(
                &db,
                select_candidate(candidate, &source, &decoded),
                probe.valid_at,
            )?;
            ensure_expected(
                facts,
                probe.valid_at,
                (measurement.count, measurement.checksum),
            )?;
            match candidate {
                CandidateKind::Source => source_measurement = Some(measurement),
                CandidateKind::Decoded => decoded_measurement = Some(measurement),
            }
        }
        probes.push(ProbeTrial {
            name: probe.name.to_owned(),
            valid_at: probe.valid_at,
            order: order.to_vec(),
            source: source_measurement.context("source measurement missing")?,
            decoded: decoded_measurement.context("decoded measurement missing")?,
        });
    }
    Ok(Trial {
        trial_index,
        probe_order,
        image: image_identity,
        probes,
    })
}

fn select_candidate<'a>(
    kind: CandidateKind,
    source: &'a CurrentProjectionCandidate,
    decoded: &'a CurrentProjectionCandidate,
) -> &'a CurrentProjectionCandidate {
    match kind {
        CandidateKind::Source => source,
        CandidateKind::Decoded => decoded,
    }
}

fn aggregate(
    db: &Minigraf,
    candidate: &CurrentProjectionCandidate,
    valid_at: i64,
) -> Result<(u64, i128)> {
    db.benchmark_current_projection_integer_aggregate_at(candidate, valid_at)
}

fn timed_aggregate(
    db: &Minigraf,
    candidate: &CurrentProjectionCandidate,
    valid_at: i64,
) -> Result<TimedAggregate> {
    let started = Instant::now();
    let (count, checksum) = aggregate(db, candidate, valid_at)?;
    Ok(TimedAggregate {
        elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
        count,
        checksum,
    })
}

fn summarize_probes(measurements: &[Trial], facts: u64) -> Result<Vec<ProbeSummary>> {
    PROBES
        .iter()
        .map(|probe| {
            let mut source = Vec::with_capacity(measurements.len());
            let mut decoded = Vec::with_capacity(measurements.len());
            let mut ratios = Vec::with_capacity(measurements.len());
            let mut decoded_wins = 0;
            let expected = expected_pair(facts, probe.valid_at);
            let mut exact = true;
            for trial in measurements {
                let sample = trial
                    .probes
                    .iter()
                    .find(|sample| sample.name == probe.name)
                    .context("probe sample missing")?;
                exact &= (sample.source.count, sample.source.checksum) == expected
                    && (sample.decoded.count, sample.decoded.checksum) == expected;
                source.push(sample.source.elapsed_ms);
                decoded.push(sample.decoded.elapsed_ms);
                ratios.push(sample.decoded.elapsed_ms / sample.source.elapsed_ms);
                decoded_wins += usize::from(sample.decoded.elapsed_ms < sample.source.elapsed_ms);
            }
            let source_ms = summarize(source)?;
            let decoded_ms = summarize(decoded)?;
            let decoded_source_ratio = summarize(ratios)?;
            let decoded_latency = decoded_ms.p50 <= 150.0;
            let decoded_tail = decoded_ms.p95 <= decoded_ms.p50 * 1.15;
            let decoded_p50_relative = decoded_ms.p50 <= source_ms.p50 * 1.10;
            let decoded_p95_relative = decoded_ms.p95 <= source_ms.p95 * 1.10;
            let admitted = exact
                && decoded_latency
                && decoded_tail
                && decoded_p50_relative
                && decoded_p95_relative;
            Ok(ProbeSummary {
                name: probe.name.to_owned(),
                valid_at: probe.valid_at,
                source_ms,
                decoded_ms,
                decoded_source_ratio,
                decoded_wins,
                gates: ProbeGates {
                    exact,
                    decoded_latency,
                    decoded_tail,
                    decoded_p50_relative,
                    decoded_p95_relative,
                    admitted,
                },
            })
        })
        .collect()
}

fn summarize(mut samples: Vec<f64>) -> Result<SeriesSummary> {
    if samples.is_empty()
        || samples
            .iter()
            .any(|sample| !sample.is_finite() || *sample <= 0.0)
    {
        bail!("invalid timing series")
    }
    samples.sort_by(f64::total_cmp);
    let p50 = nearest_rank(&samples, 50).context("p50")?;
    let p95 = nearest_rank(&samples, 95).context("p95")?;
    let max = *samples.last().context("max")?;
    let mut deviations = samples
        .iter()
        .map(|sample| (sample - p50).abs())
        .collect::<Vec<_>>();
    deviations.sort_by(f64::total_cmp);
    let mad = nearest_rank(&deviations, 50).context("mad")?;
    Ok(SeriesSummary {
        samples,
        p50,
        p95,
        max,
        mad,
    })
}

fn validate_trial_set(measurements: &[Trial], trials: usize, facts: u64) -> Result<()> {
    if measurements.len() != trials {
        bail!("trial count mismatch")
    }
    for (trial_index, trial) in measurements.iter().enumerate() {
        let expected_probe_order = rotated_probe_indices(trial_index)
            .iter()
            .map(|index| {
                PROBES
                    .get(*index)
                    .map(|probe| probe.name.to_owned())
                    .context("expected probe order")
            })
            .collect::<Result<Vec<_>>>()?;
        if trial.trial_index != trial_index || trial.probe_order != expected_probe_order {
            bail!("trial rotation mismatch")
        }
        if trial.image.row_count != facts || trial.probes.len() != PROBES.len() {
            bail!("trial shape mismatch")
        }
        for (probe_index, expected_probe) in PROBES.iter().enumerate() {
            let sample = trial
                .probes
                .iter()
                .find(|sample| sample.name == expected_probe.name)
                .context("trial probe missing")?;
            if sample.valid_at != expected_probe.valid_at
                || sample.order != candidate_order(trial_index, probe_index)
            {
                bail!("candidate order mismatch")
            }
        }
    }
    Ok(())
}

fn common_identity(measurements: &[Trial]) -> Result<ImageIdentity> {
    let first = measurements
        .first()
        .context("projection identity missing")?;
    for trial in measurements.iter().skip(1) {
        if trial.image.base_generation != first.image.base_generation
            || trial.image.manifest_generation != first.image.manifest_generation
            || trial.image.tx_count != first.image.tx_count
            || trial.image.fingerprint != first.image.fingerprint
            || trial.image.row_count != first.image.row_count
            || trial.image.padded_bytes != first.image.padded_bytes
        {
            bail!("projection identity changed across trials")
        }
    }
    Ok(ImageIdentity {
        base_generation: first.image.base_generation,
        manifest_generation: first.image.manifest_generation,
        tx_count: first.image.tx_count,
        fingerprint: first.image.fingerprint.clone(),
        row_count: first.image.row_count,
        padded_bytes: first.image.padded_bytes,
    })
}

fn candidate_order(trial_index: usize, probe_index: usize) -> [CandidateKind; 2] {
    if (trial_index + probe_index).is_multiple_of(2) {
        [CandidateKind::Source, CandidateKind::Decoded]
    } else {
        [CandidateKind::Decoded, CandidateKind::Source]
    }
}

fn rotated_probe_indices(trial_index: usize) -> [usize; 3] {
    let start = trial_index % PROBES.len();
    [
        start,
        (start + 1) % PROBES.len(),
        (start + 2) % PROBES.len(),
    ]
}

fn expected_pair(facts: u64, valid_at: i64) -> (u64, i128) {
    let mut count = 0_u64;
    let mut checksum = 0_i128;
    for value in 0..facts {
        let visible = if valid_at < TEMPORAL_BOUNDARY {
            matches!(value % 4, 0 | 2)
        } else if valid_at < TEMPORAL_AFTER {
            matches!(value % 4, 0 | 1 | 3)
        } else {
            matches!(value % 4, 0 | 1)
        };
        if visible {
            count = count.saturating_add(1);
            checksum = checksum.saturating_add(i128::from(value));
        }
    }
    (count, checksum)
}

fn ensure_expected(facts: u64, valid_at: i64, actual: (u64, i128)) -> Result<()> {
    if actual != expected_pair(facts, valid_at) {
        bail!("temporal aggregate mismatch at {valid_at}")
    }
    Ok(())
}

fn open(path: &Path) -> Result<Minigraf> {
    let mut options = OpenOptions::new();
    options.wal_checkpoint_threshold = usize::MAX;
    options.path(path).open()
}

fn child_trial(executable: &Path, graph: &Path, facts: u64, trial_index: usize) -> Result<Trial> {
    let output = Command::new(executable)
        .arg("trial")
        .arg(graph)
        .arg(facts.to_string())
        .arg(trial_index.to_string())
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!(
            "measurement child failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    serde_json::from_slice(&output.stdout).context("decode trial JSON")
}

fn command_text(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        bail!("{program} failed with {}", output.status)
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn sha256_file(path: &Path) -> Result<String> {
    let output = Command::new("sha256sum").arg(path).output()?;
    if !output.status.success() {
        bail!("sha256sum failed with {}", output.status)
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum output missing")
}

fn fixture_format_version(path: &Path) -> Result<u32> {
    let mut bytes = [0_u8; 8];
    fs::File::open(path)?.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes[4..8].try_into()?))
}

fn nearest_rank(sorted: &[f64], percentile: usize) -> Option<f64> {
    if sorted.is_empty() || percentile == 0 || percentile > 100 {
        return None;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_keeps_twentieth_sample_as_max() {
        let samples = (1..=20).map(f64::from).collect::<Vec<_>>();
        assert_eq!(nearest_rank(&samples, 50), Some(10.0));
        assert_eq!(nearest_rank(&samples, 95), Some(19.0));
        assert_eq!(nearest_rank(&samples, 100), Some(20.0));
    }

    #[test]
    fn candidate_order_is_balanced_for_every_probe() {
        for probe_index in 0..PROBES.len() {
            let source_first = (0..FULL_TRIALS)
                .filter(|trial| candidate_order(*trial, probe_index)[0] == CandidateKind::Source)
                .count();
            assert_eq!(source_first, FULL_TRIALS / 2);
        }
    }

    #[test]
    fn probe_order_rotates_cyclically() {
        assert_eq!(rotated_probe_indices(0), [0, 1, 2]);
        assert_eq!(rotated_probe_indices(1), [1, 2, 0]);
        assert_eq!(rotated_probe_indices(2), [2, 0, 1]);
        assert_eq!(rotated_probe_indices(3), [0, 1, 2]);
    }

    #[test]
    fn temporal_fixture_pairs_are_exact() {
        assert_eq!(
            expected_pair(1_000_000, TEMPORAL_BEFORE),
            (500_000, 249_999_500_000)
        );
        assert_eq!(
            expected_pair(1_000_000, TEMPORAL_BOUNDARY),
            (750_000, 374_999_500_000)
        );
        assert_eq!(
            expected_pair(1_000_000, TEMPORAL_AFTER),
            (500_000, 249_999_250_000)
        );
    }

    #[test]
    fn fixture_metadata_binds_fill_graph_and_builder() {
        let fixture = FixtureReceipt {
            schema: "vicia.temporal-projection-fixture.v1".to_owned(),
            facts: 10_000,
            fill_percent: EXPECTED_FILL_PERCENT,
            bytes: 42,
            sha256: "ab".repeat(32),
            format_version: 12,
            builder_source_commit: "cd".repeat(20),
            builder_tracked_clean: true,
        };
        assert!(
            validate_fixture_metadata(
                &fixture,
                42,
                &"ab".repeat(32),
                12,
                10_000,
                &"cd".repeat(20),
                true,
            )
            .is_ok()
        );

        let mut wrong_fill = fixture.clone();
        wrong_fill.fill_percent = 87;
        assert!(
            validate_fixture_metadata(
                &wrong_fill,
                42,
                &"ab".repeat(32),
                12,
                10_000,
                &"cd".repeat(20),
                true,
            )
            .is_err()
        );

        let mut wrong_graph = fixture.clone();
        wrong_graph.sha256 = "ef".repeat(32);
        assert!(
            validate_fixture_metadata(
                &wrong_graph,
                42,
                &"ab".repeat(32),
                12,
                10_000,
                &"cd".repeat(20),
                true,
            )
            .is_err()
        );

        let mut dirty_builder = fixture;
        dirty_builder.builder_tracked_clean = false;
        assert!(
            validate_fixture_metadata(
                &dirty_builder,
                42,
                &"ab".repeat(32),
                12,
                10_000,
                &"cd".repeat(20),
                true,
            )
            .is_err()
        );
    }
}
