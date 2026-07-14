#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [receiptPath, profile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
const facts = profile === "full" ? 1_000_000 : 10_000;
const repetitions = profile === "full" ? 20 : 5;
const fills = [75, 85, 90, 95, 100];

assert(receipt.schema === "vicia.point-path-density.v1", "schema");
assert(receipt.profile === profile, "profile");
assert(receipt.facts === facts && receipt.repetitions === repetitions, "profile shape");
assert(receipt.candidates.map((candidate) => candidate.fillPercent).join(",") === fills.join(","), "fills");
validateOrder(receipt.queryOrder, repetitions, fills);

for (const candidate of receipt.candidates) {
  const label = `fill-${candidate.fillPercent}`;
  validateSamples(candidate.pointSamplesMs, repetitions, `${label}: point samples`);
  assert(candidate.diagnosticsSamples.length === repetitions, `${label}: diagnostic count`);
  assertJsonEqual(candidate.stats, summarize(candidate.pointSamplesMs), `${label}: stats`);
  const layout = candidate.layout;
  assert(layout.formatVersion === 12, `${label}: v12 layout`);
  assert(layout.eavt.height >= 2, `${label}: EAVT height`);
  assert(layout.eavt.leaf.entries === facts, `${label}: EAVT entries`);
  assert(layout.eavt.rawLeafPages + layout.eavt.prefixLeafPages === layout.eavt.leaf.pages, `${label}: leaf codec accounting`);
  assert(candidate.graphBytes === layout.publishedBytes, `${label}: graph bytes`);
  for (const [index, diagnostics] of candidate.diagnosticsSamples.entries()) {
    const sample = `${label}: diagnostics ${index}`;
    for (const field of diagnosticFields()) {
      assert(Number.isSafeInteger(diagnostics[field]) && diagnostics[field] >= 0, `${sample}: ${field}`);
    }
    assert(diagnostics.internalPagesVisited === layout.eavt.height - 1, `${sample}: internal height`);
    assert(diagnostics.internalKeyComparisons <= diagnostics.internalKeysAvailable, `${sample}: internal comparisons`);
    assert(diagnostics.internalKeyBytesDecoded > 0, `${sample}: internal bytes`);
    assert(diagnostics.leafPagesVisited === 1, `${sample}: leaf scope`);
    assert(diagnostics.rawLeafPagesVisited + diagnostics.prefixLeafPagesVisited === 1, `${sample}: leaf codec`);
    assert(diagnostics.lowerBoundKeyComparisons === diagnostics.rawLowerBoundKeyComparisons + diagnostics.prefixRestartKeyComparisons, `${sample}: lower-bound comparisons`);
    assert(diagnostics.leafEntriesDecoded >= diagnostics.lowerBoundKeyComparisons, `${sample}: decoded comparisons`);
    assert(diagnostics.eavtProjectionDecodes === diagnostics.leafEntriesDecoded, `${sample}: EAVT projection decode`);
    assert(diagnostics.projectedEavtEmitted === 1 && diagnostics.leafEntriesEmitted === 1, `${sample}: one emitted fact`);
    assert(diagnostics.exactFactResolutions === 1, `${sample}: one fact resolution`);
    assert(diagnostics.factPageCacheHits + diagnostics.factPageCacheMisses === 1, `${sample}: fact cache accounting`);
    assert(diagnostics.projectedOwnedEavtDecodes === 0, `${sample}: owned EAVT decode`);
    assert(diagnostics.fullLeafVecPeakEntries === 0 && diagnostics.fullLeafVecPeakStructBytes === 0 && diagnostics.fullLeafVecPeakDecodedPayloadBytes === 0, `${sample}: full-leaf materialization`);
  }
}
if (profile === "full") assert(receipt.trackedClean, "full receipt requires clean tracked source");
console.log(`validated ${receipt.schema} ${profile}`);

function diagnosticFields() {
  return [
    "internalPagesVisited", "internalKeysAvailable", "internalKeyComparisons",
    "internalKeyBytesDecoded", "internalDescentElapsedNs", "leafPagesVisited",
    "rawLeafPagesVisited", "prefixLeafPagesVisited", "leafEntriesAvailable",
    "leafEntriesDecoded", "leafEntriesEmitted", "lowerBoundKeyComparisons",
    "rawLowerBoundKeyComparisons", "prefixRestartKeyComparisons",
    "prefixLinearKeyComparisons", "leafSeekElapsedNs", "rawDecodeElapsedNs",
    "prefixDecodeElapsedNs", "eavtProjectionDecodes", "projectedEavtEmitted",
    "projectedOwnedEavtDecodes", "exactFactResolutions", "factPageCacheHits",
    "factPageCacheMisses", "exactFactResolutionElapsedNs",
    "fullLeafVecPeakEntries", "fullLeafVecPeakStructBytes",
    "fullLeafVecPeakDecodedPayloadBytes",
  ];
}
function validateOrder(orders, count, candidates) {
  assert(Array.isArray(orders) && orders.length === count, "query order count");
  for (let repetition = 0; repetition < count; repetition += 1) {
    const expected = candidates.map((_, offset) => candidates[(repetition + offset) % candidates.length]);
    assertJsonEqual(orders[repetition], expected, `query order ${repetition + 1}`);
  }
}
function validateSamples(values, count, label) {
  assert(Array.isArray(values) && values.length === count, label);
  assert(values.every((value) => Number.isFinite(value) && value >= 0), `${label}: finite`);
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
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * percent / 100) - 1];
}
function assertJsonEqual(actual, expected, message) {
  assert(JSON.stringify(actual) === JSON.stringify(expected), message);
}
function assert(value, message) { if (!value) throw new Error(message); }
