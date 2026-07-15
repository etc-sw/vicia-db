#!/usr/bin/env node
import fs from "node:fs";

const [receiptPath, expectedProfile] = process.argv.slice(2);
if (!receiptPath || !expectedProfile) {
  throw new Error("usage: validate-projection-routing-receipt.mjs <receipt> <smoke|full>");
}
const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const expect = (condition, message) => { if (!condition) throw new Error(message); };
const facts = expectedProfile === "full" ? 1_000_000 : 10_000;
let expectedCount = 0;
let expectedChecksum = 0n;
for (let value = 0; value < facts; value += 1) {
  if (value % 4 === 0 || value % 4 === 1) {
    expectedCount += 1;
    expectedChecksum += BigInt(value);
  }
}
const exact = receipt.expectedCount === expectedCount
  && BigInt(receipt.expectedChecksum) === expectedChecksum
  && receipt.ledgerCount === expectedCount
  && BigInt(receipt.ledgerChecksum) === expectedChecksum
  && receipt.projectedCount === expectedCount
  && BigInt(receipt.projectedChecksum) === expectedChecksum;
const routed = receipt.projectionDiagnostics.routeAttempts === 1
  && receipt.projectionDiagnostics.completedScans === 1
  && receipt.projectionDiagnostics.ledgerFallbacks === 0;
const pageBacked = receipt.projectionDiagnostics.pagesRead > 0
  && receipt.projectionDiagnostics.fullImageDecodes === 0;
const routeP50 = receipt.projected.p50Ms <= 230;
const improvement = receipt.projected.p50Ms <= receipt.ledger.p50Ms * 0.9;
const tail = receipt.projected.p95Ms <= receipt.projected.p50Ms * 1.15;
const queryRss = receipt.peakQueryRssDeltaBytes <= 2 * 1024 * 1024;

expect(receipt.schema === "vicia.projection-routing.v1", "schema mismatch");
expect(receipt.profile === expectedProfile, "profile mismatch");
expect(receipt.facts === facts, "fact count mismatch");
expect(receipt.fixture?.schema === "vicia.temporal-projection-fixture.v1", "fixture schema mismatch");
expect(receipt.fixture.facts === facts && receipt.fixture.fillPercent === 90, "fixture provenance mismatch");
expect(receipt.fixture.builderSourceCommit === receipt.sourceCommit, "fixture source mismatch");
expect(receipt.admissionEligible === (expectedProfile === "full"), "admission eligibility mismatch");
expect(receipt.trackedClean || expectedProfile === "smoke", "full receipt requires clean source");
expect(exact, "raw ledger/projected aggregate is not exact");
expect(routed, "eligible query did not complete through the projection");
expect(pageBacked, "projection route was not page-backed");
expect(receipt.gates.exact === exact, "exact gate is not derived from raw results");
expect(receipt.gates.routed === routed, "routing gate mismatch");
expect(receipt.gates.pageBacked === pageBacked, "page-backed gate mismatch");
expect(receipt.gates.routeP50 === routeP50, "p50 gate mismatch");
expect(receipt.gates.improvement === improvement, "improvement gate mismatch");
expect(receipt.gates.tail === tail, "tail gate mismatch");
expect(receipt.gates.queryRss === queryRss, "RSS gate mismatch");
const admitted = receipt.admissionEligible && Object.values(receipt.gates).every(Boolean);
expect(receipt.admitted === admitted, "admission verdict mismatch");
expect(receipt.defaultWriteFormatChanged === false, "default write format changed");
expect(receipt.arbitraryDatalogRoutingChanged === false, "arbitrary Datalog routing changed");
process.stdout.write(`projection routing receipt valid: admitted=${receipt.admitted}\n`);
