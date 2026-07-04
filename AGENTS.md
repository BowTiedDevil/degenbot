# AGENTS.md

## Coordinating With Other Agents
When you begin a session, Read the 'pi-link-coordination' skill. Then use `link_list` to identify all agents working in this project. Send messages with `link_send` with `triggerTurn:true` to coordinate concurrent work and avoid making contradictory changes. Respond **directly** to other agents using `link_send` with `triggerTurn:true`.

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

**Important**: The Rust extension is rebuilt automatically by **uv** (not maturin) whenever you run an `uv run ...` command. There is no import-time rebuild hook: maturin's editable install is a one-time build-and-place, and the `.so` is loaded straight from `src/degenbot/degenbot_rs.abi3.so`. What keeps it fresh is the `[tool.uv] cache-keys` table in `pyproject.toml`, which watches `rust/**/Cargo.toml` and `rust/crates/*/src/**/*.rs`; when any of those is newer than the installed build, uv marks the package "installed, but not fresh" and rebuilds via maturin on the next `uv run` sync.

Prerequisite: the editable install's `.pth` must point at the live repo (`/workspaces/degenbot/src`). The devcontainer guarantees this — `UV_PROJECT_ENVIRONMENT` points at a container-local venv and `post-create.sh` runs `uv sync` to seed the editable install. Do NOT manually rebuild with `cargo build` (it produces an `.rlib`, not the abi3 `.so` uv loads) or recreate the virtual environment after making Rust code changes.

Recovery: if the `.so` ever goes stale (e.g. a venv copied from another machine whose `.pth` points at a dead path), force a clean rebuild:
`uv sync --reinstall-package degenbot`

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run lint and type checks (Rust + Python)
- `just format` - Run formatters (Rust + Python)

## Git Commits
Commit messages must follow the project convention enforced by `commitlint`. Git hooks are managed by [`prek`](https://prek.j178.dev/) and declared in [`prek.toml`](prek.toml). Run `just setup-git-hooks` once after cloning to install the hooks and the editor template. The hooks are:

- **`pre-commit`** — staged-file Markdown lint + `# noqa: PLC0415` guard (fast, file-scoped).
- **`commit-msg`** — commitlint against `.commitlintrc.yml`.
- **`pre-push`** — two concerns, both running strictly earlier than GitHub Actions:
  1. **commit-message re-lint** of the outgoing push range (safety net for `git commit --no-verify`); see `scripts/hooks/commitlint-push.sh`.
  2. **CI mirror** — every lint, build, and test the `ci.yml` workflow runs, in the same job order, on the full repo. The lint hooks use the check-only `*-check` just recipes (no `--fix`) so they cannot dirty committed files and are stricter than CI. Bypass with `git push --no-verify` (CI still runs), or re-run a subset with `prek run --hook-stage pre-push --skip rust-test` etc.

Run hooks on demand with `uv run prek run` / `uv run prek run --all-files`. For manual commit-message range checks: `just lint-commits` (default: unpushed commits) or `just lint-commits main..HEAD`.

## Backwards Compatibility
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Architecture & Domain Knowledge
**Start with the [Architectural Vision](#architectural-vision) above** — it states the long-term goal and the canonical references for the three-layer architecture. This section is the index into the remaining focused docs; read the relevant one before naming, editing, or extending a module.
- **[`CONTEXT-MAP.md`](CONTEXT-MAP.md)** — ubiquitous-language index + per-module `CONTEXT.md` pointers. Read the relevant module context before naming variables, classes, or docstrings.
- **[ADR records](docs/adr/)** — ADR-001 I/O-free pools, ADR-002 pool-type registry singleton, ADR-003 Bot as state owner, ADR-004 CL tickmap typed boundary, ADR-005 Polars-inspired three-layer FFI, ADR-006 per-chain bot orchestrator, ADR-007 pool unregister seam, ADR-008 block state machine, ADR-009 single-source-of-truth versioning, ADR-010 Alembic retention + Rust schema cutover, ADR-011 Auto-healed Alembic retirement (dump-and-restore cutover)
- **[`docs/architecture/`](docs/architecture/)** — long-form architecture

### Schema ownership & Alembic retention (see [ADR-010](docs/adr/ADR-010-alembic-retention-and-rust-schema-cutover.md))

The database schema is **Alembic-owned during the 0.6.x point releases** and becomes **Rust-owned** in a 0.7 release. The cutover mechanism (`degenbot database cutover` + the `ensure_schema` `RustOwned` branch) is built and opt-in during 0.6.x so `pip` users can upgrade a stale database through the final Alembic revision and then cutover at a time of their choosing. Dropping the Alembic dependency and deleting the migration scripts is gated to 0.7 (ergo task `JFFQV2`).

**Forbidden-until-0.7 kill list.** No change before the 0.7 retirement task may delete or stub any of:

- `src/degenbot/migrations/` (the Alembic migration scripts) — deletion is gated on the `heal` operation shipping and being proven (epic `TGIP5N`, tasks T2-T5; see ADR-011) **and** the 0.7.0 release decision (T6 / `OXKANZ`), not just on the 0.7.0 version bump;
- the `alembic` and `sqlalchemy` entries in `pyproject.toml`;
- `DatabaseSessionManager` and the SQLAlchemy `src/degenbot/database/models/` package;
- the `ALEMBIC_HEAD` constant in `rust/crates/degenbot-db/src/schema.rs`;
- the `alembic_version`-reading branch of `rust/crates/degenbot-db/src/migrate.rs::ensure_schema`;
- the `PRAGMA query_only=on` setting on the `AlembicCurrent` path in `DegenbotDb::open`.

**An import falling out of use is not permission to delete it.** If a 0.6.x task makes an Alembic/SQLAlchemy symbol unused, leave it in place and note the orphaned symbol in the task completion summary; removal is the 0.7 retirement task's exclusive responsibility.
- **[`docs/migration-guides/`](docs/migration-guides/)** — completed refactors and the rubric for evaluating a Python module against the three-layer architecture
