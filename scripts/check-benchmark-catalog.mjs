#!/usr/bin/env node

import { readFileSync } from "node:fs";

const path = process.argv[2] ?? "benchmarks/milestones.json";
const catalog = JSON.parse(readFileSync(path, "utf8"));

requireEqual(catalog.schema, "vicia.benchmark.milestones.v1", "schema");
requireType(catalog.methodology, "object", "methodology");
if (
  !Number.isInteger(catalog.methodology.p95MinimumObservations) ||
  catalog.methodology.p95MinimumObservations < 20
) {
  fail("methodology.p95MinimumObservations must be an integer of at least 20");
}
requireNonEmptyString(catalog.methodology.percentile, "methodology.percentile");
requireNonEmptyString(
  catalog.methodology.comparisonPolicy,
  "methodology.comparisonPolicy",
);
requireNonEmptyString(
  catalog.methodology.promotionPolicy,
  "methodology.promotionPolicy",
);
if (!Array.isArray(catalog.suites) || catalog.suites.length === 0) {
  fail("suites must be a non-empty array");
}

const suiteIds = new Set();
const milestoneIds = new Set();
for (const suite of catalog.suites) {
  requireNonEmptyString(suite.id, "suite.id");
  requireNonEmptyString(suite.milestone, `${suite.id}.milestone`);
  requireNonEmptyString(suite.kind, `${suite.id}.kind`);
  requireNonEmptyString(suite.owner, `${suite.id}.owner`);
  requireNonEmptyString(suite.decision, `${suite.id}.decision`);
  if (suiteIds.has(suite.id)) fail(`duplicate suite id ${suite.id}`);
  if (milestoneIds.has(suite.milestone)) {
    fail(`duplicate milestone id ${suite.milestone}`);
  }
  suiteIds.add(suite.id);
  milestoneIds.add(suite.milestone);
  if (!Array.isArray(suite.profiles) || suite.profiles.length === 0) {
    fail(`${suite.id}.profiles must be a non-empty array`);
  }

  const profileIds = new Set();
  for (const profile of suite.profiles) {
    requireNonEmptyString(profile.id, `${suite.id}.profile.id`);
    requireNonEmptyString(profile.tier, `${suite.id}/${profile.id}.tier`);
    requireNonEmptyString(profile.command, `${suite.id}/${profile.id}.command`);
    requireType(
      profile.acceptanceEligible,
      "boolean",
      `${suite.id}/${profile.id}.acceptanceEligible`,
    );
    if (profileIds.has(profile.id)) {
      fail(`duplicate profile id ${suite.id}/${profile.id}`);
    }
    profileIds.add(profile.id);
    if (!Array.isArray(profile.budgets)) {
      fail(`${suite.id}/${profile.id}.budgets must be an array`);
    }
    if (profile.acceptanceEligible && profile.budgets.length === 0) {
      fail(`${suite.id}/${profile.id} is acceptance eligible without budgets`);
    }

    const budgetMetricKeys = new Set();
    for (const budget of profile.budgets) {
      requireNonEmptyString(budget.name, `${suite.id}/${profile.id}.budget.name`);
      if (!Array.isArray(budget.metrics) || budget.metrics.length === 0) {
        fail(`${suite.id}/${profile.id}/${budget.name}.metrics must be non-empty`);
      }
      if (!["p50", "p95", "max"].includes(budget.statistic)) {
        fail(`${suite.id}/${profile.id}/${budget.name} has invalid statistic`);
      }
      requireEqual(
        budget.comparator,
        "<=",
        `${suite.id}/${profile.id}/${budget.name}.comparator`,
      );
      if (!Number.isFinite(budget.limit) || budget.limit <= 0) {
        fail(`${suite.id}/${profile.id}/${budget.name}.limit must be positive`);
      }
      requireNonEmptyString(
        budget.unit,
        `${suite.id}/${profile.id}/${budget.name}.unit`,
      );
      for (const metric of budget.metrics) {
        requireNonEmptyString(metric, `${suite.id}/${profile.id}.budget.metric`);
        const key = `${metric}\u0000${budget.statistic}`;
        if (budgetMetricKeys.has(key)) {
          fail(
            `${suite.id}/${profile.id} budgets duplicate ${metric} ${budget.statistic}`,
          );
        }
        budgetMetricKeys.add(key);
      }
    }
  }
}

const requiredProfiles = {
  criterion: ["nightly"],
  "vetch-cadence": ["smoke", "full"],
  "delta-accumulation": ["smoke", "t8b-mini", "full"],
  "agent-brief-read-path": ["smoke", "full"],
  "browser-paged-matrix": ["full"],
  "vetch-ledger-caller": ["smoke", "full"],
  "vetch-gate-d-exact-trace": ["full"],
};
for (const [suiteId, profileIds] of Object.entries(requiredProfiles)) {
  const suite = catalog.suites.find((candidate) => candidate.id === suiteId);
  if (!suite) fail(`required milestone suite ${suiteId} is missing`);
  for (const profileId of profileIds) {
    if (!suite.profiles.some((profile) => profile.id === profileId)) {
      fail(`required milestone profile ${suiteId}/${profileId} is missing`);
    }
  }
}

for (const suiteId of [
  "vetch-cadence",
  "delta-accumulation",
  "agent-brief-read-path",
  "vetch-ledger-caller",
]) {
  const suite = catalog.suites.find((candidate) => candidate.id === suiteId);
  for (const profile of suite.profiles) {
    if (
      !profile.command.includes("VICIA_BENCH_BASE_FIXTURE") ||
      !profile.command.includes("VICIA_BENCH_RECEIPT")
    ) {
      fail(`${suiteId}/${profile.id} must pin a base fixture and receipt path`);
    }
  }
}

console.log(
  `benchmark catalog OK: ${catalog.suites.length} milestones, ${catalog.suites.reduce((sum, suite) => sum + suite.profiles.length, 0)} profiles`,
);

function requireNonEmptyString(value, label) {
  requireType(value, "string", label);
  if (value.trim().length === 0) fail(`${label} must not be empty`);
}

function requireEqual(actual, expected, label) {
  if (actual !== expected) fail(`${label} must equal ${JSON.stringify(expected)}`);
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
