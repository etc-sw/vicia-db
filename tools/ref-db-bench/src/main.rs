use anyhow::{Context, Result, bail};
use cozo::{DataValue, DbInstance, Num, ScriptMutability};
use fjall::{Database as FjallDatabase, KeyspaceCreateOptions};
use grafeo::{GrafeoDB, Value as GrafeoValue};
use minigraf::{OpenOptions, QueryResult};
use redb::{Database as RedbDatabase, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
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
        }
    }

    fn boundary(self) -> &'static str {
        match self {
            Self::Vicia => "engineAggregate",
            Self::Grafeo => "engineAggregate",
            Self::Turso => "engineAggregate",
            Self::Cozo => "engineAggregate",
            Self::Redb => "ownedResultScan",
            Self::Fjall => "ownedResultScan",
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
    build_ms: f64,
    read_ms: f64,
    aggregate_samples_ms: Vec<f64>,
    count: u64,
    checksum: i128,
    memory: MemoryMeasurement,
    storage_bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryMeasurement {
    aggregate_samples_ms: Vec<f64>,
    count: u64,
    checksum: i128,
    open_baseline_rss_bytes: u64,
    workload_peak_rss_bytes: u64,
    workload_delta_rss_bytes: u64,
    retained_rss_bytes: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 5 && args[1] == "measure" {
        let measurement = measure_fresh(
            Engine::parse(&args[2])?,
            Path::new(&args[3]),
            args[4].parse()?,
        )
        .await?;
        println!("{}", serde_json::to_string(&measurement)?);
        return Ok(());
    }
    if args.len() != 6 || args[1] != "run" {
        bail!("usage: vicia-ref-db-bench run <engine> <data-dir> <facts> <repetitions>");
    }
    let engine = Engine::parse(&args[2])?;
    let dir = PathBuf::from(&args[3]);
    let facts = args[4].parse::<u64>()?;
    let repetitions = args[5].parse::<usize>()?;
    if facts == 0 || repetitions == 0 {
        bail!("facts and repetitions must be positive");
    }
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;

    let receipt = match engine {
        Engine::Vicia => run_vicia(engine, &dir, facts, repetitions)?,
        Engine::Grafeo => run_grafeo(engine, &dir, facts, repetitions)?,
        Engine::Redb => run_redb(engine, &dir, facts, repetitions)?,
        Engine::Fjall => run_fjall(engine, &dir, facts, repetitions)?,
        Engine::Turso => run_turso(engine, &dir, facts, repetitions).await?,
        Engine::Cozo => run_cozo(engine, &dir, facts, repetitions)?,
    };
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

fn receipt(
    engine: Engine,
    dir: &Path,
    facts: u64,
    build_ms: f64,
    read_ms: f64,
    samples: Vec<f64>,
    count: u64,
    checksum: i128,
) -> Result<Receipt> {
    let expected = i128::from(facts) * i128::from(facts - 1) / 2;
    if count != facts || checksum != expected {
        bail!(
            "{} correctness mismatch: count {count}/{facts}, checksum {checksum}/{expected}",
            engine.id()
        );
    }
    let memory = measure_in_child(engine, dir, samples.len())?;
    if memory.count != facts || memory.checksum != expected {
        bail!(
            "{} fresh memory measurement correctness mismatch",
            engine.id()
        );
    }
    Ok(Receipt {
        schema: "vicia.ref-db-bench.v2",
        engine: engine.id(),
        role: engine.role(),
        execution_boundary: engine.boundary(),
        facts,
        build_ms,
        read_ms,
        aggregate_samples_ms: samples,
        count,
        checksum,
        memory,
        storage_bytes: directory_bytes(dir)?,
    })
}

fn measure_in_child(engine: Engine, dir: &Path, repetitions: usize) -> Result<MemoryMeasurement> {
    let output = Command::new(std::env::current_exe()?)
        .arg("measure")
        .arg(engine.id())
        .arg(dir)
        .arg(repetitions.to_string())
        .output()
        .with_context(|| format!("launch fresh {} memory measurement", engine.id()))?;
    if !output.status.success() {
        bail!(
            "fresh {} memory measurement failed: {}",
            engine.id(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode fresh memory measurement")
}

fn run_vicia(engine: Engine, dir: &Path, facts: u64, repetitions: usize) -> Result<Receipt> {
    let path = dir.join("vicia.graph");
    let started = Instant::now();
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
    let build_ms = elapsed(started);
    let read = Instant::now();
    let result = db.execute(&format!(
        "(query [:find ?v :where [:cmp/e{} :cmp/value ?v]])",
        facts / 2
    ))?;
    validate_vicia_point(result, facts / 2)?;
    let read_ms = elapsed(read);
    let mut samples = Vec::with_capacity(repetitions);
    let mut final_pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
        final_pair = vicia_aggregate(&db)?;
        samples.push(elapsed(started));
    }
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        build_ms,
        read_ms,
        samples,
        final_pair.0,
        final_pair.1,
    )
}

fn validate_vicia_point(result: QueryResult, expected: u64) -> Result<()> {
    let QueryResult::QueryResults { results, .. } = result else {
        bail!("Vicia point query returned a non-query result");
    };
    let value = results
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_integer())
        .context("Vicia point query returned no integer")?;
    if value != i64::try_from(expected)? {
        bail!("Vicia point query mismatch");
    }
    Ok(())
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

fn run_redb(engine: Engine, dir: &Path, facts: u64, repetitions: usize) -> Result<Receipt> {
    let started = Instant::now();
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
    let build_ms = elapsed(started);
    let read = Instant::now();
    let tx = db.begin_read()?;
    let table = tx.open_table(REDB_FACTS)?;
    let value = table.get(facts / 2)?.context("redb point missing")?.value();
    if value != i64::try_from(facts / 2)? {
        bail!("redb point mismatch");
    }
    let read_ms = elapsed(read);
    drop(table);
    drop(tx);
    let mut samples = Vec::with_capacity(repetitions);
    let mut final_pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
        let tx = db.begin_read()?;
        let table = tx.open_table(REDB_FACTS)?;
        let mut count = 0_u64;
        let mut sum = 0_i128;
        for entry in table.iter()? {
            let (_, value) = entry?;
            count += 1;
            sum += i128::from(value.value());
        }
        final_pair = (count, sum);
        samples.push(elapsed(started));
    }
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        build_ms,
        read_ms,
        samples,
        final_pair.0,
        final_pair.1,
    )
}

fn key_bytes(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}

fn run_fjall(engine: Engine, dir: &Path, facts: u64, repetitions: usize) -> Result<Receipt> {
    let started = Instant::now();
    let db = FjallDatabase::builder(dir.join("fjall")).open()?;
    let items = db.keyspace("facts", KeyspaceCreateOptions::default)?;
    for start in (0..facts).step_by(BATCH as usize) {
        let mut batch = db.batch();
        for key in start..(start + BATCH).min(facts) {
            batch.insert(&items, key_bytes(key), key_bytes(key));
        }
        batch.commit()?;
    }
    let build_ms = elapsed(started);
    let read = Instant::now();
    let value = items
        .get(key_bytes(facts / 2))?
        .context("fjall point missing")?;
    if value.as_ref() != key_bytes(facts / 2) {
        bail!("fjall point mismatch");
    }
    let read_ms = elapsed(read);
    let mut samples = Vec::with_capacity(repetitions);
    let mut final_pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
        let mut count = 0_u64;
        let mut sum = 0_i128;
        for entry in items.iter() {
            let value = entry.value()?;
            let bytes: [u8; 8] = value.as_ref().try_into()?;
            count += 1;
            sum += i128::from(u64::from_be_bytes(bytes));
        }
        final_pair = (count, sum);
        samples.push(elapsed(started));
    }
    drop(items);
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        build_ms,
        read_ms,
        samples,
        final_pair.0,
        final_pair.1,
    )
}

