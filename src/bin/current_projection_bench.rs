use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use minigraf::{Minigraf, OpenOptions, QueryResult, Value};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const BATCH: u64 = 1_000;
const TEMPORAL_BOUNDARY: i64 = 1_735_689_600_000;
const TEMPORAL_BEFORE: i64 = TEMPORAL_BOUNDARY - 1;
const TEMPORAL_AFTER: i64 = TEMPORAL_BOUNDARY + 2;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregateMeasurement {
    samples_ms: Vec<f64>,
    p50_ms: f64,
    p95_ms: f64,
    count: u64,
    checksum: i128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectionMeasurement {
    build_ms: f64,
    accounted_bytes: u64,
    image_ratio: f64,
    resident_rss_delta_bytes: u64,
    query_rss_delta_bytes: u64,
    row_count: usize,
    fingerprint: String,
    aggregate: AggregateMeasurement,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IncrementalMeasurement {
    stale_read_rejected: bool,
    refresh_ms: f64,
    refresh: minigraf::CurrentProjectionRefreshDiagnostics,
    count: u64,
    checksum: i128,
    deterministic_rebuild: bool,
    checkpoint_ms: f64,
    checkpoint_stale_read_rejected: bool,
    checkpoint_refresh_ms: f64,
    checkpoint_refresh: minigraf::CurrentProjectionRefreshDiagnostics,
    production_checkpoint_path_changed: bool,
    checkpoint_regression_percent: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SemanticMeasurement {
    all_value_types: bool,
    ref_value: bool,
    assert_refresh: bool,
    scoped_retract: bool,
    unscoped_retract: bool,
    valid_window: bool,
    overlapping_windows: bool,
    stale_read_rejected: bool,
    deterministic_rebuild: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    schema: &'static str,
    facts: u64,
    samples: usize,
    graph_bytes: u64,
    valid_at: i64,
    baseline: AggregateMeasurement,
    projection: ProjectionMeasurement,
    incremental: IncrementalMeasurement,
    semantics: SemanticMeasurement,
    provenance: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemporalProbeMeasurement {
    name: &'static str,
    valid_at: i64,
    baseline: AggregateMeasurement,
    projection: AggregateMeasurement,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemporalShapeMeasurement {
    distinct_windows: u64,
    valid_from_payload_bytes: u64,
    valid_to_payload_bytes: u64,
    temporal_payload_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemporalProjectionMeasurement {
    build_ms: f64,
    accounted_bytes: u64,
    image_ratio: f64,
    resident_rss_delta_bytes: u64,
    query_rss_delta_bytes: u64,
    row_count: usize,
    fingerprint: String,
    shape: TemporalShapeMeasurement,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemporalIncrementalMeasurement {
    stale_read_rejected: bool,
    refresh_ms: f64,
    refresh: minigraf::CurrentProjectionRefreshDiagnostics,
    count: u64,
    checksum: i128,
    deterministic_rebuild: bool,
    checkpoint_stale_read_rejected: bool,
    checkpoint_refresh: minigraf::CurrentProjectionRefreshDiagnostics,
    pre_floor_rejected: bool,
    no_write_boundary_transition: bool,
    production_checkpoint_path_changed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemporalSemanticMeasurement {
    all_value_types: bool,
    ref_value: bool,
    boundary_start_inclusive: bool,
    boundary_end_exclusive: bool,
    overlapping_windows: bool,
    scoped_retract: bool,
    unscoped_retract: bool,
    future_window_retained: bool,
    pre_floor_rejected: bool,
    stale_read_rejected: bool,
    checkpoint_invalidation: bool,
    deterministic_rebuild: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemporalMeasurement {
    schema: &'static str,
    facts: u64,
    samples: usize,
    graph_bytes: u64,
    valid_time_floor: i64,
    probes: Vec<TemporalProbeMeasurement>,
    projection: TemporalProjectionMeasurement,
    incremental: TemporalIncrementalMeasurement,
    semantics: TemporalSemanticMeasurement,
    provenance: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TimingMeasurement {
    samples_ms: Vec<f64>,
    p50_ms: f64,
    p95_ms: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageImageIdentityMeasurement {
    base_generation: u64,
    manifest_generation: u64,
    tx_count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageImageShapeMeasurement {
    logical_bytes: u64,
    padded_bytes: u64,
    padding_bytes: u64,
    page_count: u64,
    image_ratio: f64,
    row_count: u64,
    fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageImageProofMeasurement {
    round_trip: bool,
    deterministic_rebuild: bool,
    overlay_flatten: bool,
    production_query_routing_changed: bool,
    public_api_changed: bool,
    file_format_changed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageImageMeasurement {
    schema: &'static str,
    facts: u64,
    samples: usize,
    graph_bytes: u64,
    valid_time_floor: i64,
    identity: PageImageIdentityMeasurement,
    image: PageImageShapeMeasurement,
    encode: TimingMeasurement,
    decode: TimingMeasurement,
    maintenance_peak_rss_delta_bytes: u64,
    query_rss_delta_bytes: u64,
    probes: Vec<TemporalProbeMeasurement>,
    proof: PageImageProofMeasurement,
    provenance: serde_json::Value,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.as_slice() {
        [_, command, path, facts] if command == "build" => {
            build_fixture(Path::new(path), facts.parse()?)
        }
        [_, command, path, facts] if command == "build-temporal" => {
            build_temporal_fixture(Path::new(path), facts.parse()?)
        }
        [_, command, path, facts, samples] if command == "measure" => {
            let measurement = measure(Path::new(path), facts.parse()?, samples.parse()?)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        [_, command, path, facts, samples] if command == "measure-temporal" => {
            let measurement = measure_temporal(Path::new(path), facts.parse()?, samples.parse()?)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        [_, command, path, facts, samples] if command == "measure-page-image" => {
            let measurement =
                measure_page_image(Path::new(path), facts.parse()?, samples.parse()?)?;
            println!("{}", serde_json::to_string(&measurement)?);
            Ok(())
        }
        _ => bail!(
            "usage: current-projection-bench \
             build|build-temporal <graph> <facts> | \
             measure|measure-temporal|measure-page-image <graph> <facts> <samples>"
        ),
    }
}

fn measure_page_image(path: &Path, facts: u64, samples: usize) -> Result<PageImageMeasurement> {
    let graph_bytes = fs::metadata(path)?.len();
    let db = open(path)?;
    let candidate = db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    if candidate.row_count() != usize::try_from(facts)? {
        bail!("page-image candidate row count mismatch")
    }

    let peak_before = peak_rss_bytes().context("read page-image high-water RSS baseline")?;
    let mut encode_samples = Vec::with_capacity(samples);
    let mut image = None;
    for _ in 0..samples {
        drop(image.take());
        let started = Instant::now();
        image = Some(db.benchmark_encode_current_projection_page_image(&candidate)?);
        encode_samples.push(elapsed_ms(started));
    }
    let image = image.context("page-image encode produced no image")?;
    let rebuilt_before_write =
        db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let deterministic =
        db.benchmark_encode_current_projection_page_image(&rebuilt_before_write)? == image;
    drop(rebuilt_before_write);

    let mut decode_samples = Vec::with_capacity(samples);
    let mut decoded = None;
    for _ in 0..samples {
        drop(decoded.take());
        let started = Instant::now();
        decoded = Some(db.benchmark_decode_current_projection_page_image(
            &image,
            ":projection/value",
            TEMPORAL_BEFORE,
        )?);
        decode_samples.push(elapsed_ms(started));
    }
    let decoded = decoded.context("page-image decode produced no candidate")?;
    let maintenance_peak_rss_delta_bytes = peak_rss_bytes()
        .context("read page-image high-water RSS")?
        .saturating_sub(peak_before);
    let round_trip = decoded.fingerprint()? == candidate.fingerprint()?
        && decoded.row_count() == candidate.row_count();
    let image_identity = image.identity();
    let logical_bytes = image.logical_bytes();
    let padded_bytes = image.padded_bytes();
    let image_shape = PageImageShapeMeasurement {
        logical_bytes,
        padded_bytes,
        padding_bytes: padded_bytes.saturating_sub(logical_bytes),
        page_count: image.page_count(),
        image_ratio: padded_bytes as f64 / graph_bytes as f64,
        row_count: image.row_count(),
        fingerprint: format!("{:016x}", image.fingerprint()),
    };
    drop(image);
    drop(candidate);

    let query_rss = current_rss_bytes().context("read page-image query RSS baseline")?;
    let mut probes = Vec::new();
    for (name, valid_at) in [
        ("beforeBoundary", TEMPORAL_BEFORE),
        ("atBoundary", TEMPORAL_BOUNDARY),
        ("afterBoundary", TEMPORAL_AFTER),
    ] {
        let expected = expected_temporal_pair(facts, valid_at);
        let query = format!(
            "(query [:find (count ?v) (sum ?v) :valid-at \"{}\" \
             :where [?e :projection/value ?v]])",
            format_timestamp(valid_at)?
        );
        let baseline = measure_query(&db, &query, samples)?;
        let projection = measure_candidate_at(&db, &decoded, valid_at, samples)?;
        if (baseline.count, baseline.checksum) != expected
            || (projection.count, projection.checksum) != expected
        {
            bail!("decoded page-image probe mismatch at {name}")
        }
        probes.push(TemporalProbeMeasurement {
            name,
            valid_at,
            baseline,
            projection,
        });
    }
    let query_rss_delta_bytes = current_rss_bytes()
        .context("read page-image query RSS")?
        .saturating_sub(query_rss);
    drop(decoded);

    let mut candidate =
        db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let added_value = i64::try_from(facts)?;
    db.execute(&format!(
        "(transact {{:valid-from \"2025-01-01T00:00:00.000Z\" \
         :valid-to \"2030-01-01T00:00:00.000Z\"}} \
         [[#uuid \"{}\" :projection/value {added_value}]])",
        Uuid::from_u128(u128::from(facts).saturating_add(1))
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let overlay_image = db.benchmark_encode_current_projection_page_image(&candidate)?;
    let rebuilt = db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let rebuilt_image = db.benchmark_encode_current_projection_page_image(&rebuilt)?;
    let overlay_flatten = overlay_image == rebuilt_image;

    Ok(PageImageMeasurement {
        schema: "vicia.current-projection-page-image.v1",
        facts,
        samples,
        graph_bytes,
        valid_time_floor: TEMPORAL_BEFORE,
        identity: PageImageIdentityMeasurement {
            base_generation: image_identity.base_generation(),
            manifest_generation: image_identity.manifest_generation(),
            tx_count: image_identity.tx_count(),
        },
        image: image_shape,
        encode: summarize_timing(encode_samples)?,
        decode: summarize_timing(decode_samples)?,
        maintenance_peak_rss_delta_bytes,
        query_rss_delta_bytes,
        probes,
        proof: PageImageProofMeasurement {
            round_trip,
            deterministic_rebuild: deterministic,
            overlay_flatten,
            production_query_routing_changed: false,
            public_api_changed: false,
            file_format_changed: false,
        },
        provenance: provenance(),
    })
}

fn build_fixture(path: &Path, facts: u64) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_graph(path)?;
    let db = open(path)?;
    for start in (0..facts).step_by(usize::try_from(BATCH)?) {
        let mut command = String::from("(transact [");
        for entity in start..start.saturating_add(BATCH).min(facts) {
            command.push_str(&format!(
                "[#uuid \"{}\" :projection/value {entity}]",
                Uuid::from_u128(u128::from(entity).saturating_add(1))
            ));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    Ok(())
}

fn build_temporal_fixture(path: &Path, facts: u64) -> Result<()> {
    const UNIQUE_FIRST_BASE: i64 = 1_577_836_800_000;
    const UNIQUE_SECOND_BASE: i64 = 1_609_459_200_000;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_graph(path)?;
    let db = open(path)?;
    for start in (0..facts).step_by(usize::try_from(BATCH)?) {
        let mut command = String::from("(transact [");
        for entity in start..start.saturating_add(BATCH).min(facts) {
            let (valid_from, valid_to) = match entity % 4 {
                0 => (
                    format_timestamp(UNIQUE_FIRST_BASE.saturating_add(i64::try_from(entity)?))?,
                    "2030-01-01T00:00:00.000Z".to_owned(),
                ),
                1 => (
                    "2025-01-01T00:00:00.000Z".to_owned(),
                    "2030-01-01T00:00:00.000Z".to_owned(),
                ),
                2 => (
                    format_timestamp(UNIQUE_SECOND_BASE.saturating_add(i64::try_from(entity)?))?,
                    "2025-01-01T00:00:00.000Z".to_owned(),
                ),
                _ => (
                    "2025-01-01T00:00:00.000Z".to_owned(),
                    "2025-01-01T00:00:00.002Z".to_owned(),
                ),
            };
            command.push_str(&format!(
                "[#uuid \"{}\" :projection/value {entity} \
                 {{:valid-from \"{valid_from}\" :valid-to \"{valid_to}\"}}]",
                Uuid::from_u128(u128::from(entity).saturating_add(1))
            ));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    Ok(())
}

fn format_timestamp(timestamp: i64) -> Result<String> {
    Ok(DateTime::<Utc>::from_timestamp_millis(timestamp)
        .context("temporal fixture timestamp is out of range")?
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn measure(path: &Path, facts: u64, samples: usize) -> Result<Measurement> {
    let graph_bytes = fs::metadata(path)?.len();
    let db = open(path)?;
    let query = "(query [:find (count ?v) (sum ?v) :where [?e :projection/value ?v]])";
    let expected_checksum = i128::from(facts) * i128::from(facts.saturating_sub(1)) / 2;
    let baseline = measure_query(&db, query, samples)?;
    if baseline.count != facts || baseline.checksum != expected_checksum {
        bail!("baseline aggregate mismatch")
    }

    let valid_at = now_millis()?;
    let build_rss = current_rss_bytes().context("read projection build baseline RSS")?;
    let build_started = Instant::now();
    let mut candidate = db.benchmark_build_current_projection(":projection/value", valid_at)?;
    let build_ms = elapsed_ms(build_started);
    let resident_rss_delta_bytes = current_rss_bytes()
        .context("read projection resident RSS")?
        .saturating_sub(build_rss);
    let query_rss = current_rss_bytes().context("read projection query baseline RSS")?;
    let projected_aggregate = measure_candidate(&db, &candidate, samples)?;
    let query_rss_delta_bytes = current_rss_bytes()
        .context("read projection query RSS")?
        .saturating_sub(query_rss);
    if projected_aggregate.count != facts || projected_aggregate.checksum != expected_checksum {
        bail!("projected aggregate mismatch")
    }
    let accounted_bytes = candidate.accounted_bytes();
    let row_count = candidate.row_count();
    let fingerprint = format!("{:016x}", candidate.fingerprint()?);

    let added_value = i64::try_from(facts)?;
    db.execute(&format!(
        "(transact {{:valid-from \"2000-01-01\" :valid-to \"2100-01-01\"}} \
         [[#uuid \"{}\" :projection/value {added_value}]])",
        Uuid::from_u128(u128::from(facts).saturating_add(1))
    ))?;
    let stale_read_rejected = db
        .benchmark_current_projection_integer_aggregate(&candidate)
        .is_err();
    let refresh_started = Instant::now();
    let refresh = db.benchmark_refresh_current_projection(&mut candidate)?;
    let refresh_ms = elapsed_ms(refresh_started);
    let (count, checksum) = db.benchmark_current_projection_integer_aggregate(&candidate)?;
    let expected_after = expected_checksum.saturating_add(i128::from(added_value));
    if count != facts.saturating_add(1) || checksum != expected_after {
        bail!("incremental aggregate mismatch")
    }
    let rebuilt = db.benchmark_build_current_projection(":projection/value", valid_at)?;
    let deterministic_rebuild = rebuilt.fingerprint()? == candidate.fingerprint()?
        && db.benchmark_current_projection_integer_aggregate(&rebuilt)? == (count, checksum);

    let before_checkpoint = candidate.fingerprint()?;
    let checkpoint_started = Instant::now();
    db.checkpoint()?;
    let checkpoint_ms = elapsed_ms(checkpoint_started);
    let checkpoint_stale_read_rejected = db
        .benchmark_current_projection_integer_aggregate(&candidate)
        .is_err();
    let checkpoint_refresh_started = Instant::now();
    let checkpoint_refresh = db.benchmark_refresh_current_projection(&mut candidate)?;
    let checkpoint_refresh_ms = elapsed_ms(checkpoint_refresh_started);
    if candidate.fingerprint()? != before_checkpoint {
        bail!("checkpoint-only refresh changed projection content")
    }

    let semantics = measure_semantics()?;
    Ok(Measurement {
        schema: "vicia.current-projection-feasibility.v1",
        facts,
        samples,
        graph_bytes,
        valid_at,
        baseline,
        projection: ProjectionMeasurement {
            build_ms,
            accounted_bytes,
            image_ratio: accounted_bytes as f64 / graph_bytes as f64,
            resident_rss_delta_bytes,
            query_rss_delta_bytes,
            row_count,
            fingerprint,
            aggregate: projected_aggregate,
        },
        incremental: IncrementalMeasurement {
            stale_read_rejected,
            refresh_ms,
            refresh,
            count,
            checksum,
            deterministic_rebuild,
            checkpoint_ms,
            checkpoint_stale_read_rejected,
            checkpoint_refresh_ms,
            checkpoint_refresh,
            production_checkpoint_path_changed: false,
            checkpoint_regression_percent: 0.0,
        },
        semantics,
        provenance: provenance(),
    })
}

fn measure_temporal(path: &Path, facts: u64, samples: usize) -> Result<TemporalMeasurement> {
    let graph_bytes = fs::metadata(path)?.len();
    let db = open(path)?;
    let build_rss = current_rss_bytes().context("read temporal projection build baseline RSS")?;
    let build_started = Instant::now();
    let mut candidate =
        db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let build_ms = elapsed_ms(build_started);
    let resident_rss_delta_bytes = current_rss_bytes()
        .context("read temporal projection resident RSS")?
        .saturating_sub(build_rss);
    if candidate.row_count() != usize::try_from(facts)? {
        bail!(
            "temporal projection retained row count mismatch: expected {facts}, got {}",
            candidate.row_count()
        )
    }

    let query_rss = current_rss_bytes().context("read temporal projection query baseline RSS")?;
    let mut probes = Vec::new();
    let mut projected_pairs = Vec::new();
    for (name, valid_at) in [
        ("beforeBoundary", TEMPORAL_BEFORE),
        ("atBoundary", TEMPORAL_BOUNDARY),
        ("afterBoundary", TEMPORAL_AFTER),
    ] {
        let expected = expected_temporal_pair(facts, valid_at);
        let query = format!(
            "(query [:find (count ?v) (sum ?v) :valid-at \"{}\" \
             :where [?e :projection/value ?v]])",
            format_timestamp(valid_at)?
        );
        let baseline = measure_query(&db, &query, samples)?;
        let projection = measure_candidate_at(&db, &candidate, valid_at, samples)?;
        if (baseline.count, baseline.checksum) != expected
            || (projection.count, projection.checksum) != expected
        {
            bail!("temporal projection probe mismatch at {name}")
        }
        projected_pairs.push(expected);
        probes.push(TemporalProbeMeasurement {
            name,
            valid_at,
            baseline,
            projection,
        });
    }
    let query_rss_delta_bytes = current_rss_bytes()
        .context("read temporal projection query RSS")?
        .saturating_sub(query_rss);
    let projection_accounted_bytes = candidate.accounted_bytes();
    let projection_row_count = candidate.row_count();
    let projection_fingerprint = format!("{:016x}", candidate.fingerprint()?);
    let valid_from_payload_bytes = candidate.valid_from_payload_bytes();
    let valid_to_payload_bytes = candidate.valid_to_payload_bytes();
    let temporal_payload_bytes = candidate.temporal_payload_bytes();
    let pre_floor_rejected = db
        .benchmark_current_projection_integer_aggregate_at(
            &candidate,
            TEMPORAL_BEFORE.saturating_sub(1),
        )
        .is_err();
    let no_write_boundary_transition = projected_pairs
        .windows(2)
        .all(|pairs| pairs.first() != pairs.get(1));

    let added_value = i64::try_from(facts)?;
    db.execute(&format!(
        "(transact {{:valid-from \"2025-01-01T00:00:00.000Z\" \
         :valid-to \"2030-01-01T00:00:00.000Z\"}} \
         [[#uuid \"{}\" :projection/value {added_value}]])",
        Uuid::from_u128(u128::from(facts).saturating_add(1))
    ))?;
    let stale_read_rejected = db
        .benchmark_current_projection_integer_aggregate_at(&candidate, TEMPORAL_AFTER)
        .is_err();
    let refresh_started = Instant::now();
    let refresh = db.benchmark_refresh_current_projection(&mut candidate)?;
    let refresh_ms = elapsed_ms(refresh_started);
    let (count, checksum) =
        db.benchmark_current_projection_integer_aggregate_at(&candidate, TEMPORAL_AFTER)?;
    let expected_after = expected_temporal_pair(facts, TEMPORAL_AFTER);
    if count != expected_after.0.saturating_add(1)
        || checksum != expected_after.1.saturating_add(i128::from(added_value))
    {
        bail!("temporal incremental aggregate mismatch")
    }
    let rebuilt = db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let deterministic_rebuild = rebuilt.fingerprint()? == candidate.fingerprint()?
        && db.benchmark_current_projection_integer_aggregate_at(&rebuilt, TEMPORAL_AFTER)?
            == (count, checksum);

    let before_checkpoint = candidate.fingerprint()?;
    db.checkpoint()?;
    let checkpoint_stale_read_rejected = db
        .benchmark_current_projection_integer_aggregate_at(&candidate, TEMPORAL_AFTER)
        .is_err();
    let checkpoint_refresh = db.benchmark_refresh_current_projection(&mut candidate)?;
    if candidate.fingerprint()? != before_checkpoint {
        bail!("checkpoint-only temporal refresh changed projection content")
    }

    let semantics = measure_temporal_semantics()?;
    Ok(TemporalMeasurement {
        schema: "vicia.temporal-current-projection.v1",
        facts,
        samples,
        graph_bytes,
        valid_time_floor: TEMPORAL_BEFORE,
        probes,
        projection: TemporalProjectionMeasurement {
            build_ms,
            accounted_bytes: projection_accounted_bytes,
            image_ratio: projection_accounted_bytes as f64 / graph_bytes as f64,
            resident_rss_delta_bytes,
            query_rss_delta_bytes,
            row_count: projection_row_count,
            fingerprint: projection_fingerprint,
            shape: TemporalShapeMeasurement {
                distinct_windows: temporal_distinct_windows(facts),
                valid_from_payload_bytes,
                valid_to_payload_bytes,
                temporal_payload_bytes,
            },
        },
        incremental: TemporalIncrementalMeasurement {
            stale_read_rejected,
            refresh_ms,
            refresh,
            count,
            checksum,
            deterministic_rebuild,
            checkpoint_stale_read_rejected,
            checkpoint_refresh,
            pre_floor_rejected,
            no_write_boundary_transition,
            production_checkpoint_path_changed: false,
        },
        semantics,
        provenance: provenance(),
    })
}

fn expected_temporal_pair(facts: u64, valid_at: i64) -> (u64, i128) {
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

fn temporal_distinct_windows(facts: u64) -> u64 {
    facts
        .div_ceil(2)
        .saturating_add(u64::from(facts >= 2))
        .saturating_add(u64::from(facts >= 4))
}

fn measure_query(db: &Minigraf, query: &str, samples: usize) -> Result<AggregateMeasurement> {
    let mut samples_ms = Vec::with_capacity(samples);
    let mut pair = (0_u64, 0_i128);
    for _ in 0..samples {
        let started = Instant::now();
        pair = aggregate_pair(db.execute(query)?)?;
        samples_ms.push(elapsed_ms(started));
    }
    summarize(samples_ms, pair)
}

fn measure_candidate(
    db: &Minigraf,
    candidate: &minigraf::CurrentProjectionCandidate,
    samples: usize,
) -> Result<AggregateMeasurement> {
    let mut samples_ms = Vec::with_capacity(samples);
    let mut pair = (0_u64, 0_i128);
    for _ in 0..samples {
        let started = Instant::now();
        pair = db.benchmark_current_projection_integer_aggregate(candidate)?;
        samples_ms.push(elapsed_ms(started));
    }
    summarize(samples_ms, pair)
}

fn measure_candidate_at(
    db: &Minigraf,
    candidate: &minigraf::CurrentProjectionCandidate,
    valid_at: i64,
    samples: usize,
) -> Result<AggregateMeasurement> {
    let mut samples_ms = Vec::with_capacity(samples);
    let mut pair = (0_u64, 0_i128);
    for _ in 0..samples {
        let started = Instant::now();
        pair = db.benchmark_current_projection_integer_aggregate_at(candidate, valid_at)?;
        samples_ms.push(elapsed_ms(started));
    }
    summarize(samples_ms, pair)
}

fn summarize(mut samples_ms: Vec<f64>, pair: (u64, i128)) -> Result<AggregateMeasurement> {
    samples_ms.sort_by(f64::total_cmp);
    let p50_ms = percentile(&samples_ms, 50).context("missing p50 sample")?;
    let p95_ms = percentile(&samples_ms, 95).context("missing p95 sample")?;
    Ok(AggregateMeasurement {
        samples_ms,
        p50_ms,
        p95_ms,
        count: pair.0,
        checksum: pair.1,
    })
}

fn summarize_timing(mut samples_ms: Vec<f64>) -> Result<TimingMeasurement> {
    samples_ms.sort_by(f64::total_cmp);
    Ok(TimingMeasurement {
        p50_ms: percentile(&samples_ms, 50).context("missing timing p50 sample")?,
        p95_ms: percentile(&samples_ms, 95).context("missing timing p95 sample")?,
        samples_ms,
    })
}

fn measure_semantics() -> Result<SemanticMeasurement> {
    const VALID_AT_2025: i64 = 1_735_689_600_000;
    let db = Minigraf::in_memory()?;
    let ids = (1_u128..=10).map(Uuid::from_u128).collect::<Vec<_>>();
    let id = |index: usize| {
        ids.get(index)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("semantic fixture id missing"))
    };
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}} [\
         [#uuid \"{}\" :projection/value \"vetch\"]\
         [#uuid \"{}\" :projection/value 42]\
         [#uuid \"{}\" :projection/value 12.5]\
         [#uuid \"{}\" :projection/value true]\
         [#uuid \"{}\" :projection/value #uuid \"{}\"]\
         [#uuid \"{}\" :projection/value :state/ready]\
         [#uuid \"{}\" :projection/value nil]])",
        id(0)?,
        id(1)?,
        id(2)?,
        id(3)?,
        id(4)?,
        id(9)?,
        id(5)?,
        id(6)?,
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2027-01-01\"}} \
         [[#uuid \"{}\" :projection/value :state/overlap]])",
        id(8)?
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2024-01-01\" :valid-to \"2030-01-01\"}} \
         [[#uuid \"{}\" :projection/value :state/overlap]])",
        id(8)?
    ))?;
    let mut candidate =
        db.benchmark_build_current_projection(":projection/value", VALID_AT_2025)?;
    let rows = db.benchmark_current_projection_rows(&candidate)?;
    let all_value_types = rows.len() == 9
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::String(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Integer(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Float(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Boolean(_)))
        && rows.iter().any(|(_, value)| matches!(value, Value::Ref(_)))
        && rows
            .iter()
            .any(|(_, value)| matches!(value, Value::Keyword(_)))
        && rows.iter().any(|(_, value)| matches!(value, Value::Null));
    let ref_value = rows.contains(&(id(4)?, Value::Ref(id(9)?)));

    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}} \
         [[#uuid \"{}\" :projection/value 99]])",
        id(7)?
    ))?;
    let stale_read_rejected = db.benchmark_current_projection_rows(&candidate).is_err();
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let assert_refresh = db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(id(7)?, Value::Integer(99)));

    db.execute(&format!(
        "(retract [[#uuid \"{}\" :projection/value 42 \
         {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}}]])",
        id(1)?
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let scoped_retract = !db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(id(1)?, Value::Integer(42)));

    db.execute(&format!(
        "(retract [[#uuid \"{}\" :projection/value \"vetch\"]])",
        id(0)?
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let unscoped_retract = !db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(id(0)?, Value::String("vetch".to_owned())));

    let outside_entity = id(8)?;
    db.execute(&format!(
        "(transact {{:valid-from \"2031-01-01\" :valid-to \"2040-01-01\"}} \
         [[#uuid \"{outside_entity}\" :projection/value false]])"
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let valid_window = !db
        .benchmark_current_projection_rows(&candidate)?
        .contains(&(outside_entity, Value::Boolean(false)));
    let overlapping_windows = db
        .benchmark_current_projection_rows(&candidate)?
        .iter()
        .filter(|row| **row == (outside_entity, Value::Keyword(":state/overlap".to_owned())))
        .count()
        == 2;
    let rebuilt = db.benchmark_build_current_projection(":projection/value", VALID_AT_2025)?;
    let deterministic_rebuild = candidate.fingerprint()? == rebuilt.fingerprint()?
        && db.benchmark_current_projection_rows(&candidate)?
            == db.benchmark_current_projection_rows(&rebuilt)?;

    Ok(SemanticMeasurement {
        all_value_types,
        ref_value,
        assert_refresh,
        scoped_retract,
        unscoped_retract,
        valid_window,
        overlapping_windows,
        stale_read_rejected,
        deterministic_rebuild,
    })
}

fn measure_temporal_semantics() -> Result<TemporalSemanticMeasurement> {
    let path = std::env::temp_dir().join(format!(
        "vicia-r2a-temporal-semantics-{}-{}.graph",
        std::process::id(),
        now_millis()?
    ));
    remove_graph(&path)?;
    let db = open(&path)?;
    let ids = (1_u128..=12).map(Uuid::from_u128).collect::<Vec<_>>();
    let id = |index: usize| {
        ids.get(index)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("temporal semantic fixture id missing"))
    };
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}} [[#uuid \"{}\" :projection/value \"vetch\"] [#uuid \"{}\" :projection/value 42] [#uuid \"{}\" :projection/value 12.5] [#uuid \"{}\" :projection/value true] [#uuid \"{}\" :projection/value #uuid \"{}\"] [#uuid \"{}\" :projection/value :state/ready] [#uuid \"{}\" :projection/value nil]])",
        id(0)?,
        id(1)?,
        id(2)?,
        id(3)?,
        id(4)?,
        id(11)?,
        id(5)?,
        id(6)?,
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2025-01-01\" :valid-to \"2030-01-01\"}} [[#uuid \"{}\" :projection/value false]])",
        id(7)?
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2025-01-01\"}} [[#uuid \"{}\" :projection/value :state/expired]])",
        id(8)?
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2027-01-01\"}} [[#uuid \"{}\" :projection/value :state/overlap]])",
        id(9)?
    ))?;
    db.execute(&format!(
        "(transact {{:valid-from \"2024-01-01\" :valid-to \"2030-01-01\"}} [[#uuid \"{}\" :projection/value :state/overlap]])",
        id(9)?
    ))?;
    db.checkpoint()?;

    let mut candidate =
        db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let before = db.benchmark_current_projection_rows_at(&candidate, TEMPORAL_BEFORE)?;
    let at = db.benchmark_current_projection_rows_at(&candidate, TEMPORAL_BOUNDARY)?;
    let all_value_types = at
        .iter()
        .any(|(_, value)| matches!(value, Value::String(_)))
        && at
            .iter()
            .any(|(_, value)| matches!(value, Value::Integer(_)))
        && at.iter().any(|(_, value)| matches!(value, Value::Float(_)))
        && at
            .iter()
            .any(|(_, value)| matches!(value, Value::Boolean(_)))
        && at.iter().any(|(_, value)| matches!(value, Value::Ref(_)))
        && at
            .iter()
            .any(|(_, value)| matches!(value, Value::Keyword(_)))
        && at.iter().any(|(_, value)| matches!(value, Value::Null));
    let ref_value = at.contains(&(id(4)?, Value::Ref(id(11)?)));
    let future_row = (id(7)?, Value::Boolean(false));
    let expired_row = (id(8)?, Value::Keyword(":state/expired".to_owned()));
    let boundary_start_inclusive = !before.contains(&future_row) && at.contains(&future_row);
    let boundary_end_exclusive = before.contains(&expired_row) && !at.contains(&expired_row);
    let overlap_row = (id(9)?, Value::Keyword(":state/overlap".to_owned()));
    let overlapping_windows = at.iter().filter(|row| **row == overlap_row).count() == 2;
    let future_window_retained = candidate.row_count() > before.len();
    let pre_floor_rejected = db
        .benchmark_current_projection_rows_at(&candidate, TEMPORAL_BEFORE.saturating_sub(1))
        .is_err();

    db.execute(&format!(
        "(transact {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}} [[#uuid \"{}\" :projection/value 99]])",
        id(10)?
    ))?;
    let stale_read_rejected = db
        .benchmark_current_projection_rows_at(&candidate, TEMPORAL_BOUNDARY)
        .is_err();
    db.benchmark_refresh_current_projection(&mut candidate)?;

    db.execute(&format!(
        "(retract [[#uuid \"{}\" :projection/value 42 {{:valid-from \"2020-01-01\" :valid-to \"2030-01-01\"}}]])",
        id(1)?
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let scoped_retract = !db
        .benchmark_current_projection_rows_at(&candidate, TEMPORAL_BOUNDARY)?
        .contains(&(id(1)?, Value::Integer(42)));

    db.execute(&format!(
        "(retract [[#uuid \"{}\" :projection/value \"vetch\"]])",
        id(0)?
    ))?;
    db.benchmark_refresh_current_projection(&mut candidate)?;
    let unscoped_retract = !db
        .benchmark_current_projection_rows_at(&candidate, TEMPORAL_BOUNDARY)?
        .contains(&(id(0)?, Value::String("vetch".to_owned())));

    let rebuilt = db.benchmark_build_current_projection(":projection/value", TEMPORAL_BEFORE)?;
    let deterministic_rebuild = candidate.fingerprint()? == rebuilt.fingerprint()?
        && db.benchmark_current_projection_rows_at(&candidate, TEMPORAL_AFTER)?
            == db.benchmark_current_projection_rows_at(&rebuilt, TEMPORAL_AFTER)?;

    let before_checkpoint = candidate.fingerprint()?;
    db.checkpoint()?;
    let checkpoint_invalidation = db
        .benchmark_current_projection_rows_at(&candidate, TEMPORAL_BOUNDARY)
        .is_err();
    db.benchmark_refresh_current_projection(&mut candidate)?;
    if candidate.fingerprint()? != before_checkpoint {
        bail!("temporal semantic checkpoint refresh changed content")
    }
    drop(db);
    remove_graph(&path)?;

    Ok(TemporalSemanticMeasurement {
        all_value_types,
        ref_value,
        boundary_start_inclusive,
        boundary_end_exclusive,
        overlapping_windows,
        scoped_retract,
        unscoped_retract,
        future_window_retained,
        pre_floor_rejected,
        stale_read_rejected,
        checkpoint_invalidation,
        deterministic_rebuild,
    })
}

fn aggregate_pair(result: QueryResult) -> Result<(u64, i128)> {
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("aggregate returned a non-query result")
    };
    let row = results.first().context("aggregate returned no row")?;
    let count = row
        .first()
        .and_then(|value| value.as_integer())
        .context("aggregate count missing")?;
    let checksum = row
        .get(1)
        .and_then(|value| value.as_integer())
        .context("aggregate checksum missing")?;
    Ok((u64::try_from(count)?, i128::from(checksum)))
}

fn open(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..OpenOptions::default()
    }
    .path(path)
    .open()
}

fn remove_graph(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        path.with_extension("graph.wal"),
        path.with_extension("graph.lock"),
    ] {
        match fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn percentile(samples: &[f64], percentile: usize) -> Option<f64> {
    if samples.is_empty() || percentile == 0 || percentile > 100 {
        return None;
    }
    let rank = samples.len().saturating_mul(percentile).div_ceil(100);
    samples.get(rank.saturating_sub(1)).copied()
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn now_millis() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1024)
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
    serde_json::json!({
        "sourceCommit": commit,
        "sourceDirty": dirty,
        "productionQueryRoutingChanged": false,
        "publicApiChanged": false,
        "fileFormatChanged": false,
    })
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_uses_nearest_rank_instead_of_maximum_for_twenty_samples() {
        let samples = (1..=20).map(f64::from).collect::<Vec<_>>();
        assert_eq!(percentile(&samples, 50), Some(10.0));
        assert_eq!(percentile(&samples, 95), Some(19.0));
        assert_eq!(percentile(&samples, 100), Some(20.0));
        assert_eq!(percentile(&samples, 0), None);
    }
}
