#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const facts = profile === "full" ? 1_000_000 : 10_000;
const repetitions = profile === "full" ? 20 : 5;
assert(receipt.schema === "vicia.checkpoint-construction.v1", "schema");
assert(receipt.baseFacts === facts && receipt.repetitions === repetitions, "profile");
assert(receipt.variants.map((v) => v.pendingFacts).join(",") === "1,10,100,1000", "variants");
for (const variant of receipt.variants) {
  assert(variant.samples.length === repetitions, `${variant.pendingFacts}: samples`);
  for (const sample of variant.samples) {
    assert(sample.count === facts && sample.checksum === facts * (facts - 1) / 2, "correctness");
    assert(sample.diagnostics.peakFactPagesInMemory === 1, "fact page bound");
    assert(sample.diagnostics.peakTypedEntries === facts + variant.pendingFacts, "typed entry bound");
    assert(sample.diagnostics.factPageVisits > 0, "page visits");
    assert(sample.diagnostics.peakSerializedBytes <= 4096, "serialized byte frontier");
  }
}
if (profile === "full") {
  assert(receipt.trackedClean, "clean full source");
  const all = receipt.variants.flatMap((variant) => variant.samples);
  assert(Math.max(...all.map((sample) => sample.recompactDeltaRssBytes)) <= 640 * 1024 * 1024, "RSS gate");
  for (const variant of receipt.variants) {
    const times = variant.samples.map((sample) => sample.recompactElapsedMs).sort((a, b) => a - b);
    const p50 = percentile(times, 50);
    const p95 = percentile(times, 95);
    assert(p50 <= 7510 * 1.10, `${variant.pendingFacts}: p50 gate`);
    assert(p95 <= p50 * 1.15, `${variant.pendingFacts}: p95 gate`);
  }
}
console.log(`validated ${receipt.schema} ${profile}`);

function percentile(values, percent) { return values[Math.ceil((values.length - 1) * percent / 100)]; }
function assert(value, message) { if (!value) throw new Error(message); }
