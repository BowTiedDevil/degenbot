## Architecture

`degenbot` is a Rust core of standalone crates with two equally first-class consumers. **Rust is the engine; Python is a driver shell, not a co-implementation.**

1. **Pure-Rust MEV bot.** `cargo add degenbot` (the umbrella crate re-exporting the cores) must yield a fully functional MEV bot using Rust components only — the core owns **everything** a functional MEV bot needs.

2. **Python-driven MEV bot.** The Python interface is a thin PyO3 **driver** over the same Rust core, translating Python calls into Rust calls.

## Concurrent Work Coordination

Check for other agents working concurrently before you begin work and any time you notice edits, files, or tree changes unrelated to your work. Do not revert or overwrite them.

pi-fabric announces peer root sessions via `<fabric-peers>` notices. Inside `fabric_exec`: `agents.peers()` / `agents.sessions()` (discover), `agents.steer({id, message})` / `agents.followUp({id, message})` (message now / after settle), `decisions.raise` + `decisions.wait` (blocking questions, answerable via `/fabric decisions`), `mesh.put` / `mesh.get` (compare-and-swap claims when two sessions must not touch the same files).

## Build and test through `just`

Every build and test command goes through the justfile. **Never hand-type `cargo test`, `cargo build`, `maturin develop`, or `uv run pytest`** — the recipes alone carry the full contract (locking, the Python libdir export, canonical feature aliases, `RUSTFLAGS`, the nextest profile, the pytest marker filters), and CI and the hooks run the recipes.

- `just test` is the default gate: the Rust lint leg first (fail fast), then standalone smoke, the whole cargo workspace suite under nextest, and the full pytest suite, in one command. Run everything, not a slice — both tracks are fast, and scoped runs hide cross-module breakage.
- Tight red-green loops may narrow through the recipe (`just test-rust-nextest -p <crate>`, `-E 'test(name)'`); re-run `just test` whole before declaring work done.
- `just test-python` (and `just test`) depends on `just dev`, the sole editable-extension build path, so the suite can never test a stale `.so`. After a Rust edit that a running bot must reflect, run `just dev`.
- Before pushing: `just pre-push` delegates to the pre-push stage; prek.toml's `stages = ["pre-push"]` entries are the one gate manifest (ADR-074).

## ADRs

New and modified ADRs live in `docs/adr/` and follow `docs/adr/README.md`. Update the decisions table there in the same change as any ADR creation or status/decision change — a stale row misroutes work (ADR-080). Keep the row's status terse and true to the ADR file's own status line.

## Backwards Compatibility

Design standalone features without a backwards compatibility layer. Add a feature flag to allow parallel implementations if necessary, followed by a hard cutover and flag removal.

## Retirement hygiene

Deleting code is the complete retirement (ADR-087). No retirement tripwires — no tests, gates, or prose whose purpose is keeping a deleted thing gone (`assert not hasattr` on retired names, retired-name greps, "do not revive"), for any surface. The compiler, the owning ADR, review, and git history are the guards. When an external consumer may still send a retired key or subcommand, fail loud at the boundary in present, testable error-path code. Enforced by the tripwire-ratchet commit gate (ADR-087 D4); live-invariant gates (no-pyo3-in-cores, FFI seam privacy, stub drift, push-gate manifest) watch present code and are not tripwires.

## Planning

Use `ergo` for all planning (`ergo --help`, `ergo quickstart`). Include detailed implementation and planning notes in the task body. The `result` key is only for tasks with a durable document deliverable — most tasks close with `{"state":"done"}` and no `result`; the merged diff is the deliverable.

## Refactoring & Feature Development

Use red/green test-driven development when refactoring and adding new features. Guidelines: `/skill:tdd`.

## Complex System State

Prefer enum-based finite state machines to manage transitions within systems. When you encounter an existing system with ad-hoc rules and detailed comments meant to clarify complex interactions, propose a refactor to encapsulate that logic into a state machine.

## Comment Hygiene

Comments carry the *why* only if it outlives its lookup: no task/epic IDs (commits carry those), no "RED/merged/post-fix" narration, no refactor provenance. Sequencing rules belong in types, acceptance criteria in named tests, history in ADRs. Full rules and the first-home test: `docs/comments.md`.

## Web

Use `agent-browser`.

## Dispatched-agent lane rules

Workers arriving by dispatch follow `/skill:dispatched-agent` before touching anything: never commit or push; run only scoped gates (`just test-rust-nextest -p <crate>`, the pytest files you touched); run commands foreground and mark detached gates PENDING in the sign-off; leave unfamiliar tree modifications untouched.

## Formatting and commit staging

Commit with the whole tree staged (`git add -A` before `git commit`): partial staging makes the pre-commit stash dance conflict with hook-side formatting changes and the commit aborts. Run `cargo fmt` only through the justfile (`just fmt-check`) — the pre-commit formatter is the authority. The drift-gate and negative-probe idioms govern every machine-emitted artifact (see GLOSSARY.md).

## Rust toolchain policy

