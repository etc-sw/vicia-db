// A0 browser open-at-scale runner — measures BrowserDb.open() latency and
// JS-heap growth against a large imported fixture. Build the wasm pkg first:
//   wasm-pack build --target web --out-dir minigraf-wasm -- --features browser
// Serve from repo root and open /examples/browser/bench.html; runner steps
// are documented in docs/BENCHMARKS.md ("Browser Open at Scale").

import init, { BrowserDb } from "../../minigraf-wasm/minigraf.js";

const DB_NAME = "minigraf-bench";
const PAGE_SIZE = 4096;
const initPromise = init();

window.benchReady = async () => {
  await initPromise;
  return true;
};

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
    const numericPages = IDBKeyRange.bound(0, Number.MAX_SAFE_INTEGER);
    const idbCount = await promisifyReq(store.count(numericPages));
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

// ── A5-6d paged 1M acceptance matrix ───────────────────────────────────────

const PAGED_PROBES = [
  {
    id: "first",
    datalog: "(query [:find ?v :where [:bench/base-1 :bench/value ?v]])",
    expected: 1,
  },
  {
    id: "middle",
    datalog:
      "(query [:find ?v :where [:bench/base-500001 :bench/value ?v]])",
    expected: 500001,
  },
  {
    id: "last",
    datalog: "(query [:find ?v :where [:bench/base-999999 :bench/flag ?v]])",
    expected: true,
  },
];

async function timedProbe(db, probe) {
  const started = performance.now();
  const decoded = JSON.parse(await db.execute(probe.datalog));
  const elapsed = performance.now() - started;
  if (
    decoded.results.length !== 1 ||
    decoded.results[0][0] !== probe.expected
  ) {
    throw new Error(
      `paged probe ${probe.id} mismatch: ${JSON.stringify(decoded.results)}`,
    );
  }
  return {
    id: probe.id,
    ms: Math.round(elapsed * 1000) / 1000,
    rows: decoded.results.length,
  };
}

