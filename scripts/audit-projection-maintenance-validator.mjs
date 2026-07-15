#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !profile) throw new Error("usage: audit-projection-maintenance-validator.mjs <receipt> <profile>");
const original = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-projection-maintenance-validator-"));
const mutations = [
  ["fixture", (receipt) => { receipt.fixture.fillPercent = 87; }],
  ["exact", (receipt) => { receipt.aggregateChecksum = "0"; }],
  ["elapsed", (receipt) => { receipt.elapsedMs = 1500.001; }],
  ["image", (receipt) => { receipt.projectionBytes = Math.floor(receipt.sourceBytes * 0.15) + 1; }],
  ["verdict", (receipt) => { receipt.admitted = !receipt.admitted; }],
];
try {
  for (const [name, mutate] of mutations) {
    const receipt = structuredClone(original);
    mutate(receipt);
    const candidate = path.join(directory, `${name}.json`);
    writeFileSync(candidate, JSON.stringify(receipt));
    const result = spawnSync(process.execPath, [
      "scripts/validate-projection-maintenance-receipt.mjs",
      candidate,
      profile,
    ]);
    if (result.status === 0) throw new Error(`validator accepted ${name} mutation`);
  }
} finally {
  rmSync(directory, { recursive: true, force: true });
}
process.stdout.write(`projection maintenance validator audit passed: ${mutations.length} mutations\n`);
