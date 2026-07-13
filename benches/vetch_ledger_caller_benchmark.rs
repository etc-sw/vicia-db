#![cfg(not(target_arch = "wasm32"))]

use anyhow::{Context, Result, bail};
use minigraf::{FactRecord, Minigraf, OpenOptions, QueryResult, Value};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

#[path = "helpers/receipt.rs"]
mod receipt;

const FIXTURE_SOURCE: &str = include_str!("../benchmarks/fixtures/vetch-ledger-caller.v1.json");
const FIXTURE_SCHEMA: &str = "vicia.vetch-ledger-caller-fixture.v1";
const SMOKE_BASE_FACTS: usize = 10_000;
const FULL_BASE_FACTS: usize = 1_000_000;
const BASE_BATCH_SIZE: usize = 1_000;
const SMOKE_SAMPLES: usize = 20;
const FULL_SAMPLES: usize = 30;

#[derive(Deserialize)]
struct CallerFixture {
    schema: String,
    source: FixtureSource,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct FixtureSource {
    repository: String,
    commit: String,
    paths: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Scenario {
    id: String,
    changes: Vec<Change>,
    proof: Proof,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Assert,
    Retract,
}

#[derive(Clone, Deserialize)]
struct Change {
    operation: Operation,
    entity: Uuid,
    attribute: String,
    value: FixtureValue,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum FixtureValue {
    String { value: String },
    Integer { value: i64 },
    Boolean { value: bool },
    Ref { value: Uuid },
    Keyword { value: String },
    Null,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Proof {
    entity: Uuid,
    attribute: String,
    expected_rows: usize,
}

#[derive(Clone, Copy)]
struct Config {
    profile: &'static str,
    base_facts: usize,
    samples: usize,
}

#[derive(Default)]
struct ScenarioSamples {
    caller_encoding: Vec<Duration>,
    datalog_materialization: Vec<Duration>,
    mutation: Vec<Duration>,
    proof_read: Vec<Duration>,
    source_bytes: Vec<f64>,
    fact_count: Vec<f64>,
}

fn main() -> Result<()> {
    let started_at = SystemTime::now();
    let config = selected_config();
    let fixture: CallerFixture = serde_json::from_str(FIXTURE_SOURCE)?;
    validate_fixture(&fixture)?;

    let root = tempfile::tempdir()?;
    let base_path = root
        .path()
        .join(format!("h0-base-{}.graph", config.profile));
    let fixture_source = receipt::install_base_fixture_if_configured(&base_path)?;
    if fixture_source.is_some() {
        verify_base_fact_count(&base_path, config.base_facts)?;
    } else {
        build_checkpointed_base(&base_path, config.base_facts)?;
    }
    let base_digest = receipt::sha256_file(&base_path)?;
    let run_path = root.path().join(format!("h0-run-{}.graph", config.profile));
    copy_graph(&base_path, &run_path)?;
    let db = open_no_auto_checkpoint(&run_path)?;

    let mut all_samples = BTreeMap::new();
    let mut correctness = Vec::new();
    for scenario in &fixture.scenarios {
        let (samples, checks) = measure_scenario(&db, scenario, config.samples)?;
        all_samples.insert(scenario.id.clone(), samples);
        correctness.extend(checks);
    }
    correctness.extend(ledger_boundary_probes()?);

    let metrics = receipt_metrics(&all_samples)?;
    let fixture_digest = format!("{:x}", Sha256::digest(FIXTURE_SOURCE.as_bytes()));
    receipt::write_if_requested(receipt::ReceiptInput {
        suite: "vetch-ledger-caller".to_owned(),
        profile: config.profile.to_owned(),
        started_at,
        configuration: json!({
            "mode": config.profile,
            "baseFacts": config.base_facts,
            "samplesPerScenario": config.samples,
            "fixtureSchema": fixture.schema,
            "fixtureSha256": fixture_digest,
            "fixtureSourceRepository": fixture.source.repository,
            "fixtureSourceCommit": fixture.source.commit,
            "fixtureSourcePaths": fixture.source.paths,
            "fixtureOrigin": if fixture_source.is_some() { "provided" } else { "generated" },
            "fixtureSource": fixture_source.map(|path| path.display().to_string()),
            "baseFixtureSha256": base_digest,
            "coldWarmPolicy": "one-checkpointed-base-warm-caller-cycles",
            "atomicExpectationDecision": "current fixtures use serialized writer admission; no database expectation admitted by H0",
            "structuredReceiptDecision": "transaction cursor plus caller-owned typed delta is sufficient for exact projection patching"
        }),
        metrics,
        files: json!({
            "baseFixtureSha256": base_digest,
            "baseBytes": std::fs::metadata(&base_path)?.len(),
            "finalBytes": std::fs::metadata(&run_path)?.len()
        }),
        correctness_checks: correctness,
    })?;
    Ok(())
}

fn ledger_boundary_probes() -> Result<Vec<receipt::CorrectnessCheck>> {
    let expectation_db = Minigraf::in_memory().map_err(db_error)?;
    expectation_db
        .execute("(transact [[:proposal :vetch_proposal/status :open]])")
        .map_err(db_error)?;
    let first_basis = query_row_count(
        &expectation_db,
        "(query [:find ?e :where [?e :vetch_proposal/status :open]])",
    )?;
    let mut first = expectation_db.begin_write().map_err(db_error)?;
    first
        .execute("(retract [[:proposal :vetch_proposal/status :open]])")
        .map_err(db_error)?;
    first
        .execute("(transact [[:proposal :vetch_proposal/status :accepted]])")
        .map_err(db_error)?;
    first.commit().map_err(db_error)?;
    let cursor_after_first = expectation_db.current_tx_count();
    let second_basis = query_row_count(
        &expectation_db,
        "(query [:find ?e :where [?e :vetch_proposal/status :open]])",
    )?;
    let serialized_rejection_consumed_no_tx =
        expectation_db.current_tx_count() == cursor_after_first;

    let read_db = Minigraf::in_memory().map_err(db_error)?;
    let mut initial = read_db.begin_write().map_err(db_error)?;
    initial
        .execute("(transact [[:source :vetch_source/title \"source\"]])")
        .map_err(db_error)?;
    initial
        .execute("(transact [[:receipt :vetch_receipt/outcome :pending]])")
        .map_err(db_error)?;
    initial.commit().map_err(db_error)?;
    let first_read_cursor = read_db.current_tx_count();
    let first_rows = query_row_count(
        &read_db,
        "(query [:find ?v :where [:source :vetch_source/title ?v]])",
    )?;
    read_db
        .execute("(transact [[:receipt :vetch_receipt/outcome :observed]])")
        .map_err(db_error)?;
    let second_rows = query_row_count(
        &read_db,
        "(query [:find ?e :where [?e :vetch_receipt/outcome :observed]])",
    )?;
    let separate_reads_can_mix_cursors =
        first_rows == 1 && second_rows == 1 && read_db.current_tx_count() != first_read_cursor;

    Ok(vec![
        receipt::CorrectnessCheck::equal(
            "serialized-admission-rejects-stale-second-verdict-without-consuming-tx",
            json!(true),
            json!(first_basis == 1 && second_basis == 0 && serialized_rejection_consumed_no_tx),
        ),
        receipt::CorrectnessCheck::equal(
            "separate-agent-brief-reads-can-mix-transaction-cursors",
            json!(true),
            json!(separate_reads_can_mix_cursors),
        ),
    ])
}

fn query_row_count(db: &Minigraf, query: &str) -> Result<usize> {
    match db.execute(query).map_err(db_error)? {
        QueryResult::QueryResults { results, .. } => Ok(results.len()),
        _ => bail!("boundary probe did not return query rows"),
    }
}

fn selected_config() -> Config {
    if std::env::args().nth(1).as_deref() == Some("smoke") {
        Config {
            profile: "smoke",
            base_facts: SMOKE_BASE_FACTS,
            samples: SMOKE_SAMPLES,
        }
    } else {
        Config {
            profile: "full",
            base_facts: FULL_BASE_FACTS,
            samples: FULL_SAMPLES,
        }
    }
}

fn validate_fixture(fixture: &CallerFixture) -> Result<()> {
    if fixture.schema != FIXTURE_SCHEMA {
        bail!("unsupported caller fixture schema");
    }
    if fixture.scenarios.len() != 4 {
        bail!("H0 caller fixture must contain exactly four scenarios");
    }
    for required in [
        "cards.move",
        "condense.admit",
        "proposal.verdict",
        "agent.brief",
    ] {
        if !fixture
            .scenarios
            .iter()
            .any(|scenario| scenario.id == required)
        {
            bail!("H0 caller fixture is missing {required}");
        }
    }
    for scenario in &fixture.scenarios {
        if scenario.changes.is_empty() {
            bail!("H0 caller scenario {} has no typed changes", scenario.id);
        }
        if scenario.proof.expected_rows == 0 {
            bail!("H0 caller scenario {} has an empty proof", scenario.id);
        }
    }
    Ok(())
}

fn measure_scenario(
    db: &Minigraf,
    scenario: &Scenario,
    sample_count: usize,
) -> Result<(ScenarioSamples, Vec<receipt::CorrectnessCheck>)> {
    let mut samples = ScenarioSamples::default();
    let mut last_tail = Vec::new();
    let mut last_proof_rows = 0usize;
    let mut last_preparation = None;
    let mut last_instance = None;

    for sample_index in 0..sample_count {
        let started = Instant::now();
        let instance = instantiate_scenario(scenario, sample_index);
        let commands = encode_commands(&instance.changes)?;
        samples.caller_encoding.push(started.elapsed());

        let started = Instant::now();
        let preparation = Minigraf::benchmark_atomic_write_preparation(&commands)?;
        samples.datalog_materialization.push(started.elapsed());
        samples.source_bytes.push(preparation.source_bytes as f64);
        samples.fact_count.push(
            preparation
                .transacted_fact_count
                .saturating_add(preparation.retracted_fact_count) as f64,
        );

        let before = db.current_tx_count();
        let started = Instant::now();
        let mut tx = db.begin_write().map_err(db_error)?;
        for command in &commands {
            tx.execute(command).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        samples.mutation.push(started.elapsed());

        let tail = db.export_fact_log_since(before).map_err(db_error)?;
        let started = Instant::now();
        let proof_rows = exact_proof_rows(db, &instance.proof)?;
        samples.proof_read.push(started.elapsed());
        last_tail = tail;
        last_proof_rows = proof_rows;
        last_preparation = Some(preparation);
        last_instance = Some(instance);
    }

    let preparation = last_preparation.context("scenario produced no preparation diagnostics")?;
    let instance = last_instance.context("scenario produced no caller instance")?;
    let expected_assertions = instance
        .changes
        .iter()
        .filter(|change| change.operation == Operation::Assert)
        .count();
    let expected_retractions = instance.changes.len().saturating_sub(expected_assertions);
    let one_tx = last_tail
        .first()
        .map(|first| {
            last_tail
                .iter()
                .all(|record| record.tx_count == first.tx_count)
        })
        .unwrap_or(false);
    let exact_identity = tail_matches_changes(&last_tail, &instance.changes)?;

    Ok((
        samples,
        vec![
            receipt::CorrectnessCheck::equal(
                &format!("{}.typed-fact-count", scenario.id),
                json!(instance.changes.len()),
                json!(last_tail.len()),
            ),
            receipt::CorrectnessCheck::equal(
                &format!("{}.assertion-count", scenario.id),
                json!(expected_assertions),
                json!(preparation.transacted_fact_count),
            ),
            receipt::CorrectnessCheck::equal(
                &format!("{}.retraction-count", scenario.id),
                json!(expected_retractions),
                json!(preparation.retracted_fact_count),
            ),
            receipt::CorrectnessCheck::equal(
                &format!("{}.one-transaction-cursor", scenario.id),
                json!(true),
                json!(one_tx),
            ),
            receipt::CorrectnessCheck::equal(
                &format!("{}.full-history-identity", scenario.id),
                json!(true),
                json!(exact_identity),
            ),
            receipt::CorrectnessCheck::equal(
                &format!("{}.exact-proof", scenario.id),
                json!(instance.proof.expected_rows),
                json!(last_proof_rows),
            ),
        ],
    ))
}

fn instantiate_scenario(scenario: &Scenario, sample_index: usize) -> Scenario {
    let mut instance = scenario.clone();
    for change in &mut instance.changes {
        change.entity = sample_uuid(change.entity, sample_index);
        if let FixtureValue::Ref { value } = &mut change.value {
            *value = sample_uuid(*value, sample_index);
        }
    }
    instance.proof.entity = sample_uuid(instance.proof.entity, sample_index);
    instance
}

fn sample_uuid(original: Uuid, sample_index: usize) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("h0:{original}:{sample_index}").as_bytes(),
    )
}

fn encode_commands(changes: &[Change]) -> Result<Vec<String>> {
    let mut commands = Vec::new();
    for operation in [Operation::Retract, Operation::Assert] {
        let selected = changes
            .iter()
            .filter(|change| change.operation == operation)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let verb = if operation == Operation::Assert {
            "transact"
        } else {
            "retract"
        };
        let mut command = format!("({verb} [");
        for change in selected {
            command.push('[');
            command.push_str(&format!("#uuid \"{}\" ", change.entity));
            command.push_str(&change.attribute);
            command.push(' ');
            command.push_str(&encode_value(&change.value)?);
            command.push(']');
        }
        command.push_str("])");
        commands.push(command);
    }
    Ok(commands)
}

fn encode_value(value: &FixtureValue) -> Result<String> {
    Ok(match value {
        FixtureValue::String { value } => serde_json::to_string(value)?,
        FixtureValue::Integer { value } => value.to_string(),
        FixtureValue::Boolean { value } => value.to_string(),
        FixtureValue::Ref { value } => format!("#uuid \"{value}\""),
        FixtureValue::Keyword { value } => value.clone(),
        FixtureValue::Null => "nil".to_owned(),
    })
}

fn tail_matches_changes(records: &[FactRecord], changes: &[Change]) -> Result<bool> {
    if records.len() != changes.len() {
        return Ok(false);
    }
    for change in changes {
        let expected = fixture_value(&change.value);
        let asserted = change.operation == Operation::Assert;
        if !records.iter().any(|record| {
            record.entity == change.entity
                && record.attribute == change.attribute
                && record.value == expected
                && record.asserted == asserted
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn fixture_value(value: &FixtureValue) -> Value {
    match value {
        FixtureValue::String { value } => Value::String(value.clone()),
        FixtureValue::Integer { value } => Value::Integer(*value),
        FixtureValue::Boolean { value } => Value::Boolean(*value),
        FixtureValue::Ref { value } => Value::Ref(*value),
        FixtureValue::Keyword { value } => Value::Keyword(value.clone()),
        FixtureValue::Null => Value::Null,
    }
}

fn exact_proof_rows(db: &Minigraf, proof: &Proof) -> Result<usize> {
    let query = format!(
        "(query [:find ?v :where [#uuid \"{}\" {} ?v]])",
        proof.entity, proof.attribute
    );
    match db.execute(&query).map_err(db_error)? {
        QueryResult::QueryResults { results, .. } => Ok(results.len()),
        _ => bail!("exact proof did not return query rows"),
    }
}

fn receipt_metrics(
    samples: &BTreeMap<String, ScenarioSamples>,
) -> Result<BTreeMap<String, receipt::MetricSeries>> {
    let mut metrics = BTreeMap::new();
    for (scenario, values) in samples {
        for (suffix, durations) in [
            ("caller_encoding", &values.caller_encoding),
            ("datalog_materialization", &values.datalog_materialization),
            ("mutation", &values.mutation),
            ("proof_read", &values.proof_read),
        ] {
            metrics.insert(
                format!("{scenario}.{suffix}"),
                receipt::MetricSeries::from_durations(durations)?,
            );
        }
        metrics.insert(
            format!("{scenario}.source_bytes"),
            receipt::MetricSeries::from_values("bytes", values.source_bytes.clone())?,
        );
        metrics.insert(
            format!("{scenario}.fact_count"),
            receipt::MetricSeries::from_values("facts", values.fact_count.clone())?,
        );
    }
    Ok(metrics)
}

fn build_checkpointed_base(path: &Path, fact_count: usize) -> Result<()> {
    let db = open_no_auto_checkpoint(path)?;
    for start in (0..fact_count).step_by(BASE_BATCH_SIZE) {
        let end = start.saturating_add(BASE_BATCH_SIZE).min(fact_count);
        let mut command = String::from("(transact [");
        for index in start..end {
            command.push_str(&format!("[:h0-base-{index} :h0/value {index}]"));
        }
        command.push_str("])");
        db.execute(&command).map_err(db_error)?;
    }
    db.checkpoint().map_err(db_error)?;
    Ok(())
}

fn verify_base_fact_count(path: &Path, expected: usize) -> Result<()> {
    let db = open_no_auto_checkpoint(path)?;
    let actual = db.export_fact_log().map_err(db_error)?.len();
    if actual != expected {
        bail!("provided base fixture fact count does not match H0 profile");
    }
    Ok(())
}

fn copy_graph(source: &Path, destination: &Path) -> Result<()> {
    std::fs::copy(source, destination)?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)?
        .sync_all()?;
    Ok(())
}

fn open_no_auto_checkpoint(path: &Path) -> Result<Minigraf> {
    OpenOptions {
        wal_checkpoint_threshold: usize::MAX,
        ..Default::default()
    }
    .path(path)
    .open()
    .map_err(db_error)
}

fn db_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{}", error)
}
