#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-temporal-projection-validator-"));

try {
  rejectMutation("exactness", (receipt) => { receipt.probes[1].projection.count -= 1; });
  rejectMutation("boundary", (receipt) => { receipt.probes[1].validAt += 1; });
  rejectMutation("latency", (receipt) => {
    receipt.probes[0].projection.p50Ms = 151;
    receipt.probes[0].baseline.p50Ms = 200;
  });
  rejectMutation("tail", (receipt) => {
    const ratio = profile === "full" ? 1.151 : 1.501;
    receipt.probes[0].projection.p95Ms = receipt.probes[0].projection.p50Ms * ratio;
  });
  rejectMutation("rss", (receipt) => {
    receipt.projection.queryRssDeltaBytes = 2 * 1024 * 1024 + 1;
  });
  rejectMutation("size", (receipt) => {
    receipt.projection.imageRatio = profile === "full" ? 0.151 : 0.301;
  });
  rejectMutation("diversity", (receipt) => { receipt.projection.shape.distinctWindows = 2; });
  rejectMutation("temporal-bytes", (receipt) => {
    receipt.projection.shape.temporalPayloadBytes += 1;
  });
  rejectMutation("pre-floor", (receipt) => { receipt.incremental.preFloorRejected = false; });
  rejectMutation("no-write", (receipt) => {
    receipt.incremental.noWriteBoundaryTransition = false;
  });
  rejectMutation("tail-scope", (receipt) => {
    receipt.incremental.refresh.touchedEntities = 2;
  });
  rejectMutation("checkpoint", (receipt) => {
    receipt.incremental.productionCheckpointPathChanged = true;
  });
  rejectMutation("semantic", (receipt) => { receipt.semantics.refValue = false; });
  rejectMutation("rebuild", (receipt) => {
    receipt.incremental.deterministicRebuild = false;
  });
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
    ["scripts/validate-temporal-projection-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error("validator accepted mutated " + name + " receipt");
}
