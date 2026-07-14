#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
const assert = (condition, message) => { if (!condition) fail(message); };
const expectedFacts = profile === "full" ? 1_000_000 : 10_000;
const expectedPairs = profile === "full" ? 5 : 1;
const expectedChecksum = expectedFacts * (expectedFacts - 1) / 2;
const tolerance = 2 * 1024 * 1024;
const retainedLimit = 16 * 1024 * 1024;
const breakdownFields = [
  "anonymousRssBytes",
  "fileBackedRssBytes",
  "heapMappingRssBytes",
  "databaseMappedRssBytes",
];

assert(receipt.schema === "vicia.aggregate-retention.v1", "schema mismatch");
assert(receipt.profile === profile, "profile mismatch");
assert(receipt.facts === expectedFacts, "fact count mismatch");
assert(receipt.pairs?.length === expectedPairs, "pair count mismatch");
assert(/^[0-9a-f]{40}$/.test(receipt.sourceCommit), "source commit missing");
if (profile === "full") assert(receipt.trackedClean === true, "full receipt requires clean tracked source");
assert(receipt.fixture?.bytes > 0, "fixture bytes missing");
assert(/^[0-9a-f]{64}$/.test(receipt.fixture?.sha256), "fixture digest missing");
assert(receipt.fixture.formatVersion === 12, "fixture format mismatch");
assert(receipt.fixture.fillPercent === 90, "fixture fill mismatch");

for (const [pairIndex, pair] of receipt.pairs.entries()) {
  assert(pair.pairIndex === pairIndex, `${pairIndex}: pair index mismatch`);
  const expectedOrder = pairIndex % 2 === 0 ? [1, 20] : [20, 1];
  assert(JSON.stringify(pair.order) === JSON.stringify(expectedOrder), `${pairIndex}: order mismatch`);
  validateMeasurement(pair.one, 1, `${pairIndex}/one`);
  validateMeasurement(pair.twenty, 20, `${pairIndex}/twenty`);
}

const oneRetained = receipt.pairs.map((pair) => pair.one.retainedDeltaAfterLiveTrimBytes);
const twentyRetained = receipt.pairs.map((pair) => pair.twenty.retainedDeltaAfterLiveTrimBytes);
const oneMedian = median(oneRetained);
const twentyMedian = median(twentyRetained);
assert(twentyMedian <= retainedLimit, `20-run retained median exceeds ${retainedLimit} bytes`);
assert(twentyMedian <= oneMedian + tolerance, `20-run retained growth exceeds ${tolerance} bytes`);

console.log(JSON.stringify({
  ok: true,
  schema: receipt.schema,
  profile,
  pairs: expectedPairs,
  oneRetainedMedianBytes: oneMedian,
  twentyRetainedMedianBytes: twentyMedian,
  retainedGrowthBytes: Math.max(0, twentyMedian - oneMedian),
}));

function validateMeasurement(measurement, iterations, label) {
  assert(measurement.iterations === iterations, `${label}: iteration count mismatch`);
  assert(measurement.samples?.length === iterations, `${label}: sample count mismatch`);
  assert(measurement.count === expectedFacts, `${label}: aggregate count mismatch`);
  assert(Number(measurement.checksum) === expectedChecksum, `${label}: checksum mismatch`);
  assert(measurement.txCountBefore === measurement.txCountAfter, `${label}: transaction cursor changed`);
  assert(measurement.graphBytesBefore === receipt.fixture.bytes, `${label}: initial graph bytes mismatch`);
  assert(measurement.graphBytesAfter === receipt.fixture.bytes, `${label}: graph bytes changed`);
  assert(measurement.walExistsBefore === false && measurement.walExistsAfter === false, `${label}: WAL appeared`);
  for (const field of [
    "baselineRssBytes",
    "processPeakRssBytes",
    "rssBeforeTrimBytes",
    "rssAfterLiveTrimBytes",
    "rssAfterDropTrimBytes",
  ]) assert(Number.isSafeInteger(measurement[field]) && measurement[field] > 0, `${label}: invalid ${field}`);
  assert(measurement.allocatorTrimSupported === true, `${label}: allocator trim unsupported`);
  assert(
    measurement.retainedDeltaBeforeTrimBytes === Math.max(0, measurement.rssBeforeTrimBytes - measurement.baselineRssBytes),
    `${label}: pre-trim retained arithmetic mismatch`,
  );
  assert(
    measurement.retainedDeltaAfterLiveTrimBytes === Math.max(0, measurement.rssAfterLiveTrimBytes - measurement.baselineRssBytes),
    `${label}: post-trim retained arithmetic mismatch`,
  );
  assert(
    measurement.liveDatabaseRssBytes === Math.max(0, measurement.rssAfterLiveTrimBytes - measurement.rssAfterDropTrimBytes),
    `${label}: live database RSS arithmetic mismatch`,
  );
  validateBreakdownDelta(
    measurement.breakdownBeforeTrim,
    measurement.baselineBreakdown,
    measurement.breakdownDeltaBeforeTrim,
    `${label}: pre-trim breakdown`,
  );
  validateBreakdownDelta(
    measurement.breakdownAfterLiveTrim,
    measurement.baselineBreakdown,
    measurement.breakdownDeltaAfterLiveTrim,
    `${label}: post-trim breakdown`,
  );
  for (const [index, sample] of measurement.samples.entries()) {
    assert(sample.iteration === index + 1, `${label}: sample sequence mismatch`);
    assert(Number.isFinite(sample.elapsedMs) && sample.elapsedMs > 0, `${label}: invalid elapsed time`);
    assert(Number.isSafeInteger(sample.rssBytes) && sample.rssBytes > 0, `${label}: invalid sample RSS`);
    assert(Number.isSafeInteger(sample.peakRssBytes) && sample.peakRssBytes >= sample.rssBytes, `${label}: invalid sample peak`);
    const diagnostics = sample.cursorDiagnostics;
    assert(diagnostics.selectedPendingEntries === 0, `${label}: selected pending entries`);
    assert(diagnostics.pendingEntriesVisited === 0, `${label}: pending entries visited`);
    assert(diagnostics.committedEntriesVisited === expectedFacts, `${label}: committed visit count`);
    assert(diagnostics.reducerEntries === expectedFacts, `${label}: reducer count`);
    assert(diagnostics.entityFlushCount === expectedFacts, `${label}: entity flush count`);
    assert(diagnostics.visitorValues === expectedFacts, `${label}: visitor count`);
    assert(diagnostics.emittedRows === expectedFacts, `${label}: emitted count`);
    assert(diagnostics.peakEntityValues === 1 && diagnostics.peakEntityWindows === 1, `${label}: reducer state widened`);
  }
  if (iterations === 20) {
    const first = median(measurement.samples.slice(0, 5).map((sample) => sample.rssBytes));
    const last = median(measurement.samples.slice(-5).map((sample) => sample.rssBytes));
    assert(last <= first + tolerance, `${label}: repeated RSS trend exceeds tolerance`);
  }
}

function validateBreakdownDelta(current, baseline, delta, label) {
  for (const field of breakdownFields) {
    assert(Number.isSafeInteger(current[field]) && current[field] >= 0, `${label}: invalid current ${field}`);
    assert(Number.isSafeInteger(baseline[field]) && baseline[field] >= 0, `${label}: invalid baseline ${field}`);
    assert(delta[field] === Math.max(0, current[field] - baseline[field]), `${label}: ${field} arithmetic mismatch`);
  }
}

function median(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.floor(sorted.length / 2)];
}
