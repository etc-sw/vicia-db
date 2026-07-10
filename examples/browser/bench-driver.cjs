// A0 browser open-at-scale driver — drives bench.html headlessly and prints
// import/open/heap measurements as JSON lines. Runner steps are documented in
// docs/BENCHMARKS.md ("Browser Open at Scale").
//
//   CHROME_PATH=<chrome binary> NODE_PATH=<dir with puppeteer> \
//     node examples/browser/bench-driver.cjs <fixture-url-path|skip-import> [runs]
//
// Requires bench.html served (python3 -m http.server 8123 from repo root) and
// fixtures from examples/generate_bench_fixture.rs. Imports in one browser,
// then measures open() in a fresh browser per run — a shared user-data-dir
// keeps IndexedDB while each run starts with a clean renderer heap.

const puppeteer = require("puppeteer");
const os = require("os");
const path = require("path");

const CHROME = process.env.CHROME_PATH;
const PAGE =
  process.env.BENCH_PAGE ?? "http://localhost:8123/examples/browser/bench.html";
const PROFILE =
  process.env.BENCH_PROFILE ?? path.join(os.tmpdir(), "minigraf-bench-profile");
const fixture = process.argv[2];
const runs = Number(process.argv[3] ?? 3);

if (!fixture) {
  console.error("usage: bench-driver.cjs <fixture-url-path|skip-import> [runs]");
  process.exit(1);
}

async function withPage(fn) {
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: true,
    protocolTimeout: 1_800_000,
    userDataDir: PROFILE,
    args: ["--enable-precise-memory-info", "--disable-gpu"],
  });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(1_800_000);
    page.on("pageerror", (e) => console.error("pageerror:", e.message));
    await page.goto(PAGE, { waitUntil: "load" });
    return await fn(page);
  } finally {
    await browser.close();
  }
}

(async () => {
  if (fixture !== "skip-import") {
    await withPage(async (page) => {
      console.log("reset:", await page.evaluate(() => window.benchReset()));
      console.log(
        "import:",
        await page.evaluate((u) => window.benchImport(u), fixture),
      );
    });
  }
  for (let i = 0; i < runs; i++) {
    await withPage(async (page) => {
      console.log(
        `open[${i}]:`,
        await page.evaluate(() => window.benchOpen()),
      );
    });
  }
})();
