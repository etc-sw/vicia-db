#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-projection-page-tail-validator-"));

try {
  reject("dirty", (receipt) => { receipt.trackedClean = false; }, profile === "full");
  reject("fixture-schema", (receipt) => { receipt.fixture.schema = "wrong"; });
  reject("fixture-facts", (receipt) => { receipt.fixture.facts -= 1; });
  reject("fixture-fill", (receipt) => { receipt.fixture.fillPercent = 87; });
  reject("fixture-hash-shape", (receipt) => { receipt.fixture.sha256 = "wrong"; });
  reject("fixture-builder", (receipt) => { receipt.fixture.builderSourceCommit = "0".repeat(40); });
  reject("trial-index", (receipt) => { receipt.measurements[0].trialIndex = 2; });
  reject("probe-order", (receipt) => { receipt.measurements[0].probeOrder.reverse(); });
  reject("candidate-order", (receipt) => { receipt.measurements[0].probes[0].order.reverse(); });
  reject("identity", (receipt) => { receipt.measurements[0].image.txCount += 1; });
  reject("exactness", (receipt) => { receipt.measurements[0].probes[0].decoded.count -= 1; });
  reject("exact-gate", (receipt) => {
    receipt.probes[0].gates.exact = !receipt.probes[0].gates.exact;
    receipt.probes[0].gates.admitted = false;
    receipt.admitted = false;
  });
  reject("false-verdict", (receipt) => { receipt.admitted = !receipt.admitted; });
  reject("scope", (receipt) => { receipt.fileFormatChanged = true; });
  if (profile === "full") {
    reject("decoded-tail", (receipt) => {
      const probe = receipt.probes[0];
      probe.decodedMs.p95 = probe.decodedMs.p50 * 1.151;
    });
    reject("p50-regression", (receipt) => {
      const probe = receipt.probes[0];
      probe.decodedMs.p50 = probe.sourceMs.p50 * 1.101;
    });
    reject("p95-regression", (receipt) => {
      const probe = receipt.probes[0];
      probe.decodedMs.p95 = probe.sourceMs.p95 * 1.101;
    });
  }
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
    "scripts/validate-projection-page-tail-receipt.mjs",
    target,
    profile,
  ], { encoding: "utf8" });
  if (result.status === 0) throw new Error(`validator accepted mutated ${name} receipt`);
}
