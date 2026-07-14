#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  ENGINES,
  ENGINE_AGGREGATE,
  OWNED_RESULT_SCAN,
  validateReceipts,
} from "./validate-ref-db-bench.mjs";

const [outputDir, profile] = process.argv.slice(2);
if (!outputDir || !profile) {
  console.error("usage: summarize-ref-db-bench.mjs <output-dir> <profile>");
  process.exit(2);
}

const receipts = readdirSync(outputDir)
  .filter((name) => /^(vicia|grafeo|sqlite|redb|fjall|turso|cozo)-trial-\d+\.json$/.test(name))
  .map((name) => JSON.parse(readFileSync(join(outputDir, name), "utf8")));
const shape = validateReceipts(receipts, profile);
const sourceCommits = referenceVersions(receipts);
const rows = ENGINES.map((engine) => summarizeEngine(receipts.filter((row) => row.engine === engine)));
const stabilityFailures = rows.flatMap((row) =>
  ["hotPointMadPercent", "distributedPointMadPercent", "missPointMadPercent", "workloadMadPercent"]
    .filter((field) => row[field] > 5)
    .map((field) => ({ engine: row.engine, metric: field, value: row[field] })),
);

const report = {
  schema: "vicia.ref-db-bench.summary.v5",
  profile,
  facts: shape.facts,
  repetitionsPerTrial: shape.repetitions,
  trials: shape.trials,
  seeds: [...new Set(receipts.map((row) => row.seed))].sort((a, b) => a - b),
  trialOrders: Array.from({ length: shape.trials }, (_, trial) =>
    receipts
      .filter((row) => row.trial === trial)
      .sort((a, b) => a.orderPosition - b.orderPosition)
      .map((row) => row.engine)),
  provenance: hostProvenance(),
  sourceCommits,
  comparisonPolicy: {
    engineAggregate: ENGINE_AGGREGATE,
    ownedResultScan: OWNED_RESULT_SCAN,
    warning: "Engine aggregate and owned result scan are different contracts and must not be ranked in one column.",
    lifecycle: "Each trial builds and closes a new database, then a fresh child measures open, first read, warmed point workloads, and aggregate/scan.",
    pointTiming: "Point samples are adaptively batched to at least 20 ms when possible; values are reported as milliseconds per operation.",
    storage: "Physical bytes describe each adapter schema, not equivalent product semantics. Vicia retains native bi-temporal ledger identity and four index orders.",
    kernelPageCache: "Not controlled or attributed; first-read and warmed-read results are reported separately.",
  },
  stability: {
    gateMadPercent: 5,
    passed: stabilityFailures.length === 0,
    failures: stabilityFailures,
  },
  groups: {
    engineAggregate: rows.filter((row) => ENGINE_AGGREGATE.includes(row.engine)),
    ownedResultScan: rows.filter((row) => OWNED_RESULT_SCAN.includes(row.engine)),
  },
};

writeFileSync(join(outputDir, "summary.json"), `${JSON.stringify(report, null, 2)}\n`);
const markdown = renderMarkdown(report);
writeFileSync(join(outputDir, "summary.md"), markdown);
console.log(markdown);

function summarizeEngine(engineReceipts) {
  const first = engineReceipts[0];
  const trialMetric = (selector) => engineReceipts.map((receipt) => median(selector(receipt)));
  const allSamples = (selector) => engineReceipts.flatMap(selector);
  const hotTrials = trialMetric((row) => row.query.pointHot.samplesMsPerOperation);
  const distributedTrials = trialMetric((row) => row.query.pointDistributed.samplesMsPerOperation);
  const missTrials = trialMetric((row) => row.query.pointMiss.samplesMsPerOperation);
  const workloadTrials = trialMetric((row) => row.query.aggregateSamplesMs);
  const hot = allSamples((row) => row.query.pointHot.samplesMsPerOperation);
  const distributed = allSamples((row) => row.query.pointDistributed.samplesMsPerOperation);
  const miss = allSamples((row) => row.query.pointMiss.samplesMsPerOperation);
  const workload = allSamples((row) => row.query.aggregateSamplesMs);
  return {
    engine: first.engine,
    role: first.role,
    boundary: first.executionBoundary,
    adapterSchema: first.adapterSchema,
    semanticScope: first.semanticScope,
    durability: first.durability,
    runtimeVersion: first.runtimeVersion,
    buildP50Ms: round(median(engineReceipts.map((row) => row.build.elapsedMs))),
    openP50Ms: round(median(engineReceipts.map((row) => row.query.openMs))),
    firstReadP50Ms: round(median(engineReceipts.map((row) => row.query.firstReadMs))),
    hotPointP50Ms: round(percentile(hot, 50)),
    hotPointP95Ms: round(percentile(hot, 95)),
    hotPointMadPercent: round(relativeMad(hotTrials)),
    distributedPointP50Ms: round(percentile(distributed, 50)),
    distributedPointP95Ms: round(percentile(distributed, 95)),
    distributedPointMadPercent: round(relativeMad(distributedTrials)),
    missPointP50Ms: round(percentile(miss, 50)),
    missPointP95Ms: round(percentile(miss, 95)),
    missPointMadPercent: round(relativeMad(missTrials)),
    workloadP50Ms: round(percentile(workload, 50)),
    workloadP95Ms: round(percentile(workload, 95)),
    workloadMaxMs: round(Math.max(...workload)),
    workloadMadPercent: round(relativeMad(workloadTrials)),
    baselineRssMiB: toMiB(median(engineReceipts.map((row) => row.query.openBaselineRssBytes))),
    deltaRssMiB: toMiB(median(engineReceipts.map((row) => row.query.workloadDeltaRssBytes))),
    retainedRssMiB: toMiB(median(engineReceipts.map((row) => row.query.retainedRssBytes))),
    queryReadMiB: toMiB(median(engineReceipts.map((row) => row.query.processDelta.readBytes))),
    queryWriteMiB: toMiB(median(engineReceipts.map((row) => row.query.processDelta.writeBytes))),
    storageMiB: toMiB(median(engineReceipts.map((row) => row.storageBytes))),
    pointOperationsPerSample: {
      hot: [...new Set(engineReceipts.map((row) => row.query.pointHot.operationsPerSample))],
      distributed: [...new Set(engineReceipts.map((row) => row.query.pointDistributed.operationsPerSample))],
      miss: [...new Set(engineReceipts.map((row) => row.query.pointMiss.operationsPerSample))],
    },
    count: first.query.count,
    checksum: first.query.checksum,
  };
}

