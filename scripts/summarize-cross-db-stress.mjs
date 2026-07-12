#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [outputDir, profile] = process.argv.slice(2);
if (!outputDir || !profile) {
  console.error("usage: summarize-cross-db-stress.mjs <output-dir> <profile>");
  process.exit(2);
}

const engines = ["vicia", "cozo", "sqlite", "redb"];
const receipts = engines.map((engine) =>
  JSON.parse(readFileSync(join(outputDir, `${engine}.json`), "utf8")),
);
const crashReceipts = new Map(
  engines.map((engine) => [
    engine,
    JSON.parse(readFileSync(join(outputDir, `${engine}-crash.json`), "utf8")),
  ]),
);
for (const receipt of receipts) {
  validateStressReceipt(receipt, receipts[0]);
  const crash = crashReceipts.get(receipt.engine);
  validateCrashReceipt(crash, receipt);
}

const rows = receipts.map((receipt) => ({
  engine: receipt.engine,
  classification: receipt.classification,
  buildMs: receipt.metrics.build.max,
  appendP95Ms: receipt.metrics.durableAppend.p95,
  readP95Ms: receipt.metrics.pointRead.p95,
  reopenP95Ms: receipt.metrics.reopen.p95,
  engineAggregateMs: receipt.metrics.engineAggregate?.elapsedMs ?? null,
  engineAggregateRssMiB: toMiB(receipt.metrics.engineAggregate?.peakRssBytes),
  materializedScanMs: receipt.metrics.materializedScan.elapsedMs,
  materializedScanRssMiB: toMiB(receipt.metrics.materializedScan.peakRssBytes),
  stressProcessRssMiB: toMiB(receipt.metrics.stressProcessPeakRssBytes),
  storageMiB: Math.round((receipt.metrics.totalStorageBytes / 1024 / 1024) * 1000) / 1000,
  bytesPerFact: receipt.metrics.bytesPerFact,
  crashRecoveredFacts: crashReceipts.get(receipt.engine).recoveredCount,
  crashIntegrity: crashReceipts.get(receipt.engine).integrityCheck,
  correctness: receipt.correctness.passed,
}));

const report = {
  schema: "vicia.cross-db-stress.summary.v2",
  profile,
  sourceCommit: receipts[0].sourceCommit,
  sourceDirty: receipts[0].sourceDirty,
  host: receipts[0].host,
  acceptanceEligible: profile === "full" && receipts[0].sourceDirty === false,
  comparisonPolicy: {
    semanticPeers: ["vicia", "cozo"],
    relationalBaseline: ["sqlite"],
    storageFloor: ["redb"],
    warning: "Do not rank redb as a graph or Datalog engine; it measures the key-value storage floor only.",
    durability: "Each append sample is one durable transaction; SQLite uses WAL plus synchronous=FULL.",
    memory: "Peak resident set is Linux /proc VmHWM for one isolated engine process.",
    scanWorkloads: {
      engineAggregate: "The query engine computes count and sum and returns one scalar row. redb is N/A because it has no query engine.",
      materializedScan: "Every engine produces one owned Vec<i64> containing all values, then the same Rust host loop computes count and checksum.",
      isolation: "Each scan workload runs in a fresh child process. Its RSS excludes build, checkpoint, append, and the other scan workload.",
    },
    stability: "Every cycle closes and reopens the database, then a continuous writer is killed with SIGKILL. Recovery must retain the announced committed prefix and match its exact arithmetic checksum.",
  },
  rows,
};

writeFileSync(join(outputDir, "summary.json"), `${JSON.stringify(report, null, 2)}\n`);

const header = [
  "engine",
  "role",
  "build ms",
  "append p95 ms",
  "read p95 ms",
  "reopen p95 ms",
  "engine aggregate ms / RSS MiB",
  "materialized scan ms / RSS MiB",
  "stress process RSS MiB",
  "storage MiB",
  "bytes/fact",
  "kill-9 recovery",
  "correct",
];
const values = rows.map((row) => [
  row.engine,
  row.classification,
  row.buildMs,
  row.appendP95Ms,
  row.readP95Ms,
  row.reopenP95Ms,
  row.engineAggregateMs === null
    ? "n/a"
    : `${row.engineAggregateMs} / ${row.engineAggregateRssMiB ?? "n/a"}`,
  `${row.materializedScanMs} / ${row.materializedScanRssMiB ?? "n/a"}`,
  row.stressProcessRssMiB ?? "n/a",
  row.storageMiB,
  row.bytesPerFact,
  `${row.crashRecoveredFacts} / ${row.crashIntegrity}`,
  row.correctness ? "yes" : "no",
]);
const markdown = [
  `# Cross-DB stress comparison (${profile})`,
  "",
  `Source: \`${receipts[0].sourceCommit}\` (${receipts[0].sourceDirty ? "dirty" : "clean"})`,
  `Testbed: \`${receipts[0].host.testbed}\` / ${receipts[0].host.cpuModel ?? "unknown CPU"}`,
  `Acceptance eligible: ${report.acceptanceEligible ? "yes" : "no"}`,
  "",
  `| ${header.join(" | ")} |`,
  `| ${header.map(() => "---").join(" | ")} |`,
  ...values.map((row) => `| ${row.join(" | ")} |`),
  "",
  "redb is a storage-floor control, not a graph/Datalog semantic peer.",
  "Engine aggregate and materialized scan are separate comparisons; they must never be ranked in one column.",
  "Every engine passed exact count/checksum validation after repeated close/reopen cycles and SIGKILL recovery.",
  "",
].join("\n");
writeFileSync(join(outputDir, "summary.md"), markdown);
console.log(markdown);

