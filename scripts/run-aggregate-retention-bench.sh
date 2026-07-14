#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="${1:-smoke}"
output_dir="${2:-target/aggregate-retention/$profile}"
case "$profile" in
  smoke|full) ;;
  *) echo "usage: $0 [smoke|full] [output-directory]" >&2; exit 2 ;;
esac

cargo build --release --features bench-internals --bin aggregate-retention-bench
target/release/aggregate-retention-bench run "$profile" "$output_dir"
node scripts/validate-aggregate-retention-receipt.mjs "$output_dir/receipt.json" "$profile"
node scripts/audit-aggregate-retention-validator.mjs "$output_dir/receipt.json" "$profile"
