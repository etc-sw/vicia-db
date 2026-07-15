#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
const shape = profile === "full"
  ? { facts: 1_000_000, samples: 40, eligible: true }
  : { facts: 10_000, samples: 6, eligible: false };
const boundary = 1_735_689_600_000;
const probes = [
  ["beforeBoundary", boundary - 1],
  ["atBoundary", boundary],
  ["afterBoundary", boundary + 2],
];
const cells = [
  ["source", 0],
  ["decoded", 1],
  ["source", 2],
  ["decoded", 0],
  ["source", 1],
  ["decoded", 2],
];

if (receipt.schema !== "vicia.current-projection-isolated-tail.v1") fail("schema");
if (receipt.profile !== profile || receipt.facts !== shape.facts
  || receipt.samplesPerCell !== shape.samples) fail("shape");
if (receipt.admissionEligible !== shape.eligible) fail("admission eligibility");
if (!/^[0-9a-f]{40}$/.test(receipt.sourceCommit ?? "")) fail("source commit");
if (profile === "full" && receipt.trackedClean !== true) fail("full source must be clean");
if (!receipt.fixture || receipt.fixture.schema !== "vicia.temporal-projection-fixture.v1"
  || receipt.fixture.facts !== shape.facts || receipt.fixture.fillPercent !== 90
  || receipt.fixture.bytes <= 0 || !/^[0-9a-f]{64}$/.test(receipt.fixture.sha256 ?? "")
  || receipt.fixture.formatVersion !== 12
  || receipt.fixture.builderSourceCommit !== receipt.sourceCommit
  || receipt.fixture.builderTrackedClean !== receipt.trackedClean) fail("fixture");
validateIdentity(receipt.projectionIdentity, shape.facts);

const expectedMeasurements = shape.samples * cells.length;
if (!Array.isArray(receipt.measurements)
  || receipt.measurements.length !== expectedMeasurements) fail("measurement count");
for (let launchIndex = 0; launchIndex < expectedMeasurements; launchIndex += 1) {
  const sample = receipt.measurements[launchIndex];
  const trialIndex = Math.floor(launchIndex / cells.length);
  const launchPosition = launchIndex % cells.length;
  const [candidate, probeIndex] = cells[(launchPosition + trialIndex) % cells.length];
  const [probe, validAt] = probes[probeIndex];
  if (sample?.launchIndex !== launchIndex || sample.trialIndex !== trialIndex
    || sample.candidate !== candidate || sample.probe !== probe
    || sample.validAt !== validAt) fail("measurement schedule");
  validateIdentity(sample.image, shape.facts);
  if (JSON.stringify(sample.image) !== JSON.stringify(receipt.projectionIdentity)) fail("measurement identity");
  validateAggregate(sample.aggregate, expectedPair(shape.facts, validAt));
}

if (!Array.isArray(receipt.probes) || receipt.probes.length !== probes.length) fail("summaries");
let expectedAdmitted = shape.eligible;
for (const [name, validAt] of probes) {
  const summary = receipt.probes.find((candidate) => candidate.name === name);
  if (!summary || summary.validAt !== validAt) fail("summary identity");
  validateSeries(summary.sourceMs, shape.samples);
  validateSeries(summary.decodedMs, shape.samples);
  const raw = receipt.measurements.filter((sample) => sample.probe === name);
  const sourceRaw = raw.filter((sample) => sample.candidate === "source");
  const decodedRaw = raw.filter((sample) => sample.candidate === "decoded");
  if (sourceRaw.length !== shape.samples || decodedRaw.length !== shape.samples) fail("candidate sample count");
  assertSeries(summary.sourceMs, summarize(sourceRaw.map((sample) => sample.aggregate.elapsedMs)), "source summary");
  assertSeries(summary.decodedMs, summarize(decodedRaw.map((sample) => sample.aggregate.elapsedMs)), "decoded summary");
  const expected = expectedPair(shape.facts, validAt);
  const exact = raw.every((sample) =>
    sample.aggregate.count === expected[0] && sample.aggregate.checksum === expected[1]);
  const decodedLatency = summary.decodedMs.p50 <= 150;
  const decodedTail = summary.decodedMs.p95 <= summary.decodedMs.p50 * 1.15;
  const p50Relative = summary.decodedMs.p50 <= summary.sourceMs.p50 * 1.10;
  const p95Relative = summary.decodedMs.p95 <= summary.sourceMs.p95 * 1.10;
  const admitted = exact && decodedLatency && decodedTail && p50Relative && p95Relative;
  const gates = summary.gates ?? {};
  if (gates.exact !== exact || gates.decodedLatency !== decodedLatency
    || gates.decodedTail !== decodedTail || gates.decodedP50Relative !== p50Relative
    || gates.decodedP95Relative !== p95Relative || gates.admitted !== admitted) fail("gate derivation");
  expectedAdmitted &&= admitted;
}
if (receipt.admitted !== expectedAdmitted) fail("admitted verdict");
if (receipt.productionQueryRoutingChanged !== false || receipt.publicApiChanged !== false
  || receipt.fileFormatChanged !== false) fail("scope");

console.log(`validated ${receipt.schema} ${profile} admitted=${receipt.admitted}`);

function validateIdentity(identity, facts) {
  if (!identity || !Number.isSafeInteger(identity.baseGeneration) || identity.baseGeneration <= 0
    || !Number.isSafeInteger(identity.manifestGeneration) || identity.manifestGeneration < 0
    || !Number.isSafeInteger(identity.txCount) || identity.txCount <= 0
    || !/^[0-9a-f]{16}$/.test(identity.fingerprint ?? "")
    || identity.rowCount !== facts || !Number.isSafeInteger(identity.paddedBytes)
    || identity.paddedBytes <= 0) fail("identity");
}

function validateAggregate(aggregate, expected) {
  if (!aggregate || !Number.isFinite(aggregate.elapsedMs) || aggregate.elapsedMs <= 0
    || aggregate.count !== expected[0] || aggregate.checksum !== expected[1]) fail("aggregate");
}

function validateSeries(series, count) {
  if (!series || !Array.isArray(series.samples) || series.samples.length !== count
    || series.samples.some((sample) => !Number.isFinite(sample) || sample <= 0)
    || !Number.isFinite(series.p50) || !Number.isFinite(series.p95)
    || !Number.isFinite(series.max) || !Number.isFinite(series.mad)
    || series.p50 <= 0 || series.p95 < series.p50 || series.max < series.p95
    || series.mad < 0) fail("series");
}

function summarize(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const p50 = nearestRank(sorted, 50);
  const deviations = sorted.map((sample) => Math.abs(sample - p50)).sort((a, b) => a - b);
  return {
    samples: sorted,
    p50,
    p95: nearestRank(sorted, 95),
    max: sorted.at(-1),
    mad: nearestRank(deviations, 50),
  };
}

function nearestRank(sorted, percentile) {
  return sorted[Math.ceil(percentile * sorted.length / 100) - 1];
}

function assertSeries(actual, expected, label) {
  if (actual.samples.length !== expected.samples.length) fail(label);
  for (let index = 0; index < expected.samples.length; index += 1) {
    if (!approximately(actual.samples[index], expected.samples[index])) fail(label);
  }
  for (const field of ["p50", "p95", "max", "mad"]) {
    if (!approximately(actual[field], expected[field])) fail(label);
  }
}

function approximately(left, right) {
  return Math.abs(left - right) <= Math.max(1, Math.abs(left), Math.abs(right)) * 1e-12;
}

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
