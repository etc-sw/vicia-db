#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-current-projection-validator-"));

try {
  rejectMutation("exactness", (receipt) => { receipt.projection.aggregate.count -= 1; });
  rejectMutation("latency", (receipt) => {
    receipt.projection.aggregate.p50Ms = 151;
    receipt.baseline.p50Ms = 200;
  });
  rejectMutation("tail", (receipt) => {
    receipt.projection.aggregate.p95Ms = receipt.projection.aggregate.p50Ms * 1.151;
  });
  rejectMutation("rss", (receipt) => { receipt.projection.queryRssDeltaBytes = 2 * 1024 * 1024 + 1; });
  rejectMutation("size", (receipt) => {
    receipt.projection.imageRatio = profile === "full" ? 0.151 : 0.201;
  });
  rejectMutation("stale", (receipt) => { receipt.incremental.staleReadRejected = false; });
  rejectMutation("tail-scope", (receipt) => { receipt.incremental.refresh.touchedEntities = 2; });
  rejectMutation("checkpoint", (receipt) => { receipt.incremental.productionCheckpointPathChanged = true; });
  rejectMutation("semantic", (receipt) => { receipt.semantics.refValue = false; });
  rejectMutation("rebuild", (receipt) => { receipt.incremental.deterministicRebuild = false; });
  console.log("audited " + source.schema + " " + profile + " validator rejection");
} finally {
  rmSync(directory, { recursive: true, force: true });
}

function rejectMutation(name, mutate) {
  const receipt = structuredClone(source);
  mutate(receipt);
  const target = path.join(directory, name + ".json");
  writeFileSync(target, JSON.stringify(receipt) + "\n");
  const result = spawnSync(
    process.execPath,
    ["scripts/validate-current-projection-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error("validator accepted mutated " + name + " receipt");
}
