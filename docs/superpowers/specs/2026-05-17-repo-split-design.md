# Repo Split Design — #231

**Date**: 2026-05-17  
**Issue**: [project-minigraf/minigraf#231](https://github.com/project-minigraf/minigraf/issues/231)  
**Status**: Approved

---

## Goal

Split language and platform binding packages out of the monorepo into independent repositories under the `project-minigraf` org. The core engine, FFI bridge crate, C bindings, REPL, benchmarks, and examples remain in `minigraf`.

**Motivation**: reduce CI noise on core changes, enable scoped ownership, improve approachability.

---

## Scope

### Phase 1 (this plan)

| New repo | Source directories | Publishes to |
|---|---|---|
| `minigraf-python` | `minigraf-ffi/python/`, `python-ci.yml`, `python-release.yml` | PyPI |
| `minigraf-node` | `minigraf-node/`, `node-ci.yml`, `node-release.yml` | npm (`minigraf`) |
| `minigraf-wasm` | `minigraf-wasm/`, `minigraf-wasi/`, `wasm-browser.yml`, `wasm-wasi.yml`, `wasm-release.yml` | npm (`@minigraf/browser`, `@minigraf/wasi`) |

### Phase 2+ (deferred)

`minigraf-java`, `minigraf-android`, `minigraf-swift` — their CI workflows (`mobile.yml`, `java-ci.yml`, `java-release.yml`) stay in the monorepo until their respective splits.

### Stays in `minigraf` permanently

- `src/` — engine
- `minigraf-ffi/` — UniFFI bridge crate (published to crates.io as `minigraf-ffi`)
- `minigraf-c/` — C header + static/shared lib (cbindgen; no independent package registry)
- `examples/`, `benches/`, `tests/`, REPL, fuzz harness
- Core CI: `rust.yml`, `rust-clippy.yml`, `rustfmt.yml`, `coverage.yml`, `coverage-gates.yml`, `cargo-audit.yml`, `bench.yml`, `fuzz.yml`, `smoke.yml`, `binary-size.yml`, `docs-check.yml`, `osv-scanner.yml`, `llvm-cov.yml`

---

## Repository Structure After Phase 1

**`Cargo.toml` workspace members:**
```toml
members = [".", "minigraf-ffi", "minigraf-c"]
# minigraf-node removed
```

**Workflow count**: drops from 27 → ~16 after Phase 1 (removes 7 binding workflows).

---

## Release Cascade

### Version lockstep

All binding repos release at the same version as core. `minigraf-node@1.2.0` on npm always corresponds to `minigraf@1.2.0` on crates.io. Binding repos have no independent versioning.

### Sequence on `git tag v1.2.0`

`release.yml` is cargo-dist owned and must not be manually edited (project policy). All custom release logic lives in a new standalone workflow: **`cascade.yml`**, triggered on the same tag pattern.

`cascade.yml` sequence:

1. `release.yml` runs in parallel — cargo-dist builds and publishes `minigraf` to crates.io
2. `cascade.yml` runs in parallel:
   a. Publishes `minigraf-ffi` via `cargo publish -p minigraf-ffi`
   b. Polls crates.io until both `minigraf@<version>` and `minigraf-ffi@<version>` are visible (max 3 min, 10s intervals)
   c. Fires `repository_dispatch` to each Phase 1 binding repo in parallel:

```yaml
- name: Dispatch to binding repos
  env:
    GH_TOKEN: ${{ secrets.MINIGRAF_RELEASE_TOKEN }}
  run: |
    VERSION="${{ github.ref_name }}"
    for REPO in minigraf-wasm minigraf-node minigraf-python; do
      gh api repos/project-minigraf/$REPO/dispatches \
        -f event_type=core-release \
        -f client_payload[version]=$VERSION
    done
```

3. Each binding repo's `repository_dispatch` receiver:
   - Updates `minigraf-ffi` version in its manifest (`Cargo.toml`, `pyproject.toml`, `package.json`)
   - Runs CI
   - Tags itself at the same version and publishes to its registry

### Authentication

`MINIGRAF_RELEASE_TOKEN`: a GitHub PAT with `contents:write` + `actions:write` scoped to the `project-minigraf` org, stored as an org-level Actions secret. Each binding repo inherits it automatically.

### Development workflow

Binding repos reference `minigraf-ffi` as a published crate. For local development against unreleased core changes, add a `[patch.crates-io]` override in the local `Cargo.toml` pointing to a local path — gitignored, never committed.

---

## Migration Steps (Phase 1)

Each binding repo follows this playbook:

1. **Create repo** under `project-minigraf` org — public, MIT OR Apache-2.0, no template
2. **Copy files** from monorepo at the split-point commit — clean initial commit, no history
3. **Update manifests** — replace path deps on `minigraf`/`minigraf-ffi` with published crate versions
4. **Move CI workflows** — copy relevant `*-ci.yml` and `*-release.yml`, update paths, add `repository_dispatch` receiver job
5. **Delete from monorepo** — remove directory and its CI workflows, update `Cargo.toml` workspace `members`
6. **Update cross-references** — `README.md`, `ROADMAP.md`, `CHANGELOG.md` in monorepo; each new repo's `README.md` links back to core

### Order within Phase 1

1. **Publish `minigraf-ffi`**: remove `publish = false` from `minigraf-ffi/Cargo.toml`, dry-run, publish — must happen before any binding repo can depend on it
2. **`minigraf-python`**: simplest (pure Python wrapper over UniFFI-generated code)
3. **`minigraf-node`**: NAPI-RS build with slightly more complex CI matrix
4. **`minigraf-wasm`**: covers both `minigraf-wasm/` and `minigraf-wasi/` into one repo

---

## Decisions Log

| Decision | Choice | Reason |
|---|---|---|
| Sync strategy | `repository_dispatch` fan-out from core | Automated, lockstep versions, no polling lag, each repo independently releasable |
| Git history | Clean start | Simpler migration; `git filter-repo` complexity not worth it for a hobby project |
| `minigraf-ffi` publishing | Publish to crates.io | Clean dep story; binding repos can't use path deps once split |
| Migration scope | Phased — Python, Node, WASM first | WASM + Node have the most CI churn; Android/Swift/Java deferred |
| `minigraf-c` | Stays in monorepo | No independent package registry; low CI noise |
| Cascade architecture | Direct fan-out (not polling, not orchestrator repo) | Simple, auditable, no shared infrastructure needed |
| Binding versioning | Lockstep with core | Clear correspondence for users; no independent versioning |
