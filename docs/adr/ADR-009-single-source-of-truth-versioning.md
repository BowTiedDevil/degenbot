# ADR-009: Single-Source-of-Truth Versioning

**Status: accepted.** Implemented for the `rust/` workspace (13 crates + the
virtual workspace root) and the `degenbot` Python package built by maturin.

## Context

degenbot is now natively dual-language: a Python package (`degenbot`, built by
maturin from the `degenbot_rs` cdylib) over a Rust workspace (13 crates under
`rust/crates/`, with `rust/Cargo.toml` a pure virtual manifest — see ADR-005's
crate-split topology). The two halves carry the project's single version, but
prior to this ADR that version lived in **two unrelated, unsynchronized places**:

- `pyproject.toml` — `version = "0.6.0a3"` hardcoded under `[project]`.
- every Rust crate — `version = "0.0.0"` as a literal in each `Cargo.toml`,
  with the PyO3 binding crate (`degenbot_rs` in `crates/degenbot-python/`) carrying
  **no `version` field at all** (defaulting to `0.0.0`).

A release therefore touched `pyproject.toml` for the wheel and left the Rust
side at `0.0.0` — a drift the layout (mirroring Polars) was *positioned* to
prevent but never wired up to. There was no single edit that moved both
halves together, and no way to read "the project's version" without knowing
which half to ask.

## Decision

Adopt the **Polars model**: a single version literal at the Cargo workspace
root, inherited by every member crate (including the PyO3 binding crate), and
bridged into the Python wheel by maturin reading the binding crate's
(workspace-inherited) version. One version, one source of truth, one edit to
bump it.

### 1. Workspace package inheritance (Rust side)

`rust/Cargo.toml` (the virtual manifest) gains a `[workspace.package]` block
naming the shared defaults every member crate inherits:

```toml
[workspace.package]
version = "0.6.0-alpha.3"
edition = "2021"
license = "MIT"
publish = false
repository = "https://github.com/BowTiedDevil/degenbot"
authors = ["BowTiedDevil <devil@bowtieddevil.com>"]
```

Every member crate's `[package]` table then writes the inherited fields as
`<field>.workspace = true` instead of a literal:

```toml
[package]
name = "degenbot-core"
description = "..."
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true
repository.workspace = true
authors.workspace = true
```

The binding crate `crates/degenbot-python/` (package name `degenbot_rs`, the
cdylib maturin builds) **also** inherits `version.workspace = true` — *that*
is the version the published wheel ends up with.

There is now exactly **one** `version` literal in the entire Rust tree: the
line in `[workspace.package]`. A virtual manifest has no `[package]` section
of its own but may declare `[workspace.package]` defaults for its members.

### 2. Dynamic versioning bridging Cargo → pyproject (Python side)

`pyproject.toml` drops the static `version = "0.6.0a3"` from `[project]` and
declares the version dynamic:

```toml
[project]
name = "degenbot"
dynamic = ["version"]
# ... no `version =` line
```

maturin (the build backend, already configured via
`[tool.maturin] manifest-path = "rust/crates/degenbot-python/Cargo.toml"`)
supplies the wheel version at build time by reading the `[package].version` of
the crate it builds — that is `degenbot_rs`'s version, which is itself
inherited from `[workspace.package]`. The `[tool.maturin]` block is unchanged;
the inheritance wiring is what connects the bridge end-to-end.

### Semver ↔ PEP 440 normalization

