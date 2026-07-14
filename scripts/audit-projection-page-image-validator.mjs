#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const source = JSON.parse(readFileSync(receiptPath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-projection-page-image-validator-"));

try {
  rejectMutation("identity", (receipt) => { receipt.identity.baseGeneration = 0; });
  rejectMutation("size", (receipt) => { receipt.image.imageRatio = profile === "full" ? 0.151 : 0.301; });
  rejectMutation("alignment", (receipt) => { receipt.image.paddedBytes += 1; });
  rejectMutation("encode", (receipt) => { receipt.encode.p50Ms = 438.064; });
  rejectMutation("decode-tail", (receipt) => { receipt.decode.p95Ms = receipt.decode.p50Ms * (profile === "full" ? 1.251 : 2.001); });
  rejectMutation("peak-rss", (receipt) => { receipt.maintenancePeakRssDeltaBytes = 128 * 1024 * 1024 + 1; });
  rejectMutation("query-rss", (receipt) => { receipt.queryRssDeltaBytes = 2 * 1024 * 1024 + 1; });
  rejectMutation("exactness", (receipt) => { receipt.probes[1].projection.count -= 1; });
  rejectMutation("query-tail", (receipt) => { receipt.probes[0].projection.p95Ms = receipt.probes[0].projection.p50Ms * (profile === "full" ? 1.151 : 1.501); });
  rejectMutation("determinism", (receipt) => { receipt.proof.deterministicRebuild = false; });
  rejectMutation("overlay", (receipt) => { receipt.proof.overlayFlatten = false; });
  rejectMutation("scope", (receipt) => { receipt.proof.fileFormatChanged = true; });
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
    ["scripts/validate-projection-page-image-receipt.mjs", target, profile],
    { encoding: "utf8" },
  );
  if (result.status === 0) throw new Error("validator accepted mutated " + name + " receipt");
}