// Seed the persistent profile through the same paged API Vetch will adopt.
// Import is intentionally O(total); the bounded claim begins with later opens.
window.benchPagedImport = async (fixtureUrl) => {
  await initPromise;
  window.gc?.();
  const heapBeforeBytes = heap();
  const started = performance.now();
  const response = await fetch(fixtureUrl);
  if (!response.ok) throw new Error(`fetch failed: ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  const fetched = performance.now();
  const db = await BrowserDb.openPaged(DB_NAME);
  const opened = performance.now();
  await db.importGraph(bytes);
  const imported = performance.now();
  const stats = await idbStats();
  return show(
    JSON.stringify({
      fixtureBytes: bytes.byteLength,
      fetchMs: Math.round((fetched - started) * 1000) / 1000,
      openEmptyMs: Math.round((opened - fetched) * 1000) / 1000,
      importMs: Math.round((imported - opened) * 1000) / 1000,
      totalMs: Math.round((imported - started) * 1000) / 1000,
      heapBeforeBytes,
      heapAfterBytes: heap(),
      stats,
    }),
  );
};

// Fresh-renderer bounded open plus first/middle/last cold and warm point reads.
window.benchPagedOpen = async () => {
  await initPromise;
  window.gc?.();
  const heapBeforeBytes = heap();
  const started = performance.now();
  const db = await BrowserDb.openPaged(DB_NAME);
  const opened = performance.now();
  const heapAfterOpenBytes = heap();
  const cold = [];
  for (const probe of PAGED_PROBES) cold.push(await timedProbe(db, probe));
  const afterCold = performance.now();
  const warm = [];
  for (const probe of PAGED_PROBES) warm.push(await timedProbe(db, probe));
  const finished = performance.now();
  return show(
    JSON.stringify({
      openMs: Math.round((opened - started) * 1000) / 1000,
      cold,
      coldTotalMs: Math.round((afterCold - opened) * 1000) / 1000,
      warm,
      warmTotalMs: Math.round((finished - afterCold) * 1000) / 1000,
      heapBeforeBytes,
      heapAfterOpenBytes,
      heapAfterQueriesBytes: heap(),
      stats: await idbStats(),
    }),
  );
};

function encodeLedgerCallerValue(value) {
  if (value.type === "string") return JSON.stringify(value.value);
  if (value.type === "integer" || value.type === "boolean") return String(value.value);
  if (value.type === "ref") return `#uuid "${value.value}"`;
  if (value.type === "keyword") return value.value;
  if (value.type === "null") return "nil";
  throw new Error(`unsupported ledger caller value ${JSON.stringify(value)}`);
}

function encodeLedgerCallerCommands(changes) {
  return [
    ["retract", changes.filter((change) => change.operation === "retract")],
    ["transact", changes.filter((change) => change.operation === "assert")],
  ]
    .filter(([, selected]) => selected.length > 0)
    .map(([verb, selected]) =>
      `(${verb} [${selected.map((change) =>
        `[#uuid "${change.entity}" ${change.attribute} ${encodeLedgerCallerValue(change.value)}]`
      ).join("")}])`
    );
}

function ledgerCallerSampleUuid(uuid, sample) {
  const parts = uuid.split("-");
  const tail = Number.parseInt(parts[4], 16) + sample * 256;
  parts[4] = tail.toString(16).padStart(12, "0");
  return parts.join("-");
}

function instantiateLedgerCallerScenario(scenario, sample) {
  return {
    ...scenario,
    changes: scenario.changes.map((change) => ({
      ...change,
      entity: ledgerCallerSampleUuid(change.entity, sample),
      value: change.value.type === "ref"
        ? { ...change.value, value: ledgerCallerSampleUuid(change.value.value, sample) }
        : change.value,
    })),
    proof: {
      ...scenario.proof,
      entity: ledgerCallerSampleUuid(scenario.proof.entity, sample),
    },
  };
}

// H0 exact caller contract on the real paged browser write path. The internal
// stage timings exist only in a bench-internals wasm package.
window.benchVetchLedgerCaller = async (samples = 20) => {
  await initPromise;
  const response = await fetch("/benchmarks/fixtures/vetch-ledger-caller.v1.json");
  if (!response.ok) throw new Error(`caller fixture fetch failed: ${response.status}`);
  const fixture = await response.json();
  const db = await BrowserDb.openPaged(DB_NAME);
  const scenarios = [];
  for (const scenario of fixture.scenarios) {
    window.gc?.();
    const heapBeforeBytes = heap();
    const measurements = {
      callerEncodingMs: [],
      preparationMs: [],
      mutationMs: [],
      publicationMs: [],
      executeAtomicMs: [],
      resultDecodeMs: [],
      proofReadMs: [],
    };
    for (let sample = 0; sample < samples; sample++) {
      const encodingStarted = performance.now();
      const instance = instantiateLedgerCallerScenario(scenario, sample);
      const commands = encodeLedgerCallerCommands(instance.changes);
      measurements.callerEncodingMs.push(performance.now() - encodingStarted);

      const executeStarted = performance.now();
      const rawReceipt = await db.executeAtomic(commands);
      measurements.executeAtomicMs.push(performance.now() - executeStarted);
      const decodeStarted = performance.now();
      const receipt = JSON.parse(rawReceipt);
      measurements.resultDecodeMs.push(performance.now() - decodeStarted);
      if (receipt.fact_count !== scenario.changes.length || !receipt.atomic) {
        throw new Error(`${scenario.id} atomic receipt mismatch: ${rawReceipt}`);
      }
      if (!receipt.benchmark) {
        throw new Error("ledger caller benchmark requires a bench-internals wasm package");
      }
      measurements.preparationMs.push(receipt.benchmark.preparation_ms);
      measurements.mutationMs.push(receipt.benchmark.mutation_ms);
      measurements.publicationMs.push(receipt.benchmark.publication_ms);

      const proofStarted = performance.now();
      const proof = JSON.parse(await db.execute(
        `(query [:find ?v :where [#uuid "${instance.proof.entity}" ${instance.proof.attribute} ?v]])`,
      ));
      measurements.proofReadMs.push(performance.now() - proofStarted);
      if (proof.results?.length !== instance.proof.expectedRows) {
        throw new Error(
          `${scenario.id} exact proof row mismatch: ${JSON.stringify(proof.results)}`,
        );
      }
    }
    const heapAfterBytes = heap();
    scenarios.push({
      id: scenario.id,
      measurements,
      heapBeforeBytes,
      heapAfterBytes,
      heapDeltaBytes: heapBeforeBytes === null || heapAfterBytes === null
        ? null
        : heapAfterBytes - heapBeforeBytes,
    });
  }
  return show(JSON.stringify({
    schema: "vicia.vetch-ledger-caller-browser.v1",
    fixtureSchema: fixture.schema,
    samples,
    scenarios,
    stats: await idbStats(),
  }));
};

