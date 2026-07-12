#!/usr/bin/env node

import { readFileSync } from "node:fs";

const receiptPath = process.argv[2];
const catalogPath = process.argv[3] ?? "benchmarks/milestones.json";
if (!receiptPath) {
  fail("usage: check-benchmark-receipt.mjs <receipt.json> [milestones.json]");
}

const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
const catalog = JSON.parse(readFileSync(catalogPath, "utf8"));
requireEqual(receipt.schema, "vicia.benchmark.receipt.v1", "schema");
requireEqual(catalog.schema, "vicia.benchmark.milestones.v1", "catalog schema");
requireNonEmptyString(receipt.suite, "suite");
requireType(receipt.passed, "boolean", "passed");
requireType(receipt.acceptanceEligible, "boolean", "acceptanceEligible");
requireFiniteNumber(receipt.startedAtUnixMs, "startedAtUnixMs");
requireFiniteNumber(receipt.completedAtUnixMs, "completedAtUnixMs");
requireFiniteNumber(receipt.totalMs, "totalMs");
if (receipt.completedAtUnixMs < receipt.startedAtUnixMs) {
  fail("completedAtUnixMs must not precede startedAtUnixMs");
}
if (
  Math.abs(
    receipt.totalMs - (receipt.completedAtUnixMs - receipt.startedAtUnixMs),
  ) > 2
) {
  fail("totalMs must agree with receipt timestamps within rounding tolerance");
}

const suite = catalog.suites.find((candidate) => candidate.id === receipt.suite);
if (!suite) fail(`suite ${receipt.suite} is absent from milestone catalog`);
requireType(receipt.milestone, "object", "milestone");
const profile = suite.profiles.find(
  (candidate) => candidate.id === receipt.milestone.profile,
);
if (!profile) {
  fail(`profile ${receipt.suite}/${receipt.milestone.profile} is absent from catalog`);
}
requireEqual(receipt.milestone.id, suite.milestone, "milestone.id");
requireEqual(receipt.milestone.decision, suite.decision, "milestone.decision");
requireEqual(receipt.milestone.kind, suite.kind, "milestone.kind");
requireEqual(receipt.milestone.owner, suite.owner, "milestone.owner");
requireEqual(receipt.milestone.tier, profile.tier, "milestone.tier");

requireType(receipt.provenance, "object", "provenance");
requireNonEmptyString(receipt.provenance.testbed, "provenance.testbed");
requireNonEmptyString(receipt.provenance.executable, "provenance.executable");
requireNonEmptyString(receipt.provenance.os, "provenance.os");
requireNonEmptyString(receipt.provenance.arch, "provenance.arch");
requireEqual(
  receipt.provenance.executableDigest?.algorithm,
  "sha256",
  "provenance.executableDigest.algorithm",
);
if (!/^[0-9a-f]{64}$/.test(receipt.provenance.executableDigest?.value ?? "")) {
  fail("provenance.executableDigest.value must be a lowercase SHA-256 digest");
}
requireType(receipt.provenance.host, "object", "provenance.host");
if (
  !Number.isInteger(receipt.provenance.host.logicalCpus) ||
  receipt.provenance.host.logicalCpus < 1
) {
  fail("provenance.host.logicalCpus must be a positive integer");
}
if (
  receipt.provenance.host.memoryBytes !== null &&
  (!Number.isInteger(receipt.provenance.host.memoryBytes) ||
    receipt.provenance.host.memoryBytes < 1)
) {
  fail("provenance.host.memoryBytes must be null or a positive integer");
}
requireNonEmptyString(receipt.provenance.host.rustc, "provenance.host.rustc");
requireNonEmptyString(receipt.provenance.host.cargo, "provenance.host.cargo");
requireType(receipt.configuration, "object", "configuration");
if (!["generated", "provided"].includes(receipt.configuration.fixtureOrigin)) {
  fail("configuration.fixtureOrigin must be generated or provided");
}
if (
  receipt.configuration.fixtureOrigin === "provided" &&
  (typeof receipt.configuration.fixtureSource !== "string" ||
    receipt.configuration.fixtureSource.length === 0)
) {
  fail("provided fixtures require configuration.fixtureSource");
}
requireType(receipt.measurements?.metrics, "object", "measurements.metrics");
requireType(receipt.measurements?.files, "object", "measurements.files");
if (
  !/^[0-9a-f]{64}$/.test(
    receipt.measurements.files.baseFixtureSha256 ?? "",
  )
) {
  fail("measurements.files.baseFixtureSha256 must be a lowercase SHA-256 digest");
}
if (!Array.isArray(receipt.correctness?.checks) || receipt.correctness.checks.length === 0) {
  fail("correctness.checks must be a non-empty array");
}
if (!Array.isArray(receipt.budgets?.limits)) {
  fail("budgets.limits must be an array");
}
if (!Array.isArray(receipt.budgets?.checks)) {
  fail("budgets.checks must be an array");
}
if (!Array.isArray(receipt.failures)) fail("failures must be an array");
requireEqual(receipt.budgets.profile, profile.id, "budgets.profile");
requireDeepEqual(receipt.budgets.limits, profile.budgets, "budgets.limits");

