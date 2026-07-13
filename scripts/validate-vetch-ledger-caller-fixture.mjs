#!/usr/bin/env node

import { readFileSync } from "node:fs";

const path = process.argv[2] ?? "benchmarks/fixtures/vetch-ledger-caller.v1.json";
const fixture = JSON.parse(readFileSync(path, "utf8"));
const requiredScenarios = new Set([
  "cards.move",
  "condense.admit",
  "proposal.verdict",
  "agent.brief",
]);
const valueTypes = new Set(["string", "integer", "boolean", "ref", "keyword", "null"]);

equal(fixture.schema, "vicia.vetch-ledger-caller-fixture.v1", "schema");
nonEmpty(fixture.source?.repository, "source.repository");
nonEmpty(fixture.source?.commit, "source.commit");
if (!Array.isArray(fixture.source?.paths) || fixture.source.paths.length === 0) {
  fail("source.paths must be non-empty");
}
if (!Array.isArray(fixture.scenarios) || fixture.scenarios.length !== requiredScenarios.size) {
  fail("fixture must contain exactly four scenarios");
}

for (const scenario of fixture.scenarios) {
  if (!requiredScenarios.delete(scenario.id)) fail(`unexpected or duplicate scenario ${scenario.id}`);
  if (!Array.isArray(scenario.changes) || scenario.changes.length === 0) {
    fail(`${scenario.id}.changes must be non-empty`);
  }
  const identities = new Set();
  for (const [index, change] of scenario.changes.entries()) {
    if (!["assert", "retract"].includes(change.operation)) {
      fail(`${scenario.id}.changes[${index}].operation is invalid`);
    }
    uuid(change.entity, `${scenario.id}.changes[${index}].entity`);
    if (typeof change.attribute !== "string" || !change.attribute.startsWith(":")) {
      fail(`${scenario.id}.changes[${index}].attribute must be a keyword`);
    }
    if (!valueTypes.has(change.value?.type)) {
      fail(`${scenario.id}.changes[${index}].value.type is invalid`);
    }
    if (change.value.type === "ref") uuid(change.value.value, `${scenario.id}.changes[${index}].value`);
    const identity = JSON.stringify([change.operation, change.entity, change.attribute, change.value]);
    if (identities.has(identity)) fail(`${scenario.id} repeats one typed fact change`);
    identities.add(identity);
  }
  uuid(scenario.proof?.entity, `${scenario.id}.proof.entity`);
  if (!Number.isInteger(scenario.proof?.expectedRows) || scenario.proof.expectedRows < 1) {
    fail(`${scenario.id}.proof.expectedRows must be positive`);
  }
}
if (requiredScenarios.size > 0) fail(`missing scenarios: ${[...requiredScenarios].join(", ")}`);

console.log(`Vetch ledger caller fixture OK: ${fixture.scenarios.length} scenarios`);

function uuid(value, label) {
  if (typeof value !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)) {
    fail(`${label} must be a UUID`);
  }
}

function nonEmpty(value, label) {
  if (typeof value !== "string" || value.length === 0) fail(`${label} must be non-empty`);
}

function equal(actual, expected, label) {
  if (actual !== expected) fail(`${label} must equal ${JSON.stringify(expected)}`);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
