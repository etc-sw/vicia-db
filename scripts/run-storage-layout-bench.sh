#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/storage-layout/$profile}"
cargo build --release --features bench-internals --bin storage-layout-bench
target/release/storage-layout-bench run "$profile" "$output_dir"
node scripts/validate-storage-layout-receipt.mjs "$output_dir/receipt.json" "$profile"
node scripts/audit-storage-layout-validator.mjs "$output_dir/receipt.json" "$profile"
