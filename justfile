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

# Prove the deterministic R2-B page codec on a 10K temporal fixture.
projection-page-image-smoke OUTPUT_DIR="target/projection-page-image/smoke":
    ./scripts/run-projection-page-image-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the detached R2-B projection page-image gate on the exact 1M fixture.
projection-page-image-full OUTPUT_DIR="target/projection-page-image/full":
    ./scripts/run-projection-page-image-bench.sh full "{{OUTPUT_DIR}}"

# Smoke the paired source-versus-decoded R2-B query-tail receipt.
projection-page-tail-smoke OUTPUT_DIR="target/projection-page-tail/smoke":
    ./scripts/run-projection-page-tail-bench.sh smoke "{{OUTPUT_DIR}}"

# Run the clean 1M paired source-versus-decoded R2-B query-tail gate.
projection-page-tail-full OUTPUT_DIR="target/projection-page-tail/full":
    ./scripts/run-projection-page-tail-bench.sh full "{{OUTPUT_DIR}}"

# Isolate one candidate and one temporal probe in every fresh measurement child.
projection-isolated-tail-smoke OUTPUT_DIR="target/projection-isolated-tail/smoke":
    ./scripts/run-projection-isolated-tail-bench.sh smoke "{{OUTPUT_DIR}}"

# Run 40 fresh children per candidate/probe cell on the canonical 1M fixture.
projection-isolated-tail-full OUTPUT_DIR="target/projection-isolated-tail/full":
    ./scripts/run-projection-isolated-tail-bench.sh full "{{OUTPUT_DIR}}"

# Smoke v13 projection image/catalog publication and reopen on 10K rows.
projection-publication-smoke OUTPUT_DIR="target/projection-publication/smoke":
    ./scripts/run-projection-publication-bench.sh smoke "{{OUTPUT_DIR}}"

# Admit v13 persisted selection and exact reopen over the canonical 1M fixture.
projection-publication-full OUTPUT_DIR="target/projection-publication/full":
    ./scripts/run-projection-publication-bench.sh full "{{OUTPUT_DIR}}"

# Smoke the public R2-C2 maintenance-owned projection publication path.
projection-maintenance-smoke OUTPUT_DIR="target/projection-maintenance/smoke":
    ./scripts/run-projection-maintenance-bench.sh smoke "{{OUTPUT_DIR}}"

# Measure the clean 1M public maintenance rebuild and its resource gates.
projection-maintenance-full OUTPUT_DIR="target/projection-maintenance/full":
    ./scripts/run-projection-maintenance-bench.sh full "{{OUTPUT_DIR}}"

# Smoke exact-watermark production projection routing on 10K facts.
projection-routing-smoke OUTPUT_DIR="target/projection-routing/smoke":
    ./scripts/run-projection-routing-bench.sh smoke "{{OUTPUT_DIR}}"

# Admit exact-watermark production projection routing on the canonical 1M fixture.
projection-routing-full OUTPUT_DIR="target/projection-routing/full":
    ./scripts/run-projection-routing-bench.sh full "{{OUTPUT_DIR}}"

# Smoke the bounded resident-tail overlay above a 10K persisted projection.
projection-tail-overlay-smoke OUTPUT_DIR="target/projection-tail-overlay/smoke":
    ./scripts/run-projection-tail-overlay-bench.sh smoke "{{OUTPUT_DIR}}"

# Admit the resident-tail route on the canonical clean 1M fixture.
projection-tail-overlay-full OUTPUT_DIR="target/projection-tail-overlay/full":
    ./scripts/run-projection-tail-overlay-bench.sh full "{{OUTPUT_DIR}}"

# Generate and validate the browser interactive/maintenance TypeScript boundary.
browser-capability-surface OUTPUT_DIR="target/browser-capability-surface":
    wasm-pack build --target web --out-dir "{{OUTPUT_DIR}}" -- --features browser
    node scripts/validate-browser-capability-surface.mjs "{{OUTPUT_DIR}}/minigraf.d.ts"

# Compare two leaf-read receipts and enforce the candidate acceptance gates.
leaf-read-path-compare BASELINE CANDIDATE:
    node scripts/compare-leaf-read-path-receipts.mjs "{{BASELINE}}" "{{CANDIDATE}}"
