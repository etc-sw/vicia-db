#!/usr/bin/env bash
set -euo pipefail

profile="${1:-smoke}"
output_dir="${2:-target/current-reader/$profile}"
mkdir -p "$output_dir"
receipt="$output_dir/receipt.json"

VICIA_CURRENT_READER_RECEIPT="$receipt" \
  cargo run --release --features bench-internals --bin current-reader-bench -- "$profile"
node scripts/validate-current-reader-receipt.mjs "$receipt" "$profile"
node scripts/audit-current-reader-validator.mjs "$receipt" "$profile"
