use anyhow::{Context, Result, bail};
use cozo::{DataValue, DbInstance, Num, ScriptMutability};
use fjall::{Database as FjallDatabase, KeyspaceCreateOptions};
use grafeo::{GrafeoDB, Value as GrafeoValue};
use minigraf::{OpenOptions, QueryResult};
use redb::{Database as RedbDatabase, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use sqlite::State as SqliteState;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use turso::Builder as TursoBuilder;

const REDB_FACTS: TableDefinition<u64, i64> = TableDefinition::new("facts");
const BATCH: u64 = 1_000;

#[derive(Clone, Copy)]
enum Engine {
    Vicia,
    Grafeo,
    Redb,
    Fjall,
    Turso,
    Cozo,
    Sqlite,
}

impl Engine {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "vicia" => Ok(Self::Vicia),
            "grafeo" => Ok(Self::Grafeo),
            "redb" => Ok(Self::Redb),
            "fjall" => Ok(Self::Fjall),
            "turso" => Ok(Self::Turso),
            "cozo" => Ok(Self::Cozo),
            "sqlite" => Ok(Self::Sqlite),
            _ => bail!("unknown engine: {value}"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Vicia => "vicia",
            Self::Grafeo => "grafeo",
            Self::Redb => "redb",
            Self::Fjall => "fjall",
            Self::Turso => "turso",
            Self::Cozo => "cozo",
            Self::Sqlite => "sqlite",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Self::Vicia => "bi-temporal Datalog product",
            Self::Grafeo => "embedded graph query peer",
            Self::Redb => "B-tree KV storage floor",
            Self::Fjall => "LSM KV storage floor",
            Self::Turso => "embedded SQL peer",
            Self::Cozo => "embedded Datalog peer",
            Self::Sqlite => "embedded SQL reference",
        }
    }

    fn boundary(self) -> &'static str {
        match self {
            Self::Vicia => "engineAggregate",
            Self::Grafeo => "engineAggregate",
            Self::Turso => "engineAggregate",
            Self::Cozo => "engineAggregate",
            Self::Sqlite => "engineAggregate",
            Self::Redb => "ownedResultScan",
            Self::Fjall => "ownedResultScan",
        }
    }

    fn adapter_schema(self) -> &'static str {
        match self {
            Self::Vicia => {
                "EAV ledger fact: entity + attribute + integer value + bi-temporal identity"
            }
            Self::Grafeo => "Fact node: integer entity property + integer value property",
            Self::Redb | Self::Fjall => "integer key -> integer value",
            Self::Turso | Self::Sqlite => {
                "facts(entity INTEGER PRIMARY KEY, value INTEGER NOT NULL)"
            }
            Self::Cozo => "facts {entity: Int => value: Int}",
        }
    }

    fn semantic_scope(self) -> &'static str {
        match self {
            Self::Vicia => "native bi-temporal current projection",
            Self::Redb | Self::Fjall => "owned KV scan storage floor",
            _ => "common current entity/value projection",
        }
    }

    fn durability(self) -> BTreeMap<String, String> {
        let pairs: &[(&str, &str)] = match self {
            Self::Vicia => &[
                ("batch", "1000 facts per ledger transaction"),
                ("barrier", "checkpoint then close"),
            ],
            Self::Redb => &[
                ("batch", "1000 entries per write transaction"),
                ("barrier", "transaction commit then close"),
            ],
            Self::Fjall => &[
                ("batch", "1000 entries per database batch"),
                ("barrier", "batch commit then close"),
            ],
            Self::Grafeo => &[
                ("batch", "engine-native node writes"),
                ("barrier", "database close"),
            ],
            Self::Turso => &[
                ("batch", "1000 rows per SQL transaction"),
                ("barrier", "COMMIT then close"),
            ],
            Self::Cozo => &[
                ("batch", "1000 rows per :put"),
                ("barrier", "mutable script completion then close"),
            ],
            Self::Sqlite => &[
                ("batch", "1000 rows per SQL transaction"),
                ("barrier", "COMMIT then close"),
                ("journalMode", "delete"),
                ("synchronous", "full"),
            ],
        };
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn runtime_version(self) -> Option<String> {
        match self {
            Self::Sqlite => Some(format!("sqlite {} (crate 0.36.0)", sqlite::version())),
            _ => None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema: &'static str,
    engine: &'static str,
    role: &'static str,
    execution_boundary: &'static str,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
    order_position: usize,
    adapter_schema: &'static str,
    semantic_scope: &'static str,
    durability: BTreeMap<String, String>,
    runtime_version: Option<String>,
    build: BuildMeasurement,
    query: QueryMeasurement,
    reopen_verified: bool,
    storage_bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildMeasurement {
    elapsed_ms: f64,
    baseline_rss_bytes: u64,
    peak_rss_bytes: u64,
    process_delta: ProcessCounters,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryMeasurement {
    open_ms: f64,
    first_read_ms: f64,
    point_hot: PointMeasurement,
    point_distributed: PointMeasurement,
    point_miss: PointMeasurement,
    aggregate_samples_ms: Vec<f64>,
    count: u64,
    checksum: i128,
    open_baseline_rss_bytes: u64,
    workload_peak_rss_bytes: u64,
    workload_delta_rss_bytes: u64,
    retained_rss_bytes: u64,
    baseline_breakdown: MemoryBreakdown,
    retained_breakdown: MemoryBreakdown,
    retained_delta_breakdown: MemoryBreakdown,
    process_delta: ProcessCounters,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PointMeasurement {
    operations_per_sample: usize,
    samples_ms_per_operation: Vec<f64>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessCounters {
    read_bytes: u64,
    write_bytes: u64,
    minor_faults: u64,
    major_faults: u64,
}

impl ProcessCounters {
    fn saturating_sub(self, baseline: Self) -> Self {
        Self {
            read_bytes: self.read_bytes.saturating_sub(baseline.read_bytes),
            write_bytes: self.write_bytes.saturating_sub(baseline.write_bytes),
            minor_faults: self.minor_faults.saturating_sub(baseline.minor_faults),
            major_faults: self.major_faults.saturating_sub(baseline.major_faults),
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryBreakdown {
    anonymous_rss_bytes: u64,
    file_backed_rss_bytes: u64,
    heap_mapping_rss_bytes: u64,
    database_mapped_rss_bytes: u64,
}

impl MemoryBreakdown {
    fn saturating_sub(self, baseline: Self) -> Self {
        Self {
            anonymous_rss_bytes: self
                .anonymous_rss_bytes
                .saturating_sub(baseline.anonymous_rss_bytes),
            file_backed_rss_bytes: self
                .file_backed_rss_bytes
                .saturating_sub(baseline.file_backed_rss_bytes),
            heap_mapping_rss_bytes: self
                .heap_mapping_rss_bytes
                .saturating_sub(baseline.heap_mapping_rss_bytes),
            database_mapped_rss_bytes: self
                .database_mapped_rss_bytes
                .saturating_sub(baseline.database_mapped_rss_bytes),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 7 && args[1] == "measure" {
        let measurement = measure_fresh(
            Engine::parse(&args[2])?,
            Path::new(&args[3]),
            args[4].parse()?,
            args[5].parse()?,
            args[6].parse()?,
        )
        .await?;
        println!("{}", serde_json::to_string(&measurement)?);
        return Ok(());
    }
    if args.len() != 8 || args[1] != "run" {
        bail!(
            "usage: vicia-ref-db-bench run <engine> <data-dir> <facts> <repetitions> <trial> <seed>"
        );
    }
    let engine = Engine::parse(&args[2])?;
    let dir = PathBuf::from(&args[3]);
    let facts = args[4].parse::<u64>()?;
    let repetitions = args[5].parse::<usize>()?;
    let trial = args[6].parse::<usize>()?;
    let seed = args[7].parse::<u64>()?;
    if facts == 0 || repetitions == 0 {
        bail!("facts and repetitions must be positive");
    }
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;

    let receipt = match engine {
        Engine::Vicia => run_vicia(engine, &dir, facts, repetitions, trial, seed)?,
        Engine::Grafeo => run_grafeo(engine, &dir, facts, repetitions, trial, seed)?,
        Engine::Redb => run_redb(engine, &dir, facts, repetitions, trial, seed)?,
        Engine::Fjall => run_fjall(engine, &dir, facts, repetitions, trial, seed)?,
        Engine::Turso => run_turso(engine, &dir, facts, repetitions, trial, seed).await?,
        Engine::Cozo => run_cozo(engine, &dir, facts, repetitions, trial, seed)?,
        Engine::Sqlite => run_sqlite(engine, &dir, facts, repetitions, trial, seed)?,
    };
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

fn receipt(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
    build: BuildMeasurement,
) -> Result<Receipt> {
    let expected = i128::from(facts) * i128::from(facts - 1) / 2;
    let query = measure_in_child(engine, dir, facts, repetitions, seed)?;
    if query.count != facts || query.checksum != expected {
        bail!("{} fresh reopen correctness mismatch", engine.id());
    }
    Ok(Receipt {
        schema: "vicia.ref-db-bench.v5",
        engine: engine.id(),
        role: engine.role(),
        execution_boundary: engine.boundary(),
        facts,
        repetitions,
        trial,
        seed,
        order_position: std::env::var("REF_DB_BENCH_ORDER_POSITION")
            .context("missing REF_DB_BENCH_ORDER_POSITION")?
            .parse()?,
        adapter_schema: engine.adapter_schema(),
        semantic_scope: engine.semantic_scope(),
        durability: engine.durability(),
        runtime_version: engine.runtime_version(),
        build,
        query,
        reopen_verified: true,
        storage_bytes: directory_bytes(dir)?,
    })
}

fn measure_in_child(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    seed: u64,
) -> Result<QueryMeasurement> {
    let output = Command::new(std::env::current_exe()?)
        .arg("measure")
        .arg(engine.id())
        .arg(dir)
        .arg(facts.to_string())
        .arg(repetitions.to_string())
        .arg(seed.to_string())
        .output()
        .with_context(|| format!("launch fresh {} memory measurement", engine.id()))?;
    if !output.status.success() {
        bail!(
            "fresh {} memory measurement failed: {}",
            engine.id(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode fresh query measurement")
}

fn run_vicia(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let path = dir.join("vicia.graph");
    let build_start = start_build();
    let db = OpenOptions::new().path(&path).open()?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut command = String::from("(transact [");
        for entity in start..(start + BATCH).min(facts) {
            command.push_str(&format!("[:cmp/e{entity} :cmp/value {entity}]"));
        }
        command.push_str("])");
        db.execute(&command)?;
    }
    db.checkpoint()?;
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

fn vicia_point(result: QueryResult) -> Result<Option<i64>> {
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("Vicia point query returned a non-query result");
    };
    Ok(results
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_integer()))
}

fn vicia_aggregate(db: &minigraf::Minigraf) -> Result<(u64, i128)> {
    let result = db.execute("(query [:find (count ?v) (sum ?v) :where [?e :cmp/value ?v]])")?;
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("Vicia aggregate returned a non-query result");
    };
    let row = results.first().context("Vicia aggregate returned no row")?;
    let count = row
        .first()
        .and_then(|v| v.as_integer())
        .context("missing count")?;
    let sum = row
        .get(1)
        .and_then(|v| v.as_integer())
        .context("missing sum")?;
    Ok((u64::try_from(count)?, i128::from(sum)))
}

fn run_redb(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let build_start = start_build();
    let db = RedbDatabase::create(dir.join("redb.db"))?;
    for start in (0..facts).step_by(BATCH as usize) {
        let tx = db.begin_write()?;
        {
            let mut table = tx.open_table(REDB_FACTS)?;
            for key in start..(start + BATCH).min(facts) {
                table.insert(key, i64::try_from(key)?)?;
            }
        }
        tx.commit()?;
    }
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

fn key_bytes(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}

fn run_fjall(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let build_start = start_build();
    let db = FjallDatabase::builder(dir.join("fjall")).open()?;
    let items = db.keyspace("facts", KeyspaceCreateOptions::default)?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut batch = db.batch();
        for key in start..(start + BATCH).min(facts) {
            batch.insert(&items, key_bytes(key), key_bytes(key));
        }
        batch.commit()?;
    }
    drop(items);
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

fn run_grafeo(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let build_start = start_build();
    let db = GrafeoDB::open(dir.join("grafeo"))?;
    for entity in 0..facts {
        let node = db.create_node(&["Fact"]);
        db.set_node_property(node, "entity", GrafeoValue::from(i64::try_from(entity)?));
        db.set_node_property(node, "value", GrafeoValue::from(i64::try_from(entity)?));
    }
    db.close()?;
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

fn cozo_open(path: &Path) -> Result<DbInstance> {
    DbInstance::new("sqlite", path.to_str().context("non-UTF8 Cozo path")?, "")
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn run_cozo(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let build_start = start_build();
    let db = cozo_open(&dir.join("cozo.db"))?;
    db.run_script(
        ":create facts {entity: Int => value: Int}",
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut script = String::from("?[entity, value] <- [");
        for entity in start..(start + BATCH).min(facts) {
            if entity != start {
                script.push(',');
            }
            script.push_str(&format!("[{entity},{entity}]"));
        }
        script.push_str("] :put facts {entity => value}");
        db.run_script(&script, BTreeMap::new(), ScriptMutability::Mutable)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

fn data_i64(value: &DataValue) -> Option<i64> {
    match value {
        DataValue::Num(Num::Int(v)) => Some(*v),
        DataValue::Num(Num::Float(v))
            if v.is_finite()
                && v.fract() == 0.0
                && *v >= i64::MIN as f64
                && *v <= i64::MAX as f64 =>
        {
            Some(*v as i64)
        }
        _ => None,
    }
}

async fn run_turso(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let path = dir.join("turso.db");
    let build_start = start_build();
    let db = TursoBuilder::new_local(&path.to_string_lossy())
        .build()
        .await?;
    let conn = db.connect()?;
    conn.execute(
        "CREATE TABLE facts(entity INTEGER PRIMARY KEY, value INTEGER NOT NULL)",
        (),
    )
    .await?;
    for start in (0..facts).step_by(BATCH as usize) {
        conn.execute("BEGIN", ()).await?;
        for value in start..(start + BATCH).min(facts) {
            conn.execute(
                "INSERT INTO facts(entity, value) VALUES (?1, ?2)",
                (i64::try_from(value)?, i64::try_from(value)?),
            )
            .await?;
        }
        conn.execute("COMMIT", ()).await?;
    }
    drop(conn);
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

fn run_sqlite(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    trial: usize,
    seed: u64,
) -> Result<Receipt> {
    let build_start = start_build();
    let connection = sqlite::open(dir.join("sqlite.db"))?;
    connection.execute(
        "PRAGMA journal_mode=DELETE;
         PRAGMA synchronous=FULL;
         CREATE TABLE facts(entity INTEGER PRIMARY KEY, value INTEGER NOT NULL);",
    )?;
    for start in (0..facts).step_by(BATCH as usize) {
        connection.execute("BEGIN IMMEDIATE")?;
        {
            let mut statement =
                connection.prepare("INSERT INTO facts(entity, value) VALUES (?1, ?2)")?;
            for value in start..(start + BATCH).min(facts) {
                let value = i64::try_from(value)?;
                statement.bind((1, value))?;
                statement.bind((2, value))?;
                while statement.next()? != SqliteState::Done {}
                statement.reset()?;
            }
        }
        connection.execute("COMMIT")?;
    }
    drop(connection);
    receipt(
        engine,
        dir,
        facts,
        repetitions,
        trial,
        seed,
        finish_build(build_start),
    )
}

async fn measure_fresh(
    engine: Engine,
    dir: &Path,
    facts: u64,
    repetitions: usize,
    seed: u64,
) -> Result<QueryMeasurement> {
    match engine {
        Engine::Vicia => {
            let started = Instant::now();
            let db = OpenOptions::new().path(dir.join("vicia.graph")).open()?;
            let open_ms = elapsed(started);
            measure_loaded(
                dir,
                open_ms,
                facts,
                repetitions,
                seed,
                |key| {
                    vicia_point(db.execute(&format!(
                        "(query [:find ?v :where [:cmp/e{key} :cmp/value ?v]])"
                    ))?)
                },
                || vicia_aggregate(&db),
            )
        }
        Engine::Grafeo => {
            let started = Instant::now();
            let db = GrafeoDB::open(dir.join("grafeo"))?;
            let session = db.session();
            let open_ms = elapsed(started);
            measure_loaded(
                dir,
                open_ms,
                facts,
                repetitions,
                seed,
                |key| {
                    let result = session.execute(&format!(
                        "MATCH (n:Fact) WHERE n.entity = {key} RETURN n.value"
                    ))?;
                    Ok(result
                        .rows()
                        .first()
                        .and_then(|row| row.first())
                        .and_then(|value| value.as_int64()))
                },
                || {
                    let result = session.execute("MATCH (n:Fact) RETURN COUNT(n), SUM(n.value)")?;
                    let row = result
                        .rows()
                        .first()
                        .context("Grafeo aggregate returned no row")?;
                    let count = row
                        .first()
                        .and_then(|v| v.as_int64())
                        .context("missing Grafeo count")?;
                    let sum = row
                        .get(1)
                        .and_then(|v| v.as_int64())
                        .context("missing Grafeo sum")?;
                    Ok((u64::try_from(count)?, i128::from(sum)))
                },
            )
        }
        Engine::Redb => {
            let started = Instant::now();
            let db = RedbDatabase::open(dir.join("redb.db"))?;
            let open_ms = elapsed(started);
            measure_loaded(
                dir,
                open_ms,
                facts,
                repetitions,
                seed,
                |key| {
                    let tx = db.begin_read()?;
                    let table = tx.open_table(REDB_FACTS)?;
                    Ok(table.get(key)?.map(|value| value.value()))
                },
                || {
                    let tx = db.begin_read()?;
                    let table = tx.open_table(REDB_FACTS)?;
                    let mut count = 0_u64;
                    let mut checksum = 0_i128;
                    for entry in table.iter()? {
                        let (_, value) = entry?;
                        count += 1;
                        checksum += i128::from(value.value());
                    }
                    Ok((count, checksum))
                },
            )
        }
        Engine::Fjall => {
            let started = Instant::now();
            let db = FjallDatabase::builder(dir.join("fjall")).open()?;
            let items = db.keyspace("facts", KeyspaceCreateOptions::default)?;
            let open_ms = elapsed(started);
            measure_loaded(
                dir,
                open_ms,
                facts,
                repetitions,
                seed,
                |key| {
                    items
                        .get(key_bytes(key))?
                        .map(|value| {
                            let bytes: [u8; 8] = value.as_ref().try_into()?;
                            Ok(i64::try_from(u64::from_be_bytes(bytes))?)
                        })
                        .transpose()
                },
                || {
                    let mut count = 0_u64;
                    let mut checksum = 0_i128;
                    for entry in items.iter() {
                        let value = entry.value()?;
                        let bytes: [u8; 8] = value.as_ref().try_into()?;
                        count += 1;
                        checksum += i128::from(u64::from_be_bytes(bytes));
                    }
                    Ok((count, checksum))
                },
            )
        }
        Engine::Turso => {
            let started = Instant::now();
            let path = dir.join("turso.db");
            let db = TursoBuilder::new_local(&path.to_string_lossy())
                .build()
                .await?;
            let conn = db.connect()?;
            let open_ms = elapsed(started);
            measure_turso_loaded(dir, open_ms, &conn, facts, repetitions, seed).await
        }
        Engine::Cozo => {
            let started = Instant::now();
            let db = cozo_open(&dir.join("cozo.db"))?;
            let open_ms = elapsed(started);
            measure_loaded(
                dir,
                open_ms,
                facts,
                repetitions,
                seed,
                |key| {
                    let point = db
                        .run_script(
                            "?[value] := *facts{entity: $id, value}",
                            BTreeMap::from([(
                                "id".to_string(),
                                DataValue::Num(Num::Int(i64::try_from(key)?)),
                            )]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    Ok(point
                        .rows
                        .first()
                        .and_then(|row| row.first())
                        .and_then(data_i64))
                },
                || cozo_aggregate(&db),
            )
        }
        Engine::Sqlite => {
            let started = Instant::now();
            let db = sqlite::open(dir.join("sqlite.db"))?;
            db.execute("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
            let open_ms = elapsed(started);
            measure_loaded(
                dir,
                open_ms,
                facts,
                repetitions,
                seed,
                |key| {
                    let mut statement = db.prepare("SELECT value FROM facts WHERE entity = ?1")?;
                    statement.bind((1, i64::try_from(key)?))?;
                    match statement.next()? {
                        SqliteState::Row => Ok(Some(statement.read::<i64, _>(0)?)),
                        SqliteState::Done => Ok(None),
                    }
                },
                || {
                    let mut statement =
                        db.prepare("SELECT COUNT(*), COALESCE(SUM(value), 0) FROM facts")?;
                    if statement.next()? != SqliteState::Row {
                        bail!("SQLite aggregate missing");
                    }
                    let count = statement.read::<i64, _>(0)?;
                    let sum = statement.read::<i64, _>(1)?;
                    Ok((u64::try_from(count)?, i128::from(sum)))
                },
            )
        }
    }
}

fn measure_loaded(
    dir: &Path,
    open_ms: f64,
    facts: u64,
    repetitions: usize,
    seed: u64,
    mut point: impl FnMut(u64) -> Result<Option<i64>>,
    mut aggregate: impl FnMut() -> Result<(u64, i128)>,
) -> Result<QueryMeasurement> {
    let baseline = current_rss_bytes().context("read open baseline RSS")?;
    let baseline_breakdown = memory_breakdown(dir)?;
    let counters = process_counters().unwrap_or_default();

    let first_started = Instant::now();
    validate_point(facts / 2, facts, point(facts / 2)?)?;
    let first_read_ms = elapsed(first_started);

    let expected = expected_pair(facts);
    validate_pair(aggregate()?, expected, "aggregate warm-up")?;
    let mut aggregate_samples_ms = Vec::with_capacity(repetitions);
    let mut pair = expected;
    for _ in 0..repetitions {
        let started = Instant::now();
        pair = aggregate()?;
        aggregate_samples_ms.push(elapsed(started));
        validate_pair(pair, expected, "aggregate sample")?;
    }
    let aggregate_retained = current_rss_bytes().context("read aggregate retained RSS")?;
    let aggregate_peak = peak_rss_bytes().context("read aggregate peak RSS")?;
    let aggregate_retained_breakdown = memory_breakdown(dir)?;
    let aggregate_process_delta = process_counters()
        .unwrap_or_default()
        .saturating_sub(counters);

    let hot = vec![facts / 2];
    let distributed = distributed_keys(facts, seed, 256);
    let misses = miss_keys(facts, 256);
    let point_hot = measure_point_workload(facts, repetitions, &hot, &mut point)?;
    let point_distributed = measure_point_workload(facts, repetitions, &distributed, &mut point)?;
    let point_miss = measure_point_workload(facts, repetitions, &misses, &mut point)?;

    Ok(finish_query_measurement(
        open_ms,
        first_read_ms,
        point_hot,
        point_distributed,
        point_miss,
        baseline,
        baseline_breakdown,
        aggregate_retained,
        aggregate_peak,
        aggregate_retained_breakdown,
        aggregate_process_delta,
        aggregate_samples_ms,
        pair,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish_query_measurement(
    open_ms: f64,
    first_read_ms: f64,
    point_hot: PointMeasurement,
    point_distributed: PointMeasurement,
    point_miss: PointMeasurement,
    baseline: u64,
    baseline_breakdown: MemoryBreakdown,
    retained: u64,
    peak: u64,
    retained_breakdown: MemoryBreakdown,
    process_delta: ProcessCounters,
    aggregate_samples_ms: Vec<f64>,
    pair: (u64, i128),
) -> QueryMeasurement {
    QueryMeasurement {
        open_ms,
        first_read_ms,
        point_hot,
        point_distributed,
        point_miss,
        aggregate_samples_ms,
        count: pair.0,
        checksum: pair.1,
        open_baseline_rss_bytes: baseline,
        workload_peak_rss_bytes: peak,
        workload_delta_rss_bytes: peak.saturating_sub(baseline),
        retained_rss_bytes: retained.saturating_sub(baseline),
        baseline_breakdown,
        retained_breakdown,
        retained_delta_breakdown: retained_breakdown.saturating_sub(baseline_breakdown),
        process_delta,
    }
}

fn measure_point_workload(
    facts: u64,
    repetitions: usize,
    keys: &[u64],
    query: &mut impl FnMut(u64) -> Result<Option<i64>>,
) -> Result<PointMeasurement> {
    const TARGET_MS: f64 = 20.0;
    const MAX_OPERATIONS: usize = 16_384;
    let mut operations = 1_usize;
    loop {
        let started = Instant::now();
        for operation in 0..operations {
            let key = keys[operation % keys.len()];
            validate_point(key, facts, query(key)?)?;
        }
        if elapsed(started) >= TARGET_MS || operations == MAX_OPERATIONS {
            break;
        }
        operations = (operations * 2).min(MAX_OPERATIONS);
    }

    let mut samples_ms_per_operation = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let started = Instant::now();
        for operation in 0..operations {
            let key = keys[operation % keys.len()];
            validate_point(key, facts, query(key)?)?;
        }
        samples_ms_per_operation.push(elapsed(started) / operations as f64);
    }
    Ok(PointMeasurement {
        operations_per_sample: operations,
        samples_ms_per_operation,
    })
}

fn cozo_aggregate(db: &DbInstance) -> Result<(u64, i128)> {
    let result = db
        .run_script(
            "?[count(value), sum(value)] := *facts{value}",
            BTreeMap::new(),
            ScriptMutability::Immutable,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let row = result.rows.first().context("Cozo aggregate missing")?;
    Ok((
        u64::try_from(
            row.first()
                .and_then(data_i64)
                .context("Cozo count missing")?,
        )?,
        i128::from(row.get(1).and_then(data_i64).context("Cozo sum missing")?),
    ))
}

async fn measure_turso_loaded(
    dir: &Path,
    open_ms: f64,
    connection: &turso::Connection,
    facts: u64,
    repetitions: usize,
    seed: u64,
) -> Result<QueryMeasurement> {
    let baseline = current_rss_bytes().context("read Turso open baseline RSS")?;
    let baseline_breakdown = memory_breakdown(dir)?;
    let counters = process_counters().unwrap_or_default();

    let first_started = Instant::now();
    validate_point(facts / 2, facts, turso_point(connection, facts / 2).await?)?;
    let first_read_ms = elapsed(first_started);

    let expected = expected_pair(facts);
    validate_pair(
        turso_aggregate(connection).await?,
        expected,
        "Turso aggregate warm-up",
    )?;
    let mut aggregate_samples_ms = Vec::with_capacity(repetitions);
    let mut pair = expected;
    for _ in 0..repetitions {
        let started = Instant::now();
        pair = turso_aggregate(connection).await?;
        aggregate_samples_ms.push(elapsed(started));
        validate_pair(pair, expected, "Turso aggregate sample")?;
    }
    let aggregate_retained = current_rss_bytes().context("read Turso aggregate retained RSS")?;
    let aggregate_peak = peak_rss_bytes().context("read Turso aggregate peak RSS")?;
    let aggregate_retained_breakdown = memory_breakdown(dir)?;
    let aggregate_process_delta = process_counters()
        .unwrap_or_default()
        .saturating_sub(counters);

    let point_hot = measure_turso_point(connection, facts, repetitions, &[facts / 2]).await?;
    let distributed = distributed_keys(facts, seed, 256);
    let point_distributed =
        measure_turso_point(connection, facts, repetitions, &distributed).await?;
    let misses = miss_keys(facts, 256);
    let point_miss = measure_turso_point(connection, facts, repetitions, &misses).await?;

    Ok(finish_query_measurement(
        open_ms,
        first_read_ms,
        point_hot,
        point_distributed,
        point_miss,
        baseline,
        baseline_breakdown,
        aggregate_retained,
        aggregate_peak,
        aggregate_retained_breakdown,
        aggregate_process_delta,
        aggregate_samples_ms,
        pair,
    ))
}

async fn measure_turso_point(
    connection: &turso::Connection,
    facts: u64,
    repetitions: usize,
    keys: &[u64],
) -> Result<PointMeasurement> {
    const TARGET_MS: f64 = 20.0;
    const MAX_OPERATIONS: usize = 16_384;
    let mut operations = 1_usize;
    loop {
        let started = Instant::now();
        for operation in 0..operations {
            let key = keys[operation % keys.len()];
            validate_point(key, facts, turso_point(connection, key).await?)?;
        }
        if elapsed(started) >= TARGET_MS || operations == MAX_OPERATIONS {
            break;
        }
        operations = (operations * 2).min(MAX_OPERATIONS);
    }

    let mut samples_ms_per_operation = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let started = Instant::now();
        for operation in 0..operations {
            let key = keys[operation % keys.len()];
            validate_point(key, facts, turso_point(connection, key).await?)?;
        }
        samples_ms_per_operation.push(elapsed(started) / operations as f64);
    }
    Ok(PointMeasurement {
        operations_per_sample: operations,
        samples_ms_per_operation,
    })
}

async fn turso_point(connection: &turso::Connection, key: u64) -> Result<Option<i64>> {
    let mut rows = connection
        .query(
            "SELECT value FROM facts WHERE entity = ?1",
            [i64::try_from(key)?],
        )
        .await?;
    Ok(rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?)
}

async fn turso_aggregate(connection: &turso::Connection) -> Result<(u64, i128)> {
    let mut rows = connection
        .query("SELECT COUNT(*), COALESCE(SUM(value), 0) FROM facts", ())
        .await?;
    let row = rows.next().await?.context("Turso aggregate missing")?;
    Ok((
        u64::try_from(row.get::<i64>(0)?)?,
        i128::from(row.get::<i64>(1)?),
    ))
}

fn distributed_keys(facts: u64, seed: u64, count: usize) -> Vec<u64> {
    let mut state = seed.max(1);
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % facts
        })
        .collect()
}

fn miss_keys(facts: u64, count: usize) -> Vec<u64> {
    (0..count)
        .map(|offset| facts.saturating_add(1 + offset as u64))
        .collect()
}

fn validate_point(key: u64, facts: u64, actual: Option<i64>) -> Result<()> {
    let expected = (key < facts).then(|| i64::try_from(key)).transpose()?;
    if actual != expected {
        bail!("point correctness mismatch for key {key}");
    }
    Ok(())
}

fn expected_pair(facts: u64) -> (u64, i128) {
    (facts, i128::from(facts) * i128::from(facts - 1) / 2)
}

fn validate_pair(actual: (u64, i128), expected: (u64, i128), workload: &str) -> Result<()> {
    if actual != expected {
        bail!("{workload} correctness mismatch");
    }
    Ok(())
}

fn start_build() -> (Instant, u64, ProcessCounters) {
    (
        Instant::now(),
        current_rss_bytes().unwrap_or_default(),
        process_counters().unwrap_or_default(),
    )
}

fn finish_build(start: (Instant, u64, ProcessCounters)) -> BuildMeasurement {
    BuildMeasurement {
        elapsed_ms: elapsed(start.0),
        baseline_rss_bytes: start.1,
        peak_rss_bytes: peak_rss_bytes().unwrap_or_default(),
        process_delta: process_counters()
            .unwrap_or_default()
            .saturating_sub(start.2),
    }
}

fn elapsed(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    line.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn process_counters() -> Option<ProcessCounters> {
    let io = fs::read_to_string("/proc/self/io").ok()?;
    let read_bytes = proc_named_value(&io, "read_bytes:")?;
    let write_bytes = proc_named_value(&io, "write_bytes:")?;

    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat
        .get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .collect();
    Some(ProcessCounters {
        read_bytes,
        write_bytes,
        minor_faults: fields.get(7)?.parse().ok()?,
        major_faults: fields.get(9)?.parse().ok()?,
    })
}

fn proc_named_value(contents: &str, name: &str) -> Option<u64> {
    contents
        .lines()
        .find(|line| line.starts_with(name))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn memory_breakdown(database_dir: &Path) -> Result<MemoryBreakdown> {
    let smaps = fs::read_to_string("/proc/self/smaps").context("read /proc/self/smaps")?;
    let database_dir = database_dir.canonicalize()?;
    let database_prefix = database_dir.to_string_lossy();
    let mut current_heap = false;
    let mut current_database = false;
    let mut total_rss = 0_u64;
    let mut anonymous_rss = 0_u64;
    let mut heap_rss = 0_u64;
    let mut database_rss = 0_u64;

    for line in smaps.lines() {
        if is_smaps_header(line) {
            let path = line.split_whitespace().nth(5).unwrap_or("");
            current_heap = path == "[heap]";
            current_database = !path.is_empty() && path.starts_with(database_prefix.as_ref());
            continue;
        }
        if let Some(bytes) = smaps_kib_value(line, "Rss:") {
            total_rss = total_rss.saturating_add(bytes);
            if current_heap {
                heap_rss = heap_rss.saturating_add(bytes);
            }
            if current_database {
                database_rss = database_rss.saturating_add(bytes);
            }
        } else if let Some(bytes) = smaps_kib_value(line, "Anonymous:") {
            anonymous_rss = anonymous_rss.saturating_add(bytes);
        }
    }

    Ok(MemoryBreakdown {
        anonymous_rss_bytes: anonymous_rss,
        file_backed_rss_bytes: total_rss.saturating_sub(anonymous_rss),
        heap_mapping_rss_bytes: heap_rss,
        database_mapped_rss_bytes: database_rss,
    })
}

fn is_smaps_header(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|range| {
        range.contains('-') && range.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
    })
}

fn smaps_kib_value(line: &str, field: &str) -> Option<u64> {
    line.strip_prefix(field)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total += if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributed_point_keys_are_seeded_and_in_range() {
        let first = distributed_keys(10_000, 42, 256);
        let second = distributed_keys(10_000, 42, 256);
        let other = distributed_keys(10_000, 43, 256);
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert!(first.iter().all(|key| *key < 10_000));
    }

    #[test]
    fn miss_keys_are_outside_the_fixture() {
        let keys = miss_keys(10_000, 256);
        assert_eq!(keys.len(), 256);
        assert!(keys.iter().all(|key| *key > 10_000));
    }

    #[test]
    fn process_counter_delta_saturates_each_field() {
        let current = ProcessCounters {
            read_bytes: 20,
            write_bytes: 5,
            minor_faults: 30,
            major_faults: 1,
        };
        let baseline = ProcessCounters {
            read_bytes: 10,
            write_bytes: 8,
            minor_faults: 12,
            major_faults: 2,
        };
        let delta = current.saturating_sub(baseline);
        assert_eq!(delta.read_bytes, 10);
        assert_eq!(delta.write_bytes, 0);
        assert_eq!(delta.minor_faults, 18);
        assert_eq!(delta.major_faults, 0);
    }
}
