# ADR-010: Alembic Retention Through 0.6.x and Rust Schema Cutover

**Status: accepted.** The schema-cutover mechanism is to be built and made
opt-in during the 0.6.x point releases (epic `2Z3Y46`); retiring the Alembic
dependency and the legacy conversion is gated to a **0.7** release.

## Context

degenbot's database schema is, today, **Alembic-owned**: the canonical schema
lives in `src/degenbot/migrations/versions/`, `alembic_version` is the
authority table, and production databases are stamped at an Alembic revision.
The Rust core (`degenbot-db`) reads these databases but does **not** write
schema to them — `DegenbotDb::open` reports one of four states
(`migrate.rs::ensure_schema`):

- `AlembicCurrent` — `alembic_version.version_num == ALEMBIC_HEAD`. The Rust
  core honors `PRAGMA query_only=on` and reads.
- `AlembicStale { head, expected }` — older revision. Refuse; the Python
  Alembic path must advance the stamp.
- `FreshStandalone { schema_version }` — empty file. Apply the embedded
  `SCHEMA_HEAD` DDL, stamp `_degenbot_db_schema_version` with
  `RUST_SCHEMA_VERSION`.
- `Unrecognized` — has tables, no `alembic_version`. Refuse (a foreign SQLite
  file passed by mistake).

This hybrid period is deliberate (migrate.rs doc comment): the Rust core
never downgrades or forwards an Alembic DB. The end state, however, is that
the schema is **Rust-owned**: future schema bumps are Rust `ALTER` scripts
tracked by `RUST_SCHEMA_VERSION`, and the `alembic_version` table is gone.

The problem is that there is **no path from Alembic ownership to Rust
ownership**. A DB that has crossed the boundary (tables present,
`alembic_version` dropped, `_degenbot_db_schema_version` stamped) is — under
today's `ensure_schema` — indistinguishable from a foreign SQLite file:
`alembic_version` is absent, `table_count > 0`, so it returns `Unrecognized`
and `DegenbotDb::open` refuses. Worse, even a `FreshStandalone` DB created by
the Rust core cannot be **re-opened**: the second open sees no
`alembic_version`, `table_count > 0`, and also returns `Unrecognized`.

Closing this gap is not a local fix; it is a schema-ownership transition that
must be coordinated with releases, because `pip` users on production
Alembic-stamped databases must be able to upgrade through the **final** Alembic
revision before ownership flips. Dropping the Alembic dependency prematurely
strands those users on a database the Rust core refuses to open.

## Decision

Two end-states, **version-gated**, with the cutover mechanism built and
proven during 0.6.x and the Alembic retirement executed only in 0.7.

### 1. During 0.6.x — Alembic retained, cutover opt-in

- The DB stays Alembic-stamped by default; the Rust core continues to read
  via `query_only=on` on the `AlembicCurrent` path.
- The schema-cutover operation is built in `degenbot-db` and surfaced as an
  explicit, opt-in CLI command, `degenbot database cutover`. **Nothing
  auto-runs cutover on `DegenbotDb::open`.** Rationale: a one-way schema-
  ownership flip on first open is surprising and hard to roll back; an
  explicit command lets users cutover deliberately, and lets a pytest prove
  the boundary deterministically.
- `ensure_schema` gains a fifth state, `RustOwned { schema_version }`, for a
  DB with tables present, no `alembic_version`, **and** a stamped
  `_degenbot_db_schema_version`. This subsumes the re-opened `FreshStandalone`
  case (it now reads back as `RustOwned` rather than `Unrecognized`) and
  covers the post-cutover state. A foreign SQLite file (tables, no alembic,
  no `_degenbot_db_schema_version`) remains `Unrecognized`.
- `convert_alembic_to_rust_owned(conn)` is the one operation that writes
  schema to an otherwise-Alembic DB, and only because the DB is *leaving*
  Alembic ownership: verify `alembic_version.version_num == ALEMBIC_HEAD`,
  drop the `alembic_version` table, stamp `_degenbot_db_schema_version` with
  `RUST_SCHEMA_VERSION`. Refuse `AlembicStale` (upgrade via Alembic first)
  and `Unrecognized`.

### 2. The cutover state machine

`ensure_schema` decisions, enumerated exhaustively over the three observable
predicates (`tables_present?`, `alembic_version?`, `_degenbot_db_schema_version?`):

| tables | alembic_version | _degenbot_db_schema_version | state |
|---|---|---|---|
| 0 | — | — | `FreshStandalone` (apply DDL + stamp; returns `FreshStandalone`) |
| >0 | at `ALEMBIC_HEAD` | — | `AlembicCurrent` |
| >0 | older than head | — | `AlembicStale` |
| >0 | absent | present | `RustOwned` |
| >0 | absent | absent | `Unrecognized` |
| 0 | present | — | `Unrecognized` (alembic_version with no tables — foreign) |

The predicate `tables_present?` counts non-`sqlite_%` tables. The
`_degenbot_db_schema_version` table is private to the Rust core and is
*not* counted as a content table (it is filtered, like `sqlite_%`).

After `FreshStandalone` applies its DDL on first open, the **next** open of
that file reads back as `RustOwned` (tables present, no alembic, version table
stamped) — `FreshStandalone` is the one-shot "I just created this" report;
`RustOwned` is the steady state for any Rust-owned DB thereafter.

### 3. Several 0.6.x point releases ship the cutover path

