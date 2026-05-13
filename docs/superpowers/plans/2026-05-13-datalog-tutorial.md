# Datalog Tutorial (Issue #234) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Write 12 verified wiki pages delivering a complete Datalog tutorial driven by the Corestore e-commerce storyline, covering every grammar construct in `Datalog-Reference.md`.

**Architecture:** A shared base dataset (`demos/tutorial_corestore_setup.txt`) bootstraps Corestore's product catalog, categories, and customers. Each wiki page extends the dataset with section-specific data, embeds verified Datalog, and cross-references `Datalog-Reference.md`. Section 10 (UDFs) adds `examples/tutorial_udfs.rs` since UDF registration requires the Rust API and cannot be demonstrated via the REPL.

**Tech Stack:** Minigraf REPL (`cargo run`), GitHub Wiki (`.wiki/` directory), Markdown, Rust (section 10 only)

---

## Prerequisites

- Create a git worktree for main-repo file changes (use `superpowers:using-git-worktrees`).
- Wiki pages live in `.wiki/` which is a **separate git repo**. Commit and push wiki changes with `cd .wiki && git add -A && git commit -m "..." && git push` after all pages are written.
- All tx counts assume a **fresh database** with commands run in tutorial order. Queries do not increment tx_count; only `transact` and `retract` do.

---

## File Structure

**New files — main repo:**
- `demos/tutorial_corestore_setup.txt` — base Corestore dataset (task 1)
- `examples/tutorial_udfs.rs` — UDF demonstration (task 12)

**New files — wiki repo (`.wiki/`):**
- `Tutorial-Setup.md`
- `Tutorial-01-Basic-Transact-Query.md`
- `Tutorial-02-As-Of.md`
- `Tutorial-03-Valid-At.md`
- `Tutorial-04-Recursive-Rules.md`
- `Tutorial-05-Negation.md`
- `Tutorial-06-Aggregates.md`
- `Tutorial-07-Expressions.md`
- `Tutorial-08-Prepared-Queries.md`
- `Tutorial-09-Disjunction.md`
- `Tutorial-10-UDFs.md`
- `Tutorial-11-Marketplace.md`

**Modified files — wiki repo:**
- `.wiki/Home.md` — add tutorial series entry under a new "Tutorials" section
- `.wiki/Learning-Resources.md` — replace the Learn Datalog Today / Datomic links with a pointer to the tutorial series

---

## Transaction Count Reference

The table below tracks cumulative tx_count after each task's data setup. All wiki page
queries must use these counts when referencing `:as-of N`.

| After task | tx_count |
|---|---|
| Base setup (task 1) | 3 |
| Section 1 data | 5 |
| Section 2 data | 8 |
| Section 3 data | 12 |
| Section 4 data | 12 (no new transacts) |
| Section 5 data | 14 |
| Section 6 data | 17 |
| Section 7 data | 17 (no new transacts) |
| Section 8 data | 17 (no new transacts) |
| Section 9 data | 18 |
| Section 10 data | 18 (Rust API only) |
| Section 11 data | 20 |

---

## Task 1: Base Corestore dataset

**Files:**
- Create: `demos/tutorial_corestore_setup.txt`

- [ ] **Step 1: Write `demos/tutorial_corestore_setup.txt`**

```
# ================================================================
# Corestore Tutorial Dataset
# ================================================================
# Base setup for the Minigraf Datalog tutorial series.
# All tutorial sections assume this has been loaded first.
#
# Run with: cargo run < demos/tutorial_corestore_setup.txt
#
# After loading: tx_count = 3
# ================================================================

# ── tx 1: Category hierarchy ─────────────────────────────────────
# Electronics
#   ├── Laptops
#   ├── Mobile
#   ├── Audio
#   │   └── Headphones
#   │       └── Noise-Cancelling
#   └── Accessories

(transact [
  [:cat-electronics :category/name "Electronics"]
  [:cat-laptops     :category/name "Laptops"]
  [:cat-laptops     :category/parent :cat-electronics]
  [:cat-mobile      :category/name "Mobile"]
  [:cat-mobile      :category/parent :cat-electronics]
  [:cat-audio       :category/name "Audio"]
  [:cat-audio       :category/parent :cat-electronics]
  [:cat-headphones  :category/name "Headphones"]
  [:cat-headphones  :category/parent :cat-audio]
  [:cat-nc          :category/name "Noise-Cancelling"]
  [:cat-nc          :category/parent :cat-headphones]
  [:cat-accessories :category/name "Accessories"]
  [:cat-accessories :category/parent :cat-electronics]
])

# ── tx 2: Product catalog ─────────────────────────────────────────
# Two products per major leaf category for window function examples.

(transact [
  [:laptop-pro    :product/name "LaptopPro 15"]
  [:laptop-pro    :product/sku "LP-15"]
  [:laptop-pro    :product/price 1299]
  [:laptop-pro    :product/category :cat-laptops]

  [:laptop-budget :product/name "BudgetBook 14"]
  [:laptop-budget :product/sku "LB-14"]
  [:laptop-budget :product/price 699]
  [:laptop-budget :product/category :cat-laptops]

  [:phone-x       :product/name "PhoneX 12"]
  [:phone-x       :product/sku "PX-12"]
  [:phone-x       :product/price 799]
  [:phone-x       :product/category :cat-mobile]

  [:phone-prev    :product/name "PhoneX 11"]
  [:phone-prev    :product/sku "PX-11"]
  [:phone-prev    :product/price 599]
  [:phone-prev    :product/category :cat-mobile]

  [:nc-headphones :product/name "NoiseCancel Pro"]
  [:nc-headphones :product/sku "NC-PRO"]
  [:nc-headphones :product/price 249]
  [:nc-headphones :product/category :cat-nc]

  [:usb-cable     :product/name "USB-C Cable 2m"]
  [:usb-cable     :product/sku "USB-C-2M"]
  [:usb-cable     :product/price 19]
  [:usb-cable     :product/category :cat-accessories]

  [:keyboard-k1   :product/name "Compact Keyboard"]
  [:keyboard-k1   :product/sku "KB-K1"]
  [:keyboard-k1   :product/price 89]
  [:keyboard-k1   :product/category :cat-accessories]

  [:monitor-27    :product/name "ClearView 27\" Monitor"]
  [:monitor-27    :product/sku "CV-27"]
  [:monitor-27    :product/price 449]
  [:monitor-27    :product/category :cat-electronics]
])

# ── tx 3: Customers ───────────────────────────────────────────────

(transact [
  [:alice :customer/name "Alice"]
  [:alice :customer/email "alice@example.com"]
  [:ben   :customer/name "Ben"]
  [:ben   :customer/email "ben@example.com"]
  [:clara :customer/name "Clara"]
  [:clara :customer/email "clara@example.com"]
])
```

- [ ] **Step 2: Verify dataset loads without errors**

```bash
cargo run < demos/tutorial_corestore_setup.txt
```

Expected: three transaction results printed, no errors. The last tx_count shown should be 3.

- [ ] **Step 3: Commit**

```bash
git add demos/tutorial_corestore_setup.txt
git commit -m "chore: add Corestore tutorial base dataset"
```

---

## Task 2: Tutorial setup page

**Files:**
- Create: `.wiki/Tutorial-Setup.md`

- [ ] **Step 1: Write `.wiki/Tutorial-Setup.md`**

