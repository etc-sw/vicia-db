#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !profile) {
  throw new Error("usage: audit-projection-publication-validator.mjs <receipt> <smoke|full>");
}
const validator = path.resolve("scripts/validate-projection-publication-receipt.mjs");
const original = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const mutations = [
  ["raw exactness", (copy) => { copy.probes[0].checksum += 1; }],
  ["stored exact verdict", (copy) => { copy.exact = !copy.exact; copy.admitted = false; }],
  ["layout range", (copy) => { copy.catalogPageStart += 1; }],
  ["format version", (copy) => { copy.publishedFormatVersion = 12; copy.admitted = false; }],
  ["scope", (copy) => { copy.productionQueryRoutingChanged = true; }],
];
const directory = fs.mkdtempSync(path.join(os.tmpdir(), "vicia-projection-publication-audit-"));
try {
  for (const [name, mutate] of mutations) {
    const copy = structuredClone(original);
    mutate(copy);
    const candidate = path.join(directory, `${name.replaceAll(" ", "-")}.json`);
    fs.writeFileSync(candidate, JSON.stringify(copy));
    const result = spawnSync(process.execPath, [validator, candidate, profile], { encoding: "utf8" });
    if (result.status === 0) {
      throw new Error(`validator accepted ${name} mutation`);
    }
  }
} finally {
  fs.rmSync(directory, { recursive: true, force: true });
}
process.stdout.write(`projection publication validator audit passed: ${mutations.length} mutations\n`);
