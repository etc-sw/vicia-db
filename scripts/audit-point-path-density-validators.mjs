#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, analysisPath, profile] = process.argv.slice(2);
if (!receiptPath || !analysisPath || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
const analysis = JSON.parse(readFileSync(analysisPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-point-density-validator-"));

try {
  rejectReceipt("sample", (value) => value.candidates[0].pointSamplesMs.pop());
  rejectReceipt("diagnostic", (value) => {
    const diagnostics = value.candidates[1].diagnosticsSamples[0];
    diagnostics.internalKeyComparisons = diagnostics.internalKeysAvailable + 1;
  });
  rejectReceipt("codec", (value) => value.candidates[2].layout.eavt.rawLeafPages += 1);
  rejectAnalysis("source", (value) => value.source.sha256 = "0".repeat(64));
  rejectAnalysis("ratio", (value) => value.candidates[3].point.p50RatioToFill75 += 0.01);
  rejectAnalysis("owner", (value) => value.verdict.implementationAdmitted = !value.verdict.implementationAdmitted);
  console.log(`audited ${receipt.schema} ${profile} validator rejection`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}

function rejectReceipt(name, mutate) {
  const value = structuredClone(receipt);
  mutate(value);
  const target = path.join(directory, `receipt-${name}.json`);
  writeFileSync(target, `${JSON.stringify(value)}\n`);
  const result = spawnSync(
    process.execPath,
    ["scripts/validate-point-path-density-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error(`receipt validator accepted ${name} mutation`);
}
function rejectAnalysis(name, mutate) {
  const value = structuredClone(analysis);
  mutate(value);
  const target = path.join(directory, `analysis-${name}.json`);
  writeFileSync(target, `${JSON.stringify(value)}\n`);
  const result = spawnSync(
    process.execPath,
    ["scripts/validate-point-path-density-analysis.mjs", receiptPath, target],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error(`analysis validator accepted ${name} mutation`);
}
