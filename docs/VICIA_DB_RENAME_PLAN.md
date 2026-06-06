# Vicia DB Rename Plan

Status: V0 rename plan and V1 docs/metadata preparation are complete. No
package, public API, file format, or repository rename has been performed.

Date: 2026-06-06

Branch: `vicia/rename-plan`

## Recommendation

Adopt **Vicia DB** as the Vetch-oriented name for this Minigraf line, but do it
as a staged successor/fork rename rather than a broad in-place rewrite.

The first implementation step after this document should be a docs/metadata
slice. Public Rust API and language-binding renames should happen only after the
compatibility policy is explicit.

## Name Rationale

`Vicia` is the botanical genus associated with vetch. That gives the database a
direct relationship to Vetch without using Earthsea-specific names or making the
brand feel derivative.

Preferred naming:

| Surface | Preferred Name | Notes |
| --- | --- | --- |
| Product / docs | Vicia DB | Human-facing name. |
| Rust package | `vicia-db` | Hyphenated package name if published separately. |
| Rust crate import | `vicia_db` | Rust import form. |
| Decision skill | `vicia-db-decision-gate` | Reusable decision workflow for storage/read-path gates. |
| File extension | Keep `.graph` initially | Avoid file-format churn during rename. |

## Relationship To Minigraf

Vicia DB should be described as a Vetch-oriented successor/fork of Minigraf:

> Vicia DB is a Vetch-oriented successor of Minigraf: an embedded, single-file,
> bi-temporal graph ledger optimized for local-first agent context.

This framing keeps the technical lineage clear while explaining why the name
changes. The rename is not only cosmetic: Vetch now imposes concrete operating
constraints around 1M+ local facts, receipt-sized writes, agent-brief read
latency, full-history identity, and background maintenance.

## Philosophy Fit

The rename is acceptable only if it preserves the existing Minigraf philosophy:

- embedded-first library, not a server
- single durable `.graph` file
- Datalog remains the query language
- no new dependencies for branding
- file-format stability remains more important than name consistency
- API compatibility is protected during the transition

The rename must not become an excuse to add vector search, BM25, multimodal blob
storage, sidecar indexes, or a client/server layer to the core database. Those
remain Vetch-side projections until a benchmark-backed proposal proves they
belong in the core.

## Compatibility Policy To Decide

The next planning gate must choose one of these options before code rename work:

| Option | Shape | Tradeoff |
| --- | --- | --- |
| Alias-first | Add `ViciaDb` as a public alias/wrapper while keeping `Minigraf`. | Lowest breakage, slightly awkward dual naming. |
| Type rename with alias | Rename primary type to `ViciaDb`, keep `type Minigraf = ViciaDb` for one compatibility window. | Clearer new identity, more doc and example churn. |
| Hard rename | Remove `Minigraf` public type. | Cleanest brand, highest compatibility risk. Not recommended yet. |

Recommendation: **type rename with alias**, but only after docs/metadata are
settled. For the first code slice, prefer alias-first if there is any uncertainty
about downstream users or bindings.

## Rename Surfaces

The rename touches more than `Cargo.toml`.

| Surface | First Action | Later Action |
| --- | --- | --- |
| `Cargo.toml` | Decide package/repository/documentation URLs. | Rename package only when publish path is ready. |
| README | Introduce Vicia DB as successor/fork. | Replace examples after API policy is chosen. |
| Rust API | None in docs-only slice. | Add `ViciaDb` alias/wrapper before removing `Minigraf`. |
| Docs | Add rename plan and update roadmap references. | Replace user-facing Minigraf naming where appropriate. |
| Tests/benches | None initially. | Rename imports after crate/package decision. |
| Language bindings | Defer. | Treat as separate releases with compatibility notes. |
| Wiki | Defer until repo rename is real. | Update after final doc sync. |
| License | Add missing license files and attribution before publishing. | Keep original notices and fork lineage. |
| GitHub/crates/docs.rs | No change until publish decision. | Create/rename only after compatibility gate. |

## License And Attribution Checklist

Before publishing a Vicia DB fork/package:

- Keep the actual `LICENSE-MIT` and `LICENSE-APACHE` files in the checkout.
- Preserve original copyright and license notices.
- State that Vicia DB is derived from/forked from Minigraf.
- Keep `MIT OR Apache-2.0` unless there is a deliberate legal reason to change.
- Do not imply endorsement by the original Minigraf project or organization.
- If the Apache-2.0 path is used, preserve any required NOTICE material.

