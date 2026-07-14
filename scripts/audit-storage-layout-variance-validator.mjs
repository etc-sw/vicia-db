#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, reportPath] = process.argv.slice(2);
if (!receiptPath || !reportPath) process.exit(2);
const source = JSON.parse(readFileSync(reportPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-storage-layout-variance-validator-"));

try {
  rejectMutation("source-sha", (report) => {
    report.source.sha256 = "0".repeat(64);
  });
  rejectMutation("sample-index", (report) => {
    report.candidates[1].checkpoint.p95.sampleIndex += 1;
  });
  rejectMutation("phase-delta", (report) => {
    report.candidates[2].checkpoint.max.groups.sync.positiveDeltaMicros += 1;
  });
  rejectMutation("point-ratio", (report) => {
    report.candidates[3].point.p50RatioToFill75 += 0.01;
  });
  rejectMutation("verdict", (report) => {
    report.verdict.checkpoint.implementationAdmitted = !report.verdict.checkpoint.implementationAdmitted;
  });
  rejectMutation("production-fill", (report) => {
    report.verdict.rollout.productionFillPercent += 1;
  });
  console.log(`audited ${source.schema} validator rejection`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}

function rejectMutation(name, mutate) {
  const report = structuredClone(source);
  mutate(report);
  const target = path.join(directory, `${name}.json`);
  writeFileSync(target, `${JSON.stringify(report)}\n`);
  const result = spawnSync(
    process.execPath,
    ["scripts/validate-storage-layout-variance.mjs", receiptPath, target],
    { encoding: "utf8" },
  );
  if (result.status === 0) {
    throw new Error(`validator accepted mutated ${name} report`);
  }
}
