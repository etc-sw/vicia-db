#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !profile) throw new Error("usage: audit-projection-routing-validator.mjs <receipt> <profile>");
const original = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const directory = fs.mkdtempSync(path.join(os.tmpdir(), "vicia-projection-routing-audit-"));
const mutations = [
  ["raw exactness", receipt => { receipt.projectedChecksum = "0"; receipt.gates.exact = false; receipt.admitted = false; }],
  ["routing evidence", receipt => { receipt.projectionDiagnostics.completedScans = 0; receipt.gates.routed = false; receipt.admitted = false; }],
  ["full image decode", receipt => { receipt.projectionDiagnostics.fullImageDecodes = 1; receipt.gates.pageBacked = false; receipt.admitted = false; }],
  ["stored gate", receipt => { receipt.gates.improvement = !receipt.gates.improvement; receipt.admitted = false; }],
];
for (const [name, mutate] of mutations) {
  const receipt = structuredClone(original);
  mutate(receipt);
  const candidate = path.join(directory, `${name.replaceAll(" ", "-")}.json`);
  fs.writeFileSync(candidate, JSON.stringify(receipt));
  const result = spawnSync("node", ["scripts/validate-projection-routing-receipt.mjs", candidate, profile]);
  if (result.status === 0) throw new Error(`validator accepted mutation: ${name}`);
}
fs.rmSync(directory, { recursive: true, force: true });
process.stdout.write("projection routing validator mutation audit passed\n");
