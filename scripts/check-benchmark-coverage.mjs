#!/usr/bin/env node

import { readFileSync } from "node:fs";

const sourcePath = process.argv[2] ?? "benches/minigraf_bench.rs";
const workflowPath = process.argv[3] ?? ".github/workflows/bench.yml";
const source = readFileSync(sourcePath, "utf8");
const workflow = readFileSync(workflowPath, "utf8");

const groups = unique(
  [...source.matchAll(/benchmark_group\(\s*"([^"]+)"\s*\)/g)].map(
    (match) => match[1],
  ),
);
const filters = unique(
  [...workflow.matchAll(/^\s+filter:\s+"([^"]+)"\s*$/gm)].map(
    (match) => match[1],
  ),
);

if (groups.length === 0) fail(`no Criterion groups found in ${sourcePath}`);
if (filters.length === 0) fail(`no benchmark filters found in ${workflowPath}`);

const invalidFilters = [];
const compiledFilters = filters.map((filter) => {
  try {
    return [filter, new RegExp(filter)];
  } catch (error) {
    invalidFilters.push(`${filter}: ${error.message}`);
    return [filter, null];
  }
});
if (invalidFilters.length > 0) {
  fail(`invalid benchmark filters:\n${invalidFilters.join("\n")}`);
}

const uncoveredGroups = groups.filter(
  (group) =>
    !compiledFilters.some(([, filter]) =>
      filter.test(`${group}/`),
    ),
);
const staleFilters = compiledFilters
  .filter(
    ([, filter]) =>
      !groups.some((group) =>
        filter.test(`${group}/`),
      ),
  )
  .map(([filter]) => filter);

if (uncoveredGroups.length > 0 || staleFilters.length > 0) {
  const problems = [];
  if (uncoveredGroups.length > 0) {
    problems.push(`uncovered Criterion groups:\n${uncoveredGroups.join("\n")}`);
  }
  if (staleFilters.length > 0) {
    problems.push(`workflow filters matching no group:\n${staleFilters.join("\n")}`);
  }
  fail(problems.join("\n\n"));
}

console.log(
  `benchmark coverage OK: ${groups.length} Criterion groups covered by ${filters.length} workflow filters`,
);

function unique(values) {
  return [...new Set(values)].sort();
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
