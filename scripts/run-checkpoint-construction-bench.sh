#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/checkpoint-construction/$profile}"
cargo build --release --features bench-internals --bin checkpoint-construction-bench
target/release/checkpoint-construction-bench run "$profile" "$output_dir"
node scripts/validate-checkpoint-construction-receipt.mjs "$output_dir/receipt.json" "$profile"