// The portability API is explicitly O(total), but it must not change the live
// sparse residency or use the eager full-store loader.
window.benchPagedExport = async () => {
  await initPromise;
  window.gc?.();
  const heapBeforeBytes = heap();
  const db = await BrowserDb.openPaged(DB_NAME);
  const heapAfterOpenBytes = heap();
  const started = performance.now();
  const bytes = await db.exportGraphAsync();
  const finished = performance.now();
  const stats = await idbStats();
  if (
    bytes.byteLength < 4096 ||
    String.fromCharCode(...bytes.slice(0, 4)) !== "MGRF" ||
    bytes.byteLength !== stats.headerPageCount * PAGE_SIZE
  ) {
    throw new Error("paged export is not a portable MGRF image");
  }
  return show(
    JSON.stringify({
      exportMs: Math.round((finished - started) * 1000) / 1000,
      exportBytes: bytes.byteLength,
      heapBeforeBytes,
      heapAfterOpenBytes,
      heapWithExportBytes: heap(),
      stats,
    }),
  );
};

// Accumulate one segment per write until the production soft threshold is
// reached. The following maintenance phase runs in a fresh renderer.
window.benchPagedGrowth = async (cycles) => {
  await initPromise;
  window.gc?.();
  const db = await BrowserDb.openPaged(DB_NAME);
  const durations = [];
  let finalWrite = null;
  const started = performance.now();
  for (let cycle = 1; cycle <= cycles; cycle++) {
    const writeStarted = performance.now();
    finalWrite = JSON.parse(
      await db.execute(
        `(transact [[:gate-e/write-${cycle} :gate-e/value ${cycle}]])`,
      ),
    );
    durations.push(performance.now() - writeStarted);
    if (cycle % 128 === 0 || cycle === cycles) {
      console.log(
        `paged-growth: ${JSON.stringify({ cycle, advice: finalWrite.advice })}`,
      );
    }
  }
  if (finalWrite?.advice !== "schedule_idle_maintenance") {
    throw new Error(
      `growth did not reach the soft maintenance threshold: ${JSON.stringify(finalWrite)}`,
    );
  }
  const sorted = [...durations].sort((left, right) => left - right);
  return show(
    JSON.stringify({
      cycles,
      totalMs: Math.round((performance.now() - started) * 1000) / 1000,
      writeP50Ms: percentile(sorted, 50),
      writeP95Ms: percentile(sorted, 95),
      writeMaxMs: percentile(sorted, 100),
      finalAdvice: finalWrite?.advice ?? null,
      heapAfterBytes: heap(),
      stats: await idbStats(),
    }),
  );
};

window.benchPagedMaintenance = async (lastCycle) => {
  await initPromise;
  window.gc?.();
  const heapBeforeBytes = heap();
  const before = await idbStats();
  const started = performance.now();
  const db = await BrowserDb.openPaged(DB_NAME);
  const opened = performance.now();
  const result = JSON.parse(await db.runIdleMaintenance());
  const maintained = performance.now();
  const after = await idbStats();
  if (
    result.delta !== "recompacted" ||
    after.idbCount >= before.idbCount
  ) {
    throw new Error(
      `maintenance did not compact the soft-threshold lineage: ${JSON.stringify({ result, before, after })}`,
    );
  }
  const verify = JSON.parse(
    await db.execute(
      `(query [:find ?v :where [:gate-e/write-${lastCycle} :gate-e/value ?v]])`,
    ),
  );
  if (verify.results?.[0]?.[0] !== lastCycle) {
    throw new Error(`maintenance verification failed: ${JSON.stringify(verify)}`);
  }
  return show(
    JSON.stringify({
      openMs: Math.round((opened - started) * 1000) / 1000,
      maintenanceMs: Math.round((maintained - opened) * 1000) / 1000,
      result,
      before,
      after,
      heapBeforeBytes,
      heapAfterBytes: heap(),
      verifyRows: verify.results.length,
    }),
  );
};
