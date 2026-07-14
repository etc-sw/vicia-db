# Vicia DB local development task surface.

# list recipes
default:
    @just --list

# Verify and publish a clean local Vicia commit into Vetch.
sync VETCH_APP_DIR="":
    ./scripts/sync-vetch-browser-package.sh "{{VETCH_APP_DIR}}"

# Verify and publish the current dirty local worktree without commit or push.
sync-local VETCH_APP_DIR="":
    VICIA_SYNC_ALLOW_DIRTY_PUBLISH=1 ./scripts/sync-vetch-browser-package.sh "{{VETCH_APP_DIR}}"

# Clone/update the reference engines under ~/db-ref and link the local harness.
db-ref-setup:
    ./scripts/setup-db-refs.sh

# Run one rotated 10K/five-sample seven-engine v5 comparison.
db-ref-bench-smoke OUTPUT_DIR="target/ref-db-bench/smoke":
    ./scripts/run-ref-db-bench.sh smoke "{{OUTPUT_DIR}}"

# Run five rotated 1M/20-sample seven-engine v5 trials.
db-ref-bench-full OUTPUT_DIR="target/ref-db-bench/full":
    ./scripts/run-ref-db-bench.sh full "{{OUTPUT_DIR}}"

# Prove selected aggregates ignore unrelated pending WAL facts (1M committed base).
pending-isolation-smoke OUTPUT_DIR="target/pending-isolation/smoke":
    ./scripts/run-pending-isolation-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the clean-source 0/10K/100K/1M unrelated pending acceptance matrix.
pending-isolation-full OUTPUT_DIR="target/pending-isolation/full":
    ./scripts/run-pending-isolation-bench.sh full "{{OUTPUT_DIR}}"

# Compare live retained RSS after one and twenty aggregates on a 10K fixture.
aggregate-retention-smoke OUTPUT_DIR="target/aggregate-retention/smoke":
    ./scripts/run-aggregate-retention-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the clean five-pair retained-memory gate on the canonical 1M fixture.
aggregate-retention-full OUTPUT_DIR="target/aggregate-retention/full":
    ./scripts/run-aggregate-retention-bench.sh full "{{OUTPUT_DIR}}"

# Measure the 10K B-tree fill-factor and storage-layout matrix.
storage-layout-smoke OUTPUT_DIR="target/storage-layout/smoke":
    ./scripts/run-storage-layout-bench.sh smoke "{{OUTPUT_DIR}}"

# Measure the clean 1M/20-sample B-tree fill-factor matrix.
storage-layout-full OUTPUT_DIR="target/storage-layout/full":
    ./scripts/run-storage-layout-bench.sh full "{{OUTPUT_DIR}}"

# Attribute exact EAVT point-path work on 10K fill fixtures.
point-path-density-smoke OUTPUT_DIR="target/point-path-density/smoke":
    ./scripts/run-point-path-density-bench.sh smoke "{{OUTPUT_DIR}}"

# Attribute exact EAVT point-path work on clean 1M/20-sample fill fixtures.
point-path-density-full OUTPUT_DIR="target/point-path-density/full":
    ./scripts/run-point-path-density-bench.sh full "{{OUTPUT_DIR}}"

# Measure bounded checkpoint/recompact construction with a 10K base.
checkpoint-construction-smoke OUTPUT_DIR="target/checkpoint-construction/smoke":
    ./scripts/run-checkpoint-construction-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the clean 1M/20-sample checkpoint construction acceptance matrix.
checkpoint-construction-full OUTPUT_DIR="target/checkpoint-construction/full":
    ./scripts/run-checkpoint-construction-bench.sh full "{{OUTPUT_DIR}}"

# Measure the restart-aware leaf read path with a 10K/five-sample fixture.
leaf-read-path-smoke OUTPUT_DIR="target/leaf-read-path/smoke":
    ./scripts/run-leaf-read-path-bench.sh smoke "{{OUTPUT_DIR}}"

# Measure the restart-aware leaf read path with the canonical 1M/20-sample fixture.
leaf-read-path-full OUTPUT_DIR="target/leaf-read-path/full":
    ./scripts/run-leaf-read-path-bench.sh full "{{OUTPUT_DIR}}"

# Prove typed current entity and reverse-reference reads stay selective at 10K.
current-reader-smoke OUTPUT_DIR="target/current-reader/smoke":
    ./scripts/run-current-reader-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the typed current-reader structural gate over a 1M VAET/EAVT fixture.
current-reader-full OUTPUT_DIR="target/current-reader/full":
    ./scripts/run-current-reader-bench.sh full "{{OUTPUT_DIR}}"

# Probe the rebuildable compact current projection on a 10K fixture.
current-projection-smoke OUTPUT_DIR="target/current-projection/smoke":
    ./scripts/run-current-projection-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the R1 current-projection admission gate on the exact 1M fixture.
current-projection-full OUTPUT_DIR="target/current-projection/full":
    ./scripts/run-current-projection-bench.sh full "{{OUTPUT_DIR}}"

# Run the R2-A temporal projection risk probe on a 10K fixture.
temporal-projection-smoke OUTPUT_DIR="target/temporal-projection/smoke":
    ./scripts/run-temporal-projection-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the R2-A temporal projection admission gate on the exact 1M fixture.
temporal-projection-full OUTPUT_DIR="target/temporal-projection/full":
    ./scripts/run-temporal-projection-bench.sh full "{{OUTPUT_DIR}}"

# Generate and validate the browser interactive/maintenance TypeScript boundary.
browser-capability-surface OUTPUT_DIR="target/browser-capability-surface":
    wasm-pack build --target web --out-dir "{{OUTPUT_DIR}}" -- --features browser
    node scripts/validate-browser-capability-surface.mjs "{{OUTPUT_DIR}}/minigraf.d.ts"

# Compare two leaf-read receipts and enforce the candidate acceptance gates.
leaf-read-path-compare BASELINE CANDIDATE:
    node scripts/compare-leaf-read-path-receipts.mjs "{{BASELINE}}" "{{CANDIDATE}}"
