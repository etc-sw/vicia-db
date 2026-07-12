#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [outputDir, profile] = process.argv.slice(2);
if (!outputDir || !profile) {
  console.error("usage: summarize-ref-db-bench.mjs <output-dir> <profile>");
  process.exit(2);
}

const engines = ["vicia", "grafeo", "redb", "fjall", "turso", "cozo"];
const receipts = engines.map((engine) =>
  JSON.parse(readFileSync(join(outputDir, `${engine}.json`), "utf8")),
);
const facts = receipts[0].facts;
const expectedChecksum = facts * (facts - 1) / 2;

for (const receipt of receipts) {
  if (
    receipt.schema !== "vicia.ref-db-bench.v2" ||
    receipt.facts !== facts ||
    receipt.count !== facts ||
    receipt.checksum !== expectedChecksum ||
    !Array.isArray(receipt.memory?.aggregateSamplesMs) ||
    receipt.memory.aggregateSamplesMs.length !== (profile === "full" ? 20 : 5) ||
    receipt.memory.count !== facts ||
    receipt.memory.checksum !== expectedChecksum
  ) {
    throw new Error(`${receipt.engine}: invalid or incorrect receipt`);
  }
}

const refRoot = process.env.DB_REF_DIR ?? join(process.env.HOME, "db-ref");
const sourceCommits = Object.fromEntries(
  engines.filter((engine) => engine !== "vicia").map((engine) => [
    engine,
    execFileSync("git", ["-C", join(refRoot, engine), "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  ]),
);
sourceCommits.vicia = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();

const rows = receipts.map((receipt) => {
  const samples = [...receipt.memory.aggregateSamplesMs].sort((a, b) => a - b);
  return {
    engine: receipt.engine,
    role: receipt.role,
    boundary: receipt.executionBoundary,
    buildMs: round(receipt.buildMs),
    pointReadMs: round(receipt.readMs),
    p50Ms: round(percentile(samples, 50)),
    p95Ms: round(percentile(samples, 95)),
    maxMs: round(samples.at(-1)),
    baselineRssMiB: toMiB(receipt.memory.openBaselineRssBytes),
    deltaRssMiB: toMiB(receipt.memory.workloadDeltaRssBytes),
    peakRssMiB: toMiB(receipt.memory.workloadPeakRssBytes),
    retainedRssMiB: toMiB(receipt.memory.retainedRssBytes),
    storageMiB: round(receipt.storageBytes / 1024 / 1024),
    count: receipt.count,
    checksum: receipt.checksum,
  };
});

const report = {
  schema: "vicia.ref-db-bench.summary.v2",
  profile,
  facts,
  repetitions: profile === "full" ? 20 : 5,
  sourceCommits,
  comparisonPolicy: {
    engineAggregate: ["vicia", "grafeo", "turso", "cozo"],
    ownedResultScan: ["redb", "fjall"],
    warning: "Engine aggregate and owned result scan are different contracts and must not be ranked in one column.",
    timing: "Aggregate samples run in a fresh child process. Database open is excluded; build runs in the parent process.",
    memory: "Baseline is VmRSS after open. Delta is VmHWM minus baseline. Retained is final VmRSS minus baseline. Values are process-wide Linux RSS.",
  },
  rows,
};
writeFileSync(join(outputDir, "summary.json"), `${JSON.stringify(report, null, 2)}\n`);

const header = ["engine", "role", "boundary", "build ms", "point read ms", "aggregate/scan p50 ms", "p95 ms", "open baseline RSS MiB", "workload delta RSS MiB", "peak RSS MiB", "retained RSS MiB", "storage MiB", "correct"];
const tableRows = rows.map((row) => [row.engine, row.role, row.boundary, row.buildMs, row.pointReadMs, row.p50Ms, row.p95Ms, row.baselineRssMiB, row.deltaRssMiB, row.peakRssMiB, row.retainedRssMiB, row.storageMiB, "yes"]);
const markdown = [
  `# Vicia reference DB comparison (${profile})`,
  "",
  `Facts: ${facts}; repetitions: ${report.repetitions}`,
  "",
  `| ${header.join(" | ")} |`,
  `| ${header.map(() => "---").join(" | ")} |`,
  ...tableRows.map((row) => `| ${row.join(" | ")} |`),
  "",
  "`engineAggregate` and `ownedResultScan` are separate contracts. redb and Fjall are storage floors, not graph/query-engine peers.",
  "Memory columns come from the fresh aggregate/scan child, so build high-water memory does not contaminate them.",
  "Every row passed the same exact count and arithmetic-checksum validation.",
  "",
].join("\n");
writeFileSync(join(outputDir, "summary.md"), markdown);
console.log(markdown);

function percentile(samples, value) {
  const rank = Math.ceil(samples.length * value / 100);
  return samples[Math.max(0, rank - 1)];
}

function round(value) {
  return Math.round(value * 1000) / 1000;
}

function toMiB(bytes) {
  return round(bytes / 1024 / 1024);
}
