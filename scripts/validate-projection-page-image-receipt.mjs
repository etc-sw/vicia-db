#!/usr/bin/env node
import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
const facts = profile === "full" ? 1_000_000 : 10_000;
const samples = profile === "full" ? 20 : 5;
const boundary = 1_735_689_600_000;

if (receipt.schema !== "vicia.current-projection-page-image.v1") fail("schema");
if (receipt.facts !== facts || receipt.samples !== samples) fail("workload shape");
if (receipt.validTimeFloor !== boundary - 1) fail("valid-time floor");
if (!Number.isSafeInteger(receipt.graphBytes) || receipt.graphBytes <= 0) fail("graph bytes");
if (!/^[0-9a-f]{40}$/.test(receipt.provenance?.sourceCommit ?? "")) fail("source commit");
if (profile === "full" && receipt.provenance?.sourceDirty !== false) fail("full source must be clean");

const identity = receipt.identity;
if (!Number.isSafeInteger(identity?.baseGeneration) || identity.baseGeneration <= 0) fail("base generation");
if (!Number.isSafeInteger(identity?.manifestGeneration) || identity.manifestGeneration < 0) fail("manifest generation");
if (!Number.isSafeInteger(identity?.txCount) || identity.txCount <= 0) fail("tx watermark");

const image = receipt.image;
if (!image || image.rowCount !== facts || !/^[0-9a-f]{16}$/.test(image.fingerprint ?? "")) fail("image identity");
if (!Number.isSafeInteger(image.pageCount) || image.pageCount <= 0
  || image.paddedBytes !== image.pageCount * 4096
  || image.logicalBytes <= 4096
  || image.paddingBytes !== image.paddedBytes - image.logicalBytes
  || image.paddingBytes < 0 || image.paddingBytes >= 7 * 4096) fail("page layout");
const sizeLimit = profile === "full" ? 0.15 : 0.30;
if (image.imageRatio > sizeLimit || image.paddedBytes > receipt.graphBytes * sizeLimit) fail("image size");

validateTiming("encode", receipt.encode);
validateTiming("decode", receipt.decode);
if (receipt.encode.p50Ms > 438.063 || receipt.decode.p50Ms > 438.063) fail("codec latency");
const timingTail = profile === "full" ? 1.25 : 2;
if (receipt.encode.p95Ms > receipt.encode.p50Ms * timingTail
  || receipt.decode.p95Ms > receipt.decode.p50Ms * timingTail) fail("codec tail");
if (receipt.maintenancePeakRssDeltaBytes > 128 * 1024 * 1024) fail("maintenance peak RSS");
if (receipt.queryRssDeltaBytes > 2 * 1024 * 1024) fail("query RSS");

const probes = [
  ["beforeBoundary", boundary - 1],
  ["atBoundary", boundary],
  ["afterBoundary", boundary + 2],
];
if (!Array.isArray(receipt.probes) || receipt.probes.length !== probes.length) fail("probes");
for (let index = 0; index < probes.length; index += 1) {
  const [name, validAt] = probes[index];
  const probe = receipt.probes[index];
  if (probe?.name !== name || probe.validAt !== validAt) fail("probe identity");
  const expected = expectedPair(facts, validAt);
  validateAggregate(name + " ledger", probe.baseline, expected, samples);
  validateAggregate(name + " image", probe.projection, expected, samples);
  if (probe.projection.p50Ms > 150) fail(name + " query latency");
  const queryTail = profile === "full" ? 1.15 : 1.5;
  if (probe.projection.p95Ms > probe.projection.p50Ms * queryTail) fail(name + " query tail");
}

const proof = receipt.proof ?? {};
for (const field of ["roundTrip", "deterministicRebuild", "overlayFlatten"]) {
  if (proof[field] !== true) fail("proof " + field);
}
for (const field of ["productionQueryRoutingChanged", "publicApiChanged", "fileFormatChanged"]) {
  if (proof[field] !== false) fail("scope " + field);
}

console.log("validated " + receipt.schema + " " + profile);

function validateTiming(name, timing) {
  if (!timing || !Array.isArray(timing.samplesMs) || timing.samplesMs.length !== samples
    || !Number.isFinite(timing.p50Ms) || !Number.isFinite(timing.p95Ms)
    || timing.p50Ms <= 0 || timing.p95Ms < timing.p50Ms) fail(name + " timing");
}

function validateAggregate(name, aggregate, expected, expectedSamples) {
  if (!aggregate || aggregate.count !== expected[0] || aggregate.checksum !== expected[1]
    || !Array.isArray(aggregate.samplesMs) || aggregate.samplesMs.length !== expectedSamples
    || !Number.isFinite(aggregate.p50Ms) || !Number.isFinite(aggregate.p95Ms)
    || aggregate.p50Ms <= 0 || aggregate.p95Ms < aggregate.p50Ms) fail(name + " aggregate");
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
