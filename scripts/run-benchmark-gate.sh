#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="${1:-smoke}"
requested_output="${2:-}"

case "$profile" in
  smoke)
    fact_count=10000
    suites=(
      "vetch-cadence:vetch_cadence_benchmark:smoke"
      "delta-accumulation:delta_accumulation_benchmark:smoke"
      "agent-brief:agent_brief_read_path_benchmark:smoke"
      "vetch-ledger-caller:vetch_ledger_caller_benchmark:smoke"
    )
    ;;
  full)
    fact_count=1000000
    suites=(
      "vetch-cadence:vetch_cadence_benchmark:full"
      "delta-accumulation:delta_accumulation_benchmark:full"
      "agent-brief:agent_brief_read_path_benchmark:full"
      "vetch-ledger-caller:vetch_ledger_caller_benchmark:full"
    )
    ;;
  delta-mini)
    fact_count=1000000
    suites=("delta-accumulation:delta_accumulation_benchmark:t8b-mini")
    ;;
  *)
    echo "usage: $0 [smoke|full|delta-mini] [output-directory]" >&2
    exit 2
    ;;
esac

for command in cargo node; do
  command -v "$command" >/dev/null || {
    echo "error: $command is required" >&2
    exit 1
  }
done

if [[ -n "$requested_output" ]]; then
  output_dir="$requested_output"
else
  run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
  output_dir="target/benchmark-receipts/${profile}-${run_id}"
fi
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"

fixture="$output_dir/base-${fact_count}.graph"
export VICIA_BENCH_TESTBED="${VICIA_BENCH_TESTBED:-local-$(hostname)}"
export VICIA_BENCH_BASE_FIXTURE="$fixture"

echo "benchmark profile: $profile"
echo "testbed: $VICIA_BENCH_TESTBED"
echo "evidence: $output_dir"

node scripts/check-benchmark-catalog.mjs
node scripts/check-benchmark-coverage.mjs
cargo run --release --example generate_bench_fixture -- "$fact_count" "$fixture"

receipts=()
for suite in "${suites[@]}"; do
  IFS=: read -r label bench mode <<<"$suite"
  receipt="$output_dir/${label}-${profile}.json"
  csv="$output_dir/${label}-${profile}.csv"
  receipts+=("$receipt")

  echo "running: $label ($mode)"
  if [[ "$bench" == "vetch_ledger_caller_benchmark" ]]; then
    VICIA_BENCH_RECEIPT="$receipt" \
      cargo bench --features bench-internals --bench "$bench" -- "$mode" >"$csv"
  else
    VICIA_BENCH_RECEIPT="$receipt" \
      cargo bench --bench "$bench" -- "$mode" >"$csv"
  fi
  node scripts/check-benchmark-receipt.mjs "$receipt"
done

node - "${receipts[@]}" <<'NODE'
const fs = require("node:fs");

console.log("\nbenchmark gate summary");
for (const path of process.argv.slice(2)) {
  const receipt = JSON.parse(fs.readFileSync(path, "utf8"));
  const failed = receipt.budgets.checks.filter((check) => !check.passed).length;
  console.log(
    `${receipt.suite}/${receipt.milestone.profile}: ` +
      `${receipt.passed ? "PASS" : "FAIL"}, ` +
      `${receipt.correctness.checks.length} correctness checks, ` +
      `${receipt.budgets.checks.length - failed}/${receipt.budgets.checks.length} budgets, ` +
      `${Math.round(receipt.totalMs)} ms`,
  );
}
NODE

echo "evidence written to: $output_dir"