The page must cover:
1. What Corestore is and who Alice, Ben, and Clara are (one short paragraph each)
2. How to install Minigraf: `cargo install minigraf` (or clone + `cargo build`)
3. How to start the REPL: `cargo run`
4. How to load the base dataset: `cargo run < demos/tutorial_corestore_setup.txt`
5. A note that tx_counts are cumulative — run sections in order for `:as-of` examples to match
6. Navigation table linking to all 11 sections

- [ ] **Step 2: Commit wiki**

```bash
cd .wiki
git add Tutorial-Setup.md
git commit -m "docs: add tutorial setup page"
git push
cd ..
```

---

## Task 3: Section 1 — Basic transact + query

**Files:**
- Create: `.wiki/Tutorial-01-Basic-Transact-Query.md`

**Scenario:** Alice browses Corestore and orders a PhoneX 12. Moments later she decides to
remove the USB-C cable she added as an afterthought — her first `retract`.

**Data setup** (cumulative tx_count: 3 → 5):

```
# ── tx 4: Alice's first order ────────────────────────────────────
(transact [
  [:alice-order-1        :order/customer :alice]
  [:alice-order-1        :order/status :placed]
  [:alice-order-1-item-1 :order-item/order :alice-order-1]
  [:alice-order-1-item-1 :order-item/product :phone-x]
  [:alice-order-1-item-1 :order-item/qty 1]
  [:alice-order-1-item-1 :order-item/price 799]
  [:alice-order-1-item-2 :order-item/order :alice-order-1]
  [:alice-order-1-item-2 :order-item/product :usb-cable]
  [:alice-order-1-item-2 :order-item/qty 1]
  [:alice-order-1-item-2 :order-item/price 19]
])

# ── tx 5: Alice removes the USB cable ────────────────────────────
(retract [
  [:alice-order-1-item-2 :order-item/order :alice-order-1]
  [:alice-order-1-item-2 :order-item/product :usb-cable]
  [:alice-order-1-item-2 :order-item/qty 1]
  [:alice-order-1-item-2 :order-item/price 19]
])
```

**Key queries for the page:**

```datalog
; 1. List all products in the catalog
(query [:find ?name ?price
        :where [?p :product/name ?name]
               [?p :product/price ?price]])
; Expected: 8 rows — all products in the catalog

; 2. What is in Alice's current order?
(query [:find ?product-name ?qty ?price
        :where [?item :order-item/order :alice-order-1]
               [?item :order-item/product ?p]
               [?p :product/name ?product-name]
               [?item :order-item/qty ?qty]
               [?item :order-item/price ?price]])
; Expected: [["PhoneX 12" 1 799]]
; (USB cable is gone — the retract removed it from the current view)

; 3. All orders and their customers
(query [:find ?customer-name ?status
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?order :order/status ?status]])
; Expected: [["Alice" :placed]]
```

**Page structure:**
1. Scenario paragraph
2. "Setting up the data" — show the transact commands above, explain EAV triples
3. "Querying the catalog" — query 1
4. "Alice places her order" — transact Alice's order
5. "Alice removes an item" — retract, explain what retract does (records asserted=false, original fact preserved in history)
6. "What's in Alice's order now?" — query 2
7. "What orders exist?" — query 3
8. "Key concepts" — EAV triples, transact, retract, basic query anatomy
9. "Next" link to section 2, "Reference" links to `Datalog-Reference.md#transact`, `#retract`, `#query`

- [ ] **Step 1: Write `.wiki/Tutorial-01-Basic-Transact-Query.md`** using the structure above, with all Datalog blocks shown

- [ ] **Step 2: Verify all queries produce expected output**

Load setup + section 1 data and run all three queries:
```bash
cargo run < demos/tutorial_corestore_setup.txt
# then in the REPL, paste the section 1 transact/retract/query blocks above
```
Confirm: query 1 returns 8 rows, query 2 returns `[["PhoneX 12" 1 799]]`, query 3 returns `[["Alice" :placed]]`.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-01-Basic-Transact-Query.md
git commit -m "docs: tutorial section 1 — basic transact + query"
git push
cd ..
```

---

## Task 4: Section 2 — `:as-of`

**Files:**
- Create: `.wiki/Tutorial-02-As-Of.md`

**Scenario:** Ben orders a LaptopPro 15 on Monday at $1,299. Two days later the price drops
to $1,259. He contacts support: "What price was shown to me at checkout?" `:as-of` answers
by replaying the database state at the moment of his order.

**Data setup** (cumulative tx_count: 5 → 8):

```
# ── tx 6: Ben places his order at $1,299 ─────────────────────────
(transact [
  [:ben-order-1        :order/customer :ben]
  [:ben-order-1        :order/status :placed]
  [:ben-order-1-item-1 :order-item/order :ben-order-1]
  [:ben-order-1-item-1 :order-item/product :laptop-pro]
  [:ben-order-1-item-1 :order-item/qty 1]
  [:ben-order-1-item-1 :order-item/price-at-purchase 1299]
])

# ── tx 7: Price drop — retract old list price ─────────────────────
(retract [
  [:laptop-pro :product/price 1299]
])

# ── tx 8: New list price ──────────────────────────────────────────
(transact [
  [:laptop-pro :product/price 1259]
])
```

**Key queries for the page:**

```datalog
; 1. Current price (no time filter)
(query [:find ?price
        :where [:laptop-pro :product/price ?price]])
; Expected: [[1259]]

; 2. Price as of tx 6 — when Ben placed his order
(query [:find ?price
        :as-of 6
        :where [:laptop-pro :product/price ?price]])
; Expected: [[1299]]
; Explanation: at tx 6, the retraction at tx 7 hadn't happened yet,
; so the $1,299 fact from tx 2 (setup) is still net-asserted.

; 3. :as-of also works with a tx counter above the latest — returns current state
(query [:find ?price
        :as-of 999
        :where [:laptop-pro :product/price ?price]])
; Expected: [[1259]]

; 4. Show Ben's recorded purchase price
(query [:find ?recorded-price
        :where [:ben-order-1-item-1 :order-item/price-at-purchase ?recorded-price]])
; Expected: [[1299]]
; This is the price captured at order time — independent of future changes.
```

**Page structure:**
1. Scenario paragraph
2. "Ben places his order" — tx 6
3. "The price drops" — tx 7 + tx 8, explain retract+transact pattern for price updates
4. "What does the system show now?" — query 1
5. "What did the system show Ben at checkout?" — query 2, explain tx_count and `:as-of`
6. "How does `:as-of` work?" — diagram: tx timeline, `:as-of 6` draws a line before the retraction
7. "The order always knows its own price" — query 4, note that `:price-at-purchase` was captured at tx time
8. "Key concepts" — transaction time, `:as-of N`, when to use tx_count vs timestamp form
9. Reference links to `Datalog-Reference.md#as-of`

- [ ] **Step 1: Write `.wiki/Tutorial-02-As-Of.md`** using the structure above with all Datalog blocks

- [ ] **Step 2: Verify**

