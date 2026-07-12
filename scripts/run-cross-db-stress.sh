#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="${1:-smoke}"
output_dir="${2:-target/cross-db-stress/$profile}"
case "$profile" in
  smoke)
    base_facts=10000
    cycles=20
    facts_per_cycle=10
    reads_per_cycle=20
    ;;
  full)
    base_facts=1000000
    cycles=100
    facts_per_cycle=100
    reads_per_cycle=100
    ;;
  *)
    echo "usage: $0 [smoke|full] [output-directory]" >&2
    exit 2
    ;;
esac

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
manifest="tools/cross-db-bench/Cargo.toml"

cargo build --release --manifest-path "$manifest"
binary="tools/cross-db-bench/target/release/vicia-cross-db-bench"

for engine in vicia cozo sqlite redb; do
  echo "running cross-db stress: $engine"
  "$binary" stress \
    "$engine" \
    "$output_dir/$engine-data" \
    "$base_facts" \
    "$cycles" \
    "$facts_per_cycle" \
    "$reads_per_cycle" \
    "$output_dir/$engine.json" \
    >"$output_dir/$engine.stdout.json"
done

node scripts/run-cross-db-crash.mjs "$binary" "$output_dir" "$facts_per_cycle"
node scripts/summarize-cross-db-stress.mjs "$output_dir" "$profile"
