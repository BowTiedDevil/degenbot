## Architecture

`degenbot` has migrated from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

**Rust is the engine; Python is a driver shell, not a co-implementation.**

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY without involving Python. That core must own **everything** a functional MEV bot needs. The Rust core must be capable of performing every action the bot requires.
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

## Concurrent Work Coordination
Check for other agents working concurrently before you begin work and any time you notice any edits, files, or changes in the working tree that are unrelated to your work. Use `/skill:pi-intercom` for both proactive checks and responding to messages.

## Build and test through `just`

Every build and test command goes through the justfile. **Never hand-type `cargo test`, `cargo build`, `maturin develop`, or `uv run pytest`.** The recipes are the only commands that carry the full contract — `--locked`, the Python libdir export for the libpython-linking harnesses, the canonical feature aliases, `RUSTFLAGS`, the nextest profile, the pytest marker filters — and they are what CI and the hooks run. A hand-typed equivalent silently drops part of that contract, so its result does not mean what a green recipe means.

- **Run everything, not a slice.** `just test` is the default gate: the standalone smoke plus the whole cargo workspace suite under nextest plus the full pytest suite, in one command. Run the full suites rather than picking files: they surface cross-module breakage (signature changes rippling into dependent crates, PyO3 seam drift, registry/vendor fallout) immediately. The nextest track is fast (~20s warm wall) and the full pytest suite is fast too, so there is no cost argument for a scoped run before you are done.
- **Build through the recipes.** `just dev` (or `just rebuild-if-stale` after Rust edits) is the sole editable-extension build path; `just build-rust-extension` builds the release-equivalent extension set. A bare `cargo build` or `maturin develop` can serve a stale `.so` or the wrong feature set — verify with `just verify-build-fresh`.
- **Tight red-green loops only.** Inside a tight loop you may narrow with the recipe's argument forwarding — `just test-rust-nextest -p <crate>` or `just test-rust-nextest -E 'test(name)'` — and then re-run `just test` whole before declaring work done. Narrow Rust runs still go through the recipe, not `cargo test`.
- **Before pushing**, check the gates with `just pre-push`, which mirrors the prek pre-push hook in order and fail-fast.

## Backwards Compatibility
Design standalone features without a backwards compatibility layer. Add a feature flag to allow parallel implementations if necessary, followed by a hard cutover and flag removal.

## Planning
Use `ergo` for all planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use red/green test-driven development when refactoring and adding new features. Use `/skill:tdd` for guidelines.

## Complex System State
Prefer enum-based finite state machines to manage transitions within systems. When you encounter an existing system with ad-hoc rules and detailed comments meant to clarify complex interactions, propose a refactor to encapsulate that logic into a state machine.

## Comment Hygiene
Comments carry the *why* only if it outlives its lookup: no task/epic IDs (commits carry those), no "RED/merged/post-fix" narration, no refactor provenance. Sequencing rules belong in types, acceptance criteria in named tests, history in ADRs. Full rules and the first-home test: `docs/comments.md`.

## Web
Use `agent-browser`.

## Dispatched-agent lane rules
Workers arriving here by dispatch (one-shot agents, actors) follow `/skill:dispatched-agent` before touching anything: never commit or push; run only scoped gates (`just test-rust-nextest -p <crate>`, the pytest files you touched); run commands foreground and mark detached gates PENDING in the sign-off; and leave unfamiliar tree modifications untouched.

## Formatting and commit staging

Run `cargo fmt` only through the justfile recipes (`just fmt-check`) — a bare
`cargo fmt` on PATH may bind a rustfmt that disagrees with the pinned toolchain, and
the pre-commit formatter is the authority. Commit with the whole tree staged
(`git add -A` before `git commit`): partial staging makes the pre-commit stash dance
conflict with hook-side formatting changes and the commit aborts. The drift-gate and
negative-probe idioms govern every machine-emitted artifact (see GLOSSARY.md).

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

### One `degenbot` console, two invocation paths

Two executables named `degenbot` exist, and they are one console by construction.
The native path is `cargo run --locked --manifest-path rust/Cargo.toml -p
degenbot-cli -- <args>` (or the built `rust/target/debug/degenbot`); the Python
path is `uv run --no-sync degenbot`, the venv shim from `[project.scripts]`
(`degenbot = "degenbot._cli:main"`). Both are thin callers of the single
composition root `degenbot_cli::run_args` over the one clap tree, so identical
output and exit codes are structural, not a convention held by hand. maturin's
`manifest-path` points only at the cdylib crate, so the wheel never installs the
binary — the venv name can only ever be the shim. Pick the native path for a
fast standalone CLI invocation; pick the shim when the call must start from
installed Python state, because its module init has already installed the typed
config and the Rust→Python log forwarder, while the native path installs both
via `sinks::boot()`. Both are supported; neither is deprecated.

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
- `just check-rust-dev-features`: the binding manifest's canonical
  `dev-features` alias (extension-module, hotpath, Prometheus hotpath,
  solver hotpath, allocator control, OTel, and mimalloc). The alias is
  intentionally outside the package defaults.
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
`just check-rust-dev-features`, `just check-rust-all-features`, `just dev`, and
`just test-hotpath` set `RUSTFLAGS=--cfg tokio_unstable` on those hotpath build
paths. Direct hotpath Cargo/maturin builds must set the same environment
explicitly — use `just test-hotpath` instead of hand-typing it.