Run the setup + section 1 + section 2 data in order. Confirm all four queries produce expected output.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-02-As-Of.md
git commit -m "docs: tutorial section 2 — :as-of"
git push
cd ..
```

---

## Task 5: Section 3 — `:valid-at`, `:any-valid-time`, per-fact valid time

**Files:**
- Create: `.wiki/Tutorial-03-Valid-At.md`

**Scenario:** Corestore runs a winter sale on the LaptopPro 15. The sale price was entered
incorrectly as $1,099 — it should have been $1,049. A support agent discovers the error and
corrects it with a backdated fact. The distinction between *what the system recorded* (`:as-of`)
and *what was actually true in the world* (`:valid-at`) becomes concrete and felt.

A second sub-scenario: the support agent wants a complete price history — all sale price
windows ever recorded — and uses `:any-valid-time` to lift the valid-time filter entirely.

**Data setup** (cumulative tx_count: 8 → 12):

```
# ── tx 9: Wrong winter sale price entered ────────────────────────
# Valid from Jan 1 to Feb 28. Entered incorrectly as $1,099.
(transact {:valid-from "2026-01-01" :valid-to "2026-02-28"}
          [[:laptop-pro :product/sale-price 1099]])

# ── tx 10: Spring sale price added (correct) ─────────────────────
(transact {:valid-from "2026-05-20" :valid-to "2026-06-30"}
          [[:laptop-pro :product/sale-price 1149]])

# ── tx 11: Retract the wrong winter sale price ────────────────────
(retract [[:laptop-pro :product/sale-price 1099]])

# ── tx 12: Correct winter sale price (backdated, same valid window) 
(transact {:valid-from "2026-01-01" :valid-to "2026-02-28"}
          [[:laptop-pro :product/sale-price 1049]])
```

**Key queries for the page:**

```datalog
; ── :valid-at ──────────────────────────────────────────────────────

; 1. What is/was the sale price on Jan 15? (current knowledge)
(query [:find ?price
        :valid-at "2026-01-15"
        :where [:laptop-pro :product/sale-price ?price]])
; Expected: [[1049]]  (corrected price, valid Jan 1–Feb 28)

; 2. No valid-time filter — what sale prices are active RIGHT NOW?
; (Today is 2026-05-13, between the two sale windows)
(query [:find ?price
        :where [:laptop-pro :product/sale-price ?price]])
; Expected: []  (no sale currently active on May 13)

; ── :as-of + :valid-at combined ────────────────────────────────────

; 3. What did the system RECORD about Jan 15, as of tx 9 (before correction)?
(query [:find ?price
        :as-of 9
        :valid-at "2026-01-15"
        :where [:laptop-pro :product/sale-price ?price]])
; Expected: [[1099]]  (the wrong price — this is what support told Ben)

; 4. What does the system NOW record about Jan 15? (after correction, tx 12)
(query [:find ?price
        :as-of 12
        :valid-at "2026-01-15"
        :where [:laptop-pro :product/sale-price ?price]])
; Expected: [[1049]]  (the corrected price)

; ── :any-valid-time ────────────────────────────────────────────────

; 5. Show ALL currently-asserted sale price windows (full audit trail)
(query [:find ?price
        :any-valid-time
        :where [:laptop-pro :product/sale-price ?price]])
; Expected: [[1049] [1149]]
; (wrong $1,099 is excluded — it was retracted; only current facts shown)

; ── Per-fact valid time (5-element form) ───────────────────────────

; 6. Same spring sale using the inline 5-element form (alternative syntax)
; This is equivalent to the {:valid-from ... :valid-to ...} transact above.
; Show as an example — do NOT run (would create a duplicate fact):
;
; (transact [[:laptop-pro :product/sale-price 1149 "2026-05-20" "2026-06-30"]])
;                                               ↑    ↑valid-from  ↑valid-to
```

**Page structure:**
1. Scenario paragraph — introduce the two time axes with a clear one-line definition each
2. "Recording the winter sale" — tx 9, explain `{:valid-from :valid-to}` syntax
3. "Adding the spring sale" — tx 10
4. "Discovering the error" — tx 11 retract, tx 12 correction
5. "`:valid-at` — querying what was true in the world" — queries 1 and 2
6. "Both axes at once" — queries 3 and 4, the bi-temporal matrix diagram (2×2: before/after correction × before/after sale)
7. "`:any-valid-time` — the full audit trail" — query 5, explain what it does and doesn't show (retracted facts excluded)
8. "Per-fact valid time syntax" — show the 5-element form as an alternative
9. "When to use which" — decision table: `:as-of` for audit/debugging, `:valid-at` for world-state queries, combined for full bi-temporal lookup
10. Reference links to `Datalog-Reference.md#bi-temporal-queries`

- [ ] **Step 1: Write `.wiki/Tutorial-03-Valid-At.md`** with all Datalog blocks above

- [ ] **Step 2: Verify**

Run setup + sections 1–3 data in order. Confirm:
- Query 1 → `[[1049]]`
- Query 2 → `[]`
- Query 3 → `[[1099]]`
- Query 4 → `[[1049]]`
- Query 5 → `[[1049] [1149]]` (order may vary)

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-03-Valid-At.md
git commit -m "docs: tutorial section 3 — :valid-at, :any-valid-time, per-fact valid time"
git push
cd ..
```

---

## Task 6: Section 4 — Recursive rules

**Files:**
- Create: `.wiki/Tutorial-04-Recursive-Rules.md`

**Scenario:** Alice searches for "everything in Electronics." Corestore's category tree has
four levels deep (Electronics → Audio → Headphones → Noise-Cancelling). A flat query only
finds direct children. A recursive rule traverses the full tree, returning all descendants at
any depth.

**Data setup:** None. Category hierarchy was loaded in the base dataset (tx 1).

**Key queries for the page:**

```datalog
; 1. Without recursion: only direct children of Electronics
(query [:find ?category-name
        :where [?cat :category/parent :cat-electronics]
               [?cat :category/name ?category-name]])
; Expected: [["Laptops"] ["Mobile"] ["Audio"] ["Accessories"]]
; (Headphones and Noise-Cancelling are missing — they are grandchildren)

; 2. Define a recursive rule: all descendants at any depth
(rule [(subcategory ?ancestor ?descendant)
       [?descendant :category/parent ?ancestor]])

(rule [(subcategory ?ancestor ?descendant)
       [?intermediate :category/parent ?ancestor]
       (subcategory ?intermediate ?descendant)])

; 3. All categories under Electronics (any depth)
(query [:find ?category-name
        :where (subcategory :cat-electronics ?cat)
               [?cat :category/name ?category-name]])
; Expected: [["Laptops"] ["Mobile"] ["Audio"] ["Accessories"]
;            ["Headphones"] ["Noise-Cancelling"]]

; 4. All products under Electronics (any depth via category)
(query [:find ?product-name ?category-name
        :where (subcategory :cat-electronics ?cat)
               [?p :product/category ?cat]
               [?p :product/name ?product-name]
               [?cat :category/name ?category-name]])
; Expected: all 8 products with their categories

; 5. All products under Audio only (2 levels: Audio → Headphones → NC)
(query [:find ?product-name
        :where (subcategory :cat-audio ?cat)
               [?p :product/category ?cat]
               [?p :product/name ?product-name]])
; Expected: [["NoiseCancel Pro"]]
```

**Page structure:**
1. Scenario paragraph
2. "The limits of flat queries" — query 1 showing missing grandchildren
3. "Defining a recursive rule" — rule 1 (base case), rule 2 (recursive case), explain fixed-point evaluation
4. "All categories under Electronics" — query 3
5. "All products under Electronics" — query 4
6. "Narrowing to a sub-tree" — query 5
7. "How fixed-point evaluation works" — brief explanation: runs until no new tuples added; cycles handled safely
8. Reference links to `Datalog-Reference.md#recursive-rules`

