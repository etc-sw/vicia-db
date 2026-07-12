use anyhow::{Context, Result, bail};
use cozo::{DataValue, DbInstance, Num, ScriptMutability};
use minigraf::{OpenOptions, QueryResult};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use sqlite::{Connection, State};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const REDB_FACTS: TableDefinition<u64, i64> = TableDefinition::new("facts");
const INSERT_BATCH: u64 = 1_000;

#[derive(Clone, Copy)]
enum Engine {
    Vicia,
    Cozo,
    Sqlite,
    Redb,
}

impl Engine {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "vicia" => Ok(Self::Vicia),
            "cozo" => Ok(Self::Cozo),
            "sqlite" => Ok(Self::Sqlite),
            "redb" => Ok(Self::Redb),
            _ => bail!("unknown engine {value}; expected vicia, cozo, sqlite, or redb"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Vicia => "vicia",
            Self::Cozo => "cozo",
            Self::Sqlite => "sqlite",
            Self::Redb => "redb",
        }
    }

    fn classification(self) -> &'static str {
        match self {
            Self::Vicia => "product-bi-temporal-datalog",
            Self::Cozo => "peer-embedded-datalog-graph",
            Self::Sqlite => "baseline-embedded-relational-eav",
            Self::Redb => "floor-embedded-key-value",
        }
    }

    fn version(self) -> &'static str {
        match self {
            Self::Vicia => "source checkout (see sourceCommit)",
            Self::Cozo => "0.7.6",
            Self::Sqlite => "bundled SQLite via sqlite 0.32.0",
            Self::Redb => "4.1.0",
        }
    }

    fn supports_engine_aggregate(self) -> bool {
        !matches!(self, Self::Redb)
    }
}

#[derive(Clone, Copy)]
enum ScanWorkload {
    EngineAggregate,
    MaterializedScan,
}

