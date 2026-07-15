#!/usr/bin/env node
import fs from "node:fs";

const [receiptPath] = process.argv.slice(2);
if (!receiptPath) throw new Error("usage: validate-browser-projection-maintenance-receipt.mjs <receipt>");
const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const expect = (condition, message) => { if (!condition) throw new Error(message); };
const outcome = receipt.maintenance?.result?.outcome;
const before = receipt.maintenance?.result?.before;
const after = receipt.maintenance?.result?.after;

expect(receipt.schema === "vicia.browser-projection-maintenance.v1", "schema mismatch");
expect(receipt.chromeVersion === "Google Chrome for Testing 150.0.7871.115", "Chrome version mismatch");
expect(receipt.source?.trackedClean === true, "source must be clean");
expect(/^[0-9a-f]{40}$/.test(receipt.source.commit), "source commit invalid");
expect(/^[0-9a-f]{64}$/.test(receipt.source.fixtureSha256), "fixture hash invalid");
expect(/^[0-9a-f]{64}$/.test(receipt.source.wasmSha256), "WASM hash invalid");
expect(receipt.import?.result?.fixtureBytes === 247_562_240, "fixture bytes mismatch");
expect(receipt.import.result.stats.headerVersion === 12, "import format mismatch");
expect(receipt.import.result.stats.headerNodeCount === 1_000_000, "fixture fact count mismatch");

const exactAuthority = outcome.attribute_count === 1
  && outcome.row_count === 500_000
  && outcome.tx_count === 1_000
  && outcome.before_pages === 60_440
  && outcome.after_pages === 64_477
  && outcome.projection_bytes === 16_531_456
  && before.headerVersion === 12
  && before.headerPageCount === outcome.before_pages
  && after.headerVersion === 13
  && after.headerPageCount === outcome.after_pages
  && after.idbCount === outcome.after_pages
  && receipt.maintenance.result.workerTerminated === true;
const gates = {
  exactAuthority,
  elapsed: receipt.maintenance.result.elapsedMs <= 30_000,
  imageBudget: outcome.projection_bytes <= receipt.import.result.fixtureBytes * 0.15,
  rss: receipt.maintenance.rss.peakDeltaBytes <= 1024 * 1024 * 1024,
  pss: receipt.maintenance.pss.peakDeltaBytes <= 1024 * 1024 * 1024,
};
for (const [name, value] of Object.entries(gates)) {
  expect(receipt.gates?.[name] === value, `${name} gate mismatch`);
  expect(value, `${name} gate failed`);
}
expect(receipt.admissionEligible === true, "admission eligibility mismatch");
expect(receipt.admitted === Object.values(gates).every(Boolean), "admission verdict mismatch");
expect(receipt.admitted, "browser projection maintenance not admitted");
process.stdout.write("browser projection maintenance receipt valid: admitted=true\n");
