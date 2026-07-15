#!/usr/bin/env node
import { readFileSync } from "node:fs";

const declarationPath = process.argv[2];
if (!declarationPath) {
  throw new Error("usage: validate-browser-capability-surface.mjs <minigraf.d.ts>");
}
const source = readFileSync(declarationPath, "utf8");

const interactive = classBody("BrowserInteractiveLedger");
requireMethods(interactive, [
  "executeAtomic",
  "open",
  "openInMemory",
  "readView",
  "readViewAnyValidTime",
  "readViewAt",
]);
forbidMethods(interactive, [
  "checkpoint",
  "execute",
  "exportGraph",
  "importGraph",
  "rebuildCurrentProjections",
  "runIdleMaintenance",
]);

const maintenance = classBody("BrowserMaintenanceLedger");
requireMethods(maintenance, [
  "exportGraph",
  "importGraph",
  "open",
  "openInMemory",
  "rebuildCurrentProjections",
  "runIdleMaintenance",
]);
forbidMethods(maintenance, [
  "checkpoint",
  "execute",
  "executeAtomic",
  "readView",
  "readViewAnyValidTime",
  "readViewAt",
]);

const compatibility = classBody("BrowserDb");
requireMethods(compatibility, [
  "execute",
  "executeAtomic",
  "exportGraph",
  "importGraph",
  "openPaged",
  "readView",
  "runIdleMaintenance",
]);
forbidMethods(compatibility, ["rebuildCurrentProjections"]);

console.log("validated browser interactive/maintenance capability surface");

function classBody(name) {
  const match = source.match(new RegExp(`export class ${name} \\{([\\s\\S]*?)\\n\\}`));
  if (!match) throw new Error(`missing class ${name}`);
  return match[1].replace(/\/\*[\s\S]*?\*\//g, "");
}

function requireMethods(body, names) {
  for (const name of names) {
    if (!methodPattern(name).test(body)) throw new Error(`missing method ${name}`);
  }
}

function forbidMethods(body, names) {
  for (const name of names) {
    if (methodPattern(name).test(body)) throw new Error(`forbidden method ${name}`);
  }
}

function methodPattern(name) {
  return new RegExp(`(?:static\\s+)?${name}\\(`);
}