This document is not legal advice; it is an engineering checklist for avoiding
avoidable open-source hygiene mistakes.

## Decision Skill Candidate

Skill name: `vicia-db-decision-gate`

Purpose:

- Reduce repeated judgment cost when deciding storage, read-path, checkpoint,
  recompact, or public API changes for Vicia DB.

Trigger examples:

- "Vicia DB checkpoint/read path/storage decision"
- "Vetch 1M baseline affects Vicia DB"
- "Should Vicia DB add an API/index/recompact behavior?"
- "Review this Vicia DB storage plan"

Core output shape:

```text
Recommendation:
Risk:
Required gate:
First slice:
Verification:
Rejected:
```

Core rules:

- Measure before optimizing.
- Keep benchmark slices separate from implementation slices.
- Preserve full-history identity:
  `entity`, `attribute`, encoded `value`, `valid_from`, `valid_to`,
  `tx_count`, `tx_id`, and `asserted`.
- Treat `Value::Ref` as mandatory coverage for Vetch graph/ledger behavior.
- Keep small write/checkpoint cost tied to pending/delta size, not committed
  graph size.
- Keep recompact idle/background/scheduled.
- Add public APIs only after the measured Vetch path proves that internal
  storage/query work is insufficient.
- Use reference databases for invariants, not dependencies.
- Separate fallback-safe recovery states from error states before changing
  publish or WAL-retire behavior.

Do not put volatile benchmark numbers directly in the skill. The skill should
route agents to `docs/BENCHMARKS.md`, `docs/DELTA_INDEX_DESIGN.md`, and
`docs/VETCH_DELTA_STORAGE_ROADMAP.md` for current evidence.

## Proposed Slice Plan

### V1 Completion

V1 introduces Vicia DB naming without changing code:

- README transition note.
- Storage roadmap link to this rename plan.
- Delta design link to this rename plan.
- License files confirmed present.

### V0: Rename Plan

This slice.

Done when:

- `docs/VICIA_DB_RENAME_PLAN.md` exists.
- No code/package/API rename has happened.
- The next slice is explicit.

### V1: Docs And Metadata Preparation

Goal:

- Introduce Vicia DB naming without breaking code.

Allowed:

- README wording update.
- Roadmap/design docs update.
- License file addition.
- Attribution wording.
- Optional badges removed or marked pending if URLs are not real yet.

Forbidden:

- Public Rust API break.
- File-format version change.
- Language binding rename.
- New dependencies.

Verification:

- `git diff --check`
- `cargo test` if any doctest/example text changes compile against Rust APIs.

### V2: Rust API Compatibility Alias

Goal:

- Add `ViciaDb` while preserving `Minigraf` compatibility.

Allowed:

- Public alias/wrapper.
- Docs/examples showing Vicia DB first.
- Tests proving `Minigraf` and `ViciaDb` open/query/checkpoint equivalently.

Forbidden:

- Removing `Minigraf`.
- Changing file extension or file format.

Verification:

- Targeted API compatibility tests.
- `cargo test`
- `cargo clippy --lib -- -D warnings`
- `cargo fmt -- --check`

### V3: Package/Repository Publish Decision

Goal:

- Decide whether to publish as `vicia-db`, rename repository, or keep a Vetch
  internal fork.

Gate:

- License files present.
- Attribution text present.
- Downstream package/binding impact listed.
- Name availability checked for the actual publish targets.

### V4: Binding And Ecosystem Rename

Goal:

- Rename bindings only after the core Rust compatibility path is stable.

Gate:

- Separate checklist for npm, PyPI, Maven, Swift, C, WASM, and docs.rs.
- No binding rename should be bundled into core storage changes.

## Non-Goals

- Do not rename storage format in this planning slice.
- Do not change `.graph` files.
- Do not alter V10 delta storage behavior.
- Do not create a public recompact API as part of rename.
- Do not fold Q2-B cleanup work into rename work.
- Do not publish to crates.io/npm/PyPI from this branch.

## Open Questions

- Should `ViciaDb` be a type alias, a newtype wrapper, or the renamed struct
  with `Minigraf` as an alias?
- Should `minigraf` remain as a compatibility crate that re-exports `vicia_db`?
- Should Vicia DB remain Vetch-internal until the Q2 cleanup lane completes?
- Which organization/repository should own the successor package?
- Should the public file extension remain `.graph` indefinitely?

## Current Recommendation

Proceed to V1 only after this plan is reviewed. Keep Q2-B storage cleanup and
Vicia rename work on separate branches.