function validateStressReceipt(receipt, baseline) {
  if (receipt.schema !== "vicia.cross-db-stress.v2" || !receipt.correctness.passed) {
    fail(`${receipt.engine}: invalid or failed comparison receipt`);
  }
  if (JSON.stringify(receipt.config) !== JSON.stringify(baseline.config)) {
    fail(`${receipt.engine}: workload config differs from the comparison baseline`);
  }
  if (JSON.stringify(receipt.host) !== JSON.stringify(baseline.host)) {
    fail(`${receipt.engine}: host provenance differs from the comparison baseline`);
  }
  if (receipt.sourceCommit !== baseline.sourceCommit || receipt.sourceDirty !== baseline.sourceDirty) {
    fail(`${receipt.engine}: source provenance differs from the comparison baseline`);
  }
  const expectedCount = receipt.config.baseFacts +
    receipt.config.cycles * receipt.config.factsPerCycle;
  const expectedChecksum = expectedCount * (expectedCount - 1) / 2;
  if (
    receipt.correctness.materializedCount !== expectedCount ||
    receipt.correctness.materializedChecksum !== expectedChecksum
  ) {
    fail(`${receipt.engine}: correctness fields do not match the workload`);
  }
  const materializedBoundaries = {
    vicia: "datalog-all-values-rust-fold",
    cozo: "cozoscript-all-values-rust-fold",
    sqlite: "sql-all-values-rust-fold",
    redb: "ordered-table-all-values-rust-fold",
  };
  const aggregateBoundaries = {
    vicia: "datalog-count-sum-scalar",
    cozo: "cozoscript-count-sum-scalar",
    sqlite: "sql-count-sum-scalar",
  };
  validateScan(
    receipt.metrics.materializedScan,
    "materializedScan",
    materializedBoundaries[receipt.engine],
    expectedCount,
    expectedChecksum,
  );
  if (receipt.engine === "redb") {
    if (receipt.metrics.engineAggregate !== null) {
      fail("redb: engine aggregate must be N/A because redb has no query engine");
    }
    if (
      receipt.correctness.engineAggregateCount !== null ||
      receipt.correctness.engineAggregateChecksum !== null
    ) {
      fail("redb: aggregate correctness must be N/A");
    }
  } else {
    validateScan(
      receipt.metrics.engineAggregate,
      "engineAggregate",
      aggregateBoundaries[receipt.engine],
      expectedCount,
      expectedChecksum,
    );
    if (
      receipt.correctness.engineAggregateCount !== expectedCount ||
      receipt.correctness.engineAggregateChecksum !== expectedChecksum
    ) {
      fail(`${receipt.engine}: aggregate correctness fields do not match the workload`);
    }
  }
  for (const [name, metric] of Object.entries({
    build: receipt.metrics.build,
    reopen: receipt.metrics.reopen,
    durableAppend: receipt.metrics.durableAppend,
    pointRead: receipt.metrics.pointRead,
  })) {
    const samples = [...metric.samples].sort((left, right) => left - right);
    if (metric.count !== samples.length || samples.length === 0) {
      fail(`${receipt.engine}/${name}: invalid sample count`);
    }
    if (
      metric.min !== samples[0] ||
      metric.p50 !== nearestRank(samples, 50) ||
      metric.p95 !== nearestRank(samples, 95) ||
      metric.p99 !== nearestRank(samples, 99) ||
      metric.max !== samples.at(-1)
    ) {
      fail(`${receipt.engine}/${name}: summary does not match raw samples`);
    }
  }
}

function validateScan(scan, workload, executionBoundary, expectedCount, expectedChecksum) {
  if (
    scan === null ||
    scan.workload !== workload ||
    scan.executionBoundary !== executionBoundary ||
    typeof scan.elapsedMs !== "number" ||
    scan.elapsedMs < 0 ||
    scan.count !== expectedCount ||
    scan.checksum !== expectedChecksum
  ) {
    fail(`${workload}: invalid isolated scan measurement`);
  }
}

function toMiB(bytes) {
  return bytes === null || bytes === undefined
    ? null
    : Math.round((bytes / 1024 / 1024) * 1000) / 1000;
}

function validateCrashReceipt(crash, stress) {
  const expectedChecksum = crash.recoveredCount * (crash.recoveredCount - 1) / 2;
  if (
    crash.schema !== "vicia.cross-db-crash.v1" ||
    !crash.passed ||
    crash.engine !== stress.engine ||
    crash.recoveredCount < crash.minimumCommittedCount ||
    crash.recoveredChecksum !== expectedChecksum ||
    crash.expectedChecksum !== expectedChecksum
  ) {
    fail(`${stress.engine}: crash recovery receipt failed validation`);
  }
}

function nearestRank(samples, percentile) {
  const rank = Math.ceil(samples.length * percentile / 100);
  return samples[Math.max(0, rank - 1)];
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
