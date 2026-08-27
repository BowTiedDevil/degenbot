## Architecture

`degenbot` has migrated from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY without involving Python. That core must own **everything** a functional MEV bot needs. The Rust core must be capable of performing every action the bot requires. **Rust is the engine; Python is a driver shell, not a co-implementation.**
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

## Backwards Compatibility
Design standalone features without a backwards compatibility layer. Implement add a feature flag to allow parallel implementations if necessary, followed by a hard cutover.

## Planning
Use `ergo` for all planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use red/green test-driven development when refactoring and adding new features. Use `/skill:tdd` for guidelines.

## Complex System State
Prefer enum-based finite state machines to manage transitions within systems. When you encounter an existing system with ad-hoc rules and detailed comments meant to clarify complex interactions, propose a refactor to encapsulate that logic into a state machine.

## Commands
See the justfile.

## Python Environment
Use `uv`.

## Profiling
Many performance critical Rust modules are instrumented with `#[hotpath::measure]` attributes and `hotpath::measure_block!` phase probes.

`hotpath` is a non-optional dependency of certain crates with `default-features = false`. `hotpath` macros resolve to **no-op stubs unless the `hotpath` Cargo feature is enabled**.

**Dev:** the `[tool.maturin] features` list in `pyproject.toml` compiles `degenbot-bot/hotpath` into every dev `uv sync` build, so the dev `.so` always has it. Profiling is toggled at runtime by the `DEGENBOT_HOTPATH_*` env vars — **no rebuild is required to profile**:
```bash
DEGENBOT_HOTPATH=1 \
HOTPATH_SHUTDOWN_MS=300000 \
HOTPATH_OUTPUT_PATH=hp.json \
HOTPATH_OUTPUT_FORMAT=json \
HOTPATH_REPORT=functions-timing,threads \
uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py
```

`HOTPATH_SHUTDOWN_MS` forces a clean timed report from the long-running bot (the guard otherwise only drops at pump exit). For a live TUI view instead of a static report: `cargo install hotpath --features=tui` then `hotpath console` in another terminal while the bot runs.

`DEGENBOT_HOTPATH=1` is an **opt-in runtime gate** (not a build gate): without it no guard is constructed, so the singleton-guard invariant can't be tripped by default runs, tests, or a Python process hosting multiple bots. Set it to construct the guard; leave it unset to run uninstrumented.

## OTel Spans (Python-driven path)
- The `degenbot-python` global tracing subscriber can export OTLP spans (epic `KDUED5`): Rust-core span sources (e.g. `degenbot.pump.block` around the pump drain loop) flow through the same subscriber that forwards records to Python `logging`, so one registry carries logs + trace context.
- **Dev-only, like `hotpath`.** The `otel` entry in the `features` block of `[tool.maturin]` compiles the layer into every dev `uv sync` build. The PyPI `maturin-action` passes its own `--features pyo3/extension-module` list, which overrides it, so release wheels ship without the OTLP client footprint.
- **Default-on in dev; opt out with `DEGENBOT_OTEL=0`.** Because the code only exists under the dev-only feature, the runtime gate defaults to enabled — no env var needed for local runs. `DEGENBOT_OTEL=0` disables it explicitly.
- **Import-time gate.** Read in pymodule init (the one justified implicit call site). Endpoint precedence:
  `OTEL_EXPORTER_OTLP_ENDPOINT` env var > `otel.endpoint` in `~/.config/degenbot/config.toml` > exporter default (`http://localhost:4318`). A Prometheus scrape endpoint also starts on `DEGENBOT_METRICS_ADDR` (default loopback 9464) whenever the layer is active.
- **Fail-open.** If the OTLP exporter build fails, the layer logs a warning and the bot continues on the no-otel assembly (byte-equivalent to the historical subscriber).

### Schema ownership & Alembic retention (see [ADR-010](docs/adr/ADR-010-alembic-retention-and-rust-schema-cutover.md))
The database schema is **Alembic-owned during the 0.6.x point releases** and becomes **Rust-owned** in a 0.7 release. The cutover mechanism (`degenbot database cutover` + the `ensure_schema` `RustOwned` branch) is built and opt-in during 0.6.x so `pip` users can upgrade a stale database through the final Alembic revision and then cutover at a time of their choosing. Dropping the Alembic dependency and deleting the migration scripts is gated to 0.7 (ergo task `JFFQV2`).

**Forbidden-until-0.7 kill list.** No change before the 0.7 retirement task may delete or stub any of:
- `src/degenbot/migrations/` (the Alembic migration scripts) — deletion is gated on the `heal` operation shipping and being proven (epic `TGIP5N`, tasks T2-T5; see ADR-011) **and** the 0.7.0 release decision (T6 / `OXKANZ`), not just on the 0.7.0 version bump;
- the `alembic` and `sqlalchemy` entries in `pyproject.toml`;
- `DatabaseSessionManager` and the SQLAlchemy `src/degenbot/database/models/` package;
- the `ALEMBIC_HEAD` constant in `rust/crates/degenbot-db/src/schema.rs`;
- the `alembic_version`-reading branch of `rust/crates/degenbot-db/src/migrate.rs::ensure_schema`;
- the `PRAGMA query_only=on` setting on the `AlembicCurrent` path in `DegenbotDb::open`.