The cutover command and the `RustOwned` branch ship across 0.6.x point
releases so that **every `pip` user can upgrade a stale database through the
final Alembic revision and then cutover** at a time of their choosing. No
0.6.x release removes Alembic or forces cutover.

### 4. 0.7 — retirement

The actual retirement is gated to a 0.7 release and tracked as the blocked
task `JFFQV2`. Only in 0.7 may an agent touch the forbidden-until-0.7 kill
list (below). The retirement:

- drops the `alembic` and (if nothing else uses it) `sqlalchemy`
  dependencies from `pyproject.toml`;
- deletes `src/degenbot/migrations/`;
- removes the `ALEMBIC_HEAD` constant in
  `rust/crates/degenbot-db/src/schema.rs` and the `alembic_version`-reading
  branch of `ensure_schema`;
- removes the `database upgrade` Alembic fall-back path (the
  `DatabaseSchemaStale` → `alembic.command.upgrade` shell);
- decides whether `DegenbotDb::open` auto-converts a stale-but-recognized
  legacy shape or refuses (resolved at 0.7 design time).

## Forbidden-until-0.7 kill list

No change before the 0.7 retirement task (`JFFQV2`) may delete or stub any of:

- `src/degenbot/migrations/` (the Alembic migration scripts);
- the `alembic` and `sqlalchemy` entries in `pyproject.toml`;
- `DatabaseSessionManager` and the SQLAlchemy `src/degenbot/database/models/`
  package;
- the `ALEMBIC_HEAD` constant in `rust/crates/degenbot-db/src/schema.rs`;
- the `alembic_version`-reading branch of
  `rust/crates/degenbot-db/src/migrate.rs::ensure_schema`;
- the `PRAGMA query_only=on` setting on the `AlembicCurrent` path in
  `DegenbotDb::open`.

**An import falling out of use is not permission to delete it.** If a 0.6.x
task makes an Alembic/SQLAlchemy symbol unused, the task leaves it in place
and notes the orphaned symbol in its completion summary; removal is the 0.7
retirement task's exclusive responsibility.

## Considered options (rejected alternatives)

- **Auto-cutover on open during 0.6.x.** Have `DegenbotDb::open` silently
  convert an `AlembicCurrent` DB to `RustOwned` on first open. **Rejected**:
  a one-way schema-ownership flip (drops `alembic_version`, switches the
  authority table) is surprising, hard to roll back, and impossible for a
  pytest to exercise deterministically against an arbitrary production DB.
  An explicit `degenbot database cutover` command makes the transition a
  deliberate user action and a precisely testable boundary.

- **Retire Alembic in 0.6.x (skip the hybrid releases).** Build the cutover
  and drop Alembic in the same release train. **Rejected**: `pip` users on
  production Alembic-stamped databases need at least one release that ships
  *both* the cutover path *and* the Alembic upgrade path, so they can move a
  stale database to `ALEMBIC_HEAD` and then cutover. Removing Alembic in the
  same release that introduces the cutover strands anyone whose database is
  not yet at head.

- **Keep Alembic indefinitely.** Treat the hybrid period as permanent.
  **Rejected**: the long-term vision (AGENTS.md) is that the Rust core owns
  everything a bot needs, including schema. Alembic is a Python build/runtime
  concern; a standalone `cargo add degenbot` consumer has no Alembic. The
  cutover mechanism is how the project gets to that end state without
  stranding existing databases.

## Consequences

- **0.6.x releases are dual-path.** Both the Alembic upgrade path and the
  Rust cutover path ship. Users may upgrade through the final Alembic
  revision *and* cutover to Rust ownership, in either order across releases.
- **`ensure_schema` gains `RustOwned`.** Both the post-cutover DB and the
  re-opened `FreshStandalone` DB report `RustOwned`; `FreshStandalone`
  remains the one-shot "just created" report. `Unrecognized` is now
  unambiguous: tables, no alembic, no `_degenbot_db_schema_version`.
- **The cutover is the one Rust-writes-schema-to-an-Alembic-DB operation.**
  Documented as an exception in `convert_alembic_to_rust_owned`'s doc
  comment; it is bounded to the boundary-crossing moment and never runs on
  the `AlembicCurrent` read path.
- **0.7 retirement is the only deletion point.** Mid-0.6.x cleanups observe
  the forbidden-until-0.7 kill list; an orphaned-but-still-needed import
  stays until 0.7.
- **The cutover boundary is pytest-proven.** A test migrates a stale
  Alembic-tagged database through `ALEMBIC_HEAD` to `RustOwned`, asserting
  no `alembic_version` table remains and the data round-trips. That test is
  the contract artifact for the whole transition.

## Related

- **ADR-005** (Polars-inspired three-layer architecture) — the standalone-
  Rust-core constraint that makes Rust-owned schema a first-class
  requirement; a `cargo add degenbot` consumer has no Alembic by
  construction.
- **`docs/adr/ADR-003-botcore-state-layer.md`** — `Bot` as the single Rust
  state owner; schema ownership is the persistence half of the same
  principle.
- **`rust/crates/degenbot-db/src/migrate.rs`** — `ensure_schema` and the
  state machine this ADR extends; the doc comment there already names the
  hybrid period as a "HARD REQUIREMENT."
- **Epic `2Z3Y46`** (`.ergo/plan-cli-db-migration.md`) — the task graph that
  builds and proves the cutover path during 0.6.x; the forbidden-until-0.7
  kill list above is mirrored in that epic's body and in `AGENTS.md`.