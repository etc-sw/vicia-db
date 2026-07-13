#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import {
  analyzeStorageLayoutVariance,
  sourceProvenance,
} from "./analyze-storage-layout-variance.mjs";

const [receiptPath, reportPath] = process.argv.slice(2);
if (!receiptPath || !reportPath) process.exit(2);

const receiptBytes = readFileSync(receiptPath);
const receipt = JSON.parse(receiptBytes);
const sourceValidation = spawnSync(
  process.execPath,
  ["scripts/validate-storage-layout-receipt.mjs", receiptPath, receipt.profile],
  { encoding: "utf8" },
);
if (sourceValidation.status !== 0) {
  throw new Error(`source receipt validation failed: ${sourceValidation.stderr || sourceValidation.stdout}`);
}

const actual = JSON.parse(readFileSync(reportPath, "utf8"));
const expected = analyzeStorageLayoutVariance(
  receipt,
  sourceProvenance(receiptPath, receiptBytes, receipt),
);
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  throw new Error("variance report does not match the source receipt");
}

console.log(`validated ${actual.schema} ${receipt.profile}`);
