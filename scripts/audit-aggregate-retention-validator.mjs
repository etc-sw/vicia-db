#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-aggregate-retention-validator-"));

try {
  rejectMutation("shape", (receipt) => { receipt.facts += 1; });
  rejectMutation("correctness", (receipt) => { receipt.pairs[0].twenty.checksum = 0; });
  rejectMutation("ordering", (receipt) => { receipt.pairs[0].order.reverse(); });
  rejectMutation("transaction", (receipt) => { receipt.pairs[0].one.txCountAfter += 1; });
  rejectMutation("file", (receipt) => { receipt.pairs[0].one.graphBytesAfter += 4096; });
  rejectMutation("wal", (receipt) => { receipt.pairs[0].twenty.walExistsAfter = true; });
  rejectMutation("arithmetic", (receipt) => { receipt.pairs[0].one.retainedDeltaAfterLiveTrimBytes += 1; });
  rejectMutation("diagnostics", (receipt) => { receipt.pairs[0].twenty.samples[0].cursorDiagnostics.reducerEntries = 0; });
  rejectMutation("trend", (receipt) => {
    const samples = receipt.pairs[0].twenty.samples;
    for (const sample of samples.slice(-5)) sample.rssBytes = samples[0].rssBytes + 3 * 1024 * 1024;
  });
  rejectMutation("retained", (receipt) => {
    const measurement = receipt.pairs[0].twenty;
    measurement.rssAfterLiveTrimBytes = measurement.baselineRssBytes + 17 * 1024 * 1024;
    measurement.retainedDeltaAfterLiveTrimBytes = 17 * 1024 * 1024;
    measurement.liveDatabaseRssBytes = Math.max(0, measurement.rssAfterLiveTrimBytes - measurement.rssAfterDropTrimBytes);
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
    ["scripts/validate-aggregate-retention-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error(`validator accepted mutated ${name} receipt`);
}
