#!/usr/bin/env node

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const [browserReceiptPath, fixturePath = "benchmarks/fixtures/vetch-ledger-caller.v1.json"] = process.argv.slice(2);
if (!browserReceiptPath) process.exit(2);
const browserReceipt = JSON.parse(readFileSync(browserReceiptPath, "utf8"));
const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
const directory = mkdtempSync(path.join(tmpdir(), "vicia-ledger-caller-validator-"));

try {
  rejectBrowserMutation("sample-count", (receipt) => {
    receipt.evidence.measured.result.scenarios[0].measurements.mutationMs.pop();
  });
  rejectBrowserMutation("stage-value", (receipt) => {
    receipt.evidence.measured.result.scenarios[1].measurements.publicationMs[0] = -1;
  });
  rejectBrowserMutation("wasm-digest", (receipt) => {
    receipt.provenance.wasmSha256 = "0";
  });
  rejectFixtureMutation("missing-scenario", (candidate) => {
    candidate.scenarios.pop();
  });
  rejectFixtureMutation("duplicate-change", (candidate) => {
    candidate.scenarios[0].changes.push(structuredClone(candidate.scenarios[0].changes[0]));
  });
  console.log("audited Vetch ledger caller validator rejection");
} finally {
  rmSync(directory, { recursive: true, force: true });
}

function rejectBrowserMutation(name, mutate) {
  rejectMutation(
    name,
    browserReceipt,
    mutate,
    "scripts/validate-vetch-ledger-caller-browser-receipt.mjs",
  );
}

function rejectFixtureMutation(name, mutate) {
  rejectMutation(
    name,
    fixture,
    mutate,
    "scripts/validate-vetch-ledger-caller-fixture.mjs",
  );
}

function rejectMutation(name, source, mutate, validator) {
  const candidate = structuredClone(source);
  mutate(candidate);
  const target = path.join(directory, `${name}.json`);
  writeFileSync(target, `${JSON.stringify(candidate)}\n`);
  const result = spawnSync(process.execPath, [validator, target], { encoding: "utf8" });
  if (result.status === 0) throw new Error(`validator accepted mutated ${name}`);
}
