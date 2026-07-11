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

async function withPage(fn, { extraArgs = [], forwardConsole = false } = {}) {
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: true,
    protocolTimeout: 1_800_000,
    userDataDir: PROFILE,
    args: ["--enable-precise-memory-info", "--disable-gpu", ...extraArgs],
  });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(1_800_000);
    page.on("pageerror", (e) => console.error("pageerror:", e.message));
    if (forwardConsole) {
      page.on("console", (m) => console.log(m.text()));
    }
    await page.goto(PAGE, { waitUntil: "load" });
    return await fn(page);
  } finally {
    await browser.close();
  }
}

// A5-2 growth mode: repeated small write executes against one live handle,
// sampling IndexedDB growth; then an export->import round-trip (measured as
// the no-remedy result); then reopen-after-growth in fresh browsers.
async function growthMain() {
  const cycles = Number(process.argv[3]);
  const factsPerCycle = Number(process.argv[4]);
  const sampleEvery = Number(process.argv[5]);
  const fixture = process.argv[6];
  const reopenRuns = Number(process.argv[7] ?? 2);
  if (!cycles || !factsPerCycle || !sampleEvery || !fixture) {
    console.error(
      "usage: bench-driver.cjs growth <cycles> <factsPerCycle> <sampleEvery> <fixture-url-path|empty> [reopenRuns]",
    );
    process.exit(1);
  }

  // Phase A: clean slate (+ optional fixture base) in its own browser.
  await withPage(async (page) => {
    console.log("reset:", await page.evaluate(() => window.benchReset()));
    if (fixture !== "empty") {
      console.log(
        "import:",
        await page.evaluate((u) => window.benchImport(u), fixture),
      );
    }
  });

  // Phase B: growth loop + round-trip in one long-lived page. Samples stream
  // through forwarded console lines; --expose-gc reduces heap-sample noise.
  await withPage(
    async (page) => {
      console.log(
        "growthSamples:",
        await page.evaluate(
          (c, f, s) => window.benchGrowth(c, f, s),
          cycles,
          factsPerCycle,
          sampleEvery,
        ),
      );
      console.log(
        "roundTrip:",
        await page.evaluate(() => window.benchGrowthRoundTrip()),
      );
    },
    { extraArgs: ["--js-flags=--expose-gc"], forwardConsole: true },
  );

  // Phase C: reopen-after-growth — open cost now tracks IDB size, not
  // logical size.
  for (let i = 0; i < reopenRuns; i++) {
    await withPage(async (page) => {
      console.log(
        `reopen[${i}]:`,
        await page.evaluate(() => window.benchOpen()),
      );
    });
  }
}

// Legacy A0 mode: import once, then measure open() in fresh browsers.
async function openMain() {
  const fixture = process.argv[2];
  const runs = Number(process.argv[3] ?? 3);
  if (!fixture) {
    console.error(
      "usage: bench-driver.cjs <fixture-url-path|skip-import> [runs]\n" +
        "       bench-driver.cjs growth <cycles> <factsPerCycle> <sampleEvery> <fixture-url-path|empty> [reopenRuns]",
    );
    process.exit(1);
  }
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
}

if (process.argv[2] === "growth") {
  growthMain();
} else {
  openMain();
}
