#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/projection-page-tail/$profile}"
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
receipt="$output_dir/receipt.json"
cleanup() {
  rm -f "$graph" "$graph.wal" "$graph.lock"
}
trap cleanup EXIT

cargo run --release --features bench-internals --bin current-projection-bench -- \
  build-temporal "$graph" "$facts"
cargo run --release --features bench-internals --bin current-projection-tail-bench -- \
  run "$graph" "$facts" "$profile" >"$receipt"
node scripts/validate-projection-page-tail-receipt.mjs "$receipt" "$profile"
node scripts/audit-projection-page-tail-validator.mjs "$receipt" "$profile"