const metrics = receipt.measurements.metrics;
const entries = Object.entries(metrics);
if (entries.length === 0) fail("receipt must contain at least one metric");
const p95Minimum = catalog.methodology.p95MinimumObservations;
for (const [name, metric] of entries) {
  requireNonEmptyString(metric.unit, `${name}.unit`);
  if (
    !Number.isInteger(metric.count) ||
    metric.count < 1 ||
    !Array.isArray(metric.samples) ||
    metric.samples.length !== metric.count
  ) {
    fail(`${name}.samples length must equal its positive integer count`);
  }
  if (metric.samples.some((sample) => !Number.isFinite(sample))) {
    fail(`${name}.samples must contain only finite numbers`);
  }
  if (
    metric.samples.some(
      (sample, index) => index > 0 && sample < metric.samples[index - 1],
    )
  ) {
    fail(`${name}.samples must be sorted in ascending order`);
  }
  const mean = metric.samples.reduce((sum, sample) => sum + sample, 0) / metric.count;
  const variance =
    metric.samples.reduce((sum, sample) => sum + (sample - mean) ** 2, 0) /
    metric.count;
  const stdDev = Math.sqrt(variance);
  const p50 = nearestRank(metric.samples, 50);
  const deviations = metric.samples
    .map((sample) => Math.abs(sample - p50))
    .sort((left, right) => left - right);
  requireEqual(metric.min, metric.samples[0], `${name}.min`);
  requireEqual(metric.p25, nearestRank(metric.samples, 25), `${name}.p25`);
  requireEqual(metric.p50, p50, `${name}.p50`);
  requireEqual(metric.p75, nearestRank(metric.samples, 75), `${name}.p75`);
  requireEqual(metric.p95, nearestRank(metric.samples, 95), `${name}.p95`);
  requireEqual(metric.p99, nearestRank(metric.samples, 99), `${name}.p99`);
  requireEqual(metric.max, metric.samples.at(-1), `${name}.max`);
  requireEqual(metric.mean, round3(mean), `${name}.mean`);
  requireEqual(metric.stdDev, round3(stdDev), `${name}.stdDev`);
  requireEqual(metric.mad, round3(nearestRank(deviations, 50)), `${name}.mad`);
  requireEqual(
    metric.cv,
    round3(mean === 0 ? 0 : stdDev / mean),
    `${name}.cv`,
  );
  requireEqual(
    metric.p95SampleCountEligible,
    metric.count >= p95Minimum,
    `${name}.p95SampleCountEligible`,
  );
}

const expectedBudgetChecks = [];
for (const budget of profile.budgets) {
  for (const metricName of budget.metrics) {
    const metric = metrics[metricName];
    if (!metric) fail(`catalog budget references missing metric ${metricName}`);
    requireEqual(metric.unit, budget.unit, `${metricName}.unit`);
    const actual = metric[budget.statistic];
    requireFiniteNumber(actual, `${metricName}.${budget.statistic}`);
    expectedBudgetChecks.push({
      name: budget.name,
      metric: metricName,
      statistic: budget.statistic,
      actual,
      limit: budget.limit,
      unit: budget.unit,
      comparator: budget.comparator,
      passed: actual <= budget.limit,
    });
  }
}
requireDeepEqual(receipt.budgets.checks, expectedBudgetChecks, "budgets.checks");

const expectedCorrectnessFailures = [];
for (const check of receipt.correctness.checks) {
  requireNonEmptyString(check.name, "correctness check name");
  const expectedPassed = deepEqual(check.expected, check.actual);
  requireEqual(check.passed, expectedPassed, `${check.name}.passed`);
  if (!expectedPassed) expectedCorrectnessFailures.push(check.name);
}
const expectedBudgetFailures = expectedBudgetChecks
  .filter((check) => !check.passed)
  .map((check) => `${check.name}: ${check.metric}`);
const expectedFailures = [...expectedCorrectnessFailures, ...expectedBudgetFailures];
requireDeepEqual(receipt.failures, expectedFailures, "failures");

const expectedPassed = expectedFailures.length === 0;
requireEqual(receipt.passed, expectedPassed, "passed");
const p95EvidenceEligible = profile.budgets
  .filter((budget) => budget.statistic === "p95")
  .flatMap((budget) => budget.metrics)
  .every((metricName) => metrics[metricName]?.p95SampleCountEligible === true);
const hasSourceCommit =
  typeof receipt.provenance.sourceCommit === "string" &&
  receipt.provenance.sourceCommit.length > 0;
const expectedAcceptanceEligible =
  profile.acceptanceEligible &&
  receipt.provenance.sourceDirty === false &&
  hasSourceCommit &&
  p95EvidenceEligible &&
  expectedPassed;
requireEqual(
  receipt.acceptanceEligible,
  expectedAcceptanceEligible,
  "acceptanceEligible",
);

if (!receipt.passed) fail(`benchmark receipt failed: ${receipt.failures.join(", ")}`);
console.log(
  `benchmark receipt OK: ${receipt.milestone.id}/${receipt.milestone.profile} (${entries.length} metrics, ${expectedBudgetChecks.length} budget checks)`,
);

function nearestRank(sorted, percentile) {
  const rank = Math.ceil((sorted.length * percentile) / 100);
  return sorted[Math.max(0, rank - 1)];
}

function round3(value) {
  return Math.round(value * 1_000) / 1_000;
}

function requireNonEmptyString(value, label) {
  requireType(value, "string", label);
  if (value.length === 0) fail(`${label} must not be empty`);
}

function requireFiniteNumber(value, label) {
  if (!Number.isFinite(value)) fail(`${label} must be a finite number`);
}

function requireEqual(actual, expected, label) {
  if (!Object.is(actual, expected)) {
    fail(`${label} must equal ${JSON.stringify(expected)}`);
  }
}

function requireDeepEqual(actual, expected, label) {
  if (!deepEqual(actual, expected)) {
    fail(`${label} does not match the milestone catalog or measured evidence`);
  }
}

function deepEqual(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function requireType(value, expected, label) {
  if (value === null || typeof value !== expected) {
    fail(`${label} must be a non-null ${expected}`);
  }
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
