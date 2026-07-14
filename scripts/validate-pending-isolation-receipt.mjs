#!/usr/bin/env node

import { readFileSync } from "node:fs";

const [receiptPath, expectedProfile] = process.argv.slice(2);
if (!receiptPath || !["smoke", "full"].includes(expectedProfile)) {
  console.error(
    "usage: validate-pending-isolation-receipt.mjs <receipt.json> <smoke|full>",
  );
  process.exit(2);
}

const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
const BASE_FACTS = 1_000_000;
const BASE_CHECKSUM = 499_999_500_000;
const CONTROL_FACTS = 10_000;
const CONTROL_CHECKSUM = BASE_CHECKSUM + 49_995_000;
const repetitions = expectedProfile === "full" ? 20 : 5;
const unrelatedCounts =
  expectedProfile === "full"
    ? [0, 10_000, 100_000, 1_000_000]
    : [0, 100, 1_000, 10_000];

assert(receipt.schema === "vicia.pending-isolation.v3", "unexpected schema");
assert(receipt.profile === expectedProfile, "unexpected profile");
assert(receipt.baseFacts === BASE_FACTS, "unexpected committed base size");
assert(receipt.repetitions === repetitions, "unexpected repetition count");
assert(receipt.warmupRepetitions === 1, "exactly one warmup is required");
assert(
  /^[0-9a-f]{40}$/.test(receipt.provenance?.sourceCommit ?? ""),
  "source commit is missing",
);
assert(
  typeof receipt.provenance?.trackedClean === "boolean" &&
    typeof receipt.provenance?.cleanStateEligible === "boolean",
  "clean-state provenance is missing",
);
if (expectedProfile === "full") {
  assert(receipt.provenance.trackedClean, "full receipt source must be clean");
  assert(
    receipt.provenance.cleanStateEligible,
    "full receipt must be clean-state eligible",
  );
}

assert(Array.isArray(receipt.variants), "variants must be an array");
assert(
  receipt.variants.length === unrelatedCounts.length + 1,
  "unexpected variant count",
);

const unrelated = unrelatedCounts.map((count) => {
  const variant = receipt.variants.find(
    (candidate) =>
      candidate.pendingKind === "unrelated" && candidate.pendingFacts === count,
  );
  assert(variant, `missing unrelated-${count}`);
  validateVariant(variant, repetitions, BASE_FACTS, BASE_CHECKSUM);
  return variant;
});
const control = receipt.variants.find(
  (variant) =>
    variant.pendingKind === "selectedControl" &&
    variant.pendingFacts === CONTROL_FACTS,
);
assert(control, "missing selected pending control");
validateVariant(
  control,
  repetitions,
  BASE_FACTS + CONTROL_FACTS,
  CONTROL_CHECKSUM,
);

const baseline = unrelated[0];
const baselineDiagnostics = diagnosticsSeries(baseline);
const expectedBaselineDiagnostics = {
  selectedPendingEntries: 0,
  selectedPendingSnapshotBytes: 0,
  committedEntriesVisited: BASE_FACTS,
  pendingEntriesVisited: 0,
  exactFactResolutions: 0,
  emittedRows: BASE_FACTS,
  peakEntityValues: 1,
  peakEntityWindows: 1,
  yieldCount: 0,
  resumeCount: 0,
  committedMergeElapsedNs: 0,
  reducerEntries: BASE_FACTS,
  reducerElapsedNs: 0,
  entityFlushCount: BASE_FACTS,
  entityFlushPrepareElapsedNs: 0,
  visitorValues: BASE_FACTS,
  visitorElapsedNs: 0,
  aggregateFinishElapsedNs: 0,
};
for (const diagnostics of baselineDiagnostics) {
  assertDeepEqual(
    diagnostics,
    expectedBaselineDiagnostics,
    "zero-pending cursor diagnostics",
  );
}

