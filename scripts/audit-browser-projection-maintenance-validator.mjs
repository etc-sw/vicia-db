#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath] = process.argv.slice(2);
if (!receiptPath) throw new Error("usage: audit-browser-projection-maintenance-validator.mjs <receipt>");
const original = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-browser-projection-validator-"));
const mutations = [
  ["fixture", (receipt) => { receipt.import.result.fixtureBytes += 1; }],
  ["authority", (receipt) => { receipt.maintenance.result.outcome.after_pages += 1; }],
  ["elapsed", (receipt) => { receipt.maintenance.result.elapsedMs = 30_001; }],
  ["pss", (receipt) => { receipt.maintenance.pss.peakDeltaBytes = 1024 * 1024 * 1024 + 1; }],
  ["verdict", (receipt) => { receipt.admitted = false; }],
];
try {
  for (const [name, mutate] of mutations) {
    const receipt = structuredClone(original);
    mutate(receipt);
    const candidate = path.join(directory, `${name}.json`);
    writeFileSync(candidate, JSON.stringify(receipt));
    const result = spawnSync(process.execPath, [
      "scripts/validate-browser-projection-maintenance-receipt.mjs",
      candidate,
    ]);
    if (result.status === 0) throw new Error(`validator accepted ${name} mutation`);
  }
} finally {
  rmSync(directory, { recursive: true, force: true });
}
process.stdout.write(`browser projection maintenance validator audit passed: ${mutations.length} mutations\n`);
