# CLI DB I/O migration to the Rust core (Alembic retained through 0.6.x)

Migrate the remaining CLI database I/O — the `exchange activate/deactivate`
write path — to route through the Rust core (`degenbot-db` + PyO3 seams),
and build + prove the schema-cutover mechanism that lets a production
Alembic-stamped DB cross from Alembic ownership into Rust ownership. Alembic
and `src/degenbot/migrations/` are **retained through the 0.6.x point
releases** so that `pip` users can upgrade a stale database through the final
Alembic revision; dropping the Alembic dependency and retiring the legacy
conversion is gated to a **0.7 release** (tracked as a blocked placeholder
task here, executed in a later epic).

## Non-goals

- **Aave CLI write path** (`aave extract`, activate, the `db_*` helpers in
  `src/degenbot/cli/aave.py`). Scoped to a separate epic — it needs new
  Aave-row Rust substrate that does not exist yet.
- **Aave CLI reads** (`position show` / `market show` / `risk`), which already
  delegate to the existing `PyDatabasePositionQuery` read seam, stay as-is.
- **`pool update` residual ORM reads** (`active_chains`, `active_exchanges`
  SELECT, the V4 `pool_manager_in_db` lookup). These are loop-orchestration
  reads that do not cross the staleness boundary fixed by the
  `db_fetch_exchange` ground-truth readback. Disposition: **stays-python**.
  Do not "migrate" them — they are not on the critical write path.
- **0.7 retirement execution.** The actual deletion of the Alembic
  dependency, `src/degenbot/migrations/`, the `ALEMBIC_HEAD` constant, and
  the `alembic_version`-reading branch of `ensure_schema` happens in 0.7,
  not in this epic. This epic only *builds and proves* the cutover path.

## Constraints

- **Alembic must stay live and shippable through 0.6.x.** No task in this
  epic (other than the 0.7-gated placeholder) may delete or stub
  `src/degenbot/migrations/`, remove the `alembic` or `sqlalchemy`
  dependency from `pyproject.toml`, delete `DatabaseSessionManager`, or
  remove the `alembic_version`-reading branch of `migrate.rs::ensure_schema`.
- **The Rust core never writes schema to an Alembic-stamped DB.** The
  `query_only=on` invariant on the `AlembicCurrent` path is preserved.
- **The cutover is opt-in during 0.6.x** via an explicit
  `degenbot database cutover` command. Automatic cutover-on-open is a 0.7
  concern, not built here. Rationale: an unannounced schema-ownership flip
  on first open is a surprising, hard-to-rollback action for a production
  DB; an explicit command lets users cutover deliberately, and lets the
  pytest exercise the full boundary deterministically.
- **Standalone-Rust constraint (AGENTS.md).** Anything a
  `cargo add degenbot` consumer needs to build an MEV bot must live in a
  core crate. The `upsert_exchange` / `set_exchange_active` /
  `upsert_pool_manager` / `convert_alembic_to_rust_owned` functions belong
  in `degenbot-db`, not the PyO3 wrapper.

## Key decisions (resolved during planning)

1. **Aave write path excluded** — separate epic (new Aave Rust substrate).
2. **Two end-states, version-gated.** During 0.6.x the DB stays
   Alembic-stamped; the Rust core reads Alembic-owned tables via
   `query_only=on`. The 0.7 release retires Alembic (drops dep +
   `migrations/` + `ALEMBIC_HEAD` + the `alembic_version` branch) after a
   production cutover release has given every pip user a path across the
   boundary.
3. **Cutover is opt-in via `degenbot database cutover`** during 0.6.x (see
   Constraints). The pytest proves the full boundary; production execution
   is a human decision.
4. **Documentation surfaces: ADR-010, top-level `AGENTS.md`, the epic body
   you are reading, and a pytest that migrates a stale Alembic-tagged DB
   through the cutover boundary.**

## Forbidden-until-0.7 kill list (guard against premature removal)

No task before the 0.7-gated retirement placeholder may touch any of:

- `src/degenbot/migrations/` (the Alembic migration scripts)
- the `alembic` and `sqlalchemy` entries in `pyproject.toml`
  `[project.dependencies]`
- `DatabaseSessionManager` and the SQLAlchemy `models/` package
- the `ALEMBIC_HEAD` constant in `rust/crates/degenbot-db/src/schema.rs`
- the `alembic_version`-reading branch of `migrate.rs::ensure_schema`
- the `DegenbotDb::open` `query_only=on` setting on the `AlembicCurrent` path

An implementing agent that sees these imports fall out of use mid-epic MUST
leave them in place and note them in the task completion summary; removal is
exclusively the 0.7 retirement task's responsibility.