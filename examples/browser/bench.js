// A0 browser open-at-scale runner — measures BrowserDb.open() latency and
// JS-heap growth against a large imported fixture. Build the wasm pkg first:
//   wasm-pack build --target web --out-dir minigraf-wasm -- --features browser
// Serve from repo root and open /examples/browser/bench.html; runner steps
// are documented in docs/BENCHMARKS.md ("Browser Open at Scale").

import init, { BrowserDb } from "../../minigraf-wasm/minigraf.js";

const DB_NAME = "minigraf-bench";
const initPromise = init();

function heap() {
  return performance.memory ? performance.memory.usedJSHeapSize : null;
}

function show(result) {
  document.getElementById("out").textContent += result + "\n";
  return result;
}

// Delete the bench IndexedDB database for a clean run.
window.benchReset = async () => {
  await initPromise;
  await new Promise((resolve, reject) => {
    const req = indexedDB.deleteDatabase(DB_NAME);
    req.onsuccess = resolve;
    req.onblocked = resolve;
    req.onerror = () => reject(req.error);
  });
  return show(JSON.stringify({ reset: true }));
};

// Fetch a .graph fixture and import it into the bench database.
// checkpoint() persists the imported pages to IndexedDB.
window.benchImport = async (fixtureUrl) => {
  await initPromise;
  const t0 = performance.now();
  const resp = await fetch(fixtureUrl);
  if (!resp.ok) throw new Error(`fetch failed: ${resp.status}`);
  const bytes = new Uint8Array(await resp.arrayBuffer());
  const fetched = performance.now();
  const db = await BrowserDb.open(DB_NAME);
  await db.importGraph(bytes);
  await db.checkpoint();
  const done = performance.now();
  return show(JSON.stringify({
    fixtureBytes: bytes.byteLength,
    fetchMs: Math.round(fetched - t0),
    importMs: Math.round(done - fetched),
  }));
};

// ── A5-2 growth measurement ─────────────────────────────────────────────────
// Long-running IndexedDB growth: repeated small write executes, sampling IDB
// page count, header page_count/node_count (page 0, little-endian: version
// u32@4, page_count u64@8, node_count u64@16 — see src/storage/mod.rs
// FileHeader layout), storage estimate, and JS heap. The raw sampling
// connection opens only AFTER BrowserDb.open has created the object store
// (opening first would create a store-less database and break later opens).

let growthDb = null;
let lastGrowthQuery = null;

