# Benchmark Development Milestones

Vicia performance work advances through measured decisions, not isolated
before/after numbers. `benchmarks/milestones.json` is the machine-readable
authority for milestone ids, owners, profiles, commands, and absolute budgets.
This document explains how to use that authority during development.

## Milestone map

| Milestone | Question it closes | Evidence tier | Owner |
|---|---|---|---|
| M1 engine mechanisms | Did a query/storage mechanism regress against same-testbed history? | Nightly Criterion + Bencher | Vicia core |
| M2 Vetch interactive cadence | Are capture, edit, receipt, point reads, and checkpoint interactive at 1M? | Native smoke + dedicated full | Vicia/Vetch boundary |
| M3 delta growth and recovery | Does repeated checkpointing remain bounded and fallback-safe? | Native smoke + T8B mini + full matrix | Vicia storage |
| M4 agent-brief selectivity | Can a just-written receipt be read without a committed-base scan? | Native smoke + dedicated full | Vicia query |
| M5 browser bounded lifecycle | Do 1M foreground browser operations stay bounded while O(total) work remains disposable? | Real-Chrome paged matrix | Vicia browser |
| M6 Vetch product acceptance | Does the exact product trace preserve cadence, memory, growth, query shape, fingerprints, maintenance, and reopen? | Vetch Gate D full | Vetch quiet surface |
| M7 cross-engine characterization | Where does Vicia spend time, memory, and bytes relative to classified embedded peers, and do all survive kill-9? | Weekly smoke + dedicated full | Vicia performance |

M1 is longitudinal evidence and does not by itself close a product milestone.
M2-M6 are absolute acceptance boundaries. M7 is comparative evidence and does
not define an overall winner or a release budget. A storage or query optimization must
name the affected milestone before implementation; otherwise it has no honest
performance completion condition.

## Development loop

1. **Name the decision.** Select the milestone whose responsibility and state
   transition match the proposed change.
2. **Capture the base.** Run the relevant full profile from a clean source
   checkout on a named testbed. Generate the fixture once and pass the exact
   `.graph` image to both base and candidate runs. Preserve its SHA-256 and both
   JSON receipts.
3. **Develop against smoke.** Run all three native smoke profiles. Correctness,
   recovery, receipt shape, and generous absolute ceilings must stay green.
4. **Run the candidate gate.** Re-run the same profile, fixture shape,
   cold/warm policy, and testbed as the base.
5. **Close or redirect.** A passing clean full/release receipt may close the
   milestone. A failure identifies the exact metric and must redirect work to
   that boundary; it does not justify broad API or storage expansion.

Base and candidate receipts from different testbeds are not a performance
comparison. Shared-runner Criterion history detects candidates for review;
dedicated-host or real-browser absolute gates close product milestones.

## Native smoke gate

The native gate exercises distinct system responsibilities:

```bash
VICIA_BENCH_TESTBED=local-a0 ./scripts/run-benchmark-gate.sh smoke
```

The script generates one exact shared fixture, runs the three native suites,
validates every receipt, prints a gate summary, and keeps the JSON/CSV evidence
under a timestamped directory. Use `full` for all three 1M release profiles or
`delta-mini` for the bounded 1M pre-optimization delta gate. An optional second
argument selects a stable output directory for CI or automation.

CI runs this exact family after compiling every benchmark. Smoke receipts may
pass their budgets but remain `acceptanceEligible: false` by definition.
For full base/candidate comparison, generate a 1M image once with the same
example and set `VICIA_BENCH_BASE_FIXTURE` for every run. The harness verifies
the provided fact count before measuring and records the canonical source path
and exact fixture SHA-256.

## Receipt evidence

`vicia.benchmark.receipt.v1` contains:

- milestone id, decision question, owner, profile, and execution tier;
- source commit, tracked-dirty state, executable and base-fixture SHA-256,
  testbed, kernel, CPU model, logical CPUs, RAM, Rust/Cargo versions, OS, and
  architecture;
- workload shape and explicit cold/warm policy;
- sorted raw observations with min, p25, p50, p75, p95, p99, max, mean,
  population standard deviation, median absolute deviation, and coefficient of
  variation;
- file/page growth and scenario-specific physical state;
- expected and actual correctness values;
- catalog-derived limits and expanded per-metric budget checks;
- a failure list, overall verdict, and acceptance eligibility.

The catalog owns the p95 observation floor. A p95 may be shown for inspection
below that floor, but it cannot support an acceptance-eligible receipt. Max is
used for deliberately sparse scenarios such as one fresh reopen or one
single-receipt read.

Receipt validation recomputes summaries from raw samples, re-evaluates every
catalog budget, verifies correctness equality, and derives the final verdict.
Changing a receipt after the run therefore cannot silently promote it.

## Milestone-specific coverage

### M2 — Vetch cadence

- 10K/20-slice smoke and 1M/100-slice full profiles.
- Capture, edit, receipt, current/as-of brief reads, checkpoint, and file growth.
- Full acceptance requires clean source, at least 20 observations for every
  p95-gated metric, all cardinality checks, and all absolute budgets.

### M3 — Delta growth and recovery

- 10K smoke: `1x20` and `10x10` checkpoint patterns.
- 1M T8B mini: `1x1000` and `10x100` pre-optimization decision gate.
- 1M full: seven fact-count/segment-count shapes through `1x10000`.
- Flush, reopen, current/as-of reads, file growth, exported delta count,
  segment count, probe count, and corrupt-newest fallback.
- The `1x10000` ceiling is a characterized long-tail limit, not permission to
  move recompact into foreground checkpointing.

### M4 — Agent brief

- 10K smoke and 1M full profiles.
- Current, formatted as-of, prepared as-of, and full-export/recent-filter
  surfaces remain separate metrics.
- Single-probe scenarios are max-gated; stream scenarios retain raw probe
  distributions.
- Transaction and probe cardinalities prove that a fast empty result cannot
  pass.

### M5 and M6 — Browser and product acceptance

These remain at their real ownership boundaries. M5 runs the existing
real-Chrome paged matrix in this repository. M6 runs the exact Vetch trace in
`vetch-app`. Their commands and product budgets are catalogued here so a Vicia
change cannot call itself release-ready after only native microbenchmarks.

## Closeout receipt set

A performance-affecting implementation is complete only when its commit or PR
records:

- the selected milestone and why it owns the change;
- base and candidate commit ids;
- exact profile and named testbed;
- both receipt artifact paths;
- correctness and budget verdicts;
- any unrun external gate under `Not-tested`;
- the next decision when a budget fails.

Do not update a budget merely because a candidate missed it. Budget changes are
separate decisions backed by caller requirements or repeated baseline evidence.
