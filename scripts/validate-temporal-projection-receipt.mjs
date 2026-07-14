#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
const facts = profile === "full" ? 1_000_000 : 10_000;
const samples = profile === "full" ? 20 : 5;
const boundary = 1_735_689_600_000;
const probes = [
  ["beforeBoundary", boundary - 1],
  ["atBoundary", boundary],
  ["afterBoundary", boundary + 2],
];

if (receipt.schema !== "vicia.temporal-current-projection.v1") fail("schema");
if (receipt.facts !== facts || receipt.samples !== samples) fail("workload shape");
if (receipt.validTimeFloor !== boundary - 1) fail("valid-time floor");
if (!Number.isSafeInteger(receipt.graphBytes) || receipt.graphBytes <= 0) fail("graph bytes");
if (!/^[0-9a-f]{40}$/.test(receipt.provenance?.sourceCommit ?? "")) fail("source commit");
if (profile === "full" && receipt.provenance?.sourceDirty !== false) {
  fail("full source must be clean");
}
for (const field of ["productionQueryRoutingChanged", "publicApiChanged", "fileFormatChanged"]) {
  if (receipt.provenance?.[field] !== false) fail(field);
}

if (!Array.isArray(receipt.probes) || receipt.probes.length !== probes.length) fail("probes");
for (let index = 0; index < probes.length; index += 1) {
  const [name, validAt] = probes[index];
  const probe = receipt.probes[index];
  if (probe?.name !== name || probe.validAt !== validAt) fail("probe identity");
  const [count, checksum] = expectedPair(facts, validAt);
  validateAggregate(name + " baseline", probe.baseline, count, checksum, samples);
  validateAggregate(name + " projection", probe.projection, count, checksum, samples);
  const improvement = (probe.baseline.p50Ms - probe.projection.p50Ms) / probe.baseline.p50Ms;
  if (!(probe.projection.p50Ms <= 150 || improvement >= 0.35)) fail(name + " latency admission");
  const tailLimit = profile === "full" ? 1.15 : 1.5;
  if (probe.projection.p95Ms > probe.projection.p50Ms * tailLimit) {
    fail(name + " projection tail");
  }
}

const projection = receipt.projection;
if (!projection || projection.rowCount !== facts) fail("projection rows");
if (!/^[0-9a-f]{16}$/.test(projection.fingerprint ?? "")) fail("fingerprint");
if (projection.queryRssDeltaBytes > 2 * 1024 * 1024) fail("query RSS");
const sizeRatioLimit = profile === "full" ? 0.15 : 0.30;
if (projection.imageRatio > sizeRatioLimit) fail("projection size ratio");
if (projection.accountedBytes > receipt.graphBytes * sizeRatioLimit) fail("projection bytes");
const expectedWindows = Math.ceil(facts / 2) + (facts >= 2 ? 1 : 0) + (facts >= 4 ? 1 : 0);
if (projection.shape?.distinctWindows !== expectedWindows) fail("temporal diversity");
if (!Number.isSafeInteger(projection.shape?.validFromPayloadBytes)
  || !Number.isSafeInteger(projection.shape?.validToPayloadBytes)
  || projection.shape.validFromPayloadBytes <= 0
  || projection.shape.validToPayloadBytes <= 0
  || projection.shape.temporalPayloadBytes
    !== projection.shape.validFromPayloadBytes + projection.shape.validToPayloadBytes) {
  fail("temporal payload");
}
if (projection.accountedBytes < projection.shape.temporalPayloadBytes) fail("temporal accounting");

const incremental = receipt.incremental;
const [afterCount, afterChecksum] = expectedPair(facts, boundary + 2);
if (!incremental?.staleReadRejected
  || !incremental.checkpointStaleReadRejected
  || !incremental.preFloorRejected
  || !incremental.noWriteBoundaryTransition) fail("temporal invalidation");
if (!incremental.deterministicRebuild) fail("incremental rebuild");
if (incremental.count !== afterCount + 1 || incremental.checksum !== afterChecksum + facts) {
  fail("incremental exactness");
}
if (incremental.refresh?.tailFactsVisited !== 1
  || incremental.refresh?.touchedEntities !== 1
  || incremental.refresh?.replacementRows !== 1) fail("tail refresh scope");
if (incremental.checkpointRefresh?.tailFactsVisited !== 0
  || incremental.checkpointRefresh?.touchedEntities !== 0
  || incremental.checkpointRefresh?.replacementRows !== 0) fail("checkpoint refresh scope");
if (incremental.productionCheckpointPathChanged !== false) fail("checkpoint path");

const semantics = receipt.semantics ?? {};
if (Object.keys(semantics).length !== 12) fail("semantic coverage");
for (const [name, passed] of Object.entries(semantics)) {
  if (passed !== true) fail("semantic " + name);
}

console.log("validated " + receipt.schema + " " + profile);

function expectedPair(total, validAt) {
  let count = 0;
  let checksum = 0;
  for (let value = 0; value < total; value += 1) {
    const visible = validAt < boundary
      ? value % 4 === 0 || value % 4 === 2
      : validAt < boundary + 2
        ? value % 4 !== 2
        : value % 4 === 0 || value % 4 === 1;
    if (visible) {
      count += 1;
      checksum += value;
    }
  }
  return [count, checksum];
}

function validateAggregate(name, aggregate, expectedCount, expectedChecksum, expectedSamples) {
  if (!aggregate || aggregate.count !== expectedCount || aggregate.checksum !== expectedChecksum) {
    fail(name + " exactness");
  }
  if (!Array.isArray(aggregate.samplesMs) || aggregate.samplesMs.length !== expectedSamples) {
    fail(name + " samples");
  }
  if (!Number.isFinite(aggregate.p50Ms) || !Number.isFinite(aggregate.p95Ms)
    || aggregate.p50Ms <= 0 || aggregate.p95Ms < aggregate.p50Ms) {
    fail(name + " latency");
  }
}
