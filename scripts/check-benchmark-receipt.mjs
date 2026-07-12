#!/usr/bin/env node

import { readFileSync } from "node:fs";

const path = process.argv[2];
if (!path) fail("usage: check-benchmark-receipt.mjs <receipt.json>");

const receipt = JSON.parse(readFileSync(path, "utf8"));
requireEqual(receipt.schema, "vicia.benchmark.receipt.v1", "schema");
requireType(receipt.suite, "string", "suite");
requireType(receipt.passed, "boolean", "passed");
requireType(receipt.acceptanceEligible, "boolean", "acceptanceEligible");
requireType(receipt.startedAtUnixMs, "number", "startedAtUnixMs");
requireType(receipt.completedAtUnixMs, "number", "completedAtUnixMs");
requireType(receipt.totalMs, "number", "totalMs");
requireType(receipt.provenance, "object", "provenance");
requireType(receipt.provenance.testbed, "string", "provenance.testbed");
requireType(receipt.provenance.executable, "string", "provenance.executable");
requireType(receipt.measurements?.metrics, "object", "measurements.metrics");
if (!Array.isArray(receipt.correctness?.checks)) {
  fail("correctness.checks must be an array");
}
if (!Array.isArray(receipt.budgets?.checks)) {
  fail("budgets.checks must be an array");
}
if (!Array.isArray(receipt.failures)) fail("failures must be an array");

const entries = Object.entries(receipt.measurements.metrics);
if (entries.length === 0) fail("receipt must contain at least one metric");
for (const [name, metric] of entries) {
  requireType(metric.count, "number", `${name}.count`);
  requireType(metric.p50, "number", `${name}.p50`);
  requireType(metric.p95, "number", `${name}.p95`);
  requireType(metric.max, "number", `${name}.max`);
  if (
    !Number.isInteger(metric.count) ||
    metric.count < 1 ||
    !Array.isArray(metric.samples) ||
    metric.samples.length !== metric.count
  ) {
    fail(`${name}.samples length must equal ${name}.count`);
  }
  if (metric.samples.some((sample) => !Number.isFinite(sample))) {
    fail(`${name}.samples must contain only finite numbers`);
  }
  if (metric.samples.some((sample, index) => index > 0 && sample < metric.samples[index - 1])) {
    fail(`${name}.samples must be sorted in ascending order`);
  }
  requireEqual(metric.p50, nearestRank(metric.samples, 50), `${name}.p50`);
  requireEqual(metric.p95, nearestRank(metric.samples, 95), `${name}.p95`);
  requireEqual(metric.max, metric.samples.at(-1), `${name}.max`);
  if (metric.p95SampleCountEligible !== (metric.count >= 20)) {
    fail(`${name}.p95SampleCountEligible must reflect the 20-observation floor`);
  }
}

if (
  receipt.acceptanceEligible &&
  entries.some(([, metric]) => metric.p95SampleCountEligible !== true)
) {
  fail("acceptance-eligible receipts require enough observations for every p95");
}
if (receipt.acceptanceEligible && receipt.provenance.sourceDirty !== false) {
  fail("acceptance-eligible receipts require a clean source checkout");
}
if (
  receipt.acceptanceEligible &&
  (typeof receipt.provenance.sourceCommit !== "string" ||
    receipt.provenance.sourceCommit.length === 0)
) {
  fail("acceptance-eligible receipts require a source commit");
}

if (!receipt.correctness.checks.every((check) => check.passed === true)) {
  fail("receipt contains a failed correctness check");
}
if (!receipt.passed) fail("receipt did not pass");

console.log(`benchmark receipt OK: ${receipt.suite} (${entries.length} metrics)`);

function requireEqual(actual, expected, label) {
  if (actual !== expected) fail(`${label} must equal ${JSON.stringify(expected)}`);
}

function requireType(value, expected, label) {
  if (
    value === null ||
    typeof value !== expected ||
    (expected === "number" && !Number.isFinite(value))
  ) {
    fail(`${label} must be a non-null ${expected}`);
  }
}

function nearestRank(sorted, percentile) {
  const rank = Math.ceil((sorted.length * percentile) / 100);
  return sorted[Math.max(0, rank - 1)];
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
