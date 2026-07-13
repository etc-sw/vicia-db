#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const facts = profile === "full" ? 1_000_000 : 10_000;
const repetitions = profile === "full" ? 20 : 5;
const fills = [75, 85, 90, 95, 100];

assert(receipt.schema === "vicia.storage-layout.v2", "schema");
assert(receipt.facts === facts && receipt.repetitions === repetitions, "profile shape");
assert(receipt.candidates.map((candidate) => candidate.fillPercent).join(",") === fills.join(","), "fill candidates");
validateOrder(receipt.checkpointOrder, repetitions, fills, "checkpoint");
validateOrder(receipt.queryOrder, repetitions, fills, "query");

const baseline = receipt.candidates[0];
for (const candidate of receipt.candidates) {
  const label = `fill-${candidate.fillPercent}`;
  validateSamples(candidate.checkpoint.elapsedSamplesMs, repetitions, `${label}: checkpoint elapsed`);
  validateSamples(candidate.checkpoint.baselineRssSamplesBytes, repetitions, `${label}: checkpoint baseline RSS`);
  validateSamples(candidate.checkpoint.peakRssSamplesBytes, repetitions, `${label}: checkpoint peak RSS`);
  validateSamples(candidate.checkpoint.deltaRssSamplesBytes, repetitions, `${label}: checkpoint delta RSS`);
  validateSamples(candidate.query.pointSamplesMs, repetitions, `${label}: point`);
  validateSamples(candidate.query.aggregateSamplesMs, repetitions, `${label}: aggregate`);
  validateSamples(candidate.query.baselineRssSamplesBytes, repetitions, `${label}: query baseline RSS`);
  validateSamples(candidate.query.peakRssSamplesBytes, repetitions, `${label}: query peak RSS`);
  validateSamples(candidate.query.deltaRssSamplesBytes, repetitions, `${label}: query delta RSS`);
  assert(candidate.query.count === facts && candidate.query.checksum === facts * (facts - 1) / 2, `${label}: correctness`);

  const expectedStats = {
    checkpointMs: summarize(candidate.checkpoint.elapsedSamplesMs),
    checkpointDeltaRssBytes: summarize(candidate.checkpoint.deltaRssSamplesBytes),
    pointMs: summarize(candidate.query.pointSamplesMs),
    aggregateMs: summarize(candidate.query.aggregateSamplesMs),
    queryDeltaRssBytes: summarize(candidate.query.deltaRssSamplesBytes),
  };
  assertJsonEqual(candidate.stats, expectedStats, `${label}: summaries`);
  const expectedGates = gates(candidate, baseline);
  assertJsonEqual(candidate.gates, expectedGates, `${label}: gates`);

  const layout = candidate.checkpoint.layout;
  const indexes = [layout.eavt, layout.aevt, layout.avet, layout.vaet];
  const classified = layout.headerBytes + layout.facts.pages * 4096 + indexes.reduce((sum, index) => sum + (index.leaf.pages + index.internal.pages) * 4096, 0) + layout.otherPublishedBytes;
  assert(classified === layout.publishedBytes, `${label}: published byte accounting`);
  for (const component of [layout.facts, ...indexes.flatMap((index) => [index.leaf, index.internal])]) {
    assert(component.payloadBytes + component.structuralBytes + component.unusedBytes === component.pages * 4096, `${label}: page byte accounting`);
  }
}

const selected = receipt.candidates.filter((candidate) => candidate.fillPercent > 75 && candidate.gates.passed).map((candidate) => candidate.fillPercent).at(-1) ?? null;
assert(receipt.selectedFillPercent === selected, "selected fill");
if (profile === "full") assert(receipt.trackedClean, "full receipt requires a clean tracked source state");
console.log(`validated ${receipt.schema} ${profile}`);

function validateOrder(orders, count, candidates, label) {
  assert(Array.isArray(orders) && orders.length === count, `${label} order count`);
  for (let repetition = 0; repetition < count; repetition += 1) {
    const expected = candidates.map((_, offset) => candidates[(repetition + offset) % candidates.length]);
    assertJsonEqual(orders[repetition], expected, `${label} order ${repetition + 1}`);
  }
}
function validateSamples(values, count, label) {
  assert(Array.isArray(values) && values.length === count, `${label} samples`);
  assert(values.every((value) => Number.isFinite(value) && value >= 0), `${label} finite samples`);
}
function summarize(values) {
  const p50 = percentile(values, 50);
  return {
    p50,
    p95: percentile(values, 95),
    max: Math.max(...values),
    mad: percentile(values.map((value) => Math.abs(value - p50)), 50),
  };
}
function percentile(values, percent) {
  assert(values.length > 0 && percent >= 1 && percent <= 100, "percentile input");
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * percent / 100) - 1];
}
function gates(candidate, baseline) {
  const size = candidate.checkpoint.graphBytes * 10 <= baseline.checkpoint.graphBytes * 9;
  const checkpoint = latencyGate(candidate.stats.checkpointMs, baseline.stats.checkpointMs);
  const point = candidate.stats.pointMs.p50 <= baseline.stats.pointMs.p50 * 1.20 && candidate.stats.pointMs.p95 <= baseline.stats.pointMs.p95 * 1.20;
  const aggregate = latencyGate(candidate.stats.aggregateMs, baseline.stats.aggregateMs);
  const rss = candidate.stats.checkpointDeltaRssBytes.p50 <= baseline.stats.checkpointDeltaRssBytes.p50 * 1.10 + 2 * 1024 * 1024 && candidate.stats.queryDeltaRssBytes.p50 <= baseline.stats.queryDeltaRssBytes.p50 * 1.10 + 2 * 1024 * 1024;
  return { size, checkpoint, point, aggregate, rss, passed: size && checkpoint && point && aggregate && rss };
}
function latencyGate(candidate, baseline) {
  return candidate.p50 <= baseline.p50 * 1.10 && candidate.p95 <= baseline.p95 * 1.10 && candidate.p95 <= candidate.p50 * 1.15;
}
function assertJsonEqual(actual, expected, message) {
  assert(JSON.stringify(actual) === JSON.stringify(expected), message);
}
function assert(value, message) { if (!value) throw new Error(message); }
