#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="${1:-smoke}"
output_dir="${2:-target/pending-isolation/$profile}"
case "$profile" in
  smoke|full) ;;
  *)
    echo "usage: $0 [smoke|full] [output-directory]" >&2
    exit 2
    ;;
esac

cargo build --release --features bench-internals --bin pending-isolation-bench
binary="target/release/pending-isolation-bench"
"$binary" run "$profile" "$output_dir"
node scripts/validate-pending-isolation-receipt.mjs \
  "$output_dir/receipt.json" "$profile"
