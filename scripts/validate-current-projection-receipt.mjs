import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
const facts = profile === "full" ? 1_000_000 : 10_000;
const samples = profile === "full" ? 20 : 5;
const checksum = facts * (facts - 1) / 2;

if (receipt.schema !== "vicia.current-projection-feasibility.v1") fail("schema");
if (receipt.facts !== facts || receipt.samples !== samples) fail("workload shape");
if (!Number.isSafeInteger(receipt.graphBytes) || receipt.graphBytes <= 0) fail("graph bytes");
if (!/^[0-9a-f]{40}$/.test(receipt.provenance?.sourceCommit ?? "")) fail("source commit");
if (profile === "full" && receipt.provenance?.sourceDirty !== false) fail("full source must be clean");
for (const field of ["productionQueryRoutingChanged", "publicApiChanged", "fileFormatChanged"]) {
  if (receipt.provenance?.[field] !== false) fail(field);
}

validateAggregate("baseline", receipt.baseline, facts, checksum, samples);
validateAggregate("projection", receipt.projection?.aggregate, facts, checksum, samples);
const improvement = (receipt.baseline.p50Ms - receipt.projection.aggregate.p50Ms)
  / receipt.baseline.p50Ms;
if (!(receipt.projection.aggregate.p50Ms <= 150 || improvement >= 0.35)) fail("latency admission");
if (receipt.projection.aggregate.p95Ms > receipt.projection.aggregate.p50Ms * 1.15) fail("projection tail");
if (receipt.projection.queryRssDeltaBytes > 2 * 1024 * 1024) fail("query RSS");
const sizeRatioLimit = profile === "full" ? 0.15 : 0.20;
if (receipt.projection.imageRatio > sizeRatioLimit) fail("projection size ratio");
if (receipt.projection.accountedBytes > receipt.graphBytes * sizeRatioLimit) fail("projection bytes");
if (receipt.projection.rowCount !== facts) fail("projection rows");
if (!/^[0-9a-f]{16}$/.test(receipt.projection.fingerprint ?? "")) fail("fingerprint");

const incremental = receipt.incremental;
if (!incremental?.staleReadRejected || !incremental.checkpointStaleReadRejected) fail("stale generation");
if (!incremental.deterministicRebuild) fail("incremental rebuild");
if (incremental.count !== facts + 1 || incremental.checksum !== checksum + facts) fail("incremental exactness");
if (incremental.refresh?.tailFactsVisited !== 1
  || incremental.refresh?.touchedEntities !== 1
  || incremental.refresh?.replacementRows !== 1) fail("tail refresh scope");
if (incremental.checkpointRefresh?.tailFactsVisited !== 0
  || incremental.checkpointRefresh?.touchedEntities !== 0) fail("checkpoint refresh scope");
if (incremental.productionCheckpointPathChanged !== false
  || incremental.checkpointRegressionPercent > 10) fail("checkpoint regression");
for (const [name, passed] of Object.entries(receipt.semantics ?? {})) {
  if (passed !== true) fail("semantic " + name);
}
if (Object.keys(receipt.semantics ?? {}).length !== 9) fail("semantic coverage");
console.log("validated " + receipt.schema + " " + profile
  + "; projection improvement " + (improvement * 100).toFixed(2) + "%");

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