const rssTolerance = 2 * 1024 * 1024;
const baselineRssDelta = baseline.measurement.workloadDeltaRssBytes;
const baselineP50 = baseline.measurement.elapsedSummaryMs.p50;
for (const variant of unrelated) {
  assert(
    variant.expectedCount === BASE_FACTS &&
      variant.expectedChecksum === BASE_CHECKSUM,
    `${variant.label}: unrelated correctness contract changed`,
  );
  for (const diagnostics of diagnosticsSeries(variant)) {
    assertDeepEqual(
      diagnostics,
      expectedBaselineDiagnostics,
      `${variant.label}: cursor diagnostics`,
    );
  }
  const rssDifference = Math.abs(
    variant.measurement.workloadDeltaRssBytes - baselineRssDelta,
  );
  assert(
    rssDifference <= rssTolerance,
    `${variant.label}: RSS delta differs from zero-pending by ${rssDifference} bytes`,
  );
  const { p50, p95 } = variant.measurement.elapsedSummaryMs;
  assert(
    p50 <= baselineP50 * 1.1,
    `${variant.label}: p50 ${p50} exceeds 10% regression gate from ${baselineP50}`,
  );
  assert(
    p95 <= p50 * 1.15,
    `${variant.label}: p95 ${p95} exceeds 115% of p50 ${p50}`,
  );
}

for (const diagnostics of diagnosticsSeries(control)) {
  assert(
    diagnostics.selectedPendingEntries === CONTROL_FACTS,
    "selected control snapshot entry count must be exact",
  );
  assert(
    diagnostics.selectedPendingSnapshotBytes > 0,
    "selected control snapshot bytes must increase",
  );
  assert(
    diagnostics.committedEntriesVisited === BASE_FACTS,
    "selected control committed visits must preserve the base",
  );
  assert(
    diagnostics.pendingEntriesVisited === CONTROL_FACTS,
    "selected control pending visits must be exact",
  );
  assert(
    diagnostics.exactFactResolutions === 0,
    "integer control must not resolve exact facts",
  );
  assert(
    diagnostics.emittedRows === BASE_FACTS + CONTROL_FACTS,
    "selected control emitted row count must be exact",
  );
  assert(
    diagnostics.peakEntityValues === 1 &&
      diagnostics.peakEntityWindows === 1,
    "selected control reducer peak must stay per-entity",
  );
  assert(
    diagnostics.yieldCount === 0 && diagnostics.resumeCount === 0,
    "native full-step control should not yield",
  );
  assert(
    diagnostics.reducerEntries === BASE_FACTS + CONTROL_FACTS &&
      diagnostics.entityFlushCount === BASE_FACTS + CONTROL_FACTS &&
      diagnostics.visitorValues === BASE_FACTS + CONTROL_FACTS,
    "selected control reducer and visitor counts must be exact",
  );
}

console.log(
  `validated ${receipt.schema} ${expectedProfile}: ${unrelated.length} unrelated variants + selected control`,
);
for (const variant of receipt.variants) {
  const summary = variant.measurement.elapsedSummaryMs;
  console.log(
    `${variant.label}: p50=${round(summary.p50)}ms p95=${round(summary.p95)}ms rssDelta=${round(variant.measurement.workloadDeltaRssBytes / 1024 / 1024)}MiB`,
  );
}