- [ ] **Step 1: Write `.wiki/Tutorial-04-Recursive-Rules.md`**

- [ ] **Step 2: Verify**

Run setup data only (no section 1–3 data needed for this section, but cumulative run is fine). Confirm queries 1–5 produce expected output. Pay attention to query 5 — only 1 result expected.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-04-Recursive-Rules.md
git commit -m "docs: tutorial section 4 — recursive rules"
git push
cd ..
```

---

## Task 7: Section 5 — Negation: `not`, `not-join`

**Files:**
- Create: `.wiki/Tutorial-05-Negation.md`

**Scenario:** Clara places a large order. Corestore ships two of her three items; the third
is delayed. `not` finds the unshipped item. Wider questions follow: which products has no
customer ever ordered? Which customers have no completed delivery?

**Data setup** (cumulative tx_count: 12 → 14):

```
# ── tx 13: Clara's order (3 items, partially shipped) ────────────
(transact [
  [:clara-order-1        :order/customer :clara]
  [:clara-order-1        :order/status :processing]
  [:clara-order-1        :order/delivery-promise "2026-05-20"]
  [:clara-order-1-item-1 :order-item/order :clara-order-1]
  [:clara-order-1-item-1 :order-item/product :nc-headphones]
  [:clara-order-1-item-1 :order-item/price 249]
  [:clara-order-1-item-2 :order-item/order :clara-order-1]
  [:clara-order-1-item-2 :order-item/product :monitor-27]
  [:clara-order-1-item-2 :order-item/price 449]
  [:clara-order-1-item-3 :order-item/order :clara-order-1]
  [:clara-order-1-item-3 :order-item/product :usb-cable]
  [:clara-order-1-item-3 :order-item/price 19]
])

# ── tx 14: Two items shipped ──────────────────────────────────────
(transact [
  [:clara-order-1-item-1 :order-item/shipped true]
  [:clara-order-1-item-2 :order-item/shipped true]
  [:ben-order-1          :order/status :delivered]
  [:ben-order-1          :order/delivery-actual "2026-05-10"]
])
```

**Key queries for the page:**

```datalog
; ── not ──────────────────────────────────────────────────────────

; 1. Which items in Clara's order have NOT shipped?
(query [:find ?product-name
        :where [?item :order-item/order :clara-order-1]
               [?item :order-item/product ?p]
               [?p :product/name ?product-name]
               (not [?item :order-item/shipped true])])
; Expected: [["USB-C Cable 2m"]]

; 2. Products that no customer has ever ordered
; (keyboard-k1 has never appeared in any order)
(query [:find ?name
        :where [?p :product/name ?name]
               (not [?item :order-item/product ?p])])
; Expected: [["Compact Keyboard"] ["BudgetBook 14"] ["PhoneX 11"] ["ClearView 27\" Monitor"]]
; (laptop-pro, phone-x, nc-headphones, usb-cable, and monitor-27 have been ordered)
; Note: the alice-order-1-item-2 (usb-cable) was RETRACTED so usb-cable has been
; ordered by clara-order-1-item-3 — it should NOT appear here.

; 3. Customers with no completed (delivered) orders — using not
(query [:find ?name
        :where [?customer :customer/name ?name]
               (not [?order :order/customer ?customer]
                    [?order :order/status :delivered])])
