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
