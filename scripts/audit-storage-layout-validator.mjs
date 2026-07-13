#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-storage-layout-validator-"));

try {
  rejectMutation("summary", (receipt) => {
    receipt.candidates[0].stats.checkpointMs.p95 += 1;
  });
  rejectMutation("gate", (receipt) => {
    receipt.candidates[1].gates.passed = !receipt.candidates[1].gates.passed;
  });
  rejectMutation("sample", (receipt) => {
    receipt.candidates[2].query.pointSamplesMs.pop();
  });
  rejectMutation("order", (receipt) => {
    receipt.checkpointOrder[0].reverse();
  });
  rejectMutation("leaf-codec", (receipt) => {
    receipt.candidates[0].checkpoint.layout.aevt.prefixLeafPages += 1;
  });
  console.log(`audited ${source.schema} ${profile} validator rejection`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}

function rejectMutation(name, mutate) {
  const receipt = structuredClone(source);
  mutate(receipt);
  const target = path.join(directory, `${name}.json`);
  writeFileSync(target, `${JSON.stringify(receipt)}\n`);
  const result = spawnSync(
    process.execPath,
    ["scripts/validate-storage-layout-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) {
    throw new Error(`validator accepted mutated ${name} receipt`);
  }
}
