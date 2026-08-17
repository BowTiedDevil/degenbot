# AGENTS.md

## Architectural Vision

**Long-term goal: a set of first-class standalone Rust crates that together form a complete, functional MEV bot.**

degenbot is migrating from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY — event decoding, pool state, solvers, pump loop, swap encoding, the lot. No Python in the build, no Python at runtime.
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

The two consumers share one Rust core, and that core must own **everything** a functional MEV bot needs: pool/token state, swap math, event decoding, solvers, the pump loop, swap encoding, and the supporting infrastructure — the database (persistence, not just ORM calls), RPC interaction, pub-sub, price oracles, DB-aware pool and lending-market updaters, simulation, and transaction submission. The Rust core must be capable of performing **every action the bot requires**, driven either by a Rust consumer directly or by a Python interface shell that instructs the Rust core to do them. The framing is: **Rust is the engine; Python is a driver shell, not a co-implementation.**

## Backwards Compatibility
Unless directed otherwise, design standalone features without a backwards compatibility layer. You may add a feature flag to allow toggling parallel implementations and a hard cutover.

## Planning
Use `ergo` for all feature planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use red/green test-driven development per the `tdd` skill when refactoring and adding new features.

## Commands
See the justfile.

## Python Environment
Use `uv`.

## Profiling
The `degenbot-bot` drain path (BlockPump → SolveCoordinator → EngineHandle) is instrumented with `#[hotpath::measure]` attributes and `hotpath::measure_block!` phase probes; the guard lifecycle lives in `rust/crates/degenbot-bot/src/profiling.rs`. hotpath is a non-optional dependency of `degenbot-bot` with `default-features = false`, so the macros resolve to **no-op stubs unless the `hotpath` Cargo feature is enabled** — zero compile-time or runtime cost in default builds, and the no-pyo3-in-cores invariant is unaffected (hotpath pulls no pyo3).

**Dev:** the `[tool.maturin] features` list in `pyproject.toml` compiles `degenbot-bot/hotpath` into every dev `uv sync` build, so the dev `.so` always has it. Profiling is then toggled at runtime by an env var — **no rebuild when you want to profile**:

```bash
DEGENBOT_HOTPATH=1 \
HOTPATH_SHUTDOWN_MS=300000 \
HOTPATH_OUTPUT_PATH=hp.json \
HOTPATH_OUTPUT_FORMAT=json \
HOTPATH_REPORT=functions-timing,threads \
uv run python examples/eth_backrun_v2_v3_v4_rust.py
```

`HOTPATH_SHUTDOWN_MS` forces a clean timed report from the long-running bot (the guard otherwise only drops at pump exit). For a live TUI view instead of a static report: `cargo install hotpath --features=tui` then `hotpath console` in another terminal while the bot runs.

`DEGENBOT_HOTPATH=1` is an **opt-in runtime gate** (not a build gate): without it no guard is constructed, so the singleton-guard invariant can't be tripped by default runs, tests, or a Python process hosting multiple bots. Set it to construct the guard; leave it unset to run uninstrumented.

**CI/CD:** release wheels exclude hotpath. The PyPI `maturin-action` passes `--features pyo3/extension-module`, which **overrides** the dev `[tool.maturin] features` list (verified empirically: the cargo invocation shows only `pyo3/extension-module`, no hotpath), so the shipped wheel's macros are no-op `lib_off` stubs with zero runtime penalty. CI's `just build-rust-extension` (`cargo build --release -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml`) is already hotpath-free for the same reason.

**Extending:** the pattern for new instrumentation is `#[hotpath::measure]` on a function (`impl_type = "Type"` for inherent methods, `label = "..."` for trait impls), or `hotpath::measure_block!("phase_name", { ... })` for sub-function phases. They're no-ops unless `hotpath` Cargo feature + `DEGENBOT_HOTPATH=1` are both on, so sprinkle liberally — same discipline as `log::debug!`. To widen coverage to a library crate, add the crate as a non-optional dep with `default-features = false` and gate the real `hotpath/hotpath` feature behind a Cargo feature on that crate (see `degenbot-bot/Cargo.toml` for the pattern).

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