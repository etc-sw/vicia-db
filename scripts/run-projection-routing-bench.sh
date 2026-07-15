#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/projection-routing/$profile}"
case "$profile" in
  smoke) facts=10000 ;;
  full) facts=1000000 ;;
  *) echo "profile must be smoke or full" >&2; exit 2 ;;
esac

mkdir -p "$output_dir"
source_graph="$output_dir/source.graph"
published_graph="$output_dir/published.graph"
fixture="$output_dir/fixture.json"
receipt="$output_dir/receipt.json"
cleanup() {
  rm -f "$source_graph" "$source_graph.wal" "$source_graph.lock"
  rm -f "$published_graph" "$published_graph.wal" "$published_graph.lock" "$fixture"
}
trap cleanup EXIT

cargo run --release --features bench-internals --bin current-projection-bench -- \
  build-temporal-provenance "$source_graph" "$facts" 90 >"$fixture"
cargo run --release --features bench-internals --bin projection-routing-bench -- \
  "$source_graph" "$published_graph" "$fixture" "$facts" "$profile" >"$receipt"
node scripts/validate-projection-routing-receipt.mjs "$receipt" "$profile"
node scripts/audit-projection-routing-validator.mjs "$receipt" "$profile"