; Expected: [["Alice"] ["Clara"]]
; (Ben's order is now :delivered; Alice and Clara have no delivered orders)

; ── not-join ─────────────────────────────────────────────────────

; 4. Same query using not-join (existential inner variable)
; ?order is existential — not listed in join-vars, so it is fresh inside the body.
; This is the correct form when the inner variable (?order) is not pre-bound.
(query [:find ?name
        :where [?customer :customer/name ?name]
               (not-join [?customer]
                         [?order :order/customer ?customer]
                         [?order :order/status :delivered])])
; Expected: [["Alice"] ["Clara"]]

; 5. Why not-join instead of not for query 4?
; The following would be a PARSE ERROR because ?order is not pre-bound:
;   (not [?order :order/customer ?customer]
;        [?order :order/status :delivered])
; not requires ALL variables in the body to be bound by outer clauses.
; not-join allows inner-only variables (?order) to be existentially quantified.
```

**Page structure:**
1. Scenario paragraph
2. "Clara's order arrives — partially" — tx 13 + tx 14
3. "Finding the unshipped item with `not`" — query 1, explain safety rule (outer bindings only)
4. "Products nobody has ordered" — query 2, note the retracted USB cable doesn't count
5. "Customers without a delivery" — query 3 using multi-clause `not`
6. "When `not` isn't enough: `not-join`" — contrast query 3 vs query 5, explain the parse error, show query 4 as the correct form
7. "Existential variables in `not-join`" — the join-vars vs body-only variable distinction
8. "Stratification" — one-paragraph explanation: why `(not (p ?x)) ... (not (q ?x))` mutual negation is rejected
9. Reference links to `Datalog-Reference.md#negation`

- [ ] **Step 1: Write `.wiki/Tutorial-05-Negation.md`**

- [ ] **Step 2: Verify**

Run setup + sections 1–5 data in order. Confirm all five queries. Query 2 result depends on what's been ordered — verify the exact set of unordered products given the full data up to tx 14.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-05-Negation.md
git commit -m "docs: tutorial section 5 — not, not-join"
git push
cd ..
```

---

## Task 8: Section 6 — Aggregates, `:with`, and window functions

**Files:**
- Create: `.wiki/Tutorial-06-Aggregates.md`

**Scenario (`:with`):** Alice orders two identical USB-C cables — same product, same price,
two separate line items. Without `:with`, the two rows collapse before summing, producing
half the correct total. Adding `:with ?item` fixes it.

**Scenario (aggregates):** Total spend per customer. Cheapest product in each category.

**Scenario (window functions):** Products ranked by price within their category.

**Data setup** (cumulative tx_count: 14 → 17):

```
# ── tx 15: Alice orders two USB-C cables ─────────────────────────
(transact [
  [:alice-order-2        :order/customer :alice]
  [:alice-order-2        :order/status :placed]
  [:alice-order-2-item-1 :order-item/order :alice-order-2]
  [:alice-order-2-item-1 :order-item/product :usb-cable]
  [:alice-order-2-item-1 :order-item/qty 1]
  [:alice-order-2-item-1 :order-item/price 19]
  [:alice-order-2-item-2 :order-item/order :alice-order-2]
  [:alice-order-2-item-2 :order-item/product :usb-cable]
  [:alice-order-2-item-2 :order-item/qty 1]
  [:alice-order-2-item-2 :order-item/price 19]
])

# ── tx 16: LaptopPro price drop again ────────────────────────────
(retract [[:laptop-pro :product/price 1259]])

# ── tx 17: New price ──────────────────────────────────────────────
(transact [[:laptop-pro :product/price 1229]])
```

**Key queries for the page:**

```datalog
; ── :with — the duplicate-row problem ────────────────────────────

; 1. WITHOUT :with — WRONG result (rows collapse before sum)
(query [:find ?name (sum ?price)
        :where [?item :order-item/order :alice-order-2]
               [?item :order-item/product ?p]
               [?p :product/name ?name]
               [?item :order-item/price ?price]])
; Expected: [["USB-C Cable 2m" 19]]   ← WRONG, should be 38

; 2. WITH :with — correct result
(query [:find ?name (sum ?price)
        :with ?item
        :where [?item :order-item/order :alice-order-2]
               [?item :order-item/product ?p]
               [?p :product/name ?name]
               [?item :order-item/price ?price]])
; Expected: [["USB-C Cable 2m" 38]]   ← correct

; ── Scalar aggregates ─────────────────────────────────────────────

; 3. Total spend per customer (across all their orders)
(query [:find ?customer-name (sum ?price)
        :with ?item
        :where [?customer :customer/name ?customer-name]
               [?order :order/customer ?customer]
               [?item :order-item/order ?order]
               [?item :order-item/price ?price]])
; Expected: [["Alice" 837]    ; 799 (phone) + 19 + 19 (two cables)
;            ["Ben" 1299]     ; price-at-purchase recorded at tx 6
;            ["Clara" 717]]   ; 249 + 449 + 19

; 4. Cheapest product in each leaf category
(query [:find ?category-name (min ?price)
        :where [?p :product/price ?price]
               [?p :product/category ?cat]
               [?cat :category/name ?category-name]])
; Expected: one row per category with its minimum price

; 5. Number of products per category
(query [:find ?category-name (count ?p)
        :where [?p :product/category ?cat]
               [?cat :category/name ?category-name]])

; ── Window functions ──────────────────────────────────────────────

; 6. Products ranked by price within their category (highest first)
(query [:find ?category-name ?product-name ?price
              (rank :over (:partition-by ?cat :order-by ?price :desc))
        :where [?p :product/name ?product-name]
               [?p :product/price ?price]
               [?p :product/category ?cat]
               [?cat :category/name ?category-name]])
; Expected: within Laptops — LaptopPro 15 rank 1, BudgetBook 14 rank 2
;           within Mobile  — PhoneX 12 rank 1, PhoneX 11 rank 2
;           within Accessories — Compact Keyboard rank 1, USB-C Cable rank 2
;           other categories  — single product, rank 1

; 7. Row number over all products ordered by price ascending
(query [:find ?product-name ?price (row-number :over (:order-by ?price))
        :where [?p :product/name ?product-name]
               [?p :product/price ?price]])
; Expected: 8 rows numbered 1–8 by ascending price

; 8. Cumulative running total of product prices (ascending)
(query [:find ?product-name ?price (sum ?price :over (:order-by ?price))
        :where [?p :product/name ?product-name]
               [?p :product/price ?price]])
; Expected: 8 rows with a running cumulative sum
```

**Page structure:**
1. Scenario paragraph (`:with` problem)
2. "The duplicate-row problem" — queries 1 and 2, side-by-side expected output, explain why grouping happens before aggregation
3. "`:with` — adding a hidden grouping key" — how it works
4. "Total spend per customer" — query 3 (uses `:with ?item` for correctness)
5. "Cheapest product per category" — query 4 (`min`)
6. "Product count per category" — query 5 (`count`)
7. "Window functions — annotating without collapsing" — contrast with aggregates
8. "Products ranked within category" — query 6 (`rank`, `:partition-by`, `:order-by`)
9. "Row numbers and running totals" — queries 7 and 8
10. Reference links to `Datalog-Reference.md#aggregation`, `#window-functions`

- [ ] **Step 1: Write `.wiki/Tutorial-06-Aggregates.md`**

- [ ] **Step 2: Verify**

Load all sections 1–6 data. Confirm:
- Query 1 returns `[["USB-C Cable 2m" 19]]` (the wrong answer — this is intentional)
- Query 2 returns `[["USB-C Cable 2m" 38]]` (the correct answer)
- Query 3 total for Alice = 837, Ben = 1299, Clara = 717

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-06-Aggregates.md
git commit -m "docs: tutorial section 6 — aggregates, :with, window functions"
git push
cd ..
```

---

## Task 9: Section 7 — Expression clauses and predicates

**Files:**
- Create: `.wiki/Tutorial-07-Expressions.md`

**Scenario:** Filter products under $500. Find orders where the actual delivery arrived after
the promised date. Calculate the delay. Validate SKU format. Compute a discounted price
arithmetically.

**Data setup:** None. All data needed is already present by section 6.

**Key queries for the page:**

```datalog
; 1. Filter products: price under $500
(query [:find ?name ?price
        :where [?p :product/name ?name]
               [?p :product/price ?price]
               [(< ?price 500)]])
; Expected: NoiseCancel Pro $249, ClearView Monitor $449, Compact Keyboard $89, USB-C Cable $19
; (BudgetBook $699 and phones excluded — above $500)

; 2. Find orders delivered after their promised date
(query [:find ?customer-name ?promise ?actual
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?order :order/delivery-promise ?promise]
               [?order :order/delivery-actual ?actual]
               [(> ?actual ?promise)]])
; Expected: none with current data (Ben was delivered on time)
; The page should note: add an overdue delivery to demonstrate:
;   (transact [[:clara-order-1 :order/delivery-actual "2026-05-25"]])
;   — then re-run: Expected: [["Clara" "2026-05-20" "2026-05-25"]]
; This transact should be run interactively as part of the demo, not in the base data.

; 3. Arithmetic binding: compute delay in days (string comparison approximation)
; Note: Minigraf stores dates as strings; for precise day arithmetic, use epoch integers.
; The tutorial should use integer epoch days for this:
;   (transact [[:clara-order-1 :order/delivery-promise-epoch 20230]   ; days since epoch
;              [:clara-order-1 :order/delivery-actual-epoch 20235]])
;
; Then:
(query [:find ?customer-name ?delay-days
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?order :order/delivery-promise-epoch ?promise]
               [?order :order/delivery-actual-epoch ?actual]
               [(- ?actual ?promise) ?delay-days]
               [(> ?delay-days 0)]])
; Expected: [["Clara" 5]]

; 4. Type predicate: find all attributes stored as integers
(query [:find ?name ?price
        :where [?p :product/name ?name]
               [?p :product/price ?price]
               [(integer? ?price)]])
; Expected: all 8 products (all prices are integers)

; 5. String predicate: products whose SKU starts with "LP"
(query [:find ?name ?sku
        :where [?p :product/name ?name]
               [?p :product/sku ?sku]
               [(starts-with? ?sku "LP")]])
; Expected: [["LaptopPro 15" "LP-15"]]

; 6. Arithmetic in aggregation: order total from qty × price
(query [:find ?customer-name (sum ?line-total)
        :with ?item
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?item :order-item/order ?order]
               [?item :order-item/price ?price]
               [?item :order-item/qty ?qty]
               [(* ?price ?qty) ?line-total]])
; Expected: totals per customer using qty-weighted prices
```

For query 3, add this transact as part of the section's interactive demo (not the base dataset):
```
(transact [
  [:clara-order-1 :order/delivery-promise-epoch 20230]
  [:clara-order-1 :order/delivery-actual-epoch 20235]
])
```

**Page structure:**
1. Scenario paragraph
2. "Filter predicates" — query 1, explain `[(< ?price 500)]` syntax
3. "Comparison predicates" — cover all comparison operators with small examples
4. "Finding late deliveries" — query 2 (no results), then add the overdue delivery interactively, re-run
5. "Arithmetic bindings — computing delay in days" — query 3, explain `[(expr) ?var]` binding form
6. "Type predicates" — query 4, list all type predicates
7. "String predicates" — query 5, list `starts-with?` `ends-with?` `contains?` `matches?`
8. "Arithmetic in aggregation" — query 6, show how `[(* ?price ?qty) ?line-total]` feeds `(sum ...)`
9. "Safety rule" — variables must be bound before appearing in an expression
10. Reference links to `Datalog-Reference.md#arithmetic-predicate-expressions`

