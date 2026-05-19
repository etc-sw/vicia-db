# Error Reference Guide Design

**Issue**: #192
**Date**: 2026-05-19
**Status**: Approved

---

## Overview

Produce `docs/ERROR_REFERENCE.md` — a single Markdown file that inventories every user-facing error in the core Minigraf Rust library, explains its cause, and gives resolution steps with a bad-input example.

**Out of scope**: FFI/binding errors (Python, Node — handled in their own repos). Runtime error codes (tracked in #277 — future work).

---

## Document Structure

`docs/ERROR_REFERENCE.md` with the following top-level layout:

```
# Minigraf Error Reference

Intro: what this doc covers, how errors surface (anyhow::Error from
execute()/prepare()), note that PRS-xxx codes are doc-only reference
identifiers (runtime codes tracked in #277).

## Quick Reference Table
| Code | Error (prefix) | Category |
|------|----------------|----------|
... all non-appendix errors ...

## PRS — Parser Errors
## QRY — Query Execution Errors
## STG — Storage Errors
## WAL — WAL Errors
## API — Database API Errors

## Appendix: Internal Errors
Brief list of "internal parser error: ..." and similar strings that
indicate a library bug rather than user error. Instructs readers to
file a bug report.
```

---

## Error Categories and Code Prefixes

| Prefix | Source module(s)                        | Description                          |
|--------|-----------------------------------------|--------------------------------------|
| `PRS`  | `src/query/datalog/parser.rs`           | Datalog/EDN parsing and validation   |
| `QRY`  | `src/query/datalog/executor.rs`, `matcher.rs`, `evaluator.rs` | Query execution, predicate, type errors |
| `STG`  | `src/storage/`                          | File format, header, B+tree, page errors |
| `WAL`  | `src/wal.rs`                            | Write-ahead log integrity and serialisation |
| `API`  | `src/db.rs`                             | Public API contract and lock errors  |

Codes are assigned sequentially within each category: `PRS-001`, `PRS-002`, … These codes are stable — if error text changes, the code stays the same. They serve as documentation anchors and as the future contract for #277.

---

## Entry Format

Each error gets a level-3 heading with its code and short name, followed by a fixed set of fields:

```markdown
### PRS-001 Unexpected end of input

**Error text**: `Unexpected end of input`

**Cause**: The input was cut off before the parser could complete an
expression. Commonly occurs when a list or vector is opened but never
closed, or when the REPL receives an empty line where a form is expected.

**Resolution**:
- Ensure all `(` and `[` are matched with `)` and `]`
- Use the REPL's multi-line mode for long queries

**Example**:
```datalog
(query {:find [?e]
        :where [[?e :name "alice"]
```
*(missing closing `]` and `}`)*
```

Rules:
- **Parameterised errors** (e.g. `"String exceeds maximum length of {} bytes"`) show the concrete limit in the error text field, e.g. `` `String exceeds maximum length of 4096 bytes` ``.
- **Cause** is 1–3 sentences focused on why the error fires, not implementation detail.
- **Resolution** is a short bulleted list (1–3 items). Link to `.wiki/Datalog-Reference.md` for query/parse errors and to `README.md` (file format section) for storage/WAL errors where relevant.
- **Example** shows the minimal bad input that triggers the error. For storage/WAL errors where a Datalog example is not applicable, show the scenario in prose instead.

---

## Internal Errors (Appendix)

Errors prefixed with `"internal parser error: ..."` or similar indicate a bug in the library, not a user mistake. These are listed briefly in an appendix with a note to open a GitHub issue rather than receiving full standard entries. This keeps the main reference focused on actionable guidance.

---

## Outbound Links

The guide links outward from entries to:
- `.wiki/Datalog-Reference.md` — for temporal modifiers (`:as-of`, `:valid-at`), aggregate/window syntax, negation, disjunction
- `README.md` — for file format version and WAL context
- `BENCHMARKS.md` — for fact size / payload limits (WAL entry size error)
- `#277` — noted in the intro as the future home of runtime codes

No inbound links are added in this issue — that is left for a follow-up doc-sync pass.

---

## Scope of Error Inventory

Errors to document (non-appendix):

**PRS (~65 errors)**: EDN tokeniser, string/keyword/symbol bounds, unexpected tokens, unclosed delimiters, query clause validation (`:as-of`, `:valid-at`, `:find`, `:where`, `:with`), aggregate/window function syntax, transaction/retraction fact format, where-clause patterns (`not`, `not-join`, `or`, `or-join`), expression operators, rule/bind-slot parsing, UUID tagged literals.

**QRY (~8 errors)**: Invalid entity/attribute/value at execution time, pseudo-attribute transact guard, unknown predicate, lock poison.

**STG (~20 errors)**: Magic number, file too short, unsupported format version, header field validation, B+tree page type, packed page format, page/count overflow errors.

**WAL (~6 errors)**: Magic number, unsupported version, fact size limit, size overflow, num_facts overflow, WAL file deletion failure.

**API (~8 errors)**: Write lock poison, unexpected command variant, attribute keyword guard, pseudo-attribute guard, prepare-only-query guards, function registry lock poison, WAL not initialised.

Total: ~107 documented entries + appendix of ~10 internal errors.

---

## Delivery

- Single file: `docs/ERROR_REFERENCE.md`
- Committed to `main` via worktree + PR (standard workflow)
- No changes to source code
- `ROADMAP.md` item `🎯 Error message guide` updated to `✅` at completion
- `CHANGELOG.md` entry added under the active phase