function validateVariant(variant, expectedSamples, expectedCount, expectedChecksum) {
  assert(variant.expectedCount === expectedCount, `${variant.label}: expected count`);
  assert(
    variant.expectedChecksum === expectedChecksum,
    `${variant.label}: expected checksum`,
  );
  assert(
    variant.measurement?.count === expectedCount,
    `${variant.label}: measured count`,
  );
  assert(
    variant.measurement?.checksum === expectedChecksum,
    `${variant.label}: measured checksum`,
  );
  assert(
    Array.isArray(variant.measurement.samples) &&
      variant.measurement.samples.length === expectedSamples,
    `${variant.label}: raw sample count`,
  );
  assert(variant.measurement.warmup, `${variant.label}: warmup missing`);
  assert(
    Number.isSafeInteger(variant.measurement.processPeakRssBytes) &&
      variant.measurement.processPeakRssBytes > 0,
    `${variant.label}: raw process peak RSS missing`,
  );
  for (const sample of [variant.measurement.warmup, ...variant.measurement.samples]) {
    assert(
      Number.isFinite(sample.elapsedMs) && sample.elapsedMs > 0,
      `${variant.label}: invalid elapsed sample`,
    );
    assert(
      Number.isSafeInteger(sample.rssBytes) && sample.rssBytes > 0,
      `${variant.label}: invalid RSS sample`,
    );
    assert(
      Number.isSafeInteger(sample.peakRssBytes) && sample.peakRssBytes > 0,
      `${variant.label}: invalid peak RSS sample`,
    );
    validateDiagnostics(sample.cursorDiagnostics, variant.label);
  }
  for (const name of [
    "baselineBreakdown",
    "retainedBreakdown",
    "retainedDeltaBreakdown",
  ]) {
    const breakdown = variant.measurement[name];
    assert(breakdown, `${variant.label}: ${name} missing`);
    for (const field of [
      "anonymousRssBytes",
      "fileBackedRssBytes",
      "heapMappingRssBytes",
      "databaseMappedRssBytes",
    ]) {
      assert(
        Number.isSafeInteger(breakdown[field]) && breakdown[field] >= 0,
        `${variant.label}: ${name}.${field}`,
      );
    }
  }
  const elapsed = variant.measurement.samples.map((sample) => sample.elapsedMs);
  const expectedSummary = summarize(elapsed);
  for (const field of ["p50", "p95", "max", "mad"]) {
    assert(
      Math.abs(expectedSummary[field] - variant.measurement.elapsedSummaryMs[field]) <
        1e-9,
      `${variant.label}: ${field} summary mismatch`,
    );
  }
  validateMemoryAudit(variant);
}

function validateMemoryAudit(variant) {
  const audit = variant.memoryAudit;
  assert(audit && typeof audit === "object", `${variant.label}: memory audit missing`);
  for (const field of [
    "rssBeforeTrimBytes",
    "rssAfterLiveTrimBytes",
    "replayRetainedRssBytes",
    "processPeakRssBytes",
    "replayOverlapAccountedBytes",
    "rssAfterDropTrimBytes",
    "liveDatabaseRssBytes",
    "liveUnaccountedRssBytes",
  ]) {
    assert(
      Number.isSafeInteger(audit[field]) && audit[field] >= 0,
      `${variant.label}: invalid memoryAudit.${field}`,
    );
  }
  assert(
    typeof audit.allocatorTrimSupported === "boolean" &&
      typeof audit.allocatorTrimReleased === "boolean",
    `${variant.label}: allocator trim state missing`,
  );
  assert(
    audit.replayRetainedRssBytes ===
      Math.max(0, audit.rssBeforeTrimBytes - audit.rssAfterLiveTrimBytes),
    `${variant.label}: replay-retained RSS arithmetic mismatch`,
  );
  assert(
    audit.liveDatabaseRssBytes ===
      Math.max(0, audit.rssAfterLiveTrimBytes - audit.rssAfterDropTrimBytes),
    `${variant.label}: live database RSS arithmetic mismatch`,
  );

  const pending = audit.pending;
  assert(pending, `${variant.label}: pending accounting missing`);
  const components = ["facts", "duplicateKeys", "eavt", "aevt", "avet", "vaet"];
  let pendingTotal = 0;
  for (const name of components) {
    const component = pending[name];
    assert(component, `${variant.label}: pending.${name} missing`);
    for (const field of [
      "entries",
      "capacity",
      "inlinePayloadBytes",
      "ownedAttributeBytes",
      "ownedAttributeAllocations",
      "ownedValueBytes",
      "ownedValueAllocations",
      "accountedBytes",
    ]) {
      assert(
        Number.isSafeInteger(component[field]) && component[field] >= 0,
        `${variant.label}: invalid pending.${name}.${field}`,
      );
    }
    assert(
      component.accountedBytes ===
        component.inlinePayloadBytes +
          component.ownedAttributeBytes +
          component.ownedValueBytes,
      `${variant.label}: pending.${name} byte sum mismatch`,
    );
    pendingTotal += component.accountedBytes;
  }
  assert(
    pendingTotal === pending.totalAccountedBytes,
    `${variant.label}: pending total mismatch`,
  );
  assert(
    pending.facts.entries === variant.pendingFacts &&
      pending.duplicateKeys.entries === variant.pendingFacts &&
      pending.eavt.entries === variant.pendingFacts &&
      pending.aevt.entries === variant.pendingFacts &&
      pending.avet.entries === variant.pendingFacts &&
      pending.vaet.entries === 0,
    `${variant.label}: pending component entry ownership mismatch`,
  );
  assert(
    Array.isArray(pending.indexRunCounts) &&
      pending.indexRunCounts.length === 4 &&
      pending.indexRunCounts.every((count) => Number.isSafeInteger(count) && count >= 0 && count <= 21),
    `${variant.label}: pending sorted run bound`,
  );

  const wal = audit.walReplay;
  const expectedPeakWalFacts = Math.min(1_000, variant.pendingFacts);
  assert(
    wal && wal.entries === (variant.pendingFacts === 0 ? 0 : 1) &&
      wal.facts === expectedPeakWalFacts,
    `${variant.label}: peak WAL batch accounting`,
  );
  assert(
    wal.totalAccountedBytes ===
      wal.entryVectorBytes +
        wal.factVectorBytes +
        wal.ownedAttributeBytes +
        wal.ownedValueBytes,
    `${variant.label}: WAL byte sum mismatch`,
  );
  for (const field of ["ownedAttributeAllocations", "ownedValueAllocations"]) {
    assert(
      Number.isSafeInteger(wal[field]) && wal[field] >= 0,
      `${variant.label}: invalid WAL ${field}`,
    );
  }
  assert(
    audit.replayOverlapAccountedBytes ===
      pending.totalAccountedBytes + wal.totalAccountedBytes,
    `${variant.label}: replay overlap sum mismatch`,
  );
  assert(
    audit.liveUnaccountedRssBytes ===
      Math.max(0, audit.liveDatabaseRssBytes - pending.totalAccountedBytes),
    `${variant.label}: live unaccounted RSS arithmetic mismatch`,
  );
}

