#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const PHASE_GROUPS = {
  construction: [
    "factPackingMicros",
    "committedIndexReadMicros",
    "pendingIndexSortMicros",
    "eavtCollectSortMicros",
    "eavtBuildMicros",
    "aevtCollectSortMicros",
    "aevtBuildMicros",
    "avetCollectSortMicros",
    "avetBuildMicros",
    "vaetCollectSortMicros",
    "vaetBuildMicros",
  ],
  sync: ["factSyncMicros", "dataSyncMicros", "publishSyncMicros"],
  integrity: ["integrityCatalogMicros"],
  publication: [
    "headerAssemblyMicros",
    "publishWriteMicros",
    "publishFinalizeMicros",
  ],
};

const POINT_REGRESSION_LIMIT = 1.2;

export function analyzeStorageLayoutVariance(receipt, provenance) {
  assert(receipt.schema === "vicia.storage-layout.v2", "source schema");
  assert(Array.isArray(receipt.candidates) && receipt.candidates.length > 1, "candidates");
  const baseline = receipt.candidates.find((candidate) => candidate.fillPercent === 75);
  assert(baseline, "fill-75 baseline");

  const candidates = receipt.candidates.map((candidate) => {
    const checkpoint = analyzeCheckpoint(candidate, receipt.checkpointOrder);
    const pointP50Ratio = candidate.stats.pointMs.p50 / baseline.stats.pointMs.p50;
    const pointP95Ratio = candidate.stats.pointMs.p95 / baseline.stats.pointMs.p95;
    const point = {
      statsMs: candidate.stats.pointMs,
      p50RatioToFill75: pointP50Ratio,
      p95RatioToFill75: pointP95Ratio,
      p95: sampleReference(
        candidate.query.pointSamplesMs,
        95,
        receipt.queryOrder,
        candidate.fillPercent,
      ),
      max: maxSampleReference(
        candidate.query.pointSamplesMs,
        receipt.queryOrder,
        candidate.fillPercent,
      ),
      failureShape:
        candidate.fillPercent === 75 || (pointP50Ratio <= POINT_REGRESSION_LIMIT && pointP95Ratio <= POINT_REGRESSION_LIMIT)
          ? "passes-relative-gate"
          : pointP50Ratio > POINT_REGRESSION_LIMIT
            ? "systematic-density-cost"
            : "tail-only",
    };
    return {
      fillPercent: candidate.fillPercent,
      graphBytes: candidate.checkpoint.graphBytes,
      checkpoint,
      point,
      aggregate: {
        statsMs: candidate.stats.aggregateMs,
        tailRatio: candidate.stats.aggregateMs.p95 / candidate.stats.aggregateMs.p50,
        gatePassed: candidate.gates.aggregate,
      },
      gates: candidate.gates,
    };
  });

  const highFill = candidates.filter((candidate) => candidate.fillPercent > 75);
  const ownerCounts = Object.fromEntries(Object.keys(PHASE_GROUPS).map((group) => [group, 0]));
  for (const candidate of highFill) {
    if (candidate.checkpoint.p95.dominantGroup === candidate.checkpoint.max.dominantGroup) {
      ownerCounts[candidate.checkpoint.p95.dominantGroup] += 1;
    }
  }
  const [commonOwner, commonOwnerFillCount] = Object.entries(ownerCounts)
    .sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]))[0];
  const acceptedCommonOwner = commonOwnerFillCount >= 3 ? commonOwner : null;
  const ownerPositions = highFill.flatMap((candidate) => [
    candidate.checkpoint.p95,
    candidate.checkpoint.max,
  ]).filter((sample) => sample.dominantGroup === acceptedCommonOwner)
    .map((sample) => sample.orderPosition);
  const positionCounts = Object.fromEntries(
    [...new Set(ownerPositions)].sort((left, right) => left - right).map((position) => [
      position,
      ownerPositions.filter((candidate) => candidate === position).length,
    ]),
  );
  const dominantPositionCount = ownerPositions.length > 0
    ? Math.max(...Object.values(positionCounts))
    : 0;
  const fixedPositionBias = ownerPositions.length > 0
    && dominantPositionCount * 2 >= ownerPositions.length;
  const systematicPointFills = highFill
    .filter((candidate) => candidate.point.failureShape === "systematic-density-cost")
    .map((candidate) => candidate.fillPercent);
  const tailOnlyPointFills = highFill
    .filter((candidate) => candidate.point.failureShape === "tail-only")
    .map((candidate) => candidate.fillPercent);
  const checkpointClassification = acceptedCommonOwner === null
    ? "unclassified"
    : acceptedCommonOwner === "sync" && !fixedPositionBias
      ? "host-io-variance"
      : "repeatable-production-phase";

  return {
    schema: "vicia.storage-layout-variance.v1",
    source: provenance,
    policy: {
      highFillPercents: highFill.map((candidate) => candidate.fillPercent),
      commonOwnerMinimumFills: 3,
      fixedPositionMinimumShare: 0.5,
      pointRegressionLimit: POINT_REGRESSION_LIMIT,
      phaseGroups: PHASE_GROUPS,
    },
    candidates,
    verdict: {
      checkpoint: {
        ownerCounts,
        commonOwner: acceptedCommonOwner,
        commonOwnerFillCount,
        positionCounts,
        fixedPositionBias,
        classification: checkpointClassification,
        implementationAdmitted:
          checkpointClassification === "repeatable-production-phase" && acceptedCommonOwner !== "sync",
      },
      point: {
        systematicFills: systematicPointFills,
        tailOnlyFills: tailOnlyPointFills,
        classification: systematicPointFills.length > 0 ? "systematic-density-cost" : "tail-only-or-passing",
        nextSlice: systematicPointFills.length > 0 ? "point-path-density-attribution" : null,
      },
      rollout: {
        sourceSelectedFillPercent: receipt.selectedFillPercent,
        productionFillPercent: 75,
        authorized: receipt.selectedFillPercent !== null,
      },
    },
  };
}