impl ScanWorkload {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "engine-aggregate" => Ok(Self::EngineAggregate),
            "materialized-scan" => Ok(Self::MaterializedScan),
            _ => bail!(
                "unknown scan workload {value}; expected engine-aggregate or materialized-scan"
            ),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::EngineAggregate => "engineAggregate",
            Self::MaterializedScan => "materializedScan",
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    base_facts: u64,
    cycles: u64,
    facts_per_cycle: u64,
    reads_per_cycle: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SampleStats {
    unit: &'static str,
    count: usize,
    min: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    mean: f64,
    samples: Vec<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Metrics {
    build: SampleStats,
    reopen: SampleStats,
    durable_append: SampleStats,
    point_read: SampleStats,
    engine_aggregate: Option<ScanMeasurement>,
    materialized_scan: ScanMeasurement,
    total_ms: f64,
    stress_process_peak_rss_bytes: Option<u64>,
    primary_file_bytes: u64,
    total_storage_bytes: u64,
    bytes_per_fact: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Correctness {
    expected_count: u64,
    expected_checksum: i128,
    materialized_count: u64,
    materialized_checksum: i128,
    engine_aggregate_count: Option<u64>,
    engine_aggregate_checksum: Option<i128>,
    repeated_reopen_cycles: u64,
    integrity_check: String,
    passed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    engine: &'static str,
    engine_version: &'static str,
    classification: &'static str,
    source_commit: Option<String>,
    source_dirty: Option<bool>,
    host: HostProvenance,
    generated_at_unix_ms: u128,
    config: Config,
    metrics: Metrics,
    correctness: Correctness,
    database_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostProvenance {
    testbed: String,
    os: &'static str,
    arch: &'static str,
    logical_cpus: usize,
    cpu_model: Option<String>,
    memory_bytes: Option<u64>,
}

struct RunMeasurements {
    build_ms: f64,
    reopen_ms: Vec<f64>,
    append_ms: Vec<f64>,
    read_ms: Vec<f64>,
    integrity: String,
    primary_path: PathBuf,
}

#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanMeasurement {
    workload: String,
    execution_boundary: String,
    elapsed_ms: f64,
    peak_rss_bytes: Option<u64>,
    count: u64,
    checksum: i128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CrashReceipt {
    schema: &'static str,
    engine: &'static str,
    minimum_committed_count: u64,
    recovered_count: u64,
    recovered_checksum: i128,
    expected_checksum: i128,
    integrity_check: String,
    passed: bool,
}

fn measure_scan_in_child(
    engine: Engine,
    db_dir: &Path,
    workload: ScanWorkload,
) -> Result<ScanMeasurement> {
    let output = Command::new(std::env::current_exe()?)
        .arg("scan")
        .arg(engine.id())
        .arg(db_dir)
        .arg(match workload {
            ScanWorkload::EngineAggregate => "engine-aggregate",
            ScanWorkload::MaterializedScan => "materialized-scan",
        })
        .output()
        .with_context(|| format!("launch isolated {} scan", engine.id()))?;
    if !output.status.success() {
        bail!(
            "isolated {} {} failed: {}",
            engine.id(),
            workload.id(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode isolated scan measurement")
}

fn measure_scan(engine: Engine, db_dir: &Path, workload: ScanWorkload) -> Result<ScanMeasurement> {
    if matches!(workload, ScanWorkload::EngineAggregate) && !engine.supports_engine_aggregate() {
        bail!("{} has no query-engine aggregate boundary", engine.id());
    }

    let (count, checksum, execution_boundary, elapsed_ms) = match engine {
        Engine::Vicia => {
            let db = OpenOptions::new().path(db_dir.join("vicia.graph")).open()?;
            let started = Instant::now();
            let (count, checksum) = match workload {
                ScanWorkload::EngineAggregate => vicia_aggregate(&db)?,
                ScanWorkload::MaterializedScan => vicia_scan(&db)?,
            };
            let boundary = match workload {
                ScanWorkload::EngineAggregate => "datalog-count-sum-scalar",
                ScanWorkload::MaterializedScan => "datalog-all-values-rust-fold",
            };
            (count, checksum, boundary, elapsed_ms(started))
        }
        Engine::Cozo => {
            let db = cozo_open(&db_dir.join("cozo.db"))?;
            let started = Instant::now();
            let (count, checksum) = match workload {
                ScanWorkload::EngineAggregate => cozo_aggregate(&db)?,
                ScanWorkload::MaterializedScan => cozo_scan(&db)?,
            };
            let boundary = match workload {
                ScanWorkload::EngineAggregate => "cozoscript-count-sum-scalar",
                ScanWorkload::MaterializedScan => "cozoscript-all-values-rust-fold",
            };
            (count, checksum, boundary, elapsed_ms(started))
        }
        Engine::Sqlite => {
            let db = sqlite_open(&db_dir.join("sqlite.db"))?;
            let started = Instant::now();
            let (count, checksum) = match workload {
                ScanWorkload::EngineAggregate => sqlite_aggregate(&db)?,
                ScanWorkload::MaterializedScan => sqlite_scan(&db)?,
            };
            let boundary = match workload {
                ScanWorkload::EngineAggregate => "sql-count-sum-scalar",
                ScanWorkload::MaterializedScan => "sql-all-values-rust-fold",
            };
            (count, checksum, boundary, elapsed_ms(started))
        }
        Engine::Redb => {
            let db = Database::open(db_dir.join("redb.db"))?;
            let started = Instant::now();
            let (count, checksum) = redb_scan(&db)?;
            (
                count,
                checksum,
                "ordered-table-all-values-rust-fold",
                elapsed_ms(started),
            )
        }
    };

    Ok(ScanMeasurement {
        workload: workload.id().to_string(),
        execution_boundary: execution_boundary.to_string(),
        elapsed_ms,
        peak_rss_bytes: peak_rss_bytes(),
        count,
        checksum,
    })
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("scan") {
        if args.len() != 5 {
            bail!(
                "usage: vicia-cross-db-bench scan <engine> <db-dir> <engine-aggregate|materialized-scan>"
            );
        }
        let measurement = measure_scan(
            Engine::parse(&args[2])?,
            Path::new(&args[3]),
            ScanWorkload::parse(&args[4])?,
        )?;
        println!("{}", serde_json::to_string(&measurement)?);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("crash-write") {
        if args.len() != 7 {
            bail!(
                "usage: vicia-cross-db-bench crash-write <engine> <db-dir> <start-entity> <cycles> <facts-per-cycle>"
            );
        }
        return crash_write(
            Engine::parse(&args[2])?,
            Path::new(&args[3]),
            args[4].parse()?,
            args[5].parse()?,
            args[6].parse()?,
        );
    }
    if args.get(1).map(String::as_str) == Some("verify") {
        if args.len() != 5 {
            bail!("usage: vicia-cross-db-bench verify <engine> <db-dir> <minimum-count>");
        }
        let receipt = verify_after_crash(
            Engine::parse(&args[2])?,
            Path::new(&args[3]),
            args[4].parse()?,
        )?;
        println!("{}", serde_json::to_string(&receipt)?);
        if !receipt.passed {
            bail!("crash recovery verification failed for {}", receipt.engine);
        }
        return Ok(());
    }
    if args.len() != 9 || args[1] != "stress" {
        bail!(
            "usage: vicia-cross-db-bench stress <engine> <db-dir> <base-facts> <cycles> <facts-per-cycle> <reads-per-cycle> <receipt.json>"
        );
    }
    let engine = Engine::parse(&args[2])?;
    let db_dir = PathBuf::from(&args[3]);
    let config = Config {
        base_facts: args[4].parse()?,
        cycles: args[5].parse()?,
        facts_per_cycle: args[6].parse()?,
        reads_per_cycle: args[7].parse()?,
    };
    let receipt_path = PathBuf::from(&args[8]);
    if config.base_facts == 0 || config.cycles == 0 || config.facts_per_cycle == 0 {
        bail!("base-facts, cycles, and facts-per-cycle must be positive");
    }

    if db_dir.exists() {
        fs::remove_dir_all(&db_dir)?;
    }
    fs::create_dir_all(&db_dir)?;
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let total_started = Instant::now();
    let run = match engine {
        Engine::Vicia => run_vicia(&db_dir, config)?,
        Engine::Cozo => run_cozo(&db_dir, config)?,
        Engine::Sqlite => run_sqlite(&db_dir, config)?,
        Engine::Redb => run_redb(&db_dir, config)?,
    };
    let expected_count = config
        .base_facts
        .checked_add(config.cycles.saturating_mul(config.facts_per_cycle))
        .context("expected fact count overflow")?;
    let expected_checksum = arithmetic_checksum(expected_count);
    let materialized_scan = measure_scan_in_child(engine, &db_dir, ScanWorkload::MaterializedScan)?;
    let engine_aggregate = if engine.supports_engine_aggregate() {
        Some(measure_scan_in_child(
            engine,
            &db_dir,
            ScanWorkload::EngineAggregate,
        )?)
    } else {
        None
    };
    let materialized_passed = materialized_scan.count == expected_count
        && materialized_scan.checksum == expected_checksum;
    let aggregate_passed = engine_aggregate.as_ref().is_none_or(|measurement| {
        measurement.count == expected_count && measurement.checksum == expected_checksum
    });
    let passed = materialized_passed && aggregate_passed;
    if !passed {
        bail!(
            "{} scan correctness failed: materialized count {}/{}, checksum {}/{}; aggregate {:?}/{:?}",
            engine.id(),
            materialized_scan.count,
            expected_count,
            materialized_scan.checksum,
            expected_checksum,
            engine_aggregate
                .as_ref()
                .map(|measurement| measurement.count),
            engine_aggregate
                .as_ref()
                .map(|measurement| measurement.checksum),
        );
    }

    let primary_file_bytes = fs::metadata(&run.primary_path)
        .with_context(|| {
            format!(
                "read primary database metadata: {}",
                run.primary_path.display()
            )
        })?
        .len();
    let total_storage_bytes = directory_bytes(&db_dir)?;
    let receipt = Receipt {
        schema: "vicia.cross-db-stress.v2",
        engine: engine.id(),
        engine_version: engine.version(),
        classification: engine.classification(),
        source_commit: git_output(&["rev-parse", "HEAD"]),
        source_dirty: git_output(&["status", "--porcelain"]).map(|value| !value.is_empty()),
        host: host_provenance(),
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        config,
        metrics: Metrics {
            build: stats(vec![run.build_ms]),
            reopen: stats(run.reopen_ms),
            durable_append: stats(run.append_ms),
            point_read: stats(run.read_ms),
            engine_aggregate: engine_aggregate.clone(),
            materialized_scan: materialized_scan.clone(),
            total_ms: round3(total_started.elapsed().as_secs_f64() * 1_000.0),
            stress_process_peak_rss_bytes: peak_rss_bytes(),
            primary_file_bytes,
            total_storage_bytes,
            bytes_per_fact: round3(total_storage_bytes as f64 / expected_count as f64),
        },
        correctness: Correctness {
            expected_count,
            expected_checksum,
            materialized_count: materialized_scan.count,
            materialized_checksum: materialized_scan.checksum,
            engine_aggregate_count: engine_aggregate
                .as_ref()
                .map(|measurement| measurement.count),
            engine_aggregate_checksum: engine_aggregate
                .as_ref()
                .map(|measurement| measurement.checksum),
            repeated_reopen_cycles: config.cycles,
            integrity_check: run.integrity,
            passed,
        },
        database_path: run.primary_path.display().to_string(),
    };
    fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt)?)?;
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

fn crash_write(
    engine: Engine,
    db_dir: &Path,
    start_entity: u64,
    cycles: u64,
    facts_per_cycle: u64,
) -> Result<()> {
    for cycle in 0..cycles {
        let start = start_entity + cycle * facts_per_cycle;
        append_engine(engine, db_dir, start, start + facts_per_cycle)?;
        println!(
            "{}",
            serde_json::json!({
                "committedCycles": cycle + 1,
                "minimumCount": start + facts_per_cycle,
            })
        );
        io::stdout().flush()?;
    }
    Ok(())
}

fn append_engine(engine: Engine, db_dir: &Path, start: u64, end: u64) -> Result<()> {
    match engine {
        Engine::Vicia => {
            let db = OpenOptions::new().path(db_dir.join("vicia.graph")).open()?;
            vicia_insert(&db, start, end)
        }
        Engine::Cozo => {
            let db = cozo_open(&db_dir.join("cozo.db"))?;
            cozo_insert(&db, start, end)
        }
        Engine::Sqlite => {
            let db = sqlite_open(&db_dir.join("sqlite.db"))?;
            sqlite_insert(&db, start, end)
        }
        Engine::Redb => {
            let db = Database::open(db_dir.join("redb.db"))?;
            redb_insert(&db, start, end)
        }
    }
}

fn verify_after_crash(engine: Engine, db_dir: &Path, minimum_count: u64) -> Result<CrashReceipt> {
    let (count, checksum, integrity) = match engine {
        Engine::Vicia => {
            let db = OpenOptions::new().path(db_dir.join("vicia.graph")).open()?;
            let (count, checksum) = vicia_scan(&db)?;
            (count, checksum, "wal-replay-open-and-full-scan".to_string())
        }
        Engine::Cozo => {
            let db = cozo_open(&db_dir.join("cozo.db"))?;
            let rows = db
                .run_script(
                    "?[value] := *facts{value}",
                    BTreeMap::new(),
                    ScriptMutability::Immutable,
                )
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let mut checksum = 0i128;
            for row in &rows.rows {
                checksum += i128::from(
                    row.first()
                        .and_then(cozo_integer)
                        .context("Cozo crash scan returned no integer")?,
                );
            }
            (
                u64::try_from(rows.rows.len())?,
                checksum,
                "transactional-open-and-full-scan".to_string(),
            )
        }
        Engine::Sqlite => {
            let db = sqlite_open(&db_dir.join("sqlite.db"))?;
            let mut aggregate =
                db.prepare("SELECT COUNT(*), COALESCE(SUM(value), 0) FROM facts")?;
            if aggregate.next()? != State::Row {
                bail!("SQLite crash aggregate returned no row");
            }
            let count = aggregate.read::<i64, _>(0)? as u64;
            let checksum = i128::from(aggregate.read::<i64, _>(1)?);
            let mut check = db.prepare("PRAGMA integrity_check")?;
            if check.next()? != State::Row {
                bail!("SQLite crash integrity_check returned no row");
            }
            (count, checksum, check.read::<String, _>(0)?)
        }
        Engine::Redb => {
            let db = Database::open(db_dir.join("redb.db"))?;
            let transaction = db.begin_read()?;
            let table = transaction.open_table(REDB_FACTS)?;
            let mut count = 0u64;
            let mut checksum = 0i128;
            for entry in table.iter()? {
                let (_, value) = entry?;
                count += 1;
                checksum += i128::from(value.value());
            }
            (
                count,
                checksum,
                "transactional-open-and-full-scan".to_string(),
            )
        }
    };
    let expected_checksum = arithmetic_checksum(count);
    let integrity_ok = engine.id() != "sqlite" || integrity == "ok";
    let passed = count >= minimum_count && checksum == expected_checksum && integrity_ok;
    Ok(CrashReceipt {
        schema: "vicia.cross-db-crash.v1",
        engine: engine.id(),
        minimum_committed_count: minimum_count,
        recovered_count: count,
        recovered_checksum: checksum,
        expected_checksum,
        integrity_check: integrity,
        passed,
    })
}

fn run_vicia(dir: &Path, config: Config) -> Result<RunMeasurements> {
    let path = dir.join("vicia.graph");
    let started = Instant::now();
    let db = OpenOptions::new().path(&path).open()?;
    for start in (0..config.base_facts).step_by(INSERT_BATCH as usize) {
        vicia_insert(&db, start, (start + INSERT_BATCH).min(config.base_facts))?;
    }
    db.checkpoint()?;
    drop(db);
    let build_ms = elapsed_ms(started);

    let mut reopen_ms = Vec::new();
    let mut append_ms = Vec::new();
    let mut read_ms = Vec::new();
    for cycle in 0..config.cycles {
        let opened = Instant::now();
        let db = OpenOptions::new().path(&path).open()?;
        reopen_ms.push(elapsed_ms(opened));
        let visible = config.base_facts + cycle * config.facts_per_cycle;
        for probe in 0..config.reads_per_cycle {
            let entity = probe_entity(cycle, probe, visible);
            let read = Instant::now();
            let value = vicia_point(&db, entity)?;
            read_ms.push(elapsed_ms(read));
            if value != entity as i64 {
                bail!("Vicia point read mismatch for entity {entity}");
            }
        }
        let appended = Instant::now();
        vicia_insert(&db, visible, visible + config.facts_per_cycle)?;
        append_ms.push(elapsed_ms(appended));
        drop(db);
    }
    Ok(RunMeasurements {
        build_ms,
        reopen_ms,
        append_ms,
        read_ms,
        integrity: "repeated-open-and-durable-append".to_string(),
        primary_path: path,
    })
}

fn vicia_insert(db: &minigraf::Minigraf, start: u64, end: u64) -> Result<()> {
    let mut command = String::from("(transact [");
    for entity in start..end {
        command.push_str(&format!("[:cmp/e{entity} :cmp/value {entity}]"));
    }
    command.push_str("])");
    db.execute(&command)?;
    Ok(())
}

fn vicia_point(db: &minigraf::Minigraf, entity: u64) -> Result<i64> {
    let result = db.execute(&format!(
        "(query [:find ?v :where [:cmp/e{entity} :cmp/value ?v]])"
    ))?;
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("Vicia point query returned a non-query result");
    };
    results
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_integer())
        .context("Vicia point query returned no integer")
}

fn vicia_scan(db: &minigraf::Minigraf) -> Result<(u64, i128)> {
    let result = db.execute("(query [:find ?v :where [?e :cmp/value ?v]])")?;
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("Vicia full scan returned a non-query result");
    };
    let values = results
        .into_iter()
        .map(|row| {
            row.into_iter()
                .next()
                .and_then(|value| value.as_integer())
                .context("Vicia scan returned no integer")
        })
        .collect::<Result<Vec<_>>>()?;
    fold_materialized_values(values)
}

fn vicia_aggregate(db: &minigraf::Minigraf) -> Result<(u64, i128)> {
    let result = db.execute("(query [:find (count ?v) (sum ?v) :where [?e :cmp/value ?v]])")?;
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("Vicia aggregate returned a non-query result");
    };
    let row = results.first().context("Vicia aggregate returned no row")?;
    let count = row
        .first()
        .and_then(|value| value.as_integer())
        .context("Vicia aggregate returned no count")?;
    let checksum = row
        .get(1)
        .and_then(|value| value.as_integer())
        .context("Vicia aggregate returned no checksum")?;
    Ok((u64::try_from(count)?, i128::from(checksum)))
}

fn run_sqlite(dir: &Path, config: Config) -> Result<RunMeasurements> {
    let path = dir.join("sqlite.db");
    let started = Instant::now();
    let connection = sqlite_open(&path)?;
    connection
        .execute("CREATE TABLE facts(entity INTEGER PRIMARY KEY, value INTEGER NOT NULL);")?;
    for start in (0..config.base_facts).step_by(INSERT_BATCH as usize) {
        sqlite_insert(
            &connection,
            start,
            (start + INSERT_BATCH).min(config.base_facts),
        )?;
    }
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(connection);
    let build_ms = elapsed_ms(started);

    let mut reopen_ms = Vec::new();
    let mut append_ms = Vec::new();
    let mut read_ms = Vec::new();
    for cycle in 0..config.cycles {
        let opened = Instant::now();
        let connection = sqlite_open(&path)?;
        reopen_ms.push(elapsed_ms(opened));
        let visible = config.base_facts + cycle * config.facts_per_cycle;
        for probe in 0..config.reads_per_cycle {
            let entity = probe_entity(cycle, probe, visible);
            let read = Instant::now();
            let mut statement = connection.prepare("SELECT value FROM facts WHERE entity = ?1")?;
            statement.bind((1, entity as i64))?;
            if statement.next()? != State::Row {
                bail!("SQLite point key missing");
            }
            let value = statement.read::<i64, _>(0)?;
            read_ms.push(elapsed_ms(read));
            if value != entity as i64 {
                bail!("SQLite point read mismatch for entity {entity}");
            }
        }
        let appended = Instant::now();
        sqlite_insert(&connection, visible, visible + config.facts_per_cycle)?;
        append_ms.push(elapsed_ms(appended));
    }
    let connection = sqlite_open(&path)?;
    let mut integrity_statement = connection.prepare("PRAGMA integrity_check")?;
    if integrity_statement.next()? != State::Row {
        bail!("SQLite integrity_check returned no row");
    }
    let integrity = integrity_statement.read::<String, _>(0)?;
    Ok(RunMeasurements {
        build_ms,
        reopen_ms,
        append_ms,
        read_ms,
        integrity,
        primary_path: path,
    })
}

fn sqlite_open(path: &Path) -> Result<Connection> {
    let connection = sqlite::open(path)?;
    connection
        .execute("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY;")?;
    Ok(connection)
}

fn sqlite_insert(connection: &Connection, start: u64, end: u64) -> Result<()> {
    connection.execute("BEGIN IMMEDIATE")?;
    let inserted = (|| -> Result<()> {
        let mut statement =
            connection.prepare("INSERT INTO facts(entity, value) VALUES(?1, ?2)")?;
        for entity in start..end {
            statement.bind((1, entity as i64))?;
            statement.bind((2, entity as i64))?;
            if statement.next()? != State::Done {
                bail!("SQLite insert did not complete");
            }
            statement.reset()?;
        }
        Ok(())
    })();
    if let Err(error) = inserted {
        let _ = connection.execute("ROLLBACK");
        return Err(error);
    }
    connection.execute("COMMIT")?;
    Ok(())
}

fn sqlite_aggregate(connection: &Connection) -> Result<(u64, i128)> {
    let mut statement =
        connection.prepare("SELECT COUNT(*), COALESCE(SUM(value), 0) FROM facts")?;
    if statement.next()? != State::Row {
        bail!("SQLite aggregate returned no row");
    }
    Ok((
        u64::try_from(statement.read::<i64, _>(0)?)?,
        i128::from(statement.read::<i64, _>(1)?),
    ))
}

fn sqlite_scan(connection: &Connection) -> Result<(u64, i128)> {
    let mut statement = connection.prepare("SELECT value FROM facts")?;
    let mut values = Vec::new();
    while statement.next()? == State::Row {
        values.push(statement.read::<i64, _>(0)?);
    }
    fold_materialized_values(values)
}

fn run_redb(dir: &Path, config: Config) -> Result<RunMeasurements> {
    let path = dir.join("redb.db");
    let started = Instant::now();
    let database = Database::create(&path)?;
    for start in (0..config.base_facts).step_by(INSERT_BATCH as usize) {
        redb_insert(
            &database,
            start,
            (start + INSERT_BATCH).min(config.base_facts),
        )?;
    }
    drop(database);
    let build_ms = elapsed_ms(started);

    let mut reopen_ms = Vec::new();
    let mut append_ms = Vec::new();
    let mut read_ms = Vec::new();
    for cycle in 0..config.cycles {
        let opened = Instant::now();
        let database = Database::open(&path)?;
        reopen_ms.push(elapsed_ms(opened));
        let visible = config.base_facts + cycle * config.facts_per_cycle;
        for probe in 0..config.reads_per_cycle {
            let entity = probe_entity(cycle, probe, visible);
            let read = Instant::now();
            let transaction = database.begin_read()?;
            let table = transaction.open_table(REDB_FACTS)?;
            let value = table
                .get(entity)?
                .context("redb point key missing")?
                .value();
            read_ms.push(elapsed_ms(read));
            if value != entity as i64 {
                bail!("redb point read mismatch for entity {entity}");
            }
        }
        let appended = Instant::now();
        redb_insert(&database, visible, visible + config.facts_per_cycle)?;
        append_ms.push(elapsed_ms(appended));
    }
    Ok(RunMeasurements {
        build_ms,
        reopen_ms,
        append_ms,
        read_ms,
        integrity: "transactional-open-and-durable-append".to_string(),
        primary_path: path,
    })
}

fn redb_insert(database: &Database, start: u64, end: u64) -> Result<()> {
    let transaction = database.begin_write()?;
    {
        let mut table = transaction.open_table(REDB_FACTS)?;
        for entity in start..end {
            table.insert(entity, entity as i64)?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn redb_scan(database: &Database) -> Result<(u64, i128)> {
    let transaction = database.begin_read()?;
    let table = transaction.open_table(REDB_FACTS)?;
    let mut values = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        values.push(value.value());
    }
    fold_materialized_values(values)
}

fn run_cozo(dir: &Path, config: Config) -> Result<RunMeasurements> {
    let path = dir.join("cozo.db");
    let started = Instant::now();
    let database = cozo_open(&path)?;
    database
        .run_script(
            ":create facts {entity: Int => value: Int}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    for start in (0..config.base_facts).step_by(INSERT_BATCH as usize) {
        cozo_insert(
            &database,
            start,
            (start + INSERT_BATCH).min(config.base_facts),
        )?;
    }
    drop(database);
    let build_ms = elapsed_ms(started);

    let mut reopen_ms = Vec::new();
    let mut append_ms = Vec::new();
    let mut read_ms = Vec::new();
    for cycle in 0..config.cycles {
        let opened = Instant::now();
        let database = cozo_open(&path)?;
        reopen_ms.push(elapsed_ms(opened));
        let visible = config.base_facts + cycle * config.facts_per_cycle;
        for probe in 0..config.reads_per_cycle {
            let entity = probe_entity(cycle, probe, visible);
            let read = Instant::now();
            let rows = database
                .run_script(
                    "?[value] := *facts{entity: $entity, value}",
                    BTreeMap::from([("entity".to_string(), DataValue::from(entity as i64))]),
                    ScriptMutability::Immutable,
                )
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            read_ms.push(elapsed_ms(read));
            let value = rows
                .rows
                .first()
                .and_then(|row| row.first())
                .and_then(cozo_integer)
                .context("Cozo point query returned no integer")?;
            if value != entity as i64 {
                bail!("Cozo point read mismatch for entity {entity}");
            }
        }
        let appended = Instant::now();
        cozo_insert(&database, visible, visible + config.facts_per_cycle)?;
        append_ms.push(elapsed_ms(appended));
    }
    Ok(RunMeasurements {
        build_ms,
        reopen_ms,
        append_ms,
        read_ms,
        integrity: "transactional-open-and-durable-append".to_string(),
        primary_path: path,
    })
}

fn cozo_open(path: &Path) -> Result<DbInstance> {
    DbInstance::new("sqlite", path, "").map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn cozo_insert(database: &DbInstance, start: u64, end: u64) -> Result<()> {
    let mut script = String::from("?[entity, value] <- [");
    for entity in start..end {
        if entity != start {
            script.push(',');
        }
        script.push_str(&format!("[{entity},{entity}]"));
    }
    script.push_str("] :put facts {entity => value}");
    database
        .run_script(&script, BTreeMap::new(), ScriptMutability::Mutable)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

fn cozo_integer(value: &DataValue) -> Option<i64> {
    match value {
        DataValue::Num(Num::Int(value)) => Some(*value),
        _ => None,
    }
}

fn cozo_exact_integer(value: &DataValue) -> Option<i64> {
    match value {
        DataValue::Num(Num::Int(value)) => Some(*value),
        DataValue::Num(Num::Float(value))
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value <= i64::MAX as f64 =>
        {
            #[allow(clippy::cast_possible_truncation)]
            Some(*value as i64)
        }
        _ => None,
    }
}

fn cozo_aggregate(database: &DbInstance) -> Result<(u64, i128)> {
    let rows = database
        .run_script(
            "?[count(value), sum(value)] := *facts{value}",
            BTreeMap::new(),
            ScriptMutability::Immutable,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let row = rows
        .rows
        .first()
        .context("Cozo aggregate returned no row")?;
    let count = row
        .first()
        .and_then(cozo_integer)
        .context("Cozo aggregate returned no count")?;
    let checksum = row
        .get(1)
        .and_then(cozo_exact_integer)
        .context("Cozo aggregate returned no checksum")?;
    Ok((u64::try_from(count)?, i128::from(checksum)))
}

fn cozo_scan(database: &DbInstance) -> Result<(u64, i128)> {
    let rows = database
        .run_script(
            "?[value] := *facts{value}",
            BTreeMap::new(),
            ScriptMutability::Immutable,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let values = rows
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .next()
                .as_ref()
                .and_then(cozo_integer)
                .context("Cozo scan returned no integer")
        })
        .collect::<Result<Vec<_>>>()?;
    fold_materialized_values(values)
}

fn fold_materialized_values(values: Vec<i64>) -> Result<(u64, i128)> {
    let count = u64::try_from(values.len())?;
    let checksum = values.into_iter().map(i128::from).sum();
    Ok((count, checksum))
}

fn probe_entity(cycle: u64, probe: u64, visible: u64) -> u64 {
    cycle
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(probe.wrapping_mul(0x85EB_CA6B))
        % visible
}

fn arithmetic_checksum(count: u64) -> i128 {
    i128::from(count) * i128::from(count.saturating_sub(1)) / 2
}

fn elapsed_ms(started: Instant) -> f64 {
    round3(started.elapsed().as_secs_f64() * 1_000.0)
}

fn stats(mut samples: Vec<f64>) -> SampleStats {
    samples.sort_by(f64::total_cmp);
    let count = samples.len();
    let sum: f64 = samples.iter().sum();
    SampleStats {
        unit: "ms",
        count,
        min: samples[0],
        p50: percentile(&samples, 50),
        p95: percentile(&samples, 95),
        p99: percentile(&samples, 99),
        max: samples[count - 1],
        mean: round3(sum / count as f64),
        samples,
    }
}

fn percentile(samples: &[f64], percentile: usize) -> f64 {
    let rank = (samples.len() * percentile).div_ceil(100);
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_bytes(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

fn git_output(args: &[&str]) -> Option<String> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn host_provenance() -> HostProvenance {
    HostProvenance {
        testbed: std::env::var("VICIA_BENCH_TESTBED")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| command_output("hostname", &[]))
            .unwrap_or_else(|| "unknown".to_string()),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        cpu_model: fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|cpuinfo| {
                cpuinfo.lines().find_map(|line| {
                    line.strip_prefix("model name\t:")
                        .map(|value| value.trim().to_string())
                })
            }),
        memory_bytes: fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|meminfo| {
                let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
                line.split_whitespace()
                    .nth(1)?
                    .parse::<u64>()
                    .ok()?
                    .checked_mul(1024)
            }),
    }
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