Cargo versions use semver (`0.6.0-alpha.3`); PyPI/PEP 440 wants the
`0.6.0a3` form. **maturin normalizes the Cargo version into a PEP 440-compliant
one** when generating wheel metadata, so the single literal is written in
semver form (Cargo's native dialect) and the Python side receives the
conformant form automatically. The `0.6.0-alpha.3` literal therefore produces
a `0.6.0a3` wheel — no manual transliteration per release.

### Bumping

A version bump is now a single edit to `[workspace.package].version` in
`rust/Cargo.toml`. Both the Rust crates and the published Python wheel follow
it. `just version` prints the single source of truth (via `cargo metadata`)
for verification at any point in a release.

## Considered options (rejected alternatives)

- **Status quo (two literal versions, manual sync).** Keep `0.6.0a3` in
  `pyproject.toml` and `0.0.0` in the Rust crates. **Rejected**: the whole
  point of the Polars-modeled layout (ADR-005) is that Rust and Python are
  one project; a release that touches one half and leaves the other at a
  sentinel value is a drift bug waiting to happen, and there is no place to
  read "the" version from.

- **`cargo-workspaces` / external `cargo set-version` tooling.** Introduce a
  tool that walks every `Cargo.toml` and rewrites the literal. **Rejected**:
  duplicates the source of truth across 13 files, requires a tool invocation
  (not a text edit) to bump, and adds a build dependency. Workspace package
  inheritance is the Cargo-native mechanism for exactly this and needs no
  tooling — the literal lives in one place by construction.

- **`pyproject.toml` as the single source (Python → Rust).** Make maturin
  read the version from `pyproject.toml` and inject it into the Cargo build.
  **Rejected**: maturin's bridge runs Rust → Python (it reads the *crate's*
  version to set the wheel's), not the reverse. Inverting it would require a
  build-time Cargo → Python FFI step or a custom `build.rs` parsing TOML,
  fighting maturin's direction. The Rust side is the cleaner authority:
  the workspace root is a natural single point, and Cargo's inheritance +
  maturin's read-crate-version bridge compose without custom code.

- **`workspace = false` per-crate independent versions.** Let each crate
  carry its own semver (e.g. `degenbot-core` at `0.1.0`, `degenbot-rpc` at
  `0.2.0`), Crates.io-style. **Rejected**: the crates are
  `publish = false` and developed in lockstep as one project — independent
  per-crate versions would be bookkeeping with no consumer (nothing is
  published independently to crates.io today, and ADR-005 defers the standalone
  `cargo add` publish surface). When a crate graduates to independent
  publishing, *that* crate can opt out of `version.workspace = true` for an
  independent cadence without disturbing the rest (the inheritance is
  per-field, opt-out). Until then one shared version matches how the project
  is actually released.

## Consequences

- **One literal, one edit.** `[workspace.package].version` in
  `rust/Cargo.toml` is the project's version. Bumping it moves every Rust
  crate and the published Python wheel together; there is no second file to
  remember.
- **No per-crate `version =` lines anywhere.** All 13 member crates inherit;
  the binding crate (`degenbot_rs`) inherits the version maturin reads, so the
  wheel version is no longer a separate concern from the crate version.
- **`pyproject.toml` carries no static version.** `dynamic = ["version"]`
  under `[project]`; the version is supplied by maturin at build time. The
  single literal lives on the Rust side, because that is the side maturin's
  bridge reads *from* (crate → wheel), and a virtual workspace root is a
  natural single point.
- **Semver is the canonical dialect.** The literal is written as
  `0.6.0-alpha.3` (Cargo/semver); maturin's normalization produces the PEP 440
  `0.6.0a3` for PyPI. Release tooling and humans edit the semver form.
- **Per-crate independent publishing remains an opt-out, not a rewrite.** A
  crate that needs its own cadence drops `version.workspace = true` for a
  literal without touching the others — the inheritance is per-field. This
  composes with ADR-005's deferred standalone-publish surface: when
  `cargo add degenbot-core` ships, that crate may pin its own semver while
  the binding crate follows the workspace.

## Related

- **ADR-005** (Polars-inspired three-layer architecture) — establishes the
  crate-split topology this ADR versions. The workspace-virtual-manifest +
  `crates/degenbot-python` peer layout is ADR-005's; this ADR adds the
  versioning half of the same Polars model (Polars itself uses
  `[workspace.package]` + `version.workspace = true` across `polars-core`/
  `-plan`/`-python`/etc., with `dynamic = ["version"]` in `pyproject.toml`).
- **`rust/AGENTS.md`** — the workspace member list and virtual-manifest
  convention; `[workspace.package]` defaults apply to exactly those members.
- **`pyproject.toml` `[tool.maturin]`** — `manifest-path` points at the
  binding crate whose (inherited) version maturin reads. Unchanged by this
  ADR; the bridge is connected by the inheritance wiring in that crate's
  `Cargo.toml`.