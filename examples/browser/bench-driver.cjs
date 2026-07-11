// A0 browser open-at-scale driver — drives bench.html headlessly and prints
// import/open/heap measurements as JSON lines. Runner steps are documented in
// docs/BENCHMARKS.md ("Browser Open at Scale").
//
//   CHROME_PATH=<chrome binary> NODE_PATH=<dir with puppeteer> \
//     node examples/browser/bench-driver.cjs <fixture-url-path|skip-import> [runs]
//   CHROME_PATH=<chrome binary> NODE_PATH=<dir with puppeteer> \
//     node examples/browser/bench-driver.cjs paged-matrix \
//       <fixture-url-path> [openRuns] [growthCycles]
//
// Requires bench.html served (python3 -m http.server 8123 from repo root) and
// fixtures from examples/generate_bench_fixture.rs. Imports in one browser,
// then measures open() in a fresh browser per run — a shared user-data-dir
// keeps IndexedDB while each run starts with a clean renderer heap.

let puppeteer;
try {
  puppeteer = require("puppeteer");
} catch (_error) {
  puppeteer = require("puppeteer-core");
}
const { execFileSync } = require("child_process");
const fs = require("fs");
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
    return await fn(page, browser);
  } finally {
    await browser.close();
  }
}

function browserTreeMemoryBytes(rootPid) {
  const rows = execFileSync("ps", ["-eo", "pid=,ppid=,rss="], {
    encoding: "utf8",
  })
    .trim()
    .split("\n")
    .map((line) => line.trim().split(/\s+/).map(Number))
    .filter((row) => row.length === 3 && row.every(Number.isFinite));
  const children = new Map();
  for (const [pid, ppid, rssKiB] of rows) {
    if (!children.has(ppid)) children.set(ppid, []);
    children.get(ppid).push([pid, rssKiB]);
  }
  const queue = [rootPid];
  const seen = new Set();
  let rssKiB = 0;
  let pssKiB = 0;
  let privateKiB = 0;
  while (queue.length > 0) {
    const pid = queue.pop();
    if (seen.has(pid)) continue;
    seen.add(pid);
    const row = rows.find(([candidate]) => candidate === pid);
    if (row) rssKiB += row[2];
    try {
      const rollup = fs.readFileSync(`/proc/${pid}/smaps_rollup`, "utf8");
      const value = (label) => {
        const match = rollup.match(new RegExp(`^${label}:\\s+(\\d+) kB$`, "m"));
        return match ? Number(match[1]) : 0;
      };
      pssKiB += value("Pss");
      privateKiB += value("Private_Clean") + value("Private_Dirty");
    } catch (_error) {
      // A short-lived process may exit between the ps snapshot and smaps read.
    }
    for (const [child] of children.get(pid) ?? []) queue.push(child);
  }
  return {
    rssBytes: rssKiB * 1024,
    pssBytes: pssKiB * 1024,
    privateBytes: privateKiB * 1024,
  };
}

async function measureBrowserRss(browser, operation) {
  const pid = browser.process()?.pid;
  if (!pid) return { result: await operation(), rss: null };
  const before = browserTreeMemoryBytes(pid);
  const peak = { ...before };
  const timer = setInterval(() => {
    try {
      const current = browserTreeMemoryBytes(pid);
      peak.rssBytes = Math.max(peak.rssBytes, current.rssBytes);
      peak.pssBytes = Math.max(peak.pssBytes, current.pssBytes);
      peak.privateBytes = Math.max(peak.privateBytes, current.privateBytes);
    } catch (_error) {
      // The browser may exit between the operation resolving and cleanup.
    }
  }, 200);
  try {
    const result = await operation();
    const after = browserTreeMemoryBytes(pid);
    peak.rssBytes = Math.max(peak.rssBytes, after.rssBytes);
    peak.pssBytes = Math.max(peak.pssBytes, after.pssBytes);
    peak.privateBytes = Math.max(peak.privateBytes, after.privateBytes);
    const metric = (key, measurement) => ({
      measurement,
      beforeBytes: before[key],
      peakBytes: peak[key],
      afterBytes: after[key],
      peakDeltaBytes: peak[key] - before[key],
    });
    return {
      result,
      rss: metric(
        "rssBytes",
        "200 ms sampled sum of Linux Chrome process-tree RSS; shared pages may be counted once per process",
      ),
      pss: metric(
        "pssBytes",
        "200 ms sampled sum of Linux Chrome process-tree proportional set size",
      ),
      private: metric(
        "privateBytes",
        "200 ms sampled sum of Linux Chrome process-tree private clean plus private dirty bytes",
      ),
    };
  } finally {
    clearInterval(timer);
  }
}