fn run_grafeo(engine: Engine, dir: &Path, facts: u64, repetitions: usize) -> Result<Receipt> {
    let started = Instant::now();
    let db = GrafeoDB::open(dir.join("grafeo"))?;
    for entity in 0..facts {
        let node = db.create_node(&["Fact"]);
        db.set_node_property(node, "entity", GrafeoValue::from(i64::try_from(entity)?));
        db.set_node_property(node, "value", GrafeoValue::from(i64::try_from(entity)?));
    }
    let build_ms = elapsed(started);
    let session = db.session();
    let read = Instant::now();
    let query = format!(
        "MATCH (n:Fact) WHERE n.entity = {} RETURN n.value",
        facts / 2
    );
    let value: i64 = session.execute(&query)?.scalar()?;
    if value != i64::try_from(facts / 2)? {
        bail!("Grafeo point mismatch");
    }
    let read_ms = elapsed(read);
    let mut samples = Vec::with_capacity(repetitions);
    let mut final_pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
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
        final_pair = (u64::try_from(count)?, i128::from(sum));
        samples.push(elapsed(started));
    }
    drop(session);
    db.close()?;
    receipt(
        engine,
        dir,
        facts,
        build_ms,
        read_ms,
        samples,
        final_pair.0,
        final_pair.1,
    )
}

