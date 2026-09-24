## Architecture

`degenbot` has migrated from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY without involving Python. That core must own **everything** a functional MEV bot needs. The Rust core must be capable of performing every action the bot requires. **Rust is the engine; Python is a driver shell, not a co-implementation.**
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

## Concurrent Work Coordination
Check for other agents working concurrently before you begin work and any time you notice any edits, files, or changes in the working tree that are unrelated to your work. Use `/skill:pi-intercom` for instructions on using the inter-agent communication system. If another agent sends you a message, use `/skill:pi-intercom` to learn how to respond.

## Backwards Compatibility
Design standalone features without a backwards compatibility layer. Implement add a feature flag to allow parallel implementations if necessary, followed by a hard cutover.

## Planning
Use `ergo` for all planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use red/green test-driven development when refactoring and adding new features. Use `/skill:tdd` for guidelines.

## Complex System State
Prefer enum-based finite state machines to manage transitions within systems. When you encounter an existing system with ad-hoc rules and detailed comments meant to clarify complex interactions, propose a refactor to encapsulate that logic into a state machine.

## Comment Hygiene
Comments carry the *why* only if it outlives its lookup: no task/epic IDs (commits carry those), no "RED/merged/post-fix" narration, no refactor provenance. Sequencing rules belong in types, acceptance criteria in named tests, history in ADRs. Full rules and the first-home test: `docs/comments.md`.

## Commands
See the justfile.

## Web
Use `agent-browser`.

## Rust toolchain policy

The repository-root `rust-toolchain.toml` pins local development and release
builds to Rust 1.98.1 with `clippy` and `rustfmt`. From the repository root,
run `rustup show active-toolchain` to verify the override or `just toolchain`
to print the active compiler and Cargo versions. The workspace MSRV remains
Rust 1.97; the `MSRV (Rust 1.97)` CI job runs
`cargo +1.97.0 check --manifest-path rust/Cargo.toml --workspace --all-targets --locked`.
Release workflows use the same 1.98.1 development channel rather than floating
`stable`. Do not raise the MSRV without an explicit dependency-compatibility
decision and an update to this policy.

## Rust workspace selection

Cargo commands without a package selector use the workspace's two pure-Rust
entry-point defaults: `degenbot` and `degenbot-cli`. Plain `cargo build` does not
build the PyO3 `degenbot_rs` extension or non-publishable sample crates. Build
the extension explicitly with
`cargo build --locked --manifest-path rust/Cargo.toml -p degenbot_rs --features extension-module`;
maturin is already pinned to that crate's manifest. Commands intended to cover
every member must retain an explicit `--workspace` selector.

## Rust build profiles

Local development uses the workspace `[profile.dev]` intentionally: `opt-level = 1`,
no LTO or stripping, and line-tables-only debug information. This favors
representative optimization over an unoptimized debug build while keeping
`cargo test`, `just dev`, and editable-install rebuilds substantially cheaper
than release. The development extension therefore exercises representative
optimization rather than an unoptimized debug build.

Release builds use the workspace `[profile.release]`: thin LTO, stripping, and
`codegen-units = 1` for the final extension link. The listed core-library
package overrides intentionally use `codegen-units = 16` to reduce compile time;
Cargo has no per-package LTO or strip override, and the final thin-LTO link
reconverges those units. Do not change these release values or the per-package
policy without an explicit compatibility and build-time decision.

## Rust feature matrix

Rust validation is split into named lanes rather than one `--all-features` gate:

- `just lint-rust-check` / `just check-rust-default`: workspace Clippy/check
  with each crate's declared default features and no `--all-features`.
- `just check-rust-consumer`: the pure-Rust `degenbot` umbrella and its
  standalone consumer examples, without the PyO3 binding crate.
- `just check-rust-binding-default`: `degenbot_rs` with its intentionally broad
  default domain surface, but without `extension-module`.
- `just check-rust-dev-features`: the exact development-wheel feature list:
  `extension-module`, `degenbot-bot/hotpath`,
  `degenbot-bot/hotpath-prometheus`, `degenbot-solvers/hotpath`,
  `degenbot-bot/allocator-ctrl`, `otel`, and `mimalloc`.
- `just check-rust-extension-release` / `just build-rust-extension`: the
  release-equivalent extension set, `extension-module` (forwarding to
  `pyo3/extension-module`) plus binding defaults. Release wheels use
  `maturin --release --features pyo3/extension-module` and exclude all
  development-only profiling, telemetry, allocator-control, and mimalloc
  features.