function referenceVersions(receipts) {
  const refRoot = process.env.DB_REF_DIR ?? join(process.env.HOME, "db-ref");
  const commits = Object.fromEntries(
    ["grafeo", "redb", "fjall", "turso", "cozo"].map((engine) => [
      engine,
      command("git", ["-C", join(refRoot, engine), "rev-parse", "HEAD"]),
    ]),
  );
  commits.vicia = command("git", ["rev-parse", "HEAD"]);
  commits.sqlite = receipts.find((row) => row.engine === "sqlite")?.runtimeVersion;
  return commits;
}

function hostProvenance() {
  const binary = "tools/ref-db-bench/target/release/vicia-ref-db-bench";
  return {
    cpuModel: readFileSync("/proc/cpuinfo", "utf8").match(/^model name\s*:\s*(.+)$/m)?.[1] ?? null,
    logicalCpus: Number(command("getconf", ["_NPROCESSORS_ONLN"])),
    kernel: command("uname", ["-srmo"]),
    filesystem: command("stat", ["-f", "-c", "%T", outputDir]),
    rustc: command("rustc", ["--version"]),
    cargo: command("cargo", ["--version"]),
    benchmarkBinarySha256: createHash("sha256").update(readFileSync(binary)).digest("hex"),
    viciaDirty: command("git", ["status", "--porcelain"]) !== "",
  };
}

function renderMarkdown(report) {
  const lifecycleHeader = ["engine", "build p50 ms", "open p50 ms", "first read p50 ms", "storage MiB"];
  const pointHeader = ["engine", "hot p50/p95 ms", "distributed p50/p95 ms", "miss p50/p95 ms", "trial MAD max %"];
  const workloadHeader = ["engine", "role", "workload p50 ms", "p95 ms", "max ms", "trial MAD %", "baseline RSS MiB", "delta RSS MiB", "retained RSS MiB", "correct"];
  const lifecycle = (rows) => table(lifecycleHeader, rows.map((row) => [row.engine, row.buildP50Ms, row.openP50Ms, row.firstReadP50Ms, row.storageMiB]));
  const points = (rows) => table(pointHeader, rows.map((row) => [row.engine, `${row.hotPointP50Ms}/${row.hotPointP95Ms}`, `${row.distributedPointP50Ms}/${row.distributedPointP95Ms}`, `${row.missPointP50Ms}/${row.missPointP95Ms}`, Math.max(row.hotPointMadPercent, row.distributedPointMadPercent, row.missPointMadPercent)]));
  const workloads = (rows) => table(workloadHeader, rows.map((row) => [row.engine, row.role, row.workloadP50Ms, row.workloadP95Ms, row.workloadMaxMs, row.workloadMadPercent, row.baselineRssMiB, row.deltaRssMiB, row.retainedRssMiB, "yes"]));
  return [
    `# Vicia reference DB comparison (${report.profile}, v5)`,
    "",
    `Facts: ${report.facts}; trials: ${report.trials}; samples/trial: ${report.repetitionsPerTrial}`,
    `Stability gate (trial median MAD <= ${report.stability.gateMadPercent}%): ${report.stability.passed ? "pass" : "FAIL"}`,
    "",
    "## Lifecycle and physical storage", "", ...lifecycle([...report.groups.engineAggregate, ...report.groups.ownedResultScan]), "",
    "## Warm point workloads", "", ...points([...report.groups.engineAggregate, ...report.groups.ownedResultScan]), "",
    "## Engine aggregate", "", ...workloads(report.groups.engineAggregate), "",
    "## Owned result scan storage floors", "", ...workloads(report.groups.ownedResultScan), "",
    "`engineAggregate` and `ownedResultScan` remain separate contracts.",
    "Every trial builds and closes its database, then measures reopen and queries in a fresh process.",
    "Point workloads use adaptive operation batches; first-read latency is reported separately from warmed latency.",
    "Storage bytes are physical adapter output, not equivalent semantic capacity; Vicia retains native bi-temporal ledger identity.",
    "Kernel page cache is neither dropped nor attributed to a process.",
    "Every sample passed point or exact count/checksum validation.",
    "",
  ].join("\n");
}

function table(header, rows) {
  return [
    `| ${header.join(" | ")} |`,
    `| ${header.map(() => "---").join(" | ")} |`,
    ...rows.map((row) => `| ${row.join(" | ")} |`),
  ];
}

function percentile(values, percent) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(sorted.length * percent / 100) - 1)];
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

function relativeMad(values) {
  const center = median(values);
  if (center === 0) return values.every((value) => value === 0) ? 0 : Infinity;
  return median(values.map((value) => Math.abs(value - center))) / center * 100;
}

function round(value) {
  return Math.round(value * 1_000_000) / 1_000_000;
}

function toMiB(bytes) {
  return round(bytes / 1024 / 1024);
}

function command(program, args) {
  return execFileSync(program, args, { encoding: "utf8" }).trim();
}
