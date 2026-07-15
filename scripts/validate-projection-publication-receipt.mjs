#!/usr/bin/env node
import fs from "node:fs";

const [receiptPath, expectedProfile] = process.argv.slice(2);
if (!receiptPath || !expectedProfile) {
  throw new Error("usage: validate-projection-publication-receipt.mjs <receipt> <smoke|full>");
}
const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
const fail = (message) => { throw new Error(message); };
const expect = (condition, message) => { if (!condition) fail(message); };
const pageSize = 4096;

expect(receipt.schema === "vicia.projection-publication.v1", "schema mismatch");
expect(receipt.profile === expectedProfile, "profile mismatch");
expect(receipt.facts === (expectedProfile === "full" ? 1_000_000 : 10_000), "fact count mismatch");
expect(receipt.admissionEligible === (expectedProfile === "full"), "admission eligibility mismatch");
expect(receipt.trackedClean || expectedProfile === "smoke", "full receipt requires clean source");
expect(receipt.fixture?.schema === "vicia.temporal-projection-fixture.v1", "fixture schema mismatch");
expect(receipt.fixture.facts === receipt.facts, "fixture fact count mismatch");
expect(receipt.fixture.fillPercent === 90, "fixture fill mismatch");
expect(receipt.fixture.builderSourceCommit === receipt.sourceCommit, "fixture source mismatch");
expect(receipt.sourceFormatVersion === 12, "source must be v12");
expect(receipt.publishedFormatVersion === 13, "published image must be v13");
expect(receipt.generation > 0, "catalog generation must be positive");
expect(receipt.imagePageCount > 0, "image range must be non-empty");
expect(receipt.catalogPageCount === 1, "single-entry catalog must occupy one page");
expect(receipt.catalogPageStart === receipt.imagePageStart + receipt.imagePageCount, "catalog must follow image");
expect(receipt.publishedPageCount === receipt.catalogPageStart + receipt.catalogPageCount, "published page count mismatch");
expect(receipt.projectionIdentity.paddedBytes === receipt.imagePageCount * pageSize, "image byte count mismatch");
expect(receipt.publishedBytes === receipt.sourceBytes + (receipt.imagePageCount + receipt.catalogPageCount) * pageSize, "file growth mismatch");
expect(receipt.publishedBytes === receipt.publishedPageCount * pageSize, "published file length mismatch");
expect(Number.isFinite(receipt.publishMs) && receipt.publishMs >= 0, "publish timing invalid");
expect(Number.isFinite(receipt.reopenDecodeMs) && receipt.reopenDecodeMs >= 0, "reopen timing invalid");

const expectedPair = (facts, validAt) => {
  const boundary = 1_735_689_600_000;
  const after = boundary + 2;
  let count = 0;
  let checksum = 0n;
  for (let value = 0; value < facts; value += 1) {
    const remainder = value % 4;
    const visible = validAt < boundary
      ? remainder === 0 || remainder === 2
      : validAt < after
        ? remainder === 0 || remainder === 1 || remainder === 3
        : remainder === 0 || remainder === 1;
    if (visible) {
      count += 1;
      checksum += BigInt(value);
    }
  }
  return [count, checksum.toString()];
};
expect(Array.isArray(receipt.probes) && receipt.probes.length === 3, "probe count mismatch");
const derivedExact = receipt.probes.every((probe) => {
  const [count, checksum] = expectedPair(receipt.facts, probe.validAt);
  return probe.count === count && String(probe.checksum) === checksum;
});
expect(receipt.exact === derivedExact, "stored exact verdict is not derived from raw probes");
expect(derivedExact, "persisted probes are not exact");
const derivedAdmitted = receipt.admissionEligible
  && derivedExact
  && receipt.sourceFormatVersion === 12
  && receipt.publishedFormatVersion === 13
  && receipt.catalogPageCount === 1;
expect(receipt.admitted === derivedAdmitted, "stored admission verdict mismatch");
expect(receipt.productionQueryRoutingChanged === false, "production routing changed");
expect(receipt.publicApiChanged === false, "public API changed");
expect(receipt.defaultWriteFormatChanged === false, "default writer changed");

process.stdout.write(`projection publication receipt valid: admitted=${receipt.admitted}\n`);