function promisifyReq(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function percentile(sorted, p) {
  if (sorted.length === 0) return null;
  const idx = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return Math.round(sorted[Math.max(0, idx)] * 1000) / 1000;
}

async function idbStats() {
  const conn = await promisifyReq(indexedDB.open(DB_NAME));
  try {
    const store = conn.transaction(DB_NAME, "readonly").objectStore(DB_NAME);
    const idbCount = await promisifyReq(store.count());
    const page0 = await promisifyReq(store.get(0));
    let headerVersion = null, headerPageCount = null, headerNodeCount = null;
    if (page0) {
      const dv = new DataView(page0.buffer, page0.byteOffset, page0.byteLength);
      headerVersion = dv.getUint32(4, true);
      headerPageCount = Number(dv.getBigUint64(8, true));
      headerNodeCount = Number(dv.getBigUint64(16, true));
    }
    return { idbCount, headerVersion, headerPageCount, headerNodeCount };
  } finally {
    conn.close();
  }
}

async function growthSample(cycle, elapsedMs, windowExecMs) {
  window.gc?.();
  const stats = await idbStats();
  const estimate = navigator.storage?.estimate
    ? await navigator.storage.estimate()
    : null;
  const sorted = [...windowExecMs].sort((a, b) => a - b);
  return {
    cycle,
    idbCount: stats.idbCount,
    idbMB: Math.round((stats.idbCount * 4096) / 1024 / 102.4) / 10,
    headerPageCount: stats.headerPageCount,
    headerNodeCount: stats.headerNodeCount,
    storageUsage: estimate ? estimate.usage : null,
    heapUsed: heap(),
    elapsedMs: Math.round(elapsedMs),
    execP50: percentile(sorted, 50),
    execP95: percentile(sorted, 95),
    execMax: percentile(sorted, 100),
  };
}

// Run `cycles` write executes of `factsPerCycle` tuples each against DB_NAME,
// sampling every `sampleEvery` cycles. Keeps the live handle in `growthDb`
// for benchGrowthRoundTrip. Streams samples via console.log for progress.
window.benchGrowth = async (cycles, factsPerCycle, sampleEvery) => {
  await initPromise;
  growthDb = await BrowserDb.open(DB_NAME);
  const samples = [];
  const t0 = performance.now();
  let windowExecMs = [];

  for (let i = 1; i <= cycles; i++) {
    let tuples = "";
    for (let j = 0; j < factsPerCycle; j++) {
      tuples += `[:g${i}-${j} :n ${i * factsPerCycle + j}] `;
    }
    const e0 = performance.now();
    await growthDb.execute(`(transact [${tuples}])`);
    windowExecMs.push(performance.now() - e0);

    if (i % sampleEvery === 0 || i === cycles) {
      const s = await growthSample(i, performance.now() - t0, windowExecMs);
      samples.push(s);
      console.log(`growth: ${JSON.stringify(s)}`);
      windowExecMs = [];
    }
  }
  lastGrowthQuery =
    `(query [:find ?v :where [:g${cycles}-0 :n ?v]])`;
  return show(JSON.stringify(samples));
};

// Repeat the growth workload while invoking the real browser maintenance
// surface at caller-owned idle boundaries. Each maintenance sample captures
// the physical page count immediately before and after atomic replacement and
// the time spent rebuilding the full-history image.
window.benchMaintainedGrowth = async (
  cycles,
  factsPerCycle,
  sampleEvery,
  maintenanceEvery,
) => {
  await initPromise;
  growthDb = await BrowserDb.open(DB_NAME);
  const samples = [];
  const maintenances = [];
  const t0 = performance.now();
  let windowExecMs = [];

  for (let i = 1; i <= cycles; i++) {
    let tuples = "";
    for (let j = 0; j < factsPerCycle; j++) {
      tuples += `[:m${i}-${j} :n ${i * factsPerCycle + j}] `;
    }
    const e0 = performance.now();
    const writeResult = JSON.parse(
      await growthDb.execute(`(transact [${tuples}])`),
    );
    windowExecMs.push(performance.now() - e0);

    if (i % maintenanceEvery === 0) {
      const before = await growthSample(i, performance.now() - t0, windowExecMs);
      const m0 = performance.now();
      const outcome = JSON.parse(await growthDb.runIdleMaintenance());
      const maintenanceMs = performance.now() - m0;
      const after = await growthSample(i, performance.now() - t0, []);
      const entry = {
        cycle: i,
        maintenanceMs: Math.round(maintenanceMs * 1000) / 1000,
        writeAdvice: writeResult.advice,
        outcome,
        before,
        after,
      };
      maintenances.push(entry);
      console.log(`maintenance: ${JSON.stringify(entry)}`);
      windowExecMs = [];
    } else if (i % sampleEvery === 0 || i === cycles) {
      const sample = await growthSample(
        i,
        performance.now() - t0,
        windowExecMs,
      );
      samples.push(sample);
      console.log(`maintained-growth: ${JSON.stringify(sample)}`);
      windowExecMs = [];
    }
  }

  lastGrowthQuery = `(query [:find ?v :where [:m${cycles}-0 :n ?v]])`;
  return show(JSON.stringify({ samples, maintenances }));
};

// exportGraph -> importGraph round-trip on the live growth handle, sampling
// before/after. Expected result: identical size — export serialises the full
// 0..page_count range including superseded pages, so the round-trip is NOT a
// compaction remedy. Verifies data survives via point queries.
window.benchGrowthRoundTrip = async () => {
  if (!growthDb) throw new Error("run benchGrowth first");
  const before = await growthSample(-1, 0, []);
  const t0 = performance.now();
  const blob = growthDb.exportGraph();
  const exported = performance.now();
  await growthDb.importGraph(blob);
  const imported = performance.now();
  const after = await growthSample(-1, 0, []);

  const growthRows = lastGrowthQuery
    ? JSON.parse(await growthDb.execute(lastGrowthQuery)).results.length
    : null;
  const baseRows = JSON.parse(
    await growthDb.execute(
      "(query [:find ?v :where [:bench/base-1 :bench/value ?v]])"
    )
  ).results.length;

  return show(JSON.stringify({
    exportMs: Math.round(exported - t0),
    exportBytes: blob.byteLength,
    importMs: Math.round(imported - exported),
    before,
    after,
    verifyGrowthRows: growthRows,
    verifyBaseRows: baseRows,
  }));
};

// Measure open + first query on the previously imported database.
// Run after a page reload so the heap baseline excludes import residue.
window.benchOpen = async () => {
  await initPromise;
  const heapBefore = heap();
  const t0 = performance.now();
  const db = await BrowserDb.open(DB_NAME);
  const opened = performance.now();
  const raw = await db.execute(
    "(query [:find ?v :where [:bench/base-1 :bench/value ?v]])"
  );
  const queried = performance.now();
  const heapAfter = heap();
  return show(JSON.stringify({
    openMs: Math.round(opened - t0),
    firstQueryMs: Math.round((queried - opened) * 1000) / 1000,
    rows: JSON.parse(raw).results.length,
    heapBeforeBytes: heapBefore,
    heapAfterBytes: heapAfter,
  }));
};
