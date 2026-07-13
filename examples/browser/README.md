# Minigraf Browser Capability Demo

This no-bundler demo runs the recommended persistent browser shape:

- `BrowserInteractiveLedger` owns atomic foreground writes and bounded reads;
- each maintenance, export, or import operation gets a disposable module
  worker with `BrowserMaintenanceLedger`;
- foreground and maintenance lifetimes use the same caller-owned Web Lock;
- strict import is verified by reopening the destination interactively.

## Build and Run

From the repository root:

```bash
wasm-pack build --target web --out-dir minigraf-wasm -- --features browser
python3 -m http.server 8080
```

Open `http://127.0.0.1:8080/examples/browser/?reset=1`. The page shows a
structured receipt containing the foreground write, Bob lookup, maintenance
outcome, export size, and Alice/Bob import proof.

`app.js` keeps foreground work inside the interactive capability. It gives
every query explicit row and byte budgets and frees the read view and ledger.
`maintenance-worker.js` accepts exactly one operation, acquires the same lock,
opens the maintenance capability, posts its result, and is terminated by the
caller after success or failure.

## Headless Chrome Receipt

The driver uses the same external `puppeteer`/`puppeteer-core` convention as
the browser benchmark driver and adds no runtime dependency to Minigraf:

```bash
CHROME_PATH=/usr/local/bin/google-chrome \
NODE_PATH=<directory-containing-puppeteer-core> \
CAPABILITY_DEMO_PAGE='http://127.0.0.1:8080/examples/browser/?reset=1' \
node examples/browser/capability-demo-driver.cjs
```

It rejects unless the write is IndexedDB-published, the bounded foreground
query returns Bob, the complete graph export imports into a second database,
the reopened database returns Alice and Bob, and no worker remains active.

## Raw Compatibility

`BrowserDb` remains supported throughout 1.x for advanced Datalog, benchmarks,
fixtures, and migration recovery without a capability replacement. It is not
the default persistent application example. See
`docs/API_COMPATIBILITY_AND_MIGRATION.md` for the replacement-first 2.0 policy.

The generated `minigraf-wasm/` directory is a local build artifact. The browser
package is browser-only; use `@minigraf/node` for server-side Node.js.
