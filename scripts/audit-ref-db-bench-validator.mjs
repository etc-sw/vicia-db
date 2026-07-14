#!/usr/bin/env node

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { validateReceipts } from "./validate-ref-db-bench.mjs";

const [outputDir, profile] = process.argv.slice(2);
if (!outputDir || !profile) {
  console.error("usage: audit-ref-db-bench-validator.mjs <output-dir> <smoke|full>");
  process.exit(2);
}

const receipts = readdirSync(outputDir)
  .filter((name) => /^(vicia|grafeo|sqlite|redb|fjall|turso|cozo)-trial-\d+\.json$/.test(name))
  .map((name) => JSON.parse(readFileSync(join(outputDir, name), "utf8")));
validateReceipts(receipts, profile);

const mutations = [
  ["checksum", (rows) => { rows[0].query.checksum += 1; }],
  ["sample deletion", (rows) => { rows[0].query.aggregateSamplesMs.pop(); }],
  ["SQLite durability", (rows) => { find(rows, "sqlite").durability.synchronous = "normal"; }],
  ["boundary", (rows) => { find(rows, "sqlite").executionBoundary = "ownedResultScan"; }],
  ["order", (rows) => { rows[0].orderPosition = 99; }],
  ["reopen", (rows) => { rows[0].reopenVerified = false; }],
  ["timing", (rows) => { rows[0].query.openMs = Number.NaN; }],
  ["runtime version", (rows) => { find(rows, "sqlite").runtimeVersion = null; }],
];

for (const [name, mutate] of mutations) {
  const candidate = structuredClone(receipts);
  mutate(candidate);
  let rejected = false;
  try {
    validateReceipts(candidate, profile);
  } catch {
    rejected = true;
  }
  if (!rejected) throw new Error(`validator accepted mutation: ${name}`);
}
console.log(`ref-db validator mutation audit passed (${mutations.length} mutations)`);

function find(rows, engine) {
  return rows.find((row) => row.engine === engine);
}
