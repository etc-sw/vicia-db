# SIMD Benchmarking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three Criterion benchmark groups comparing scalar vs explicit SIMD (via `wide`) for valid-time filter, tx-time as-of filter, and aggregation sum, then write a crossover analysis and recommendation.

**Architecture:** All SIMD code lives in `benches/simd_helpers.rs` (a new file). Both scalar and SIMD benchmarks operate on synthetic `Vec<i64>`/`Vec<u64>` data — `Fact` internals are `pub(crate)` and inaccessible from bench code, so we benchmark the kernel loops directly. `wide` is added to the non-WASM `[dev-dependencies]` block only (zero binary size impact). A single `bench_simd` function registers all three groups.

**Tech Stack:** Rust stable, `criterion 0.8`, `wide 0.7`, `cargo bench`

---

## File Structure

| File | Change |
|---|---|
| `Cargo.toml` | Add `wide = "0.7"` to non-WASM dev-deps |
| `benches/simd_helpers.rs` | New: `valid_time_filter_simd`, `as_of_filter_simd`, `sum_simd_i64` + unit tests |
| `benches/minigraf_bench.rs` | Add `mod simd_helpers;`, `bench_simd` function, register in `criterion_group!` |
| `docs/simd-analysis.md` | New: benchmark results table, crossover analysis, recommendation |

---

### Task 1: Git worktree

**Files:** None

- [ ] **Step 1: Create worktree**

```bash
git worktree add .worktrees/issue-229-simd-benchmarking -b feature/issue-229-simd-benchmarking
cd .worktrees/issue-229-simd-benchmarking
```

Expected: `.worktrees/issue-229-simd-benchmarking` created on new branch `feature/issue-229-simd-benchmarking`.

---

### Task 2: Add `wide` to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

`wide` must live in the same cfg-gated dev-dep block as `criterion` so it is not compiled for WASM targets.

- [ ] **Step 1: Add `wide` to the non-WASM dev-dependencies block**

Open `Cargo.toml`. Find the block:
```toml
[target.'cfg(not(target_arch = "wasm32"))'.dev-dependencies]
tempfile = "3"
criterion = { version = "0.8", features = ["html_reports"] }
```

Add `wide` on the line after `criterion`:
```toml
[target.'cfg(not(target_arch = "wasm32"))'.dev-dependencies]
tempfile = "3"
criterion = { version = "0.8", features = ["html_reports"] }
wide = "0.7"
```

- [ ] **Step 2: Verify the build**

```bash
cargo check --benches 2>&1 | tail -5
```