function summary(values) {
  const sorted = [...values].sort((left, right) => left - right);
  const pick = (percentile) => {
    const index = Math.min(
      sorted.length - 1,
      Math.max(0, Math.ceil((percentile / 100) * sorted.length) - 1),
    );
    return Math.round(sorted[index] * 1000) / 1000;
  };
  return { p50: pick(50), p95: pick(95), max: pick(100) };
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

// A5-4 maintained-growth mode: the same small-write lineage, but with the
// production BrowserDb maintenance policy called at explicit idle boundaries.
async function maintainedGrowthMain() {
  const cycles = Number(process.argv[3]);
  const factsPerCycle = Number(process.argv[4]);
  const sampleEvery = Number(process.argv[5]);
  const maintenanceEvery = Number(process.argv[6]);
  const fixture = process.argv[7];
  const reopenRuns = Number(process.argv[8] ?? 2);
  if (
    !cycles ||
    !factsPerCycle ||
    !sampleEvery ||
    !maintenanceEvery ||
    !fixture
  ) {
    console.error(
      "usage: bench-driver.cjs maintained-growth <cycles> <factsPerCycle> <sampleEvery> <maintenanceEvery> <fixture-url-path|empty> [reopenRuns]",
    );
    process.exit(1);
  }

  await withPage(async (page) => {
    console.log("reset:", await page.evaluate(() => window.benchReset()));
    if (fixture !== "empty") {
      console.log(
        "import:",
        await page.evaluate((u) => window.benchImport(u), fixture),
      );
    }
  });

  await withPage(
    async (page) => {
      console.log(
        "maintainedGrowth:",
        await page.evaluate(
          (c, f, s, m) => window.benchMaintainedGrowth(c, f, s, m),
          cycles,
          factsPerCycle,
          sampleEvery,
          maintenanceEvery,
        ),
      );
    },
    { extraArgs: ["--js-flags=--expose-gc"], forwardConsole: true },
  );

  for (let i = 0; i < reopenRuns; i++) {
    await withPage(async (page) => {
      console.log(
        `reopen[${i}]:`,
        await page.evaluate(() => window.benchOpen()),
      );
    });
  }
}

// A5-4 WorkerGlobalScope smoke: prove that the same generated WASM package can
// open IndexedDB, durably write, query, and call maintenance without a Window.
async function workerSmokeMain() {
  await withPage(async (page) => {
    const result = await page.evaluate(async () => {
      const moduleUrl = new URL(
        "../../minigraf-wasm/minigraf.js",
        location.href,
      ).href;
      const dbName = `minigraf-worker-smoke-${Date.now()}`;
      const source = `
        import init, { BrowserDb } from ${JSON.stringify(moduleUrl)};
        try {
          await init();
          const db = await BrowserDb.open(${JSON.stringify(dbName)});
          const write = JSON.parse(await db.execute(
            '(transact [[:worker :value "ok"]])'
          ));
          const query = JSON.parse(await db.execute(
            '(query [:find ?v :where [:worker :value ?v]])'
          ));
          const maintenance = JSON.parse(await db.runIdleMaintenance());
          postMessage({
            ok: true,
            write,
            query,
            maintenance,
            hasWindow: typeof window !== 'undefined',
          });
        } catch (error) {
          postMessage({ ok: false, error: String(error), stack: error?.stack });
        }
      `;
      const url = URL.createObjectURL(
        new Blob([source], { type: "text/javascript" }),
      );
      try {
        return await new Promise((resolve, reject) => {
          const worker = new Worker(url, { type: "module" });
          worker.onmessage = (event) => {
            worker.terminate();
            resolve(event.data);
          };
          worker.onerror = (event) => {
            worker.terminate();
            reject(new Error(event.message));
          };
        });
      } finally {
        URL.revokeObjectURL(url);
      }
    });
    if (
      !result.ok ||
      result.hasWindow ||
      result.write?.durability !== "published" ||
      result.query?.results?.[0]?.[0] !== "ok"
    ) {
      throw new Error(`worker smoke failed: ${JSON.stringify(result)}`);
    }
    console.log("workerSmoke:", JSON.stringify(result));
  });
}

// A5-6d: one reproducible 1M acceptance run for the bounded Vetch path.
// Import is measured separately because it is explicitly O(total); all later
// phases launch fresh renderers against the same persisted profile.
async function pagedMatrixMain() {
  const fixture = process.argv[3];
  const runs = Number(process.argv[4] ?? 5);
  const growthCycles = Number(process.argv[5] ?? 1024);
  if (!fixture || !runs || !growthCycles) {
    console.error(
      "usage: bench-driver.cjs paged-matrix <fixture-url-path> [openRuns] [growthCycles]",
    );
    process.exit(1);
  }

  const evidence = {
    fixture,
    runs,
    growthCycles,
    chrome: CHROME,
    profile: PROFILE,
    import: null,
    opens: [],
    openSummary: null,
    export: null,
    growth: null,
    preMaintenanceOpens: [],
    preMaintenanceOpenSummary: null,
    maintenance: null,
    postMaintenanceOpen: null,
    environment: {
      platform: process.platform,
      release: os.release(),
      cpu: os.cpus()[0]?.model ?? null,
      logicalCpus: os.cpus().length,
      totalMemoryBytes: os.totalmem(),
      node: process.version,
    },
  };

  evidence.import = await withPage(
    async (page, browser) => {
      await page.evaluate(() => window.benchReset());
      await page.evaluate(() => window.benchReady());
      return measureBrowserRss(browser, async () =>
        JSON.parse(
          await page.evaluate((url) => window.benchPagedImport(url), fixture),
        ),
      );
    },
    { extraArgs: ["--js-flags=--expose-gc"], forwardConsole: true },
  );
  console.log("pagedImport:", JSON.stringify(evidence.import));

  for (let run = 0; run < runs; run++) {
    const measured = await withPage(
      async (page, browser) => {
        await page.evaluate(() => window.benchReady());
        return measureBrowserRss(browser, async () =>
          JSON.parse(await page.evaluate(() => window.benchPagedOpen())),
        );
      },
      { extraArgs: ["--js-flags=--expose-gc"] },
    );
    evidence.opens.push(measured);
    console.log(`pagedOpen[${run}]:`, JSON.stringify(measured));
  }
  evidence.openSummary = {
    openMs: summary(evidence.opens.map((entry) => entry.result.openMs)),
    coldFirstMs: summary(
      evidence.opens.map((entry) => entry.result.cold[0].ms),
    ),
    coldMiddleMs: summary(
      evidence.opens.map((entry) => entry.result.cold[1].ms),
    ),
    coldLastMs: summary(
      evidence.opens.map((entry) => entry.result.cold[2].ms),
    ),
    warmFirstMs: summary(
      evidence.opens.map((entry) => entry.result.warm[0].ms),
    ),
    warmMiddleMs: summary(
      evidence.opens.map((entry) => entry.result.warm[1].ms),
    ),
    warmLastMs: summary(
      evidence.opens.map((entry) => entry.result.warm[2].ms),
    ),
    heapOpenDeltaBytes: summary(
      evidence.opens.map(
        (entry) =>
          entry.result.heapAfterOpenBytes - entry.result.heapBeforeBytes,
      ),
    ),
    rssPeakDeltaBytes: summary(
      evidence.opens.map((entry) => entry.rss.peakDeltaBytes),
    ),
    pssPeakDeltaBytes: summary(
      evidence.opens.map((entry) => entry.pss.peakDeltaBytes),
    ),
    privatePeakDeltaBytes: summary(
      evidence.opens.map((entry) => entry.private.peakDeltaBytes),
    ),
  };
  console.log("pagedOpenSummary:", JSON.stringify(evidence.openSummary));

  evidence.export = await withPage(
    async (page, browser) => {
      await page.evaluate(() => window.benchReady());
      return measureBrowserRss(browser, async () =>
        JSON.parse(await page.evaluate(() => window.benchPagedExport())),
      );
    },
    {
      extraArgs: ["--js-flags=--expose-gc --max-old-space-size=4096"],
      forwardConsole: true,
    },
  );
  console.log("pagedExport:", JSON.stringify(evidence.export));

  evidence.growth = await withPage(
    async (page, browser) => {
      await page.evaluate(() => window.benchReady());
      return measureBrowserRss(browser, async () =>
        JSON.parse(
          await page.evaluate(
            (cycles) => window.benchPagedGrowth(cycles),
            growthCycles,
          ),
        ),
      );
    },
    {
      extraArgs: ["--js-flags=--expose-gc --max-old-space-size=4096"],
      forwardConsole: true,
    },
  );
  console.log("pagedGrowth:", JSON.stringify(evidence.growth));

  for (let run = 0; run < runs; run++) {
    const measured = await withPage(
      async (page, browser) => {
        await page.evaluate(() => window.benchReady());
        return measureBrowserRss(browser, async () =>
          JSON.parse(await page.evaluate(() => window.benchPagedOpen())),
        );
      },
      { extraArgs: ["--js-flags=--expose-gc"] },
    );
    evidence.preMaintenanceOpens.push(measured);
    console.log(`pagedPreMaintenanceOpen[${run}]:`, JSON.stringify(measured));
  }
  evidence.preMaintenanceOpenSummary = {
    openMs: summary(
      evidence.preMaintenanceOpens.map((entry) => entry.result.openMs),
    ),
    coldTotalMs: summary(
      evidence.preMaintenanceOpens.map((entry) => entry.result.coldTotalMs),
    ),
    warmTotalMs: summary(
      evidence.preMaintenanceOpens.map((entry) => entry.result.warmTotalMs),
    ),
    pssPeakDeltaBytes: summary(
      evidence.preMaintenanceOpens.map((entry) => entry.pss.peakDeltaBytes),
    ),
  };
  console.log(
    "pagedPreMaintenanceOpenSummary:",
    JSON.stringify(evidence.preMaintenanceOpenSummary),
  );

  evidence.maintenance = await withPage(
    async (page, browser) => {
      await page.evaluate(() => window.benchReady());
      return measureBrowserRss(browser, async () =>
        JSON.parse(
          await page.evaluate(
            (lastCycle) => window.benchPagedMaintenance(lastCycle),
            growthCycles,
          ),
        ),
      );
    },
    {
      extraArgs: ["--js-flags=--expose-gc --max-old-space-size=4096"],
      forwardConsole: true,
    },
  );
  console.log("pagedMaintenance:", JSON.stringify(evidence.maintenance));

  evidence.postMaintenanceOpen = await withPage(
    async (page, browser) => {
      await page.evaluate(() => window.benchReady());
      return measureBrowserRss(browser, async () =>
        JSON.parse(await page.evaluate(() => window.benchPagedOpen())),
      );
    },
    { extraArgs: ["--js-flags=--expose-gc"] },
  );
  console.log(
    "pagedPostMaintenanceOpen:",
    JSON.stringify(evidence.postMaintenanceOpen),
  );
  console.log("pagedMatrix:", JSON.stringify(evidence));
}

// Legacy A0 mode: import once, then measure open() in fresh browsers.
async function openMain() {
  const fixture = process.argv[2];
  const runs = Number(process.argv[3] ?? 3);
  if (!fixture) {
    console.error(
        "usage: bench-driver.cjs <fixture-url-path|skip-import> [runs]\n" +
        "       bench-driver.cjs paged-matrix <fixture-url-path> [openRuns] [growthCycles]\n" +
        "       bench-driver.cjs growth <cycles> <factsPerCycle> <sampleEvery> <fixture-url-path|empty> [reopenRuns]\n" +
        "       bench-driver.cjs maintained-growth <cycles> <factsPerCycle> <sampleEvery> <maintenanceEvery> <fixture-url-path|empty> [reopenRuns]\n" +
        "       bench-driver.cjs worker-smoke",
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

let main;
if (process.argv[2] === "paged-matrix") {
  main = pagedMatrixMain();
} else if (process.argv[2] === "growth") {
  main = growthMain();
} else if (process.argv[2] === "maintained-growth") {
  main = maintainedGrowthMain();
} else if (process.argv[2] === "worker-smoke") {
  main = workerSmokeMain();
} else {
  main = openMain();
}

main.catch((error) => {
  console.error(error?.stack ?? error);
  process.exitCode = 1;
});
