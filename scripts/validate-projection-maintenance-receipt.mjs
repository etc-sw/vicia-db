#!/usr/bin/env node
import fs from "node:fs";

const [receiptPath, expectedProfile] = process.argv.slice(2);
if (!receiptPath || !expectedProfile) {
  throw new Error("usage: validate-projection-maintenance-receipt.mjs <receipt> <smoke|full>");
}
const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const expect = (condition, message) => { if (!condition) throw new Error(message); };
const facts = expectedProfile === "full" ? 1_000_000 : 10_000;
const pageSize = 4096;

expect(receipt.schema === "vicia.projection-maintenance.v1", "schema mismatch");
expect(receipt.profile === expectedProfile, "profile mismatch");
expect(receipt.facts === facts, "fact count mismatch");
expect(receipt.admissionEligible === (expectedProfile === "full"), "admission eligibility mismatch");
expect(receipt.trackedClean || expectedProfile === "smoke", "full receipt requires clean source");
expect(receipt.fixture?.schema === "vicia.temporal-projection-fixture.v1", "fixture schema mismatch");
expect(receipt.fixture.facts === facts, "fixture fact count mismatch");
expect(receipt.fixture.fillPercent === 90, "fixture fill mismatch");
expect(receipt.fixture.builderSourceCommit === receipt.sourceCommit, "fixture source mismatch");
expect(receipt.checkpoint === "noop", "clean fixture rebuild should not checkpoint");
expect(receipt.generation === 1, "first maintenance generation mismatch");
expect(receipt.baseGeneration > 0 && receipt.manifestGeneration === 0, "ledger identity mismatch");
expect(receipt.txCount === facts / 1000, "transaction watermark mismatch");
expect(receipt.attributeCount === 1, "attribute count mismatch");
expect(receipt.rowCount > 0 && receipt.rowCount <= facts, "row count mismatch");
expect(receipt.projectionBytes > 0 && receipt.projectionBytes % pageSize === 0, "projection bytes invalid");
expect(receipt.beforePages * pageSize === receipt.sourceBytes, "source page count mismatch");
expect(receipt.afterPages * pageSize === receipt.publishedBytes, "published page count mismatch");
expect(receipt.afterPages > receipt.beforePages, "publication did not grow page authority");
expect(receipt.arenaReused === false, "first publication cannot reuse an arena");
expect(Number.isFinite(receipt.elapsedMs) && receipt.elapsedMs >= 0, "elapsed timing invalid");
expect(Number.isSafeInteger(receipt.baselineRssBytes) && receipt.baselineRssBytes > 0, "RSS baseline invalid");
expect(Number.isSafeInteger(receipt.peakRssDeltaBytes), "RSS delta invalid");

const remainderCounts = [Math.ceil(facts / 4), Math.floor((facts + 2) / 4)];
const expectedCount = remainderCounts[0] + remainderCounts[1];
let expectedChecksum = 0n;
for (let value = 0; value < facts; value += 1) {
  if (value % 4 === 0 || value % 4 === 1) expectedChecksum += BigInt(value);
}
const derivedExact = receipt.aggregateCount === expectedCount
  && BigInt(receipt.aggregateChecksum) === expectedChecksum;
expect(receipt.exact === derivedExact, "stored exactness is not derived from raw aggregate");
expect(receipt.gates.exact === derivedExact, "exact gate mismatch");
expect(receipt.gates.elapsed === (receipt.elapsedMs <= 1500), "elapsed gate mismatch");
expect(receipt.gates.imageBudget === (receipt.projectionBytes <= Math.floor(receipt.sourceBytes * 0.15)), "image gate mismatch");
expect(receipt.gates.peakRss === (receipt.peakRssDeltaBytes <= 128 * 1024 * 1024), "RSS gate mismatch");
const derivedAdmitted = receipt.admissionEligible && Object.values(receipt.gates).every(Boolean);
expect(receipt.admitted === derivedAdmitted, "admission verdict mismatch");
expect(receipt.productionQueryRoutingChanged === false, "production routing changed");
expect(receipt.defaultWriteFormatChanged === false, "default writer changed");

process.stdout.write(`projection maintenance receipt valid: admitted=${receipt.admitted}\n`);