- [ ] **Step 1: Write `.wiki/Tutorial-07-Expressions.md`**

- [ ] **Step 2: Verify**

Verify queries 1, 4, 5, and 6 against cumulative data. For query 2, follow the interactive demo steps. For query 3, add the epoch transact and confirm result `[["Clara" 5]]`.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-07-Expressions.md
git commit -m "docs: tutorial section 7 — expression clauses and predicates"
git push
cd ..
```

---

## Task 10: Section 8 — Prepared queries with bind slots

**Files:**
- Create: `.wiki/Tutorial-08-Prepared-Queries.md`

**Scenario:** Corestore's backend runs the same queries for every customer page load, every
product lookup, every delivery status check. Parsing the same query string on every call is
wasteful. Prepared queries parse and plan once; bind slots (`$slot`) substitute values at
execute time.

**Data setup:** None.

**Key queries and Rust API code for the page:**

The page must show both the Datalog string and the Rust API call side by side.

```rust
// Rust snippet 1: price lookup by SKU
let pq = db.prepare(
    "(query [:find ?price
             :where [$product :product/price ?price]])"
)?;

let laptop_price = pq.execute(&[
    ("product", BindValue::Entity(laptop_pro_id)),
])?;
// → [[Value::Integer(1229)]]

let phone_price = pq.execute(&[
    ("product", BindValue::Entity(phone_x_id)),
])?;
// → [[Value::Integer(799)]]
```

```rust
// Rust snippet 2: order status as-of a specific tx
let pq = db.prepare(
    "(query [:find ?status
             :as-of $tx
             :where [$order :order/status ?status]])"
)?;

let status_at_order_time = pq.execute(&[
    ("tx",    BindValue::TxCount(6)),
    ("order", BindValue::Entity(ben_order_id)),
])?;
// → [[Value::Keyword(":placed")]]
```

```rust
// Rust snippet 3: sale price valid on a given date (both bind slots)
let pq = db.prepare(
    "(query [:find ?price
             :valid-at $date
             :where [$product :product/sale-price ?price]])"
)?;

let winter_price = pq.execute(&[
    ("product", BindValue::Entity(laptop_pro_id)),
    ("date",    BindValue::Timestamp(1735689600000)), // 2026-01-01 in ms
])?;
// → [[Value::Integer(1049)]]
```

```datalog
; REPL equivalent of snippet 1 (for readers following along without Rust)
(query [:find ?price
        :where [:laptop-pro :product/price ?price]])
; → [[1229]]
; Prepared queries are a Rust API feature — the REPL always uses literal values.
```

**Page structure:**
1. Scenario paragraph — why prepared queries matter (parse-once, execute-many)
2. "Bind slot syntax" — `$identifier` in entity, value, `:as-of`, `:valid-at` positions
3. "Prepare once, execute many" — snippets 1 and 2
4. "Temporal bind slots" — snippet 3, explain `BindValue::Timestamp` (milliseconds)
5. "What cannot be parameterised" — attribute position is not allowed, explain why (index selection at prepare time)
6. "Live fact store" — each `execute()` sees current state; facts transacted after `prepare()` are visible
7. "REPL vs Rust API" — note that the REPL always uses literal values; prepared queries are a Rust API concern
8. Reference links to `Datalog-Reference.md#prepared-statements-rust-api`

- [ ] **Step 1: Write `.wiki/Tutorial-08-Prepared-Queries.md`** with all Rust and Datalog snippets above

- [ ] **Step 2: Verify**

This section is Rust API only. Verify by reading `examples/` for correct BindValue import path:

```bash
grep -r "BindValue" src/
```

Confirm the import path is `minigraf::BindValue` and update the Rust snippets if the actual module path differs.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-08-Prepared-Queries.md
git commit -m "docs: tutorial section 8 — prepared queries with bind slots"
git push
cd ..
```

---

## Task 11: Section 9 — Disjunction: `or`, `or-join`

**Files:**
- Create: `.wiki/Tutorial-09-Disjunction.md`

**Scenario:** Corestore's delivery tracking page needs to show orders that are in *either*
"shipped" or "out for delivery" status. A product search needs to match items in *either*
the Audio *or* Mobile category. Clara's refund covers items from either of two cancelled
orders — branches introduce different inner variables, requiring `or-join`.

**Data setup** (cumulative tx_count: 17 → 18):

```
# ── tx 18: Add more order statuses and a cancelled order for Clara ─
(transact [
  [:ben-order-1        :order/status :delivered]    ; already :delivered, idempotent-ish
  [:clara-order-2        :order/customer :clara]
  [:clara-order-2        :order/status :cancelled]
  [:clara-order-2-item-1 :order-item/order :clara-order-2]
  [:clara-order-2-item-1 :order-item/product :keyboard-k1]
  [:clara-order-2-item-1 :order-item/price 89]
  [:clara-order-1        :order/status :shipped]
])
```

**Key queries for the page:**

```datalog
; 1. Orders in status :shipped OR :delivered
(query [:find ?customer-name ?status
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?order :order/status ?status]
               (or [?order :order/status :shipped]
                   [?order :order/status :delivered])])
; Expected: Clara's order (:shipped), Ben's order (:delivered)

; 2. Products in "Audio" OR "Mobile" category (direct, non-recursive)
(query [:find ?name ?category-name
        :where [?p :product/name ?name]
               [?p :product/category ?cat]
               [?cat :category/name ?category-name]
               (or [?cat :category/name "Audio"]
                   [?cat :category/name "Mobile"])])
; Expected: PhoneX 12 (Mobile), PhoneX 11 (Mobile)
; (NoiseCancel Pro is in :cat-nc "Noise-Cancelling", not direct "Audio")

; 3. Same query using or with a variable (note: both branches must bind ?category-name)
(query [:find ?name ?category-name
        :where [?p :product/name ?name]
               [?p :product/category ?cat]
               (or (and [?cat :category/name ?category-name]
                        [?cat :category/name "Audio"])
                   (and [?cat :category/name ?category-name]
                        [?cat :category/name "Mobile"]))])
; Expected: same as query 2

; 4. or-join: items refundable from Clara's order-1 OR order-2
; Each branch introduces a private variable for the order entity.
(query [:find ?product-name ?refund-price
        :where [?item :order-item/product ?p]
               [?p :product/name ?product-name]
               [?item :order-item/price ?refund-price]
               (or-join [?item]
                 (and [?item :order-item/order :clara-order-1])
                 (and [?item :order-item/order :clara-order-2]))])
; Expected: all items from both of Clara's orders

; 5. or inside a rule
(rule [(active-order ?customer ?order)
       [?order :order/customer ?customer]
       (or [?order :order/status :placed]
           [?order :order/status :processing]
           [?order :order/status :shipped])])

(query [:find ?customer-name
        :where [?customer :customer/name ?customer-name]
               (active-order ?customer ?order)])
