#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const facts = profile === "full" ? 1_000_000 : 10_000;
const repetitions = profile === "full" ? 20 : 5;
assert(receipt.schema === "vicia.storage-layout.v1", "schema");
assert(receipt.facts === facts && receipt.repetitions === repetitions, "profile shape");
assert(receipt.candidates.map((candidate) => candidate.fillPercent).join(",") === "75,85,90,95,100", "fill candidates");
for (const candidate of receipt.candidates) {
  assert(candidate.checkpoint.elapsedSamplesMs.length === repetitions, `fill-${candidate.fillPercent}: checkpoint samples`);
  assert(candidate.checkpoint.baselineRssSamplesBytes.length === repetitions, `fill-${candidate.fillPercent}: checkpoint baseline RSS samples`);
  assert(candidate.checkpoint.peakRssSamplesBytes.length === repetitions, `fill-${candidate.fillPercent}: checkpoint peak RSS samples`);
  assert(candidate.checkpoint.deltaRssSamplesBytes.length === repetitions, `fill-${candidate.fillPercent}: checkpoint delta RSS samples`);
  assert(candidate.query.pointSamplesMs.length === repetitions, `fill-${candidate.fillPercent}: point samples`);
  assert(candidate.query.aggregateSamplesMs.length === repetitions, `fill-${candidate.fillPercent}: aggregate samples`);
  assert(candidate.query.count === facts && candidate.query.checksum === facts * (facts - 1) / 2, `fill-${candidate.fillPercent}: correctness`);
  const layout = candidate.checkpoint.layout;
  const indexes = [layout.eavt, layout.aevt, layout.avet, layout.vaet];
  const classified = layout.headerBytes + layout.facts.pages * 4096 + indexes.reduce((sum, index) => sum + (index.leaf.pages + index.internal.pages) * 4096, 0) + layout.otherPublishedBytes;
  assert(classified === layout.publishedBytes, `fill-${candidate.fillPercent}: published byte accounting`);
  for (const component of [layout.facts, ...indexes.flatMap((index) => [index.leaf, index.internal])]) {
    assert(component.payloadBytes + component.structuralBytes + component.unusedBytes === component.pages * 4096, `fill-${candidate.fillPercent}: page byte accounting`);
  }
}
if (profile === "full") assert(receipt.trackedClean, "full receipt requires a clean tracked source state");
if (receipt.selectedFillPercent !== null) {
  assert([85, 90, 95, 100].includes(receipt.selectedFillPercent), "selected fill candidate");
}
console.log(`validated ${receipt.schema} ${profile}`);

function assert(value, message) { if (!value) throw new Error(message); }