fn cozo_open(path: &Path) -> Result<DbInstance> {
    DbInstance::new("sqlite", path.to_str().context("non-UTF8 Cozo path")?, "")
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn run_cozo(engine: Engine, dir: &Path, facts: u64, repetitions: usize) -> Result<Receipt> {
    let started = Instant::now();
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
    let build_ms = elapsed(started);
    let read = Instant::now();
    let point = db
        .run_script(
            "?[value] := *facts{entity: $id, value}",
            BTreeMap::from([(
                "id".to_string(),
                DataValue::Num(Num::Int((facts / 2) as i64)),
            )]),
            ScriptMutability::Immutable,
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let value = point
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(data_i64)
        .context("Cozo point missing")?;
    if value != i64::try_from(facts / 2)? {
        bail!("Cozo point mismatch");
    }
    let read_ms = elapsed(read);
    let mut samples = Vec::with_capacity(repetitions);
    let mut final_pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
        let result = db
            .run_script(
                "?[count(value), sum(value)] := *facts{value}",
                BTreeMap::new(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let row = result.rows.first().context("Cozo aggregate missing")?;
        final_pair = (
            u64::try_from(
                row.first()
                    .and_then(data_i64)
                    .context("Cozo count missing")?,
            )?,
            i128::from(row.get(1).and_then(data_i64).context("Cozo sum missing")?),
        );
        samples.push(elapsed(started));
    }
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        build_ms,
        read_ms,
        samples,
        final_pair.0,
        final_pair.1,
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

async fn run_turso(engine: Engine, dir: &Path, facts: u64, repetitions: usize) -> Result<Receipt> {
    let path = dir.join("turso.db");
    let started = Instant::now();
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
    let build_ms = elapsed(started);
    let read = Instant::now();
    let mut rows = conn
        .query(
            "SELECT value FROM facts WHERE entity = ?1",
            [i64::try_from(facts / 2)?],
        )
        .await?;
    let row = rows.next().await?.context("Turso point missing")?;
    let value = row.get::<i64>(0)?;
    if value != i64::try_from(facts / 2)? {
        bail!("Turso point mismatch");
    }
    let read_ms = elapsed(read);
    let mut samples = Vec::with_capacity(repetitions);
    let mut final_pair = (0, 0);
    for _ in 0..repetitions {
        let started = Instant::now();
        let mut rows = conn
            .query("SELECT COUNT(*), COALESCE(SUM(value), 0) FROM facts", ())
            .await?;
        let row = rows.next().await?.context("Turso aggregate missing")?;
        final_pair = (
            u64::try_from(row.get::<i64>(0)?)?,
            i128::from(row.get::<i64>(1)?),
        );
        samples.push(elapsed(started));
    }
    drop(row);
    drop(rows);
    drop(conn);
    drop(db);
    receipt(
        engine,
        dir,
        facts,
        build_ms,
        read_ms,
        samples,
        final_pair.0,
        final_pair.1,
    )
}

async fn measure_fresh(
    engine: Engine,
    dir: &Path,
    repetitions: usize,
) -> Result<MemoryMeasurement> {
    match engine {
        Engine::Vicia => {
            let db = OpenOptions::new().path(dir.join("vicia.graph")).open()?;
            measure_loaded(repetitions, || vicia_aggregate(&db))
        }
        Engine::Grafeo => {
            let db = GrafeoDB::open(dir.join("grafeo"))?;
            let session = db.session();
            measure_loaded(repetitions, || {
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
            })
        }
        Engine::Redb => {
            let db = RedbDatabase::open(dir.join("redb.db"))?;
            measure_loaded(repetitions, || {
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
            })
        }
        Engine::Fjall => {
            let db = FjallDatabase::builder(dir.join("fjall")).open()?;
            let items = db.keyspace("facts", KeyspaceCreateOptions::default)?;
            measure_loaded(repetitions, || {
                let mut count = 0_u64;
                let mut checksum = 0_i128;
                for entry in items.iter() {
                    let value = entry.value()?;
                    let bytes: [u8; 8] = value.as_ref().try_into()?;
                    count += 1;
                    checksum += i128::from(u64::from_be_bytes(bytes));
                }
                Ok((count, checksum))
            })
        }
        Engine::Turso => {
            let path = dir.join("turso.db");
            let db = TursoBuilder::new_local(&path.to_string_lossy())
                .build()
                .await?;
            let conn = db.connect()?;
            let baseline = current_rss_bytes().context("read Turso baseline RSS")?;
            let mut samples = Vec::with_capacity(repetitions);
            let mut pair = (0_u64, 0_i128);
            for _ in 0..repetitions {
                let started = Instant::now();
                let mut rows = conn
                    .query("SELECT COUNT(*), COALESCE(SUM(value), 0) FROM facts", ())
                    .await?;
                let row = rows.next().await?.context("Turso aggregate missing")?;
                pair = (
                    u64::try_from(row.get::<i64>(0)?)?,
                    i128::from(row.get::<i64>(1)?),
                );
                samples.push(elapsed(started));
            }
            finish_memory_measurement(baseline, samples, pair)
        }
        Engine::Cozo => {
            let db = cozo_open(&dir.join("cozo.db"))?;
            measure_loaded(repetitions, || {
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
            })
        }
    }
}

fn measure_loaded(
    repetitions: usize,
    mut workload: impl FnMut() -> Result<(u64, i128)>,
) -> Result<MemoryMeasurement> {
    let baseline = current_rss_bytes().context("read open baseline RSS")?;
    let mut samples = Vec::with_capacity(repetitions);
    let mut pair = (0_u64, 0_i128);
    for _ in 0..repetitions {
        let started = Instant::now();
        pair = workload()?;
        samples.push(elapsed(started));
    }
    finish_memory_measurement(baseline, samples, pair)
}

fn finish_memory_measurement(
    baseline: u64,
    samples: Vec<f64>,
    pair: (u64, i128),
) -> Result<MemoryMeasurement> {
    let retained = current_rss_bytes().context("read retained RSS")?;
    let peak = peak_rss_bytes().context("read peak RSS")?;
    Ok(MemoryMeasurement {
        aggregate_samples_ms: samples,
        count: pair.0,
        checksum: pair.1,
        open_baseline_rss_bytes: baseline,
        workload_peak_rss_bytes: peak,
        workload_delta_rss_bytes: peak.saturating_sub(baseline),
        retained_rss_bytes: retained.saturating_sub(baseline),
    })
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