function diagnosticsSeries(variant) {
  return [
    variant.measurement.warmup.cursorDiagnostics,
    ...variant.measurement.samples.map((sample) => sample.cursorDiagnostics),
  ];
}

function validateDiagnostics(diagnostics, label) {
  assert(diagnostics && typeof diagnostics === "object", `${label}: diagnostics`);
  for (const field of [
    "selectedPendingEntries",
    "selectedPendingSnapshotBytes",
    "committedEntriesVisited",
    "pendingEntriesVisited",
    "exactFactResolutions",
    "emittedRows",
    "peakEntityValues",
    "peakEntityWindows",
    "yieldCount",
    "resumeCount",
    "committedMergeElapsedNs",
    "reducerEntries",
    "reducerElapsedNs",
    "entityFlushCount",
    "entityFlushPrepareElapsedNs",
    "visitorValues",
    "visitorElapsedNs",
    "aggregateFinishElapsedNs",
  ]) {
    assert(
      Number.isSafeInteger(diagnostics[field]) && diagnostics[field] >= 0,
      `${label}: invalid diagnostics.${field}`,
    );
  }
}

function summarize(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const p50 = nearestRank(sorted, 50);
  const p95 = nearestRank(sorted, 95);
  const deviations = sorted.map((sample) => Math.abs(sample - p50)).sort((a, b) => a - b);
  return {
    p50,
    p95,
    max: sorted.at(-1),
    mad: nearestRank(deviations, 50),
  };
}

function nearestRank(sorted, percentile) {
  const rank = Math.ceil((sorted.length * percentile) / 100);
  return sorted[Math.max(0, rank - 1)];
}

function assertDeepEqual(actual, expected, label) {
  assert(
    JSON.stringify(actual) === JSON.stringify(expected),
    `${label}: ${JSON.stringify(actual)} != ${JSON.stringify(expected)}`,
  );
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function round(value) {
  return Math.round(value * 1000) / 1000;
}
