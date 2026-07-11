# Minigraf Browser Demo

Demonstrates `@minigraf/browser` running in a plain browser page with no bundler.

## Build

From the repo root:

```bash
wasm-pack build --target web --features browser --out-dir minigraf-wasm
```

This produces `minigraf-wasm/` containing `minigraf.js`, `minigraf_bg.wasm`, and
`minigraf.d.ts`.

## Serve

```bash
# From the repo root (not the examples/browser/ directory):
python3 -m http.server 8080
```

Open `http://localhost:8080/examples/browser/` in Chrome or Firefox.

## What it does

- Opens an IndexedDB-backed database named `"minigraf-demo"` through the eager
  `BrowserDb.open()` compatibility API.
- Transacts facts about Alice and Bob.
- Queries Alice's friends with Datalog.
- Exports the `.graph` blob and imports it into a fresh in-memory database.
- Logs all results to the browser console (open with F12).

## Bounded persistent use

The small demo deliberately keeps the original synchronous export flow. For a
large v11 authority database, use the generation-aware paged API and its
asynchronous verified export:

```js
const db = await BrowserDb.openPaged("my-vicia-db");
const result = await db.execute(
  "(query [:find ?name :where [?e :person/name ?name]])",
);
const graphBytes = await db.exportGraphAsync();
```

`openPaged()` bootstraps bounded catalog/manifest metadata and fetches verified
fact/index pages when queries demand them. `exportGraphAsync()` reads and
serialises the complete published prefix through the same v11 verifier;
synchronous
`exportGraph()` remains the eager/in-memory compatibility API. The 1M paged
performance matrix is still pending, so this API boundary is implemented but
not yet a Gate E scale claim.

## Notes

- Data persists across page reloads (stored in IndexedDB).
- Hold a caller-owned Web Lock for the lifetime of a writing handle. Independent
  stale handles reject on exact page-0 authority mismatch, but Vicia does not
  choose a cross-tab writer for the application.
- The `minigraf-wasm/` directory is committed — the files are up to date after pulling.
- This package (`@minigraf/browser`) is **browser-only**. For Node.js, use
  `@minigraf/node`.
