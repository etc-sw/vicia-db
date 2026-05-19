---
name: Cookbook design (#190)
description: Design spec for the Minigraf cookbook — four wiki pages of problem-oriented query patterns covering graph traversal, temporal queries, bitemporal modeling, and application workflows
type: spec
issue: 190
wave: 8
---

# Cookbook Design — Issue #190

## Overview

A cookbook of common Minigraf patterns, published as four wiki pages. Each page is self-contained and uses the **Problem / Pattern / Notes** recipe format: one sentence describing the problem, the Datalog (or Datalog + Rust API where the native API is required), and 1–3 bullet notes on how it works, pitfalls, and variations.

The cookbook is the "look things up" complement to the tutorials (which teach features in sequence). Pages are organized by use-case domain rather than by feature.

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Location | Wiki (`.wiki/Cookbook-*.md`) | Consistent with tutorials; most discoverable |
| Structure | 4 separate pages + ToC in `Home.md` + sidebar section | Mirrors tutorial structure; pages are independently linkable |
| Code style | Datalog-first; Rust API only where native API is required | Keeps recipes language-agnostic; avoids duplicating Use-Cases.md |
| Domain | Varied, realistic per recipe | Shows breadth of applicability; tutorials already use Corestore |
| Recipe depth | Layered (Problem / Pattern / Notes) | Scannable for experts; enough context for learners |

## Pages

### Page 1 — Graph Traversal Patterns (`Cookbook-Graph-Traversal.md`)

Domain examples: org charts, dependency graphs, social networks, call graphs.

| # | Recipe |
|---|---|
| 1 | Direct neighbors (1-hop) |
| 2 | Transitive closure — all reachable nodes via recursive rules |
| 3 | Reachability check — does a path exist between two nodes? |
| 4 | Leaf nodes — entities with no outgoing edges (`not-join`) |
| 5 | Common ancestors of two nodes |
| 6 | Descendants of multiple roots (`or-join`) |
| 7 | Neighbor count / degree (aggregation) |
| 8 | Type-filtered traversal — traverse specific edge types, stop at predicate |
| 9 | Edge reification (property graphs) — model edge properties as entity attributes |

### Page 2 — Audit and Time-Travel Idioms (`Cookbook-Time-Travel.md`)

Domain examples: financial ledger, config history, document versioning.

Organized by the four temporal query types from the RecallGraph taxonomy
(https://recallgraph.hashnode.dev/temporal-query-types):

**Point-in-Time**

| # | Recipe |
|---|---|
| 1 | TT snapshot by tx count (`:as-of N`) |
| 2 | TT snapshot by wall-clock time (`:as-of "timestamp"`) |
| 3 | VT snapshot — standalone `:valid-at "date"` |
| 4 | Bi-temporal snapshot — `:as-of` + `:valid-at` combined |

**Time Interval**

| # | Recipe |
|---|---|
| 5 | All changes to an attribute across all history |
| 6 | Changes between two tx counts (`:db/tx-count` range filter) |
| 7 | Facts valid during a date range — VT overlap condition |

**Time-Point Lookup**

| # | Recipe |
|---|---|
| 8 | When was X first asserted? (min `:db/tx-count`) |
| 9 | When did X become valid? (`:db/valid-from`) |
| 10 | When did X expire? (`:db/valid-to` on retracted/bounded fact) |

**Time-Interval Lookup**

| # | Recipe |
|---|---|
| 11 | How long was X continuously valid? (arithmetic on `:db/valid-from`/`:db/valid-to`) |
| 12 | During which periods was X true? (enumerate all valid intervals) |

**Supporting patterns**

| # | Recipe |
|---|---|
| 13 | Audit trail with actor — reified tx entity carrying `:tx/actor` |

### Page 3 — Bitemporal Modeling (`Cookbook-Bitemporal-Modeling.md`)

Domain examples: employment history, insurance policies, health records.

The "write side" complement to page 2's query patterns.

| # | Recipe |
|---|---|
| 1 | Point-in-time facts — no explicit valid-time (defaults to tx time, open-ended) |
| 2 | Bounded facts — explicit `valid-from`/`valid-to` |
| 3 | Open-ended current facts — `valid-from` set, no `valid-to` |
| 4 | Retroactive correction — retract wrong fact, re-assert with corrected valid-time |
| 5 | Future-dated assertion — `valid-from` > now |
| 6 | Modeling overlapping periods — two facts for same attribute with overlapping valid ranges |
| 7 | Closing an open-ended fact — adding `valid-to` to a previously open-ended fact |
| 8 | Correction vs. retraction — when to retract+reassert vs. valid-time bounding |

### Page 4 — Application Workflow Patterns (`Cookbook-Application-Workflows.md`)

Domain examples: AI agent reasoning loop, offline health app, task planner.

**Agent memory**

| # | Recipe |
|---|---|
| 1 | Store, update, and retract beliefs — basic belief lifecycle |
| 2 | Audit a past decision — rewind to knowledge state at decision time |
| 3 | High-frequency belief queries with prepared statements (Rust API) |
| 4 | GraphRAG pattern — vector store entry point + Minigraf graph/temporal navigation |

**Offline-first**

| # | Recipe |
|---|---|
| 5 | Record facts offline, correct on sync — retroactive correction preserves original |
| 6 | Detect what changed since last sync — `:db/tx-count` range query |

**Task and dependency graphs**

| # | Recipe |
|---|---|
| 7 | Model a task DAG — recursive rule for transitive blocking |
| 8 | Query tasks unblocked at a given tx snapshot — recursive reachability + `:as-of` |

**Multi-tenant / fleet patterns**

| # | Recipe |
|---|---|
| 9 | One `.graph` per agent/user — architecture note on embedded single-file model |

## Navigation Changes

### `Home.md`

Add a **Cookbook** section after the Tutorials section and before Pages:

```markdown
## Cookbook

Problem-oriented recipes for common Minigraf patterns. Each recipe is self-contained:
a one-line problem statement, the Datalog (or Rust API where needed), and brief notes.

- [Graph Traversal Patterns](Cookbook-Graph-Traversal) — neighbors, transitive closure,
  reachability, leaf detection, degree, property graphs
- [Audit and Time-Travel Idioms](Cookbook-Time-Travel) — point-in-time, time-interval,
  time-point lookup, and time-interval lookup across both time axes
- [Bitemporal Modeling](Cookbook-Bitemporal-Modeling) — how to structure data for
  bitemporal queries: bounded facts, retroactive corrections, overlapping periods
- [Application Workflow Patterns](Cookbook-Application-Workflows) — agent memory,
  offline-first state, task DAGs, GraphRAG, multi-tenant patterns
```

### `_Sidebar.md`

Add a **Cookbook** section between Tutorial and Reference:

```markdown
## Cookbook

- [Graph Traversal](Cookbook-Graph-Traversal)
- [Time Travel](Cookbook-Time-Travel)
- [Bitemporal Modeling](Cookbook-Bitemporal-Modeling)
- [App Workflows](Cookbook-Application-Workflows)
```

## Acceptance Criteria Mapping

| Acceptance criterion (issue #190) | Covered by |
|---|---|
| Graph traversal patterns | Page 1 (all 9 recipes) |
| Audit and time-travel query idioms | Page 2 (all 13 recipes, organized by temporal query type) |
| Bitemporal modeling examples | Page 3 (all 8 recipes) |
| Agent memory workflow | Page 4, recipes 1–4 |
| Offline-first state workflow | Page 4, recipes 5–6 |

## Out of Scope

- Temporal graph traversals (temporal network analysis) — noted in the RecallGraph article's appendix as an ongoing research area; Minigraf's Datalog engine does not have dedicated temporal traversal operators. Document as a known gap if relevant.
- Language binding examples beyond Rust — covered in Use-Cases.md.
- Performance guidance — covered by issue #191 (Wave 8, separate deliverable).
