import { readFileSync } from "node:fs";

const [baselinePath, candidatePath] = process.argv.slice(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
const sorted = (values) => [...values].sort((a, b) => a - b);
const percentile = (values, fraction) => sorted(values)[Math.ceil(values.length * fraction) - 1];
const p50 = (values) => percentile(values, 0.5);
const p95 = (values) => percentile(values, 0.95);
if (baseline.schema !== "vicia.leaf-read-path.v1" || candidate.schema !== baseline.schema) throw new Error("receipt schema mismatch");
if (baseline.profile !== candidate.profile || baseline.facts !== candidate.facts || baseline.fixture.sha256 !== candidate.fixture.sha256) throw new Error("comparison provenance mismatch");
const bp95 = p95(baseline.point.samplesMsPerOperation);
const cp95 = p95(candidate.point.samplesMsPerOperation);
const ba50 = p50(baseline.aggregate.samplesMs);
const ca50 = p50(candidate.aggregate.samplesMs);
const ca95 = p95(candidate.aggregate.samplesMs);
const cursor = candidate.aggregate.cursorDiagnostics;
const gates = {
  noFullLeafMaterialization: candidate.aggregate.diagnostics.fullLeafVecPeakEntries === 0 && candidate.aggregate.diagnostics.fullLeafVecPeakStructBytes === 0 && candidate.aggregate.diagnostics.fullLeafVecPeakDecodedPayloadBytes === 0,
  projectedCount: candidate.aggregate.diagnostics.projectedAevtEmitted === candidate.facts,
  noOwnedProjectedDecode: candidate.aggregate.diagnostics.projectedOwnedAevtDecodes === 0,
  phaseAttribution: Number.isFinite(candidate.aggregate.diagnosticQueryElapsedNs) && candidate.aggregate.diagnosticQueryElapsedNs > 0 && cursor && cursor.committedEntriesVisited === candidate.facts && cursor.reducerEntries === candidate.facts && cursor.entityFlushCount === candidate.facts && cursor.visitorValues === candidate.facts && cursor.emittedRows === candidate.facts,
  pointAbsolute: cp95 <= 0.050,
  pointNoRegression: cp95 <= bp95,
  aggregate: ca50 <= 230 || ca50 <= ba50 * 0.90,
  aggregateTail: ca95 <= ca50 * 1.15,
  rss: candidate.aggregate.workloadDeltaRssBytes <= baseline.aggregate.workloadDeltaRssBytes + 2 * 1024 * 1024,
};
console.log(JSON.stringify({ baseline: { pointP95Ms: bp95, aggregateP50Ms: ba50 }, candidate: { pointP95Ms: cp95, aggregateP50Ms: ca50, aggregateP95Ms: ca95 }, gates }, null, 2));
if (Object.values(gates).some((passed) => !passed)) throw new Error("leaf read candidate failed acceptance gates");
