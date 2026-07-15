import init, {
  BrowserMaintenanceLedger,
} from "../../minigraf-wasm/minigraf.js";

self.addEventListener("message", async ({ data }) => {
  try {
    await init();
    const response = await navigator.locks.request(
      `minigraf:${data.dbName}`,
      async () => withMaintenanceLedger(data.dbName, data),
    );
    const transfer = response.bytes ? [response.bytes.buffer] : [];
    self.postMessage({ ok: true, ...response }, transfer);
  } catch (error) {
    self.postMessage({
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}, { once: true });

async function withMaintenanceLedger(dbName, request) {
  const ledger = await BrowserMaintenanceLedger.open(dbName);
  try {
    switch (request.operation) {
      case "maintenance":
        return { outcome: JSON.parse(await ledger.runIdleMaintenance()) };
      case "projection":
        return {
          outcome: JSON.parse(
            await ledger.rebuildCurrentProjections(request.attributes),
          ),
        };
      case "export": {
        const bytes = (await ledger.exportGraph()).slice();
        return { bytes };
      }
      case "import":
        await ledger.importGraph(request.bytes);
        return { status: "imported" };
      default:
        throw new Error(`unsupported maintenance operation: ${request.operation}`);
    }
  } finally {
    ledger.free();
  }
}