- `just check-rust-all-features`: exhaustive workspace compilation as an
  explicit diagnostic/secondary gate only. It is not the default gate because
  it enables test-only and mutually exclusive build variants.

CI runs the default, consumer, binding-default, development-only, and release
feature checks before the all-features diagnostic. The default gate therefore
remains meaningful even when the diagnostic is enabled.

Hotpath's full Tokio `RuntimeMetrics` getters are behind Tokio's unstable API.
The repository `.cargo/config.toml` intentionally has no global rustflag;
`just check-rust-dev-features`, `just check-rust-all-features`, and `just dev`
set `RUSTFLAGS=--cfg tokio_unstable` on those hotpath build paths. Direct hotpath
Cargo/maturin builds must provide the same environment explicitly, for example
`RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg tokio_unstable" cargo test -p degenbot-bot --features hotpath`.

## Rust test scope

Prefer the full-suite gate (`just test-rust`, or `cargo test --workspace --manifest-path rust/Cargo.toml`) to validate changes. Per-crate `cargo test -p <crate>` is fine inside a tight red-green loop, but re-run the workspace suite before declaring work done.

Why: resolver v3 unifies features for `--workspace`, so its artifacts are the one warm, canonical set in `rust/target`. A `-p <crate>` selection unifies features differently (core crates lose the `pyo3` feature the binding layer enables; dep features like tokio's shrink), so cargo stores a second rlib set under different metadata hashes — alternating between the two rebuilds shared dependencies on every shared edit (measured: `-p degenbot-simulation` recompiled 6 just-built crates in ~1m; a leaf crate pays nothing). The workspace run also executes every crate's suite, catching cross-crate fallout (signature changes rippling into dependents, e.g. examples/settlement_bot) that a scoped run never sees.

## Rebuilding the Rust `.so` after edits
`uv run maturin develop` and even `cargo clean -p <crate>` do **not** reliably force a from-source recompile of the PyO3 `.so` — maturin uses cached artifacts across different feature-flag hash variants and `uv sync` installs a pre-built wheel in milliseconds. An apparently successful rebuild (~0.3–6s compile, no errors) silently ships a **stale `.so`** that doesn't contain the changes. This has bitten multiple sessions.

The only reliable way to force the `.so` to pick up Rust source changes:

```bash
uv sync --reinstall-package degenbot
```

Workflow after any Rust edit — verify, don't guess:

1. `just verify-build-fresh`. Exit 0 ⇒ the installed extension already
   contains your edits; no rebuild needed.
2. Exit 1 ⇒ run the reinstall above, then verify again. Only trust a bot run
   (or a pytest suite) once the check exits 0.

### Verifying freshness with the build receipt

Do not trust a silent "successful" rebuild — verify it. Every compile of
`degenbot_rs` runs `rust/crates/degenbot-python/build.rs`, which fingerprints
the crate's sources **plus every sibling crate under `rust/crates` and the
workspace manifests / repo-root `.cargo` config that its build could link**
(via the shared `build_scan.rs` scanner, with per-file+per-tree
`cargo:rerun-if-changed` re-triggering — the pre-63a362961 build emitted
no rerun triggers, so dep-only edits never advanced the receipt and a
stale wheel could pass). It writes `<count> <fingerprint>` to a receipt file
(`.build-number`, gitignored, at the repo root), embedding both values in the
compiled library. The counter advances only when the fingerprint (source
content) changes, so test/feature-variant rebuilds never false-positive.

```bash
uv run --no-sync python -m degenbot.build_info   # exit 1 if stale
# or:
just verify-build-fresh
# or from Python:
from degenbot.build_info import verify_build_fresh; verify_build_fresh()
# raw values: degenbot._ffi.build_number() / degenbot._ffi.build_fingerprint()
```

The check compares the installed fingerprint against the repo receipt, so any
material built from different sources than the installed artifact (the
cached-wheel failure mode) is caught, and no-change recompiles stay fresh.
`pytest tests/test_build_info.py` gates on this too — a stale `.so` fails the
suite. The receipt lives outside `rust/target` so `cargo clean` and
`just gc-target` can never roll it back. After Rust edits expect the gate to
flag staleness until you rebuild the wheel (`uv sync --reinstall-package
degenbot`) — that is the detector working, so run the rebuild, not a skip. A
reported number of 0 (or a missing fingerprint) means `build.rs` did not run —
investigate before trusting the build.

## Python Environment
Use `uv`.