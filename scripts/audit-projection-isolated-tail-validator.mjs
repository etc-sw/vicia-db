#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-projection-isolated-tail-validator-"));

try {
  reject("dirty", (receipt) => { receipt.trackedClean = false; }, profile === "full");
  reject("fixture-fill", (receipt) => { receipt.fixture.fillPercent = 87; });
  reject("fixture-builder", (receipt) => { receipt.fixture.builderSourceCommit = "0".repeat(40); });
  reject("measurement-count", (receipt) => { receipt.measurements.pop(); });
  reject("launch-index", (receipt) => { receipt.measurements[0].launchIndex = 2; });
  reject("trial-index", (receipt) => { receipt.measurements[0].trialIndex = 2; });
  reject("candidate", (receipt) => { receipt.measurements[0].candidate = "decoded"; });
  reject("probe", (receipt) => { receipt.measurements[0].probe = "atBoundary"; });
  reject("identity", (receipt) => { receipt.measurements[0].image.txCount += 1; });
  reject("exactness", (receipt) => { receipt.measurements[0].aggregate.count -= 1; });
  reject("summary", (receipt) => { receipt.probes[0].decodedMs.p95 *= 1.01; });
  reject("exact-gate", (receipt) => {
    receipt.probes[0].gates.exact = false;
    receipt.probes[0].gates.admitted = false;
    receipt.admitted = false;
  });
  reject("false-verdict", (receipt) => { receipt.admitted = !receipt.admitted; });
  reject("scope", (receipt) => { receipt.productionQueryRoutingChanged = true; });
  console.log(`audited ${source.schema} ${profile} validator rejection`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}

function reject(name, mutate, enabled = true) {
  if (!enabled) return;
  const receipt = structuredClone(source);
  mutate(receipt);
  const target = path.join(directory, `${name}.json`);
  writeFileSync(target, JSON.stringify(receipt) + "\n");
  const result = spawnSync(process.execPath, [
    "scripts/validate-projection-isolated-tail-receipt.mjs",
    target,
    profile,
  ], { encoding: "utf8" });
  if (result.status === 0) throw new Error(`validator accepted mutated ${name} receipt`);
}
