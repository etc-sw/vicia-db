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
    trials=1
    ;;
  full)
    facts=1000000
    repetitions=20
    trials=5
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

engines=(vicia grafeo sqlite redb fjall turso cozo)
seed_base="${REF_DB_BENCH_SEED:-2026071401}"
for ((trial = 0; trial < trials; trial++)); do
  seed=$((seed_base + trial))
  for ((position = 0; position < ${#engines[@]}; position++)); do
    engine_index=$(((trial + position) % ${#engines[@]}))
    engine="${engines[$engine_index]}"
    echo "running ref-db benchmark: trial=$trial position=$position engine=$engine" >&2
    REF_DB_BENCH_ORDER_POSITION="$position" \
      "$binary" run "$engine" "$output_dir/$engine-trial-$trial-data" \
      "$facts" "$repetitions" "$trial" "$seed" \
      >"$output_dir/$engine-trial-$trial.json"
  done
done

node scripts/summarize-ref-db-bench.mjs "$output_dir" "$profile"
node scripts/audit-ref-db-bench-validator.mjs "$output_dir" "$profile"
