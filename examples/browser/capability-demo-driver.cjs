#!/usr/bin/env node

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

let puppeteer;
try {
  puppeteer = require("puppeteer");
} catch (_error) {
  puppeteer = require("puppeteer-core");
}

const chrome = process.env.CHROME_PATH;
if (!chrome) throw new Error("CHROME_PATH must point to a Chrome executable");

const pageUrl = process.env.CAPABILITY_DEMO_PAGE
  ?? "http://127.0.0.1:8080/examples/browser/?reset=1";
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "minigraf-capability-demo-"));

async function main() {
  const browser = await puppeteer.launch({
    executablePath: chrome,
    headless: true,
    userDataDir: profile,
  });
  try {
    const page = await browser.newPage();
    page.on("pageerror", (error) => console.error("pageerror:", error.message));
    await page.goto(pageUrl, { waitUntil: "load" });
    await page.waitForFunction(() => window.capabilityDemoReceipt !== undefined);
    const receipt = await page.evaluate(() => window.capabilityDemoReceipt);

    assert.equal(receipt.status, "passed", receipt.error ?? "demo failed");
    assert.deepEqual(receipt.friendNames, ["Bob"]);
    assert.deepEqual(receipt.importedNames, ["Alice", "Bob"]);
    assert.ok(receipt.exportBytes >= 4096, "export must contain a full graph page");
    assert.equal(receipt.write.durability, "published");
    assert.equal(receipt.importStatus, "imported");
    assert.deepEqual(receipt.workerOperations, ["maintenance", "export", "import"]);
    assert.equal(receipt.activeWorkerCount, 0);
    console.log(JSON.stringify(receipt, null, 2));
  } finally {
    await browser.close();
  }
}

main().finally(() => {
  fs.rmSync(profile, { recursive: true, force: true });
}).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
