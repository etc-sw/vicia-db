#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="${1:-smoke}"
output_dir="${2:-target/leaf-read-path/$profile}"
case "$profile" in
  smoke) facts=10000; samples=5 ;;
  full) facts=1000000; samples=20 ;;
  *) echo "usage: $0 [smoke|full] [output-directory]" >&2; exit 2 ;;
esac

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
fixture="${VICIA_LEAF_READ_FIXTURE:-$repo_root/target/leaf-read-path/shared/fill-90-v12-${facts}.graph}"

cargo build --release --features bench-internals --bin leaf-read-path-bench
binary="$repo_root/target/release/leaf-read-path-bench"
"$binary" build "$fixture" "$facts"
"$binary" point "$fixture" "$facts" "$samples" >"$output_dir/point.json"
"$binary" aggregate "$fixture" "$facts" "$samples" >"$output_dir/aggregate.json"

node scripts/write-leaf-read-path-receipt.mjs \
  "$profile" "$facts" "$fixture" "$output_dir/point.json" \
  "$output_dir/aggregate.json" "$output_dir/receipt.json"
node scripts/validate-leaf-read-path-receipt.mjs "$output_dir/receipt.json" "$profile"
