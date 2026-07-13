#!/usr/bin/env node

import { readFileSync } from "node:fs";

const path = process.argv[2];
if (!path) fail("usage: validate-vetch-ledger-caller-browser-receipt.mjs <receipt.json>");
const receipt = JSON.parse(readFileSync(path, "utf8"));
equal(receipt.schema, "vicia.vetch-ledger-caller-browser-receipt.v1", "schema");
equal(receipt.passed, true, "passed");
if (!/^[0-9a-f]{40}$/.test(receipt.provenance?.sourceCommit ?? "")) {
  fail("provenance.sourceCommit must be a Git SHA-1");
}
if (typeof receipt.provenance?.sourceDirty !== "boolean") {
  fail("provenance.sourceDirty must be boolean");
}
if (!/^[0-9a-f]{64}$/.test(receipt.provenance?.wasmSha256 ?? "")) {
  fail("provenance.wasmSha256 must be a SHA-256 digest");
}
if (!Number.isInteger(receipt.samples) || receipt.samples < 20) {
  fail("receipt must contain at least 20 samples per scenario");
}
const result = receipt.evidence?.measured?.result;
equal(result?.schema, "vicia.vetch-ledger-caller-browser.v1", "evidence schema");
equal(result?.fixtureSchema, "vicia.vetch-ledger-caller-fixture.v1", "fixture schema");
if (!Array.isArray(result?.scenarios) || result.scenarios.length !== 4) {
  fail("receipt must contain four browser scenarios");
}
const fields = [
  "callerEncodingMs",
  "preparationMs",
  "mutationMs",
  "publicationMs",
  "executeAtomicMs",
  "resultDecodeMs",
  "proofReadMs",
];
for (const scenario of result.scenarios) {
  for (const field of fields) {
    const samples = scenario.measurements?.[field];
    if (!Array.isArray(samples) || samples.length !== receipt.samples) {
      fail(`${scenario.id}.${field} sample count mismatch`);
    }
    if (samples.some((sample) => !Number.isFinite(sample) || sample < 0)) {
      fail(`${scenario.id}.${field} contains an invalid sample`);
    }
  }
  if (
    scenario.heapDeltaBytes !== null &&
    !Number.isFinite(scenario.heapDeltaBytes)
  ) {
    fail(`${scenario.id}.heapDeltaBytes is invalid`);
  }
}
if (!Number.isFinite(receipt.evidence?.measured?.pss?.peakDeltaBytes)) {
  fail("browser PSS evidence is missing");
}
console.log(`Vetch ledger caller browser receipt OK: ${result.scenarios.length} scenarios x ${receipt.samples} samples`);

function equal(actual, expected, label) {
  if (actual !== expected) fail(`${label} must equal ${JSON.stringify(expected)}`);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
