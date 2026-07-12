#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [binary, outputDir, factsPerCycle] = process.argv.slice(2);
if (!binary || !outputDir || !factsPerCycle) {
  console.error("usage: run-cross-db-crash.mjs <binary> <output-dir> <facts-per-cycle>");
  process.exit(2);
}

for (const engine of ["vicia", "cozo", "sqlite", "redb"]) {
  const stress = JSON.parse(readFileSync(join(outputDir, `${engine}.json`), "utf8"));
  const minimumCount = await killWriter(
    engine,
    join(outputDir, `${engine}-data`),
    stress.correctness.actualCount,
    Number(factsPerCycle),
  );
  const verified = spawnSync(
    binary,
    ["verify", engine, join(outputDir, `${engine}-data`), String(minimumCount)],
    { encoding: "utf8" },
  );
  if (verified.status !== 0) {
    console.error(verified.stderr || verified.stdout);
    process.exit(1);
  }
  const receipt = JSON.parse(verified.stdout.trim());
  writeFileSync(
    join(outputDir, `${engine}-crash.json`),
    `${JSON.stringify(receipt, null, 2)}\n`,
  );
  console.log(
    `${engine} crash recovery: ${receipt.recoveredCount} facts, ` +
      `${receipt.integrityCheck}, ${receipt.passed ? "PASS" : "FAIL"}`,
  );
}

function killWriter(engine, dbDir, startEntity, batch) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      binary,
      ["crash-write", engine, dbDir, String(startEntity), "10000", String(batch)],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    let stdout = "";
    let stderr = "";
    let minimumCount = null;
    let killed = false;
    const timeout = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`${engine} crash writer did not reach five commits: ${stderr}`));
    }, 30_000);

    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      const lines = stdout.split("\n");
      stdout = lines.pop() ?? "";
      for (const line of lines) {
        if (!line) continue;
        const progress = JSON.parse(line);
        if (!killed && progress.committedCycles >= 5) {
          minimumCount = progress.minimumCount;
          killed = child.kill("SIGKILL");
        }
      }
    });
    child.on("error", reject);
    child.on("exit", (_code, signal) => {
      clearTimeout(timeout);
      if (!killed || signal !== "SIGKILL" || minimumCount === null) {
        reject(new Error(`${engine} writer was not killed at a committed boundary: ${stderr}`));
        return;
      }
      resolve(minimumCount);
    });
  });
}
