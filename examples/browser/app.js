// Minigraf capability-scoped browser demo — no bundler required.
// Build first: wasm-pack build --target web --out-dir minigraf-wasm -- --features browser
// Then serve from repo root: python3 -m http.server 8080
// Open: http://localhost:8080/examples/browser/

import init, {
  BrowserInteractiveLedger,
} from "../../minigraf-wasm/minigraf.js";

const SOURCE_DB = "minigraf-capability-demo";
const IMPORTED_DB = "minigraf-capability-demo-imported";
const QUERY_MAX_ROWS = 16;
const QUERY_MAX_BYTES = 8 * 1024;
const activeWorkers = new Set();

async function main() {
  await init();
  if (new URLSearchParams(location.search).has("reset")) {
    await Promise.all([deleteDatabase(SOURCE_DB), deleteDatabase(IMPORTED_DB)]);
  }

  const foreground = await withInteractiveLedger(SOURCE_DB, async (ledger) => {
    const write = JSON.parse(await ledger.executeAtomic([
      `(transact [
        [:alice :person/name "Alice"]
        [:alice :person/age 30]
        [:alice :friend :bob]
        [:bob :person/name "Bob"]
      ])`,
    ]));
    const friendNames = await queryRows(
      ledger,
      `(query [:find ?friend-name
               :where [:alice :friend ?friend]
                      [?friend :person/name ?friend-name]])`,
    );
    return { write, friendNames: friendNames.map((row) => row[0]) };
  });

  const maintenance = await runMaintenanceWorker({
    operation: "maintenance",
    dbName: SOURCE_DB,
  });
  const exported = await runMaintenanceWorker({
    operation: "export",
    dbName: SOURCE_DB,
  });
  const exportBytes = exported.bytes.byteLength;
  const imported = await runMaintenanceWorker(
    {
      operation: "import",
      dbName: IMPORTED_DB,
      bytes: exported.bytes,
    },
    [exported.bytes.buffer],
  );

  const importedNames = await withInteractiveLedger(IMPORTED_DB, async (ledger) =>
    (await queryRows(
      ledger,
      `(query [:find ?name :where [?entity :person/name ?name]])`,
    )).map((row) => row[0]).sort(),
  );

  return {
    status: "passed",
    database: SOURCE_DB,
    write: foreground.write,
    friendNames: foreground.friendNames,
    maintenance: maintenance.outcome,
    exportBytes,
    importStatus: imported.status,
    importedNames,
    workerOperations: ["maintenance", "export", "import"],
    activeWorkerCount: activeWorkers.size,
  };
}

async function withInteractiveLedger(dbName, operation) {
  return navigator.locks.request(lockName(dbName), async () => {
    const ledger = await BrowserInteractiveLedger.open(dbName);
    try {
      return await operation(ledger);
    } finally {
      ledger.free();
    }
  });
}

async function queryRows(ledger, datalog) {
  const view = ledger.readView();
  try {
    const result = JSON.parse(
      await view.query(datalog, QUERY_MAX_ROWS, QUERY_MAX_BYTES),
    );
    return result.results ?? [];
  } finally {
    view.free();
  }
}

function runMaintenanceWorker(request, transfer = []) {
  const worker = new Worker("./maintenance-worker.js", { type: "module" });
  activeWorkers.add(worker);
  return new Promise((resolve, reject) => {
    const finish = (settle, value) => {
      worker.terminate();
      activeWorkers.delete(worker);
      settle(value);
    };
    worker.addEventListener("message", ({ data }) => {
      if (data?.ok) finish(resolve, data);
      else finish(reject, new Error(data?.error ?? "maintenance worker failed"));
    }, { once: true });
    worker.addEventListener("error", (event) => {
      finish(reject, event.error ?? new Error(event.message));
    }, { once: true });
    worker.postMessage(request, transfer);
  });
}

function lockName(dbName) {
  return `minigraf:${dbName}`;
}

function deleteDatabase(dbName) {
  return new Promise((resolve, reject) => {
    const request = indexedDB.deleteDatabase(dbName);
    request.addEventListener("success", () => resolve());
    request.addEventListener("blocked", () => reject(new Error(
      `IndexedDB deletion blocked for ${dbName}`,
    )));
    request.addEventListener("error", () => reject(
      request.error ?? new Error(`IndexedDB deletion failed for ${dbName}`),
    ));
  });
}

const status = document.querySelector("#status");
const output = document.querySelector("#output");

main().then((receipt) => {
  window.capabilityDemoReceipt = receipt;
  status.textContent = "Capability demo passed";
  output.textContent = JSON.stringify(receipt, null, 2);
  console.log("Minigraf capability demo:", receipt);
}).catch((error) => {
  const receipt = {
    status: "failed",
    error: error instanceof Error ? error.message : String(error),
    activeWorkerCount: activeWorkers.size,
  };
  window.capabilityDemoReceipt = receipt;
  status.textContent = "Capability demo failed";
  output.textContent = JSON.stringify(receipt, null, 2);
  console.error(error);
});
