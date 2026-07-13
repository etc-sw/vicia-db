import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
const samples = profile === "full" ? 20 : profile === "smoke" ? 5 : fail("invalid profile");
if (receipt.schema !== "vicia.leaf-read-path.v1" || receipt.profile !== profile) fail("schema/profile mismatch");
if (receipt.facts !== (profile === "full" ? 1_000_000 : 10_000)) fail("fact count mismatch");
if (receipt.point.samplesMsPerOperation?.length !== samples || receipt.point.rawSingleQuerySamplesMs?.length !== samples) fail("point sample count mismatch");
if (receipt.aggregate.samplesMs?.length !== samples) fail("aggregate sample count mismatch");
const expected = receipt.facts * (receipt.facts - 1) / 2;
if (receipt.aggregate.count !== receipt.facts || Number(receipt.aggregate.checksum) !== expected) fail("correctness mismatch");
if (!/^[0-9a-f]{64}$/.test(receipt.fixture.sha256)) fail("fixture digest missing");
for (const section of [receipt.point.diagnostics, receipt.aggregate.diagnostics]) {
  if (!section || Object.values(section).some((value) => !Number.isFinite(value) || value < 0)) fail("invalid diagnostics");
}
const aggregateDiagnostics = receipt.aggregate.diagnostics;
const cursorDiagnostics = receipt.aggregate.cursorDiagnostics;
if (!Number.isFinite(receipt.aggregate.diagnosticQueryElapsedNs) || receipt.aggregate.diagnosticQueryElapsedNs <= 0) fail("aggregate diagnostic query timing missing");
if (!cursorDiagnostics || Object.values(cursorDiagnostics).some((value) => !Number.isFinite(value) || value < 0)) fail("invalid current-attribute cursor diagnostics");
if (aggregateDiagnostics.projectedAevtEmitted !== receipt.facts) fail("projected AEVT count mismatch");
if (aggregateDiagnostics.projectedOwnedAevtDecodes !== 0) fail("projected path decoded owned AEVT keys");
if (aggregateDiagnostics.fullLeafVecPeakEntries !== 0 || aggregateDiagnostics.fullLeafVecPeakStructBytes !== 0 || aggregateDiagnostics.fullLeafVecPeakDecodedPayloadBytes !== 0) fail("full-leaf materialization returned");
if (cursorDiagnostics.committedEntriesVisited !== receipt.facts || cursorDiagnostics.reducerEntries !== receipt.facts) fail("current reducer entry count mismatch");
if (cursorDiagnostics.entityFlushCount !== receipt.facts || cursorDiagnostics.visitorValues !== receipt.facts || cursorDiagnostics.emittedRows !== receipt.facts) fail("current flush/visitor count mismatch");
for (const name of ["committedMergeElapsedNs", "reducerElapsedNs", "entityFlushPrepareElapsedNs", "visitorElapsedNs", "aggregateFinishElapsedNs"]) {
  if (cursorDiagnostics[name] <= 0) fail(`current phase timing missing: ${name}`);
}
console.log(`leaf read receipt OK: ${profile} (${samples} samples)`);
