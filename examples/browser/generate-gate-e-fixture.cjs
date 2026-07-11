// Generate tests/fixtures/gate_e/browser.graph through the real BrowserDb
// WASM facade. Run from a served repository checkout:
//
//   wasm-pack build --target web --features browser --out-dir minigraf-wasm
//   python3 -m http.server 8123
//   CHROME_PATH=/path/to/chrome NODE_PATH=/path/to/node_modules \
//     node examples/browser/generate-gate-e-fixture.cjs

const fs = require("node:fs");
const path = require("node:path");
const puppeteer = require("puppeteer");

const chrome = process.env.CHROME_PATH;
if (!chrome) {
  throw new Error("CHROME_PATH is required");
}

const pageUrl =
  process.env.GATE_E_PAGE ??
  "http://127.0.0.1:8123/examples/browser/generate-gate-e-fixture.html";
const output = path.resolve(
  process.env.GATE_E_OUTPUT ?? "tests/fixtures/gate_e/browser.graph",
);

(async () => {
  const browser = await puppeteer.launch({
    executablePath: chrome,
    headless: true,
    args: ["--disable-gpu"],
  });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(120_000);
    await page.goto(pageUrl, { waitUntil: "load" });
    await page.waitForFunction(() => typeof window.generateGateEFixture === "function");
    const result = await page.evaluate(() => window.generateGateEFixture());
    const bytes = Buffer.from(result.base64, "base64");
    if (bytes.length !== result.byteLength) {
      throw new Error(`base64 length mismatch: ${bytes.length} != ${result.byteLength}`);
    }
    fs.writeFileSync(output, bytes);
    process.stdout.write(`Written: ${output} (${bytes.length} bytes)\n`);
  } finally {
    await browser.close();
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