`rust-toolchain.toml` pins development and release builds (1.98.1 with `clippy` and `rustfmt`); the workspace MSRV is Rust 1.97, checked by CI's `MSRV (Rust 1.97)` job (`just check-msrv` locally). Do not raise the MSRV without an explicit dependency-compatibility decision and an update to this policy. Gates certify tiers, not a toolchain identity (`scripts/toolchain_tier.sh`): a compliant toolchain never fails a gate on lint/format vintage; the strict layer (clippy `--deny warnings`, exact fmt) is CI's verdict.

## Rust workspace selection

Cargo commands without a package selector build the workspace's two pure-Rust entry points (`degenbot`, `degenbot-cli`); plain `cargo build` does not build the PyO3 `degenbot_rs` extension — build it through the recipes. Commands intended to cover every member retain an explicit `--workspace` selector.

Two executables named `degenbot` are one console by construction: the native path (`cargo run -p degenbot-cli`) and the Python venv shim (`uv run --no-sync degenbot`, from `[project.scripts]`), both thin callers of the single composition root `degenbot_cli::run_args` over the one clap tree. Pick the native path for a fast standalone invocation; pick the shim when the call must start from installed Python state. Both are supported; neither is deprecated. Reference: `docs/rust-cli.md`.

## Rust feature matrix

Rust validation is split into named lanes, not one `--all-features` gate; each recipe's comment states its intent:

- `just lint-rust-check` / `just check-rust-default` — default features, no `--all-features`.
- `just check-rust-consumer` — the pure-Rust umbrella and its examples, without the PyO3 binding crate.
- `just check-rust-binding-default` — `degenbot_rs`'s broad default domain surface, without `extension-module`.
- `just check-rust-dev-features` — the canonical `dev-features` alias, intentionally outside package defaults.
- `just check-rust-extension-release` / `just build-rust-extension` — the release-equivalent extension set.
- `just check-rust-all-features` — diagnostic only, never the default gate.

CI runs the default, consumer, binding-default, dev-only, and release checks before the all-features diagnostic.

## Rust test scope

Whole-workspace runs go through `just test-rust-nextest` — never a hand-typed `cargo test`; `just test-timing [top]` ranks slow tests from its JUnit report. `just test-rust` (cargo) remains the canonical gate before declaring work done: nextest skips doc-tests, and CI runs cargo. Narrowing goes through the recipe; re-run the whole workspace suite before declaring work done.

A failure that passes locally and fails in CI gets `just test-rust-ci` first. If it reproduces there, the bug is real; if not, the build config is the story, and the test asserts a layout-level fact it should not (ADR-072).

## Python test scope

The canonical Python gate is `just test-python` (CI and the pre-push hook run it directly) — never a hand-typed `uv run pytest`. Prefer the whole suite: it is fast, and it is the run that surfaces seam and cross-module regressions. pytest-timeout fails a hung test with a timeout report instead of parking its xdist worker (budget in `pyproject.toml`). The ordering-sensitive suites have a repeat-run gate: `just test-flake-probe` runs them three consecutive times and fails on any failing run.

## Fork/RPC-gated suites (`just test-fork`)

The one on-demand lane for the legs every offline gate deliberately deselects: the pytest live-RPC tiers and the Rust live revm fork-truth legs (the `#[ignore]`d `live_*` tests in degenbot-simulation). Fails closed unless a declared endpoint answers an `eth_chainId` probe; the devcontainer's reth satisfies both, and any reachable Ethereum endpoint works. Minutes of live-RPC work — never in `test`, pre-push, or CI.

## Build-Artifact Housekeeping

`rust/target` holds several independently rebuildable cache families; `just gc-target` reports and sweeps them per-family (`DRY_RUN=1` previews, `AGE=N` sets the age threshold) across both environment roots. A large `rust/target` is feature-variant fan-out, not a debuginfo problem; sweep after bursts of lane-switching. Family policy: `scripts/gc-target.sh`.

## Python Environment

Use `uv`. The `--no-sync` prefix is defined once as the justfile `py_run` variable; `just venv` prints the active venv path, `just py <cmd>` runs ad-hoc commands against it. Python linters run through `uvx` (ephemeral, latest); the exceptions (`ty`, `maturin`) need the project venv and stay on `py_run`. On the host, direnv loads a gitignored `.envrc` (template: `.envrc.example`) that redirects the venv, bytecode cache, and cargo target dir; the devcontainer sets equivalents in `containerEnv`.

## Node access

An archive-capable Ethereum mainnet node (reth) is ALWAYS available in this environment. The URIs come from the devcontainer's `containerEnv` (`TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`, WS on `:8546`), which overrides `tests.env` inside the container; a host `tests.env`'s `localhost:8545` form is the same node seen from the host. Never conclude "no node access" from tests.env alone, and never defer live-node work (golden-capture recording, probes, span reconnaissance) on that basis.

## Local node identity

The node this environment points at is **reth**, not anvil. Identify a node by asking it, never by which binary is installed: `just node-identity`. A locally spawned anvil is a separate node and fails closed on both `eth_callMany` bundle shapes, so nothing would ever be submitted against it. Measured shape matrix and sim-gate implications: `/skill:node-identity`.