export function sourceProvenance(receiptPath, bytes, receipt) {
  return {
    path: path.relative(process.cwd(), path.resolve(receiptPath)),
    sha256: createHash("sha256").update(bytes).digest("hex"),
    schema: receipt.schema,
    sourceCommit: receipt.sourceCommit,
    trackedClean: receipt.trackedClean,
    profile: receipt.profile,
    facts: receipt.facts,
    repetitions: receipt.repetitions,
  };
}

function analyzeCheckpoint(candidate, orders) {
  const diagnostics = candidate.checkpoint.diagnosticsSamples;
  assert(diagnostics.length === candidate.checkpoint.elapsedSamplesMs.length, "diagnostic samples");
  const phaseMedians = Object.fromEntries(
    Object.values(PHASE_GROUPS).flat().map((phase) => [
      phase,
      percentile(diagnostics.map((sample) => sample[phase]), 50),
    ]),
  );
  return {
    statsMs: candidate.stats.checkpointMs,
    p95: checkpointSample(
      candidate,
      sampleIndex(candidate.checkpoint.elapsedSamplesMs, 95),
      orders,
      phaseMedians,
    ),
    max: checkpointSample(
      candidate,
      maxSampleIndex(candidate.checkpoint.elapsedSamplesMs),
      orders,
      phaseMedians,
    ),
  };
}

function checkpointSample(candidate, index, orders, phaseMedians) {
  const sample = candidate.checkpoint.diagnosticsSamples[index];
  const phases = Object.fromEntries(Object.entries(phaseMedians).map(([phase, medianMicros]) => {
    const deltaMicros = sample[phase] - medianMicros;
    return [phase, {
      medianMicros,
      sampleMicros: sample[phase],
      deltaMicros,
      positiveDeltaMicros: Math.max(0, deltaMicros),
    }];
  }));
  const groups = Object.fromEntries(Object.entries(PHASE_GROUPS).map(([group, members]) => [
    group,
    {
      sampleMicros: members.reduce((sum, phase) => sum + phases[phase].sampleMicros, 0),
      medianMicros: members.reduce((sum, phase) => sum + phases[phase].medianMicros, 0),
      deltaMicros: members.reduce((sum, phase) => sum + phases[phase].deltaMicros, 0),
      positiveDeltaMicros: members.reduce((sum, phase) => sum + phases[phase].positiveDeltaMicros, 0),
    },
  ]));
  const dominantGroup = Object.entries(groups)
    .sort((left, right) => right[1].positiveDeltaMicros - left[1].positiveDeltaMicros || left[0].localeCompare(right[0]))[0][0];
  return {
    sampleIndex: index,
    sampleNumber: index + 1,
    orderPosition: orderPosition(orders, index, candidate.fillPercent),
    elapsedMs: candidate.checkpoint.elapsedSamplesMs[index],
    elapsedDeltaFromP50Ms: candidate.checkpoint.elapsedSamplesMs[index] - candidate.stats.checkpointMs.p50,
    dominantGroup,
    groups,
    phases,
  };
}

function sampleReference(values, percent, orders, fillPercent) {
  const index = sampleIndex(values, percent);
  return {
    sampleIndex: index,
    sampleNumber: index + 1,
    orderPosition: orderPosition(orders, index, fillPercent),
    elapsedMs: values[index],
  };
}

function maxSampleReference(values, orders, fillPercent) {
  const index = maxSampleIndex(values);
  return {
    sampleIndex: index,
    sampleNumber: index + 1,
    orderPosition: orderPosition(orders, index, fillPercent),
    elapsedMs: values[index],
  };
}

function orderPosition(orders, sampleIndexValue, fillPercent) {
  assert(Array.isArray(orders[sampleIndexValue]), "sample order");
  const position = orders[sampleIndexValue].indexOf(fillPercent);
  assert(position >= 0, "fill in sample order");
  return position;
}

function sampleIndex(values, percent) {
  const target = percentile(values, percent);
  return values.indexOf(target);
}

function maxSampleIndex(values) {
  return values.indexOf(Math.max(...values));
}

function percentile(values, percent) {
  assert(values.length > 0 && percent >= 1 && percent <= 100, "percentile input");
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * percent / 100) - 1];
}

function assert(value, message) {
  if (!value) throw new Error(message);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [receiptPath, outputPath] = process.argv.slice(2);
  if (!receiptPath || !outputPath) process.exit(2);
  const bytes = readFileSync(receiptPath);
  const receipt = JSON.parse(bytes);
  const report = analyzeStorageLayoutVariance(receipt, sourceProvenance(receiptPath, bytes, receipt));
  writeFileSync(outputPath, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`analyzed ${receipt.schema} ${receipt.profile} variance`);
}
