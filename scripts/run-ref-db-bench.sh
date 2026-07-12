#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="${1:-smoke}"
output_dir="${2:-target/ref-db-bench/$profile}"
case "$profile" in
  smoke)
    facts=10000
    repetitions=5
    ;;
  full)
    facts=1000000
    repetitions=20
    ;;
  *)
    echo "usage: $0 [smoke|full] [output-directory]" >&2
    exit 2
    ;;
esac

if [[ ! -f tools/ref-db-bench/Cargo.toml ]]; then
  echo "reference benchmark manifest is missing; run 'just db-ref-setup' first" >&2
  exit 2
fi

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
manifest="tools/ref-db-bench/Cargo.toml"
cargo build --release --manifest-path "$manifest"
binary="tools/ref-db-bench/target/release/vicia-ref-db-bench"

for engine in vicia grafeo redb fjall turso cozo; do
  echo "running ref-db benchmark: $engine" >&2
  "$binary" run "$engine" "$output_dir/$engine-data" "$facts" "$repetitions" \
    >"$output_dir/$engine.json"
done

node scripts/summarize-ref-db-bench.mjs "$output_dir" "$profile"
