#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/current-projection/$profile}"
case "$profile" in
  smoke)
    facts=10000
    samples=5
    ;;
  full)
    facts=1000000
    samples=20
    ;;
  *)
    echo "profile must be smoke or full" >&2
    exit 2
    ;;
esac

mkdir -p "$output_dir"
graph="$output_dir/work.graph"
receipt="$output_dir/receipt.json"
cleanup() {
  rm -f "$graph" "$graph.wal" "$graph.lock"
}
trap cleanup EXIT

cargo run --release --features bench-internals --bin current-projection-bench -- \
  build "$graph" "$facts"
cargo run --release --features bench-internals --bin current-projection-bench -- \
  measure "$graph" "$facts" "$samples" >"$receipt"
node scripts/validate-current-projection-receipt.mjs "$receipt" "$profile"
node scripts/audit-current-projection-validator.mjs "$receipt" "$profile"
