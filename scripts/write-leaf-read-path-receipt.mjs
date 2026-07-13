import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, statSync, writeFileSync } from "node:fs";

const [profile, factsText, fixture, pointPath, aggregatePath, outputPath] = process.argv.slice(2);
const facts = Number(factsText);
const point = JSON.parse(readFileSync(pointPath, "utf8"));
const aggregate = JSON.parse(readFileSync(aggregatePath, "utf8"));
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
const sourceDirty = execFileSync("git", ["status", "--porcelain"], { encoding: "utf8" }).trim() !== "";
const fixtureSha256 = createHash("sha256").update(readFileSync(fixture)).digest("hex");
const receipt = {
  schema: "vicia.leaf-read-path.v1",
  profile,
  facts,
  sourceCommit,
  sourceDirty,
  fixture: { path: fixture, bytes: statSync(fixture).size, sha256: fixtureSha256, fillPercent: 90, formatVersion: 12 },
  point,
  aggregate,
};
writeFileSync(outputPath, `${JSON.stringify(receipt, null, 2)}\n`);