Expected: `Finished` — no errors. `wide` resolves successfully.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add wide 0.7 to non-WASM dev-dependencies (#229)"
```

---

### Task 3: Failing unit tests for SIMD helpers

**Files:**
- Create: `benches/simd_helpers.rs`

Write the file skeleton and unit tests only — the three functions are declared but bodies are `todo!()`. Tests verify that SIMD output matches scalar output, so they will fail at runtime (panic on `todo!()`). The file also needs allow attributes for clippy lints that fire in benchmark/helper code.

- [ ] **Step 1: Create `benches/simd_helpers.rs` with stubs and tests**

```rust
//! SIMD kernel benchmarks for issue #229.
//!
//! All three functions operate on pre-extracted numeric slices.
//! `Fact` internals are `pub(crate)` and unavailable from bench code;
//! benchmarks use synthetic data to isolate the hot loop.
//!
//! `wide` API notes (verify against docs.rs/wide/0.7):
//!   - `i64x4::new([a, b, c, d])` — construct from array
//!   - `i64x4::splat(v)` — broadcast scalar to all lanes
//!   - `a.cmp_le(b)` — returns i64x4 with all-bits-1 where ≤, 0 elsewhere
//!   - `a.cmp_gt(b)` — returns i64x4 with all-bits-1 where >, 0 elsewhere
//!   - `let arr: [i64; 4] = simd_val.into()` — extract lanes
//!   - `u64x2` is the widest unsigned-64 type in wide 0.7 (no u64x4)

#![allow(clippy::cast_possible_truncation)] // synthetic bench data: N ≤ 1M fits in i64
#![allow(clippy::cast_sign_loss)]           // tx_count cast: monotonic counter, always positive

use wide::{i64x4, u64x2};

/// Count facts where `valid_from[i] <= ts && ts < valid_to[i]`.
///
/// Processes 4 facts per SIMD step using `i64x4`. Scalar tail handles remainder.
/// Both slices must be the same length.
pub fn valid_time_filter_simd(valid_from: &[i64], valid_to: &[i64], ts: i64) -> usize {
    todo!("implement in Task 4")
}

/// Count facts where `tx_counts[i] <= threshold`.
///
/// Processes 2 facts per SIMD step using `u64x2` (widest unsigned-64 in wide 0.7).
/// Scalar tail handles remainder.
pub fn as_of_filter_simd(tx_counts: &[u64], threshold: u64) -> usize {
    todo!("implement in Task 4")
}

/// Sum all values using `i64x4` horizontal reduction.
///
/// Accumulates 4 lanes in parallel; reduces to scalar at the end.
/// Wrapping arithmetic matches Rust's default release-mode overflow behaviour.
pub fn sum_simd_i64(values: &[i64]) -> i64 {
    todo!("implement in Task 4")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── valid_time_filter_simd ────────────────────────────────────────────────

    #[test]
    fn test_valid_time_filter_matches_scalar_partial() {
        // 6 facts (non-multiple of 4 to exercise tail path)
        // valid window: [i, i + 3]; ts = 2; facts 0,1,2 pass, facts 3,4,5 fail
        let valid_from: Vec<i64> = (0_i64..6).collect();
        let valid_to: Vec<i64> = valid_from.iter().map(|&vf| vf + 3).collect();
        let ts = 2_i64;

        let scalar = valid_from
            .iter()
            .zip(valid_to.iter())
            .filter(|(&vf, &vt)| vf <= ts && ts < vt)
            .count();

        assert_eq!(valid_time_filter_simd(&valid_from, &valid_to, ts), scalar);
    }

    #[test]
    fn test_valid_time_filter_matches_scalar_exact_chunk() {
        // 8 facts (exactly two SIMD chunks)
        let valid_from: Vec<i64> = (0_i64..8).collect();
        let valid_to: Vec<i64> = valid_from.iter().map(|&vf| vf + 4).collect();
        let ts = 3_i64;

        let scalar = valid_from
            .iter()
            .zip(valid_to.iter())
            .filter(|(&vf, &vt)| vf <= ts && ts < vt)
            .count();

        assert_eq!(valid_time_filter_simd(&valid_from, &valid_to, ts), scalar);
    }

    #[test]
    fn test_valid_time_filter_empty() {
        assert_eq!(valid_time_filter_simd(&[], &[], 0), 0);
    }

    // ── as_of_filter_simd ─────────────────────────────────────────────────────

    #[test]
    fn test_as_of_filter_matches_scalar_partial() {
        // 9 tx_counts (non-multiple of 2 to exercise tail)
        let tx_counts: Vec<u64> = (1..=9).collect();
        let threshold = 5_u64;

        let scalar = tx_counts.iter().filter(|&&tc| tc <= threshold).count();

        assert_eq!(as_of_filter_simd(&tx_counts, threshold), scalar);
    }

    #[test]
    fn test_as_of_filter_matches_scalar_exact_chunk() {
        // 10 tx_counts (exactly five 2-wide chunks)
        let tx_counts: Vec<u64> = (1..=10).collect();
        let threshold = 7_u64;

        let scalar = tx_counts.iter().filter(|&&tc| tc <= threshold).count();

        assert_eq!(as_of_filter_simd(&tx_counts, threshold), scalar);
    }

    #[test]
    fn test_as_of_filter_empty() {
        assert_eq!(as_of_filter_simd(&[], 100), 0);
    }

    // ── sum_simd_i64 ──────────────────────────────────────────────────────────

    #[test]
    fn test_sum_simd_matches_scalar_partial() {
        // 9 values (non-multiple of 4 to exercise tail)
        let values: Vec<i64> = (1_i64..=9).collect();
        let scalar: i64 = values.iter().sum();

        assert_eq!(sum_simd_i64(&values), scalar);
    }

    #[test]
    fn test_sum_simd_matches_scalar_exact_chunk() {
        // 8 values (exactly two 4-wide chunks)
        let values: Vec<i64> = (1_i64..=8).collect();
        let scalar: i64 = values.iter().sum();

        assert_eq!(sum_simd_i64(&values), scalar);
    }

    #[test]
    fn test_sum_simd_empty() {
        assert_eq!(sum_simd_i64(&[]), 0);
    }

    #[test]
    fn test_sum_simd_negatives() {
        let values = vec![-3_i64, -2, -1, 0, 1, 2, 3];
        let scalar: i64 = values.iter().sum();

        assert_eq!(sum_simd_i64(&values), scalar);
    }
}
```

- [ ] **Step 2: Run the tests and confirm they panic on `todo!()`**

```bash
cargo test -p minigraf simd_helpers 2>&1 | head -20
```

Expected: tests panic with `not yet implemented: implement in Task 4`. This is the TDD red phase.

---

### Task 4: Implement SIMD helper functions

**Files:**
- Modify: `benches/simd_helpers.rs`

Replace each `todo!()` body with the real implementation. Use `chunks_exact` for SIMD iteration (avoids `clippy::indexing_slicing`). Use `get(rem..).unwrap_or(&[])` for the scalar tail (avoids `clippy::indexing_slicing` and `clippy::unwrap_used`).

Before writing, verify the exact `wide 0.7` API at `https://docs.rs/wide/0.7` — specifically:
- Is `cmp_le` the correct method name for ≤ comparison on `i64x4`?
- Does `cmp_gt` exist for `i64x4`?
- Does `u64x2` support `cmp_le`?
- Can you extract lanes with `let arr: [i64; 4] = v.into()` or is it `v.to_array()`?

- [ ] **Step 1: Implement `valid_time_filter_simd`**

Replace the `todo!()` in `valid_time_filter_simd` with:

```rust
pub fn valid_time_filter_simd(valid_from: &[i64], valid_to: &[i64], ts: i64) -> usize {
    let ts_v = i64x4::splat(ts);
    let mut count = 0usize;

    for (vf_chunk, vt_chunk) in valid_from
        .chunks_exact(4)
        .zip(valid_to.chunks_exact(4))
    {
        // Slice patterns: chunks_exact guarantees len == 4, so the else branch
        // is unreachable (unreachable = "allow" in workspace lints)
        let [vf0, vf1, vf2, vf3] = *vf_chunk else { unreachable!() };
        let [vt0, vt1, vt2, vt3] = *vt_chunk else { unreachable!() };

        let vf = i64x4::new([vf0, vf1, vf2, vf3]);
        let vt = i64x4::new([vt0, vt1, vt2, vt3]);

        // vf <= ts  AND  ts < vt  (i.e., vt > ts)
        // Note: verify method names against docs.rs/wide/0.7 before finalising
        let lo = vf.cmp_le(ts_v);
        let hi = vt.cmp_gt(ts_v);
        let mask = lo & hi;

        // All-bits-1 = true lane (i64 = -1); 0 = false lane
        let arr: [i64; 4] = mask.into();
        count += arr.iter().filter(|&&x| x != 0).count();
    }

    // Scalar tail for remaining facts (0–3 elements)
    let rem = (valid_from.len() / 4) * 4;
    for (&vf, &vt) in valid_from
        .get(rem..)
        .unwrap_or(&[])
        .iter()
        .zip(valid_to.get(rem..).unwrap_or(&[]).iter())
    {
        if vf <= ts && ts < vt {
            count += 1;
        }
    }

    count
}
```

- [ ] **Step 2: Implement `as_of_filter_simd`**

Replace the `todo!()` in `as_of_filter_simd` with:

```rust
pub fn as_of_filter_simd(tx_counts: &[u64], threshold: u64) -> usize {
    let thr_v = u64x2::splat(threshold);
    let mut count = 0usize;

    for chunk in tx_counts.chunks_exact(2) {
        // Slice pattern: chunks_exact guarantees len == 2
        let [a, b] = *chunk else { unreachable!() };
        let v = u64x2::new([a, b]);

        // Note: verify u64x2::cmp_le exists in wide 0.7; if not, use manual comparison:
        //   let mask = u64x2::new([(if a <= threshold { u64::MAX } else { 0 }), (if b <= threshold { u64::MAX } else { 0 })]);
        let mask = v.cmp_le(thr_v);
        let arr: [u64; 2] = mask.into();
        count += arr.iter().filter(|&&x| x != 0).count();
    }

    let rem = (tx_counts.len() / 2) * 2;
    for &tc in tx_counts.get(rem..).unwrap_or(&[]) {
        if tc <= threshold {
            count += 1;
        }
    }

    count
}
```

**Note:** `u64x2` is only 2-wide (wide 0.7 has no `u64x4`). If `u64x2::cmp_le` does not exist, replace the SIMD comparison with the manual mask construction shown in the comment above.

- [ ] **Step 3: Implement `sum_simd_i64`**

Replace the `todo!()` in `sum_simd_i64` with:

```rust
pub fn sum_simd_i64(values: &[i64]) -> i64 {
    let mut acc = i64x4::splat(0_i64);

    for chunk in values.chunks_exact(4) {
        let [a, b, c, d] = *chunk else { unreachable!() };
        acc += i64x4::new([a, b, c, d]);
    }

    // Horizontal reduction: sum the 4 lanes
    let lanes: [i64; 4] = acc.into();
    let mut sum: i64 = lanes.iter().sum();

    // Scalar tail
    let rem = (values.len() / 4) * 4;
    for &v in values.get(rem..).unwrap_or(&[]) {
        sum = sum.wrapping_add(v);
    }

    sum
}
```

- [ ] **Step 4: Run the tests — verify all pass**

```bash
cargo test -p minigraf simd_helpers 2>&1 | tail -15
```

Expected: all 10 tests PASS. If the `wide` API differs from the pseudocode (e.g., method not found), read `cargo test` errors, check `https://docs.rs/wide/0.7`, adapt accordingly.

- [ ] **Step 5: Run clippy to confirm no new lint errors**

```bash
cargo clippy -- -D warnings 2>&1 | grep "^error" | wc -l
```

Expected: 0.

- [ ] **Step 6: Commit**

```bash
git add benches/simd_helpers.rs
git commit -m "feat(bench): add SIMD helper kernels for temporal filter and sum (#229)"
```

---

### Task 5: Add `bench_simd` to the benchmark file

**Files:**
- Modify: `benches/minigraf_bench.rs`

Add `mod simd_helpers;` at the top, add the `bench_simd` function, and register it in `criterion_group!`.

- [ ] **Step 1: Add `mod simd_helpers;` after the existing `mod helpers;` line**

Open `benches/minigraf_bench.rs`. Line 20 is `mod helpers;`. Insert immediately after it:

```rust
#[cfg(not(target_arch = "wasm32"))]
mod simd_helpers;
```

The `cfg` gate is needed because `wide` is only in the non-WASM dev-deps.

- [ ] **Step 2: Add `use criterion::black_box;` to the imports**

Find the existing use statement:
```rust
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
```

Replace with:
```rust
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
```

- [ ] **Step 3: Add the `bench_simd` function**

Add the following before the `criterion_group!` macro (after `bench_predicate_pushdown`):

```rust
// ── SIMD kernel benchmarks (Issue #229) ──────────────────────────────────────
//
// Both scalar and SIMD variants operate on synthetic Vec<i64>/Vec<u64> data.
// Fact internals (valid_from, valid_to, tx_count) are pub(crate) and unavailable
// from bench code; benchmarks isolate the hot loop rather than the full query path.
// Cross-reference the existing time_travel/ and aggregation/ groups for full-query costs.

#[cfg(not(target_arch = "wasm32"))]
fn bench_simd(c: &mut Criterion) {
    const SCALES: &[(&str, usize)] = &[
        ("100", 100),
        ("1k", 1_000),
        ("10k", 10_000),
        ("100k", 100_000),
        ("1m", 1_000_000),
    ];

    // ── simd_temporal: valid-time range filter ──────────────────────────────
    //
    // Synthetic data: valid_from[i] = i, valid_to[i] = i + n/2.
    // ts = n/4 → ~50% of facts are valid at ts (those with valid_from ≤ n/4 < valid_to).
    {
        let mut group = c.benchmark_group("simd_temporal");
        group.sample_size(10);

        for &(label, n) in SCALES {
            let n_i64 = i64::try_from(n).unwrap_or(i64::MAX);
            let valid_from: Vec<i64> = (0_i64..n_i64).collect();
            let valid_to: Vec<i64> = valid_from.iter().map(|&vf| vf + n_i64 / 2).collect();
            let ts = n_i64 / 4;

            group.bench_with_input(BenchmarkId::new("scalar", label), &n, |b, _| {
                b.iter(|| {
                    valid_from
                        .iter()
                        .zip(valid_to.iter())
                        .filter(|(&vf, &vt)| vf <= black_box(ts) && black_box(ts) < vt)
                        .count()
                })
            });

            group.bench_with_input(BenchmarkId::new("simd", label), &n, |b, _| {
                b.iter(|| {
                    simd_helpers::valid_time_filter_simd(
                        black_box(&valid_from),
                        black_box(&valid_to),
                        black_box(ts),
                    )
                })
            });
        }

        group.finish();
    }

    // ── simd_as_of: tx-time as-of filter ───────────────────────────────────
    //
    // Synthetic data: tx_counts = 1..=n (monotonic counter).
    // threshold = n/2 → 50% of facts pass the filter.
    {
        let mut group = c.benchmark_group("simd_as_of");
        group.sample_size(10);

        for &(label, n) in SCALES {
            let n_u64 = u64::try_from(n).unwrap_or(u64::MAX);
            let tx_counts: Vec<u64> = (1..=n_u64).collect();
            let threshold = n_u64 / 2;

            group.bench_with_input(BenchmarkId::new("scalar", label), &n, |b, _| {
                b.iter(|| {
                    tx_counts
                        .iter()
                        .filter(|&&tc| tc <= black_box(threshold))
                        .count()
                })
            });

            group.bench_with_input(BenchmarkId::new("simd", label), &n, |b, _| {
                b.iter(|| {
                    simd_helpers::as_of_filter_simd(
                        black_box(&tx_counts),
                        black_box(threshold),
                    )
                })
            });
        }

        group.finish();
    }

    // ── simd_aggregate: i64 horizontal sum ─────────────────────────────────
    //
    // Synthetic data: values = 0..n as i64.
    // Measures horizontal reduction performance.
    {
        let mut group = c.benchmark_group("simd_aggregate");
        group.sample_size(10);

        for &(label, n) in SCALES {
            let n_i64 = i64::try_from(n).unwrap_or(i64::MAX);
            let values: Vec<i64> = (0_i64..n_i64).collect();

            group.bench_with_input(BenchmarkId::new("scalar", label), &n, |b, _| {
                b.iter(|| black_box(&values).iter().copied().sum::<i64>())
            });

            group.bench_with_input(BenchmarkId::new("simd", label), &n, |b, _| {
                b.iter(|| simd_helpers::sum_simd_i64(black_box(&values)))
            });
        }

        group.finish();
    }
}
```

- [ ] **Step 4: Register `bench_simd` in `criterion_group!`**

Find the `criterion_group!` macro at the end of the file. Add `bench_simd` after `bench_predicate_pushdown`:

```rust
criterion_group!(
    benches,
    bench_insert,
    bench_insert_file,
    bench_query,
    bench_time_travel,
    bench_recursion,
    bench_negation,
    bench_disjunction,
    bench_aggregation,
    bench_expr,
    bench_window,
    bench_temporal_metadata,
    bench_udf,
    bench_aggregation_extras,
    bench_query_extras,
    bench_open,
    bench_checkpoint,
    bench_concurrent,
    bench_concurrent_file,
    bench_concurrent_btree_scan,
    bench_prepared,
    bench_retract,
    bench_btree_lookup,
    bench_predicate_pushdown,
    bench_simd,       // Issue #229
);
```

**Note:** `bench_simd` is defined under `#[cfg(not(target_arch = "wasm32"))]`. If the compiler complains about the `bench_simd` reference in `criterion_group!` being absent on WASM, wrap the entire criterion_group!/criterion_main! with the same cfg — but this is unlikely since `criterion` itself is already WASM-gated.

- [ ] **Step 5: Verify the bench binary compiles**

```bash
cargo build --benches 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 6: Run just the SIMD benchmarks to verify they execute**

```bash
cargo bench -- simd 2>&1 | grep -E "simd_|Benchmarking|error" | head -30
```

Expected: Groups `simd_temporal`, `simd_as_of`, `simd_aggregate` all run and report timing numbers. No errors.

- [ ] **Step 7: Run the full test suite to check for regressions**

```bash
cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add benches/minigraf_bench.rs
git commit -m "feat(bench): add simd_temporal, simd_as_of, simd_aggregate benchmark groups (#229)"
```

---

### Task 6: Run benchmarks and write `docs/simd-analysis.md`

**Files:**
- Create: `docs/simd-analysis.md`

Run the full benchmark suite for the new SIMD groups, collect results, and write the analysis document. This is the deliverable that closes issue #229.

- [ ] **Step 1: Run all three SIMD benchmark groups**

```bash
cargo bench -- simd 2>&1 | tee /tmp/simd_bench_results.txt
```

This will take several minutes (5 dataset sizes × 2 variants × 3 groups × ~10 samples each). Let it complete fully.

- [ ] **Step 2: Also collect the existing time_travel results for comparison context**

```bash
cargo bench -- time_travel 2>&1 | tee /tmp/time_travel_results.txt
```

These give the full-query costs for context.

- [ ] **Step 3: Extract the mean ns/iter values from the output**

From `/tmp/simd_bench_results.txt`, find lines like:
```
simd_temporal/scalar/100    time:   [XXX ns XXX ns XXX ns]
simd_temporal/simd/100      time:   [XXX ns XXX ns XXX ns]
```

Record the middle (mean) value for each. Build a table with columns: group, variant, size, mean_ns.

- [ ] **Step 4: Write `docs/simd-analysis.md`**

Use the template below, filling in actual numbers from Step 3:

```markdown
# SIMD Benchmarking Analysis (Issue #229)

**Date:** 2026-05-16  
**Environment:** [fill in: CPU model, OS, Rust toolchain, wide version]  
**Branch:** feature/issue-229-simd-benchmarking

---

## Method

Both scalar and SIMD benchmarks operate on synthetic numeric slices (not live DB facts — `Fact` internals are `pub(crate)`). The benchmarks isolate the hot kernel loop; extraction overhead is identical for both paths (none). For full-query costs, see the `time_travel/` group results below.

---

## Results: simd_temporal (valid-time range filter)

`valid_from[i] <= ts && ts < valid_to[i]` — ~50% selectivity

| Size | Scalar (ns) | SIMD (ns) | Speedup |
|------|------------|-----------|---------|
| 100  | XXX        | XXX       | X.Xx    |
| 1K   | XXX        | XXX       | X.Xx    |
| 10K  | XXX        | XXX       | X.Xx    |
| 100K | XXX        | XXX       | X.Xx    |
| 1M   | XXX        | XXX       | X.Xx    |

**Crossover point:** [first size where SIMD is faster, or "none observed at any tested size"]

---

## Results: simd_as_of (tx-time as-of filter)

`tx_count[i] <= threshold` — 50% selectivity. Note: uses `u64x2` (2-wide) — wide 0.7 has no `u64x4`.

| Size | Scalar (ns) | SIMD (ns) | Speedup |
|------|------------|-----------|---------|
| 100  | XXX        | XXX       | X.Xx    |
| 1K   | XXX        | XXX       | X.Xx    |
| 10K  | XXX        | XXX       | X.Xx    |
| 100K | XXX        | XXX       | X.Xx    |
| 1M   | XXX        | XXX       | X.Xx    |

**Crossover point:** [first size where SIMD is faster, or "none observed"]

---

## Results: simd_aggregate (i64 horizontal sum)

Sum of N i64 values via `i64x4` 4-wide reduction.

| Size | Scalar (ns) | SIMD (ns) | Speedup |
|------|------------|-----------|---------|
| 100  | XXX        | XXX       | X.Xx    |
| 1K   | XXX        | XXX       | X.Xx    |
| 10K  | XXX        | XXX       | X.Xx    |
| 100K | XXX        | XXX       | X.Xx    |
| 1M   | XXX        | XXX       | X.Xx    |

**Crossover point:** [first size where SIMD is faster, or "none observed"]

---

## Post-#208 Residual Analysis

After B+Tree selective lookup (#208), queries with bound entity or entity+attribute patterns
route through index-backed lookups rather than full fact scans. Typical residual fact counts
reaching the temporal filter after selective lookup (from `btree_lookup/entity_point` data):

- Entity-point query on 1K facts: ~1 fact residual (index lookup, no filter needed)
- Entity-point query on 100K facts: ~1 fact residual
- Full-attribute scan on 100K facts: [N facts from btree_lookup/attribute_scan]

For unbound queries (no selective lookup), all N facts reach the temporal filter.
SIMD gains apply to this case.

---

## Full-Query Context

From `time_travel/as_of_counter` and `time_travel/valid_at` (existing benchmarks):

| Size | as_of_counter (ns) | valid_at (ns) |
|------|-------------------|---------------|
| 1K   | XXX               | XXX           |
| 10K  | XXX               | XXX           |
| 100K | XXX               | XXX           |
| 1M   | XXX               | XXX           |

The full query cost includes: fact loading, temporal filtering, pattern matching, result projection.
If temporal filtering is Z% of query cost, SIMD speedup of Sx translates to ~Z*(S-1)/S % query improvement.

---

## Recommendation

[Fill in ONE of the following:]

**Integrate:** SIMD wins at sizes ≥ [N] which covers typical unbound-query workloads. Recommend
opening a follow-up issue to promote `simd_helpers` functions into production code paths in
`src/graph/storage.rs` (temporal filters) and `src/query/datalog/functions.rs` (sum aggregation).
Prerequisite: a struct-of-arrays storage layout change would allow zero-copy slice extraction.

**Skip:** SIMD is slower or provides < 10% gain at all practical embedded sizes (≤ 100K facts).
LLVM autovectorization (enabled by `lto = true` and `codegen-units = 1` in release profile) already
captures most of the available gain. Recommend closing #229 without further SIMD work.

**Revisit with SoA layout:** SIMD wins at ≥ [N] facts but extraction overhead (not measured here —
facts are struct-of-arrays-inaccessible from bench code) would dominate in production. A
struct-of-arrays storage layout change is a prerequisite for SIMD to pay off. Recommend opening
a separate issue to evaluate SoA storage before revisiting SIMD.
```

- [ ] **Step 5: Commit the analysis document**

```bash
git add docs/simd-analysis.md
git commit -m "docs: SIMD benchmarking analysis and recommendation (#229)"
```

---

### Task 7: Full verification and PR

**Files:** None (no new code changes unless a check fails)

- [ ] **Step 1: Full test suite**

```bash
cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 2: Clippy clean**

```bash
cargo clippy -- -D warnings 2>&1 | grep "^error" | wc -l
```

Expected: 0.

- [ ] **Step 3: Format check**

```bash
cargo fmt --check
```

Expected: no output. If there is a diff, run `cargo fmt` and commit `style: cargo fmt (#229)`.

- [ ] **Step 4: Verify WASM build is unaffected**

```bash
cargo build --features wasm --target wasm32-unknown-unknown 2>&1 | grep "^error" | wc -l
```

Expected: 0. (`wide` is in the non-WASM dev-deps only; WASM build should not even see it.)

- [ ] **Step 5: Check commit log**

```bash
git log main..HEAD --oneline
```

Expected: 5 commits (worktree excluded):
```
docs: SIMD benchmarking analysis and recommendation (#229)
feat(bench): add simd_temporal, simd_as_of, simd_aggregate benchmark groups (#229)
feat(bench): add SIMD helper kernels for temporal filter and sum (#229)
chore: add wide 0.7 to non-WASM dev-dependencies (#229)
```

- [ ] **Step 6: Open PR**

```bash
gh pr create \
  --title "feat(bench): SIMD benchmarking and crossover analysis (#229)" \
  --body "$(cat <<'EOF'
## Summary
- Adds `wide 0.7` as a non-WASM dev-dependency (zero production binary size impact)
- New `benches/simd_helpers.rs`: `valid_time_filter_simd`, `as_of_filter_simd`, `sum_simd_i64` with 10 unit tests
- Three new Criterion benchmark groups: `simd_temporal`, `simd_as_of`, `simd_aggregate` — each at 5 dataset sizes (100 → 1M), scalar vs SIMD side-by-side
- `docs/simd-analysis.md`: crossover findings and final recommendation

Closes #229

## Test plan
- [ ] All existing tests pass (`cargo test`)
- [ ] 10 new unit tests for SIMD helpers (scalar equivalence)
- [ ] SIMD benchmark groups compile and run (`cargo bench -- simd`)
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] WASM build unaffected

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 7: Monitor CI until green**

```bash
gh pr checks --watch
```

Expected: all checks green.
