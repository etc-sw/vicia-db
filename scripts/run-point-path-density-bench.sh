#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/point-path-density/$profile}"
cargo build --release --features bench-internals --bin storage-layout-bench
target/release/storage-layout-bench point-density "$profile" "$output_dir"
node scripts/validate-point-path-density-receipt.mjs "$output_dir/receipt.json" "$profile"
node scripts/analyze-point-path-density.mjs "$output_dir/receipt.json" "$output_dir/analysis.json"
node scripts/validate-point-path-density-analysis.mjs "$output_dir/receipt.json" "$output_dir/analysis.json"
node scripts/audit-point-path-density-validators.mjs "$output_dir/receipt.json" "$output_dir/analysis.json" "$profile"