## Rust test scope

For whole-workspace runs, use `just test-rust-nextest` — never a hand-typed `cargo test`. It runs the identical workspace artifacts under nextest's process-per-test scheduler (measured ~20s warm wall vs ~90s for `cargo test --workspace`) and records per-test durations as JUnit XML at `rust/target/nextest/ci/nextest-ci.junit.xml`; rank slowest tests with `just test-timing [top]`. The recipe pins `--locked`, exports the Python libdir the libpython-linking harnesses need, and sets `--no-fail-fast` because a tree-wide rebuild advances `.build-number` after degenbot-cli embeds it — the receipt gate fails once on the next compile, and one such failure must not abort the run. `just test-rust` (cargo) remains the canonical gate before declaring work done: nextest skips doc-tests, and CI runs cargo. Per-crate narrowing inside a tight red-green loop goes through the recipe too — `just test-rust-nextest -p <crate>` (or `-E 'test(name)'`) — but re-run the whole workspace suite through the recipe before declaring work done.

Why: resolver v3 unifies features for `--workspace`, so its artifacts are the one warm, canonical set in `rust/target`. A `-p <crate>` selection unifies features differently (core crates lose the `pyo3` feature the binding layer enables; dep features like tokio's shrink), so cargo stores a second rlib set under different metadata hashes — alternating between the two rebuilds shared dependencies on every shared edit (measured: `-p degenbot-simulation` recompiled 6 just-built crates in ~1m; a leaf crate pays nothing). The workspace run also executes every crate's suite, catching cross-crate fallout (signature changes rippling into dependents, e.g. examples/settlement_bot) that a scoped run never sees.

## Python test scope

The canonical Python gate is `just test-python` (CI and the pre-push hook run it directly) — never a hand-typed `uv run pytest`. Prefer the whole suite over selecting individual test files: the full run is fast and it is the run that surfaces seam and cross-module regressions. The suite carries pytest-timeout so a hung test fails with a timeout report instead of parking its xdist worker (timeouts and marker filters are configured in `tests/conftest.py`). The ordering-sensitive suites (`tests/arbitrage/test_arbitrage_session.py`, `tests/operator/test_operator_channel.py`) have a repeat-run gate: `just test-flake-probe` runs them three consecutive times with `-p no:cacheprovider` and fails on any failing run.

## Build-Artifact Housekeeping

`rust/target` holds several independently rebuildable cache families (normal
Cargo, maturin, coverage, LLVM coverage, Criterion, wheels, rustdoc). The
family policy lives in `scripts/gc-target.sh`; `just gc-target` reports and
sweeps them per-family. Preview before cleanup:

```bash
DRY_RUN=1 just gc-target        # report candidates; delete nothing
AGE=7 just gc-target            # destructive seven-day sweep
```

`AGE=N` uses `find -mtime +N`, so `AGE=0` means older than one day rather than
literally every file. `target/maturin` and the repository-root `.build-number`
receipt are always outside the deletion boundary.

## Rebuilding the Rust `.so` after edits
Maturin and Cargo can serve cached artifacts, so an apparently successful
rebuild can still ship a **stale `.so`**. Verify with the build receipt rather
than inferring freshness from command output:

```
just rebuild-if-stale
```

The recipe checks the installed extension against current sources and runs
`just dev` only when stale. Only trust a bot run (or a pytest suite) once the
check exits 0. The receipt mechanics and failure modes: `/skill:rust-rebuild`.

The console also self-reports at startup: a `console may be stale` warning on
stderr means the loaded extension's build identity no longer matches the repo
— stop, `just dev`, re-run the command. Never continue a drive past it: a
stale console's observations are untrusted. A warning that survives a clean
rebuild is a defect to file, not a signal to rebuild again.

## Python Environment
Use `uv`.

## Local node identity

The node this environment points at is **reth**, not anvil. Identify a node by
asking it, never by which binary is installed: `just node-identity`. A locally
spawned anvil is a separate node, and anvil fails closed on both `eth_callMany`
bundle shapes, so nothing would ever be submitted against it. The measured
shape matrix and sim-gate implications: `/skill:node-identity`.
