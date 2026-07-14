#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import {
  analyzePointPathDensity,
  pointDensitySource,
} from "./analyze-point-path-density.mjs";

const [receiptPath, analysisPath] = process.argv.slice(2);
if (!receiptPath || !analysisPath) process.exit(2);
const receiptBytes = readFileSync(receiptPath);
const receipt = JSON.parse(receiptBytes);
const validation = spawnSync(
  process.execPath,
  ["scripts/validate-point-path-density-receipt.mjs", receiptPath, receipt.profile],
  { encoding: "utf8" },
);
if (validation.status !== 0) {
  throw new Error(`source receipt validation failed: ${validation.stderr || validation.stdout}`);
}
const actual = JSON.parse(readFileSync(analysisPath, "utf8"));
const expected = analyzePointPathDensity(
  receipt,
  pointDensitySource(receiptPath, receiptBytes, receipt),
);
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  throw new Error("point-density analysis does not match source receipt");
}
console.log(`validated ${actual.schema} ${receipt.profile}`);