; Expected: Alice (:placed), Clara (:shipped) — Ben is :delivered (not active)
```

**Page structure:**
1. Scenario paragraph
2. "Finding orders by multiple statuses with `or`" — query 1
3. "Branch variable safety" — all branches must introduce the same new variable names
4. "Matching across categories" — query 2, note that `or` binds no new variable here
5. "Binding variables inside `or` branches with `and`" — query 3
6. "`or-join` — branches with private variables" — query 4, contrast with `or`
7. "`or` inside a rule body" — query 5
8. "Safety rules" — `:join_vars` in `or-join` must be pre-bound; branch variable parity for `or`
9. Reference links to `Datalog-Reference.md#disjunction`

- [ ] **Step 1: Write `.wiki/Tutorial-09-Disjunction.md`**

- [ ] **Step 2: Verify**

Load cumulative data through section 9. Verify all five queries. Query 1 should return two rows (Clara :shipped, Ben :delivered). Query 5 should return Alice and Clara only.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-09-Disjunction.md
git commit -m "docs: tutorial section 9 — or, or-join"
git push
cd ..
```

---

## Task 12: Section 10 — User-defined functions

**Files:**
- Create: `.wiki/Tutorial-10-UDFs.md`
- Create: `examples/tutorial_udfs.rs`

**Scenario:** Corestore needs two custom functions that the built-in set cannot provide:
a `valid-promo?` predicate that validates discount codes against an internal format, and a
`delivery-score` aggregate that computes a weighted on-time delivery rating per customer.

**`examples/tutorial_udfs.rs`:**

```rust
//! Tutorial: User-Defined Functions (UDFs)
//!
//! Demonstrates `register_predicate` and `register_aggregate` from the
//! Minigraf Datalog tutorial series (Section 10).
//!
//! Run with: cargo run --example tutorial_udfs

use minigraf::{Minigraf, OpenOptions, Value};

fn main() -> anyhow::Result<()> {
    let db = OpenOptions::new().open()?;

    // ── Load base data ──────────────────────────────────────────────
    db.execute(r#"(transact [
        [:alice :customer/name "Alice"]
        [:ben   :customer/name "Ben"]
        [:clara :customer/name "Clara"]
        [:order-a :order/customer :alice]
        [:order-a :order/on-time true]
        [:order-a :order/weight 2]
        [:order-b :order/customer :alice]
        [:order-b :order/on-time false]
        [:order-b :order/weight 1]
        [:order-c :order/customer :ben]
        [:order-c :order/on-time true]
        [:order-c :order/weight 3]
        [:promo-1 :promo/code "CORESTORE-SUMMER2026"]
        [:promo-2 :promo/code "INVALID_CODE"]
    ])"#)?;

    // ── Register predicate UDF: valid-promo? ────────────────────────
    // A valid promo code: starts with "CORESTORE-" and is at least 15 chars.
    db.register_predicate("valid-promo?", |v| {
        matches!(v, Value::String(s)
            if s.starts_with("CORESTORE-") && s.len() >= 15)
    })?;

    // Use valid-promo? in a where clause
    let valid_promos = db.execute(
        "(query [:find ?code
                 :where [?p :promo/code ?code]
                        [(valid-promo? ?code)]])"
    )?;
    println!("Valid promos: {:?}", valid_promos);
    // Expected: [["CORESTORE-SUMMER2026"]]

    // ── Register aggregate UDF: delivery-score ──────────────────────
    // Weighted on-time rate: sum(weight if on-time) / sum(weight)
    // State: (weighted_on_time: i64, total_weight: i64)
    db.register_aggregate(
        "delivery-score",
        || Box::new((0i64, 0i64)),
        |state, val| {
            // val is a Value::Boolean(on_time) — but we need weight too.
            // Encode as: pass (on_time_weight) pre-computed in the query.
            if let Value::Integer(w) = val {
                let s = state.downcast_mut::<(i64, i64)>().unwrap();
                s.1 += w.abs();
                if *w >= 0 { s.0 += *w; }
            }
        },
        |state| {
            let s = state.downcast_ref::<(i64, i64)>().unwrap();
            if s.1 == 0 {
                Value::Null
            } else {
                Value::Float(s.0 as f64 / s.1 as f64)
            }
        },
    )?;

    // Pre-compute signed weight: positive if on-time, negative if late
    // Use arithmetic binding in the query to encode on-time as +weight, late as -weight.
    // Note: Minigraf doesn't have if/else; encode with two or-join branches.
    let scores = db.execute(r#"
        (query [:find ?name (delivery-score ?signed-weight)
                :with ?order
                :where [?customer :customer/name ?name]
                       [?order :order/customer ?customer]
                       [?order :order/weight ?w]
                       (or-join [?order ?w ?signed-weight]
                         (and [?order :order/on-time true]
                              [(?w) ?signed-weight])
                         (and [?order :order/on-time false]
                              [((* ?w -1)) ?signed-weight]))])
    "#)?;
    println!("Delivery scores: {:?}", scores);
    // Alice: order-a on-time w=2, order-b late w=1 → score = 2/3 ≈ 0.667
    // Ben:   order-c on-time w=3 → score = 3/3 = 1.0

    Ok(())
}
```

Note: the `or-join` + arithmetic approach above is a pedagogically honest demonstration of
how to work around Minigraf's lack of `if/else` in expressions. Simplify the aggregate UDF
example if the `or-join` makes it too complex for the tutorial page — an alternative is to
pre-aggregate in Rust before transacting a `:order/signed-weight` fact.

**Page structure:**
1. Scenario paragraph
2. "Predicate UDFs — `register_predicate`" — show the `valid-promo?` registration and usage
3. "Using a predicate UDF in a `:where` clause" — show the query
4. "Aggregate UDFs — `register_aggregate`" — show `delivery-score` registration
5. "Using an aggregate UDF in `:find`" — show the scores query
6. "UDFs in window clauses" — show how a UDF aggregate can be used in `:over (...)` (brief example)
7. "Runtime resolution" — parse-then-register ordering, error on unregistered call
8. "Running the example" — `cargo run --example tutorial_udfs`
9. Reference links to `Datalog-Reference.md#user-defined-functions`

- [ ] **Step 1: Write `examples/tutorial_udfs.rs`** using the code above

- [ ] **Step 2: Verify the example compiles and runs**

```bash
cargo run --example tutorial_udfs
```

Expected output:
```
Valid promos: [["CORESTORE-SUMMER2026"]]
Delivery scores: [["Alice", 0.6666...], ["Ben", 1.0]]
```

Fix any compilation errors before proceeding.

- [ ] **Step 3: Write `.wiki/Tutorial-10-UDFs.md`** with the Rust snippets and page structure above

- [ ] **Step 4: Commit main repo**

```bash
git add examples/tutorial_udfs.rs
git commit -m "chore: add tutorial UDF example (section 10)"
```

- [ ] **Step 5: Commit wiki**

```bash
cd .wiki
git add Tutorial-10-UDFs.md
git commit -m "docs: tutorial section 10 — user-defined functions"
git push
cd ..
```

---

## Task 13: Section 11 — Marketplace

**Files:**
- Create: `.wiki/Tutorial-11-Marketplace.md`

