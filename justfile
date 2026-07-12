# Vicia DB local development task surface.

# list recipes
default:
    @just --list

# Build the browser WASM package from this checkout and atomically install it
# into Vetch's local @vicia-db/browser package boundary.
#
# Default target: sibling ~/projects/vetch-app checkout.
# Worktree verification: just sync /absolute/path/to/vetch-worktree
sync VETCH_APP_DIR="":
    ./scripts/sync-vetch-browser-package.sh "{{VETCH_APP_DIR}}"

# Clone/update the reference engines under ~/db-ref and link the local harness.
db-ref-setup:
    ./scripts/setup-db-refs.sh

# Run the 10K/five-sample reference comparison and print/write a Markdown table.
db-ref-bench-smoke OUTPUT_DIR="target/ref-db-bench/smoke":
    ./scripts/run-ref-db-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the 1M/20-sample reference comparison.
db-ref-bench-full OUTPUT_DIR="target/ref-db-bench/full":
    ./scripts/run-ref-db-bench.sh full "{{OUTPUT_DIR}}"
