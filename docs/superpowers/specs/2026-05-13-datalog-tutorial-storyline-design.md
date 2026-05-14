# Datalog Tutorial Storyline Design

**Date:** 2026-05-13
**Issue:** [#234](https://github.com/project-minigraf/minigraf/issues/234)
**Parent:** [#230](https://github.com/project-minigraf/minigraf/issues/230)

---

## Overview

The Minigraf Datalog tutorial will be delivered as a series of wiki pages (GitHub Wiki),
driven by a single coherent real-world storyline. The storyline provides narrative scaffolding
so readers encounter each language feature in a context they already understand emotionally —
rather than in a contrived, isolated example.

The domain is **e-commerce order management**, chosen because:
- Nearly every reader has placed an online order
- Price changes and delivery reschedules are a widely-felt frustration
- These pain points map directly onto Minigraf's bi-temporal model (what the system recorded
  vs what was actually true)
- The domain supports all other language features naturally (category hierarchies → recursion,
  unshipped items → negation, order totals → aggregation, etc.)

---

## Setting

**Corestore** — a fictional single-seller online consumer electronics retailer.

Product domain: everyday consumer electronics (phones, laptops, headphones, cameras, tablets,
keyboards, monitors). No AI-specific hardware (GPUs, accelerators, etc.).

The single-seller model is used for sections 1–10. A marketplace layer (third-party sellers)
is introduced in section 11 only for scenarios that genuinely require it.

---

## Cast

| Character | Archetype | Tutorial role |
|---|---|---|
| **Alice** | Frequent, no-fuss buyer | Grounds the reader. Her orders are clean and successful. Used for foundational sections. |
| **Ben** | Price-watcher, detail-oriented | Orders a laptop, notices a post-checkout price drop, then catches a retroactive pricing correction. Anchors all bi-temporal sections. |
| **Clara** | Complicated history | Split shipments, rescheduled deliveries, cancelled items. Her orders are never quite right. Drives negation, aggregates, disjunction, and UDF sections. |
| **Marketplace sellers** *(section 11 only)* | Third-party vendors | Introduced when the single-seller model can no longer express the scenario naturally. |

---

## Narrative Style

Each section opens with a one-paragraph plain-English scenario describing what a character
is experiencing. The Datalog follows from that scenario. The story is scaffolding — it earns
the syntax rather than dressing it up.

Example opening (section 2):
> Ben placed his order on Monday afternoon. By Wednesday the price had dropped $40. He opened
> a support ticket demanding a price match — and wanted proof of what the system had shown him
> at checkout. With `:as-of`, we can answer that question exactly.

---

## Section Mapping

### Section 1 — Basic transact + query

**Scenario:** Alice browses Corestore, finds a phone she likes, and places an order. Moments
later she decides to remove one of the accessories — her first `retract`.

**Features covered:**
- `transact` — build the product catalog, Alice's customer record, her order and line items
- `retract` — Alice cancels one item immediately after ordering
- `query` with `:find` / `:where` — basic pattern matching and variable binding
- Multi-clause joins

---

### Section 2 — Bi-temporal queries: `:as-of`

**Scenario:** Ben orders a laptop on Monday. On Wednesday the price drops $40. He contacts
support and asks: "What did the system show me at checkout?" `:as-of` answers by travelling
back to the transaction-time state of the database at the moment of his order.

**Features covered:**
- `:as-of N` (by tx counter)
- `:as-of "timestamp"` (by wall-clock time)
- Contrast: same query with and without `:as-of`

---

### Section 3 — Bi-temporal queries: `:valid-at`, `:any-valid-time`, per-fact valid time

**Scenario (`:valid-at` + per-fact valid time):** A pricing error on Ben's laptop is
discovered: the "official" sale price was always $899, but the system had recorded $949.
Support corrects it by transacting a backdated fact. `:valid-at` lets us query what the
price *should have been* on any given date, independently of when the correction was entered.
This is the moment the distinction between transaction time and valid time becomes concrete.

**Scenario (`:any-valid-time`):** A support agent needs the *complete price history* for
Ben's laptop — every price ever recorded, including all superseded versions.
A plain `:valid-at` query returns only what was valid at one point; `:any-valid-time` lifts
the filter entirely and returns every version across all valid-time windows.

**Features covered:**
- `:valid-at "timestamp"`
- Per-fact valid time override (5-element fact: `[e a v valid-from valid-to]`)
- Transaction-level `{:valid-from … :valid-to …}` map on `transact`
- `:any-valid-time`
- Combined `:as-of` + `:valid-at` (both axes at once)

---

### Section 4 — Recursive rules

**Scenario:** Alice searches for "everything in Electronics." Corestore's catalog has a
multi-level category hierarchy (Electronics → Audio → Headphones → Noise-Cancelling).
A flat query only finds direct children. A recursive rule traverses the full tree.

**Features covered:**
- `rule` command
- Base case + recursive case
- Rule invocation inside `query`
- Semi-naive fixed-point evaluation (convergence on cycles)

---

### Section 5 — Negation: `not`, `not-join`

**Scenario:** Clara's order was split into three shipments. Two have shipped; one hasn't.
We find the unshipped items with `not`. Then broader questions: which products has no customer
ever ordered? Which customers have no completed delivery? The `not-join` variant handles the
case where we need to existentially quantify an inner variable (e.g. "no shipment of any
carrier has arrived for this item").

**Features covered:**
- `(not ...)` — single and multi-clause
- `(not-join [join-vars] ...)` — existential inner variables
- Safety constraint: all outer variables must be pre-bound
- Stratification rules and negative cycle detection (briefly, with an example of a rejected pair)
- `not` inside a rule body

---

### Section 6 — Aggregates, `:with`, and window functions

**Scenario (aggregates):** How much has each customer spent in total? What is the average
delivery delay across all of Clara's orders?

**Scenario (`:with`):** Alice's order contains two identical USB-C cables — same product,
same price, different line items. Without `:with`, the two rows collapse before the sum runs,
halving the total. Adding `:with ?line-item` forces each line item to be counted as a
distinct row. This is demonstrated with before/after queries and their differing outputs.

**Scenario (window functions):** What is the running price history of Ben's laptop — each
recorded price ranked in order, with a cumulative view of how the price has moved?

**Features covered:**
- Scalar aggregates: `count`, `count-distinct`, `sum`, `sum-distinct`, `min`, `max`
- Grouping by plain variables alongside aggregates
- `:with` clause — motivation, syntax, before/after example
- Window functions: `sum :over`, `avg :over`, `rank :over`, `row-number :over`
- `:partition-by`, `:order-by`, `:desc`
- Mixed aggregate + window in one query

---

### Section 7 — Expression clauses and predicates

**Scenario:** Find all products under $500. Find orders where the actual delivery date
landed after the promised date, and calculate the delay in days. Validate that a product
SKU matches a known format. Check whether a discount code string starts with a specific prefix.

**Features covered:**
- Filter predicates: `<`, `>`, `<=`, `>=`, `=`, `!=`
- Arithmetic bindings: `[(* ?price ?qty) ?subtotal]`
- Type predicates: `string?`, `integer?`, `float?`, `boolean?`, `nil?`
- String predicates: `starts-with?`, `ends-with?`, `contains?`, `matches?`
- Safety constraint: variables must be bound before appearing in an expression
- Binding expressions adding new variables to the bound set

---

### Section 8 — Prepared queries with bind slots

**Scenario:** Corestore's backend runs the same queries thousands of times per day with
different parameters — "orders for customer X", "price of SKU Y as of date Z", "delivery
status for order W". Prepared queries parse and plan once, then execute with substituted
bind values.

**Features covered:**
- `$slot` syntax in entity, value, `:as-of`, and `:valid-at` positions
- `db.prepare()` / `pq.execute(&[...])` Rust API
- `BindValue` variants: `Entity`, `Val`, `TxCount`, `Timestamp`, `AnyValidTime`
- Attribute position is not parameterisable (and why)
- Prepared queries see live fact store state at each `execute()` call

---

### Section 9 — Disjunction: `or`, `or-join`

**Scenario:** Find all of Clara's orders that are in *either* "shipped" or "out for delivery"
status — a customer-facing tracking page needs both. Then find products in *either* the
"Audio" or "Mobile Accessories" category. Finally, Clara's refund covers items from either
of two separate cancelled orders — `or-join` handles the case where the branches introduce
different private variables.

**Features covered:**
- `(or ...)` — same new-variable set across all branches
- `(or-join [join-vars] ...)` — branch-private variables
- `(and ...)` inside an `or` branch
- Nesting `or` inside `or-join`
- Safety: all `join_vars` must be pre-bound; branch variable-set parity for `or`

---

### Section 10 — User-defined functions

**Scenario (predicate UDF):** Corestore wants to flag orders whose discount code matches a
complex internal validation rule — too specific for the built-in string predicates.
A `register_predicate` call adds `valid-promo?` as a filter in any `:where` clause.

**Scenario (aggregate UDF):** The logistics team tracks delivery reliability with a custom
weighted score. A `register_aggregate` call adds `delivery-score` as a grouping aggregate,
giving per-seller (later, per-region) reliability metrics.

**Features covered:**
- `register_predicate(name, fn)` — use in `[(name? ?var)]` `:where` clauses
- `register_aggregate(name, init, step, finalise)` — use in `:find` and `:over` clauses
- Runtime resolution: queries referencing UDFs parse before registration; fail at execute if unregistered
- UDF aggregate inside a window `:over` clause

---

### Section 11 — Marketplace

**Scenario:** Corestore opens a marketplace. Two third-party sellers join. Clara places an
order that routes to an external seller — delivery SLAs differ from Corestore's own, and a
dispute arises over a missed delivery window. We need to query across seller entities, compare
SLAs, and find orders where the seller's promised window diverges from the platform's recorded
estimate. These scenarios cannot be expressed with the single-seller model because there is no
seller entity to join against.

**Features covered:**
- Introducing a new entity type (seller) and its relationships
- Cross-entity joins (order → seller → SLA)
- Recursive rules across seller–product–order graph
- Negation and aggregation applied to multi-seller data
- Synthesis: combining features from all previous sections in realistic queries

---

## Grammar Coverage Checklist

| Grammar construct | Section(s) |
|---|---|
| `transact-cmd` | 1 |
| `retract-cmd` | 1 |
| `rule-cmd` | 4 |
| `query-cmd` | 1 |
| `valid-time-map` (`:valid-from` / `:valid-to`) | 3 |
| Per-fact valid time (5-element vector) | 3 |
| `:find` with variables | 1 |
| `:find` with aggregate-expr | 6 |
| `:find` with window-expr | 6 |
| `:where` pattern-clause | 1 |
| `:where` expr-clause (filter) | 7 |
| `:where` expr-clause (binding) | 7 |
| `:where` not-clause | 5 |
| `:where` not-join-clause | 5 |
| `:where` or-clause | 9 |
| `:where` or-join-clause | 9 |
| `:where` and-branch inside or | 9 |
| `:where` rule-invocation | 4 |
| `:as-of` (integer and timestamp) | 2 |
| `:valid-at` (timestamp) | 3 |
| `:any-valid-time` | 3 |
| `:with` | 6 |
| Combined `:as-of` + `:valid-at` | 3 |
| Scalar aggregates (count, count-distinct, sum, sum-distinct, min, max) | 6 |
| Window functions (sum, avg, rank, row-number :over) | 6 |
| `:partition-by`, `:order-by`, `:desc` | 6 |
| Comparison operators | 7 |
| Arithmetic operators + binding | 7 |
| String predicates | 7 |
| Type predicates | 7 |
| `$bind-slot` in entity/value/`:as-of`/`:valid-at` | 8 |
| `register_predicate` UDF | 10 |
| `register_aggregate` UDF | 10 |
| UDF in `:over` window clause | 10 |

---

## Delivery

- **Format:** GitHub Wiki (series of wiki pages)
- **Dependency:** Written after #233 (EBNF + semantics) is merged, so tutorials can
  cross-reference the formal grammar in `Datalog-Reference.md`
- **Cross-references:** Each section links to the relevant subsection of `Datalog-Reference.md`
