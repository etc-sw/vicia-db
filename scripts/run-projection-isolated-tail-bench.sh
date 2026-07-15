#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/projection-isolated-tail/$profile}"
case "$profile" in
  smoke) facts=10000 ;;
  full) facts=1000000 ;;
  *)
    echo "profile must be smoke or full" >&2
    exit 2
    ;;
esac

mkdir -p "$output_dir"
graph="$output_dir/work.graph"
fixture="$output_dir/fixture.json"
receipt="$output_dir/receipt.json"
cleanup() {
  rm -f "$graph" "$graph.wal" "$graph.lock" "$fixture"
}
trap cleanup EXIT

cargo run --release --features bench-internals --bin current-projection-bench -- \
  build-temporal-provenance "$graph" "$facts" 90 >"$fixture"
cargo run --release --features bench-internals --bin current-projection-tail-bench -- \
  isolated-run "$graph" "$fixture" "$facts" "$profile" >"$receipt"
node scripts/validate-projection-isolated-tail-receipt.mjs "$receipt" "$profile"
node scripts/audit-projection-isolated-tail-validator.mjs "$receipt" "$profile"
