import { readFileSync } from "node:fs";

const [path, profile] = process.argv.slice(2);
if (!path || !["smoke", "full"].includes(profile)) process.exit(2);
const receipt = JSON.parse(readFileSync(path, "utf8"));
const fail = (message) => { throw new Error(message); };
if (receipt.schema !== "vicia.current-reader.v1") fail("schema");
if (receipt.profile !== profile) fail("profile");
if (receipt.facts !== (profile === "full" ? 1_000_000 : 10_000)) fail("fact count");
if (receipt.samples !== (profile === "full" ? 20 : 5)) fail("sample count");
if (!/^[0-9a-f]{40}$/.test(receipt.provenance?.sourceCommit ?? "")) fail("source commit");
if (profile === "full" && receipt.provenance?.sourceDirty !== false) fail("full source must be clean");
for (const [name, result] of Object.entries(receipt.reads)) {
  if (result.rows !== 1) fail(`${name} row count`);
  if (!Number.isFinite(result.p50Ms) || !Number.isFinite(result.p95Ms) || result.p50Ms < 0 || result.p95Ms < result.p50Ms) fail(`${name} latency summary`);
  if (result.p95Ms > 10) fail(`${name} p95 budget`);
  const diagnostics = result.diagnostics;
  if (diagnostics.fullLeafVecPeakEntries !== 0 || diagnostics.fullLeafVecPeakStructBytes !== 0 || diagnostics.fullLeafVecPeakDecodedPayloadBytes !== 0) fail(`${name} full leaf materialization`);
  if (diagnostics.leafPagesVisited > 2) fail(`${name} leaf selectivity`);
}
if (receipt.reads.entities.diagnostics.projectedOwnedEavtDecodes !== 0) fail("owned EAVT decode");
if (receipt.reads.entities.diagnostics.projectedEavtEmitted !== 1) fail("EAVT emitted");
if (receipt.reads.refsTo.diagnostics.projectedOwnedVaetDecodes !== 0) fail("owned VAET decode");
if (receipt.reads.refsTo.diagnostics.projectedVaetEmitted !== 1) fail("VAET emitted");
if (!receipt.passed) fail("receipt verdict");
console.log(`validated ${receipt.schema} ${profile}`);
