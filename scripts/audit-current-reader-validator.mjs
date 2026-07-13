#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-current-reader-validator-"));

try {
  rejectMutation("shape", (receipt) => { receipt.facts += 1; });
  rejectMutation("latency", (receipt) => { receipt.reads.refsTo.p95Ms = 10.001; });
  rejectMutation("leaf-scope", (receipt) => { receipt.reads.entities.diagnostics.leafPagesVisited = 3; });
  rejectMutation("materialization", (receipt) => { receipt.reads.refsTo.diagnostics.fullLeafVecPeakEntries = 1; });
  rejectMutation("owned-key", (receipt) => { receipt.reads.refsTo.diagnostics.projectedOwnedVaetDecodes = 1; });
  rejectMutation("emission", (receipt) => { receipt.reads.entities.diagnostics.projectedEavtEmitted = 0; });
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
    ["scripts/validate-current-reader-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error(`validator accepted mutated ${name} receipt`);
}
