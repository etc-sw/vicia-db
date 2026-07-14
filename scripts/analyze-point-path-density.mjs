#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const STRUCTURAL_FIELDS = [
  "internalPagesVisited",
  "internalKeyComparisons",
  "internalKeyBytesDecoded",
  "leafEntriesAvailable",
  "rawLowerBoundKeyComparisons",
  "prefixRestartKeyComparisons",
  "prefixLinearKeyComparisons",
  "leafEntriesDecoded",
  "exactFactResolutions",
  "factPageCacheMisses",
];
const PHASE_FIELDS = [
  "internalDescentElapsedNs",
  "leafSeekElapsedNs",
  "rawDecodeElapsedNs",
  "prefixDecodeElapsedNs",
  "exactFactResolutionElapsedNs",
];
const POINT_LIMIT = 1.2;
const CORRELATION_LIMIT = 0.8;

export function analyzePointPathDensity(receipt, source) {
  const baseline = receipt.candidates.find((candidate) => candidate.fillPercent === 75);
  if (!baseline) throw new Error("fill-75 baseline missing");
  const baselineMedians = diagnosticMedians(baseline.diagnosticsSamples);
  const candidates = receipt.candidates.map((candidate) => {
    const medians = diagnosticMedians(candidate.diagnosticsSamples);
    return {
      fillPercent: candidate.fillPercent,
      graphBytes: candidate.graphBytes,
      eavt: {
        height: candidate.layout.eavt.height,
        rawLeafPages: candidate.layout.eavt.rawLeafPages,
        prefixLeafPages: candidate.layout.eavt.prefixLeafPages,
        leafEntries: candidate.layout.eavt.leaf.entries,
      },
      point: {
        statsMs: candidate.stats,
        p50RatioToFill75: candidate.stats.p50 / baseline.stats.p50,
        p95RatioToFill75: candidate.stats.p95 / baseline.stats.p95,
      },
      diagnosticMedians: medians,
      diagnosticRatiosToFill75: Object.fromEntries(
        Object.keys(medians).map((field) => [field, ratio(medians[field], baselineMedians[field])]),
      ),
    };
  });
  const systematicFills = candidates
    .filter((candidate) => candidate.fillPercent > 75 && candidate.point.p50RatioToFill75 > POINT_LIMIT)
    .map((candidate) => candidate.fillPercent);
  const tailOnlyFills = candidates
    .filter((candidate) => candidate.fillPercent > 75 && candidate.point.p50RatioToFill75 <= POINT_LIMIT && candidate.point.p95RatioToFill75 > POINT_LIMIT)
    .map((candidate) => candidate.fillPercent);
  const p50Values = candidates.map((candidate) => candidate.point.statsMs.p50);
  const fieldEvidence = Object.fromEntries([...STRUCTURAL_FIELDS, ...PHASE_FIELDS].map((field) => {
    const values = candidates.map((candidate) => candidate.diagnosticMedians[field]);
    const allSystematicAboveBaseline = systematicFills.length > 0 && systematicFills.every((fill) => {
      const candidate = candidates.find((entry) => entry.fillPercent === fill);
      return candidate.diagnosticMedians[field] > baselineMedians[field];
    });
    const correlation = pearson(values, p50Values);
    return [field, {
      kind: STRUCTURAL_FIELDS.includes(field) ? "structural" : "phase",
      medians: Object.fromEntries(candidates.map((candidate, index) => [candidate.fillPercent, values[index]])),
      allSystematicAboveBaseline,
      correlationWithPointP50: correlation,
      admittedOwner: allSystematicAboveBaseline && correlation >= CORRELATION_LIMIT,
    }];
  }));
  const admittedOwners = Object.entries(fieldEvidence)
    .filter(([, evidence]) => evidence.admittedOwner)
    .map(([field]) => field);
  return {
    schema: "vicia.point-path-density-analysis.v1",
    source,
    policy: {
      pointRegressionLimit: POINT_LIMIT,
      ownerCorrelationMinimum: CORRELATION_LIMIT,
      ownerRequiresEverySystematicFillAboveBaseline: true,
      structuralFields: STRUCTURAL_FIELDS,
      phaseFields: PHASE_FIELDS,
    },
    candidates,
    fieldEvidence,
    verdict: {
      systematicFills,
      tailOnlyFills,
      admittedOwners,
      classification: admittedOwners.length > 0 ? "production-owner-identified" : "no-production-owner",
      implementationAdmitted: admittedOwners.length > 0,
      rolloutAuthorized: false,
    },
  };
}

export function pointDensitySource(receiptPath, bytes, receipt) {
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

function diagnosticMedians(samples) {
  return Object.fromEntries([...STRUCTURAL_FIELDS, ...PHASE_FIELDS].map((field) => [
    field,
    percentile(samples.map((sample) => sample[field]), 50),
  ]));
}
function percentile(values, percent) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * percent / 100) - 1];
}
function ratio(value, baseline) {
  if (baseline === 0) return value === 0 ? 1 : null;
  return value / baseline;
}
function pearson(left, right) {
  const leftMean = left.reduce((sum, value) => sum + value, 0) / left.length;
  const rightMean = right.reduce((sum, value) => sum + value, 0) / right.length;
  let numerator = 0;
  let leftSquared = 0;
  let rightSquared = 0;
  for (let index = 0; index < left.length; index += 1) {
    const leftDelta = left[index] - leftMean;
    const rightDelta = right[index] - rightMean;
    numerator += leftDelta * rightDelta;
    leftSquared += leftDelta * leftDelta;
    rightSquared += rightDelta * rightDelta;
  }
  const denominator = Math.sqrt(leftSquared * rightSquared);
  return denominator === 0 ? 0 : numerator / denominator;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [receiptPath, outputPath] = process.argv.slice(2);
  if (!receiptPath || !outputPath) process.exit(2);
  const bytes = readFileSync(receiptPath);
  const receipt = JSON.parse(bytes);
  const analysis = analyzePointPathDensity(receipt, pointDensitySource(receiptPath, bytes, receipt));
  writeFileSync(outputPath, `${JSON.stringify(analysis, null, 2)}\n`);
  console.log(`analyzed ${receipt.schema} ${receipt.profile}`);
}
