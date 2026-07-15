#!/usr/bin/env node
import fs from "node:fs";

const [receiptPath, expectedProfile] = process.argv.slice(2);
if (!receiptPath || !expectedProfile) {
  throw new Error("usage: validate-projection-tail-overlay-receipt.mjs <receipt> <smoke|full>");
}
const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const expect = (condition, message) => { if (!condition) throw new Error(message); };
const facts = expectedProfile === "full" ? 1_000_000 : 10_000;
const tailFacts = 1_024;
let baseCount = 0;
let baseChecksum = 0n;
for (let value = 0; value < facts; value += 1) {
  if (value % 4 === 0 || value % 4 === 1) {
    baseCount += 1;
    baseChecksum += BigInt(value);
  }
}
let tailChecksum = 0n;
for (let value = facts; value < facts + tailFacts; value += 1) {
  tailChecksum += BigInt(value);
}
const expectedCount = baseCount + tailFacts;
const expectedChecksum = baseChecksum + tailChecksum;
const exact = receipt.expectedCount === expectedCount
  && BigInt(receipt.expectedChecksum) === expectedChecksum
  && receipt.observedCount === expectedCount
  && BigInt(receipt.observedChecksum) === expectedChecksum;
const refresh = receipt.refreshDiagnostics;
const cached = receipt.cachedDiagnostics;
const routed = refresh.completedScans === 1
  && refresh.ledgerFallbacks === 0
  && refresh.tailRefreshes === 1
  && refresh.tailFactsVisited === tailFacts
  && refresh.tailEntitiesRebuilt === tailFacts
  && refresh.tailBudgetFallbacks === 0
  && cached.completedScans === 1
  && cached.ledgerFallbacks === 0
  && cached.tailCacheHits === 1
  && cached.tailBudgetFallbacks === 0;
const pageBacked = refresh.pagesRead > 0
  && refresh.fullImageDecodes === 0
  && cached.pagesRead > 0
  && cached.fullImageDecodes === 0;
const p50 = receipt.residentTail.p50Ms <= 230;
const noTailRelative = receipt.residentTail.p50Ms <= receipt.noTail.p50Ms * 1.15;
const tail = receipt.residentTail.p95Ms <= receipt.residentTail.p50Ms * 1.15;
const queryRss = receipt.peakQueryRssDeltaBytes <= 2 * 1024 * 1024;

expect(receipt.schema === "vicia.projection-tail-overlay.v1", "schema mismatch");
expect(receipt.profile === expectedProfile, "profile mismatch");
expect(receipt.facts === facts && receipt.tailFacts === tailFacts, "fact count mismatch");
expect(receipt.fixture?.schema === "vicia.temporal-projection-fixture.v1", "fixture schema mismatch");
expect(receipt.fixture.facts === facts && receipt.fixture.fillPercent === 90, "fixture provenance mismatch");
expect(receipt.fixture.builderSourceCommit === receipt.sourceCommit, "fixture source mismatch");
expect(receipt.admissionEligible === (expectedProfile === "full"), "admission eligibility mismatch");
expect(receipt.trackedClean || expectedProfile === "smoke", "full receipt requires clean source");
expect(exact, "raw resident-tail aggregate is not exact");
expect(routed, "resident tail did not refresh and reuse through the projection route");
expect(pageBacked, "resident-tail projection route was not page-backed");
expect(receipt.gates.exact === exact, "exact gate is not derived from raw results");
expect(receipt.gates.routed === routed, "routing gate mismatch");
expect(receipt.gates.pageBacked === pageBacked, "page-backed gate mismatch");
expect(receipt.gates.p50 === p50, "p50 gate mismatch");
expect(receipt.gates.noTailRelative === noTailRelative, "no-tail-relative gate mismatch");
expect(receipt.gates.tail === tail, "tail gate mismatch");
expect(receipt.gates.queryRss === queryRss, "RSS gate mismatch");
const admitted = receipt.admissionEligible && Object.values(receipt.gates).every(Boolean);
expect(receipt.admitted === admitted, "admission verdict mismatch");
expect(receipt.fileFormatChanged === false, "file format changed");
expect(receipt.publicApiChanged === false, "public API changed");
expect(receipt.arbitraryDatalogRoutingChanged === false, "arbitrary Datalog routing changed");
process.stdout.write(`projection tail overlay receipt valid: admitted=${receipt.admitted}\n`);
