# AGENTS.md

## Coordinating With Other Agents
When you begin a session, use `link_list` to check for other agents working in this project. Announce yourself with a brief description of your task using `link_send`. Send messages to coordinate concurrent work and avoid making contradictory changes.

## Architectural Vision

**Long-term goal: a set of first-class standalone Rust crates that together form a complete, functional MEV bot — no Python required.**

degenbot is migrating from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY — event decoding, pool state, solvers, pump loop, swap encoding, the lot. No Python in the build, no Python at runtime.
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

The two consumers share one Rust core, and that core must eventually own **everything** a functional MEV bot needs: pool/token state, swap math, event decoding, solvers, the pump loop, swap encoding, *and* the infrastructure currently still Python-only — the database (persistence, not just ORM calls), RPC interaction, pub-sub, price oracles, DB-aware pool and lending-market updaters, simulation, and transaction submission. There is no piece of bot functionality that lives in Python indefinitely. The end state is a Rust core that can do **every action the bot requires**, driven either by a Rust consumer directly or by a Python interface shell that instructs the Rust core to do them. The framing is: **Rust is the engine; Python is a driver shell, not a co-implementation.**

**Today many components are still pure Python** (database via SQLAlchemy, RPC via web3.py, publisher/subscriber, price oracles, the DB-aware pool and lending-market updaters, simulation, submission). These are *all* on the migration path — none of them is a permanent Python responsibility. They are migrated **one at a time**: each port moves a piece of responsibility into a Rust core crate and converts the corresponding Python from an implementation into a **delegating shell**. The first and canonical migration is the **Polars-inspired three-layer architecture** (ADR-005), whereby a user drives a Rust-owned `Bot` through a PyO3 wrapper that translates Python calls to the Rust core:

| Layer | Where it lives | Holds |
|-------|-----------------|-------|
| **Rust core** | `rust/crates/degenbot-{core,-cl-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot}` — **zero `pyo3`** (enforced by `just check-no-pyo3-in-cores`) | data + state-machine logic + pure math + protocols (DexIdentity, encoders, decoders) |
| **PyO3 wrapper** | `rust/crates/degenbot-python/src/<domain>/**` | `#[pyclass]`/`#[pyfunction]` only — arg extraction → GIL release → core call → result wrap. **No business logic.** |
| **Python companion** | `src/degenbot/**` | user-facing API, docstrings, I/O orchestration, immutable config dual-tracking, `Fraction`-based display |

The **standalone-Rust-core constraint** is first-class: anything a standalone Rust consumer (`examples/standalone_consumer.rs`, `cargo add degenbot`) would need to build an MEV bot must live in a core crate from day one — never "move it later," which strands it across the future crate boundary.

**Directive for all refactoring and feature work:** every change must align with this direction. When evaluating a module against the architecture, apply the triage rubric in [`docs/migration-guides/three-layer-transition.md`](docs/migration-guides/three-layer-transition.md) and choose one of four dispositions (`done` / `partial` / `port-now` / `stays-python`). Do not introduce a Python mirror of Rust-owned state, do not add `pyo3` to a core crate (outside a feature gate), do not strand standalone-usable logic on the Python side, and do not build a backwards-compatibility layer for retired implementations.

**Canonical references:**
- [ADR-005](docs/adr/ADR-005-polars-inspired-three-layer-architecture.md) — the three-layer FFI decision (read this before any FFI/state-ownership work)
- [ADR-003](docs/adr/ADR-003-botcore-state-layer.md) — `Bot` as the single Rust state owner
- [`docs/architecture/rust-owned-bot.md`](docs/architecture/rust-owned-bot.md) — component map + pump/engine lifecycle ("Rust is the engine, Python is the cockpit")
- [`docs/migration-guides/three-layer-transition.md`](docs/migration-guides/three-layer-transition.md) — the rubric for evaluating a Python module and moving its responsibility to Rust
- [`rust/AGENTS.md`](rust/AGENTS.md) — the generic three-layer rule, the Nine Rules, GIL discipline
- [`rust/CONTEXT.md`](rust/CONTEXT.md) — glossary; {Polars-Inspired Three-Layer Architecture}, {PyBot}, {PyLiquidityPool}, {PyErc20Token}

## Planning
Use `ergo` for all feature planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use Red/Green TDD while refactoring and implementing new features.

## Commands
Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is automatically rebuilt on import by maturin. Do NOT manually rebuild the extension, recreate the virtual environment, or reinstall the package after making Rust code changes. Any `uv run ...` command will trigger a rebuild if needed.

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run lint and type checks (Rust + Python)
- `just format` - Run formatters (Rust + Python)

## Git Commits
Commit messages must follow the project convention enforced by `commitlint`. Git hooks are managed by [`prek`](https://prek.j178.dev/) and declared in [`prek.toml`](prek.toml). Run `just setup-git-hooks` once after cloning to install the hooks (`pre-commit` Markdown lint + noqa guard, `commit-msg` commitlint, `pre-push` commit-message range re-lint as a safety net for `--no-verify` bypasses) and the editor template. Run hooks on demand with `uv run prek run` / `uv run prek run --all-files`. For manual commit-message range checks: `just lint-commits` (default: unpushed commits) or `just lint-commits main..HEAD`.

## Backwards Compatibility
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Architecture & Domain Knowledge
**Start with the [Architectural Vision](#architectural-vision) above** — it states the long-term goal and the canonical references for the three-layer architecture. This section is the index into the remaining focused docs; read the relevant one before naming, editing, or extending a module.
- **[`CONTEXT-MAP.md`](CONTEXT-MAP.md)** — ubiquitous-language index + per-module `CONTEXT.md` pointers. Read the relevant module context before naming variables, classes, or docstrings.
- **[ADR records](docs/adr/)** — ADR-001 I/O-free pools, ADR-002 pool-type registry singleton, ADR-003 Bot as state owner, ADR-004 CL tickmap typed boundary, ADR-005 Polars-inspired three-layer FFI, ADR-006 per-chain bot orchestrator, ADR-007 pool unregister seam, ADR-008 block state machine, ADR-009 single-source-of-truth versioning
- **[`docs/architecture/`](docs/architecture/)** — long-form architecture
- **[`docs/migration-guides/`](docs/migration-guides/)** — completed refactors and the rubric for evaluating a Python module against the three-layer architecture