**Scenario:** Corestore opens its platform to third-party sellers. Two sellers join. Clara
places an order that routes to an external seller — delivery SLAs differ from Corestore's
own, and a dispute arises when the seller's estimated window diverges from the platform's
record. These queries require joining against a seller entity; the single-seller model cannot
express them.

**Data setup** (cumulative tx_count: 18 → 20):

```
# ── tx 19: Seller entities ────────────────────────────────────────
(transact [
  [:corestore-direct :seller/name "Corestore Direct"]
  [:corestore-direct :seller/sla-days 3]
  [:techsource       :seller/name "TechSource"]
  [:techsource       :seller/sla-days 7]
])

# ── tx 20: Marketplace order from Clara via TechSource ────────────
(transact [
  [:clara-order-3        :order/customer :clara]
  [:clara-order-3        :order/seller :techsource]
  [:clara-order-3        :order/status :placed]
  [:clara-order-3        :order/delivery-promise "2026-05-28"]
  [:clara-order-3-item-1 :order-item/order :clara-order-3]
  [:clara-order-3-item-1 :order-item/product :laptop-budget]
  [:clara-order-3-item-1 :order-item/price 699]
])
```

**Key queries for the page:**

```datalog
; 1. All orders showing which seller fulfilled them
; (Corestore-direct orders have no :order/seller — use not-join to detect)
(query [:find ?customer-name ?seller-name ?status
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?order :order/status ?status]
               (or-join [?order ?seller-name]
                 (and [?order :order/seller ?seller]
                      [?seller :seller/name ?seller-name])
                 (and (not-join [?order]
                                [?order :order/seller ?any-seller])
                      [(?corestore-direct) ?seller-name]))])
; Note: the last branch is illustrative — in practice, default to "Corestore Direct"
; via a rule. Show both the naive and rule-based approaches.

; 2. Orders where the seller SLA is > 5 days
(query [:find ?customer-name ?seller-name ?sla
        :where [?order :order/customer ?customer]
               [?customer :customer/name ?customer-name]
               [?order :order/seller ?seller]
               [?seller :seller/name ?seller-name]
               [?seller :seller/sla-days ?sla]
               [(> ?sla 5)]])
; Expected: [["Clara" "TechSource" 7]]

; 3. Recursive rule: all orders reachable through a customer–seller relationship
(rule [(customer-of-seller ?customer ?seller)
       [?order :order/customer ?customer]
       [?order :order/seller ?seller]])

(query [:find ?customer-name ?seller-name
        :where (customer-of-seller ?customer ?seller)
               [?customer :customer/name ?customer-name]
               [?seller :seller/name ?seller-name]])
; Expected: [["Clara" "TechSource"]]

; 4. Bi-temporal: what did the system record about Clara's TechSource order at placement?
(query [:find ?promise
        :as-of 20
        :where [:clara-order-3 :order/delivery-promise ?promise]])
; Expected: [["2026-05-28"]]

; 5. Seller with no delivered orders (negation across sellers)
(query [:find ?seller-name
        :where [?seller :seller/name ?seller-name]
               (not-join [?seller]
                         [?order :order/seller ?seller]
                         [?order :order/status :delivered])])
; Expected: [["TechSource"]]  (Corestore Direct has no :order/seller facts at all — see note)
```

**Page structure:**
1. Scenario paragraph — why single-seller fails here
2. "Adding sellers to the model" — tx 19, tx 20
3. "Querying across seller entities" — query 2 (simpler first)
4. "Defaulting to the platform seller" — discuss the or-join approach in query 1, then show the rule-based alternative
5. "Recursive seller-customer relationships" — query 3
6. "Bi-temporal queries still work across sellers" — query 4
7. "Sellers with no completed deliveries" — query 5
8. "Synthesis" — note how all concepts from sections 1–10 compose naturally
9. Reference links to all relevant `Datalog-Reference.md` sections

- [ ] **Step 1: Write `.wiki/Tutorial-11-Marketplace.md`**

- [ ] **Step 2: Verify**

Load all cumulative data (sections 1–11). Confirm query 2 returns TechSource with SLA 7, query 5 returns TechSource.

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Tutorial-11-Marketplace.md
git commit -m "docs: tutorial section 11 — marketplace"
git push
cd ..
```

---

## Task 14: Navigation and index updates

**Files:**
- Modify: `.wiki/Home.md`
- Modify: `.wiki/Learning-Resources.md`

- [ ] **Step 1: Update `.wiki/Home.md`**

Add a "Tutorials" section before "Reference" with links to all 12 pages:

```markdown
## Tutorials

A step-by-step introduction to Minigraf's Datalog dialect, driven by a real-world
e-commerce storyline. Each section builds on the last.

- [Setup — Install Minigraf and load the Corestore dataset](Tutorial-Setup)
- [1. Basic transact + query](Tutorial-01-Basic-Transact-Query)
- [2. `:as-of` — time travel by transaction time](Tutorial-02-As-Of)
- [3. `:valid-at` and `:any-valid-time` — time travel by valid time](Tutorial-03-Valid-At)
- [4. Recursive rules](Tutorial-04-Recursive-Rules)
- [5. Negation — `not` and `not-join`](Tutorial-05-Negation)
- [6. Aggregates, `:with`, and window functions](Tutorial-06-Aggregates)
- [7. Expression clauses and predicates](Tutorial-07-Expressions)
- [8. Prepared queries with bind slots](Tutorial-08-Prepared-Queries)
- [9. Disjunction — `or` and `or-join`](Tutorial-09-Disjunction)
- [10. User-defined functions](Tutorial-10-UDFs)
- [11. Marketplace — multi-seller queries](Tutorial-11-Marketplace)
```

- [ ] **Step 2: Update `.wiki/Learning-Resources.md`**

Replace the "Learn Datalog Today" and Datomic tutorial links with:

```markdown
## Minigraf Datalog Tutorial

- [Tutorial Series](Tutorial-Setup) — a hands-on, eleven-section walkthrough of Minigraf's
  Datalog dialect, driven by an e-commerce storyline (Corestore). Covers every language
  feature from basic transact/query through recursive rules, bi-temporal queries, negation,
  aggregation, window functions, prepared queries, disjunction, UDFs, and marketplace
  multi-entity scenarios.
```

Keep the external links below it as "Further Reading".

- [ ] **Step 3: Commit wiki**

```bash
cd .wiki
git add Home.md Learning-Resources.md
git commit -m "docs: add tutorial series navigation to Home and Learning-Resources"
git push
cd ..
```

- [ ] **Step 4: Commit main repo + open PR**

```bash
git add demos/tutorial_corestore_setup.txt examples/tutorial_udfs.rs
git commit -m "docs: tutorial series for issue #234 (sections 1–11)"
```

Then open a PR against main referencing issue #234.

---

## Self-Review Checklist

- [x] `retract` covered — Section 1 (Alice cancels USB cable)
- [x] `:any-valid-time` has a concrete worked example — Section 3 (full sale price audit)
- [x] `:with` has a before/after example — Section 6 (duplicate USB cables)
- [x] All 11 grammar constructs from the spec coverage checklist are mapped to a section
- [x] Tx counts are documented and deterministic
- [x] Section 10 (UDFs) uses Rust API + `examples/tutorial_udfs.rs`; not REPL-only
- [x] Section 11 marketplace is additive — all concepts from prior sections compose
- [x] No placeholder steps — every step has actual Datalog or Rust code
- [x] Wiki push instructions in every task
