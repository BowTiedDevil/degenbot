---
title: Database CLI Commands
category: cli
tags:
  - state-management
  - database
  - cli
related_files:
  - ../rust-cli.md
  - ../../src/degenbot/db/__init__.py
  - ../../src/degenbot/_ffi/db.pyi
complexity: standard
---

# Database CLI Commands

## Overview

The Database CLI provides commands for managing the SQLite database used by degenbot to store pool metadata, liquidity positions, Aave market data, and other blockchain-derived information. Commands are available for creating, backing up, compacting, inspecting, healing, and resetting the database. The argv vocabulary is Rust-owned (ADR-051) — see the [Rust CLI page](../rust-cli.md).

## Background: Database Architecture

### SQLite with Write-Ahead Logging (WAL)

Degenbot uses SQLite with **WAL mode** for improved concurrency and performance:

- **WAL mode**: Allows simultaneous reads and writes
- **Journal file**: Stores changes before committing to main database
- **Auto vacuum**: Enabled to reclaim free space automatically
- **Checkpointing**: WAL checkpoint performed before backups for consistency

### Database Schema

The database contains multiple tables organized by domain:

- **ERC20 tokens**: `erc20_tokens` - Token metadata for all tracked assets
- **Pools**: `pools` plus the family-specific V2/V3/V4 tables for pool metadata
- **Liquidity positions**: `liquidity_positions` and managed-pool position tables for tick-level data
- **Initialization maps**: `initialization_maps` for tick bitmaps
- **Aave V3**: `aave_v3_markets`, `aave_v3_assets`, `aave_v3_users`, position tables, and `aave_v3_contracts`
- **Exchanges**: `exchanges` for tracking active DEX deployments
- **Pool managers**: `pool_managers` for Uniswap V4 pool managers

The Rust `degenbot-db` crate owns the schema and its typed row representations. Python callers use the stable mirror in `src/degenbot/db/`; there is no Python ORM or session layer.

### Schema ownership (Rust-owned)

The database schema is **Rust-owned** (ADR-052). The current schema revision is
stamped in `_degenbot_db_schema_version`; a legacy `alembic_version` table marks
the file as Alembic-era, and `ensure_schema` **heals it at open** — an
out-of-place rebuild to the current Rust `SCHEMA_HEAD`, preserving the old file
as a `*.bak`. There is no in-tree migration-script directory and no step the
user must apply by hand. See
[ADR-052](../adr/ADR-052-db-auto-upgrade-alembic-retirement.md) for the
forward version-lock and the heal-at-open contract.

## Commands

The command vocabulary is **Rust-owned**: [`degenbot-cli`](../../rust/crates/shells/degenbot-cli/src/argv.rs) declares it over `degenbot-cli-core`'s [database arms](../../rust/crates/shells/degenbot-cli-core/src/database.rs). The authoritative flag/exit-code reference is the [Rust CLI page](../rust-cli.md); the domain behaviour below is unchanged.

### `degenbot database backup`

Back up the database to a `.bak` file.

```bash
degenbot database backup
```

#### Behavior

1. **Checkpoint WAL**: Performs a full WAL checkpoint to ensure data consistency
2. **Create backup**: Copies database to `[database_path].bak`
3. **Error handling**: Raises `BackupExists` if backup file already exists
4. **Confirmation**: Prompts to overwrite existing backup if found

#### Example Usage

```bash
degenbot database backup
```

#### Backup File Location

The backup file is created in the same directory as the database with a `.bak` suffix:
- Database: `/path/to/database.db`
- Backup: `/path/to/database.db.bak`

### `degenbot database reset`

Remove and recreate the database with an empty schema.

```bash
degenbot database reset
```

#### Behavior

1. **Confirmation**: Prompts user to confirm deletion
2. **Remove database**: Deletes the existing database file
3. **Create new database**: Initializes with current schema
4. **Configure SQLite**: Sets WAL mode, auto vacuum, and creates all tables
5. **Stamp schema version**: Writes `_degenbot_db_schema_version` at the Rust `SCHEMA_HEAD`
6. **Initial vacuum**: Performs VACUUM to optimize storage

#### Example Usage

```bash
degenbot database reset
```

### `degenbot database upgrade` (RETIRED)

The subcommand is **retired** (ADR-052 D4): the database upgrades itself at
open. The Rust console still accepts the argv, but renders a pointed error and
exits non-zero:

```
the database upgrades itself at open; for an explicit repair, run `degenbot database heal`
```

Use `degenbot database heal` for an explicit out-of-place repair, or
`degenbot database inspect` to read the schema state without writing.

### `degenbot database compact`

Compact the database to reclaim free space.

```bash
degenbot database compact
```

#### Behavior

1. **Connect to database**: Opens SQLite connection
2. **Run VACUUM**: Rebuilds database file, removing free space and defragmenting
3. **Log completion**: Records compaction completion in logs

#### Example Usage

```bash
degenbot database compact
```

#### When to Use

Use after large deletions, before backups, or when database has grown significantly with many deletions.

## Database Initialization

When a new database is created (via `reset` or programmatically), the Rust
`degenbot-db` core performs the same steps the Python path once did, with no
Alembic stamp:

1. Create/connect the SQLite file.
2. Enable WAL journal mode for concurrent reads/writes.
3. Enable `auto_vacuum=FULL` to reclaim space.
4. Create every table at the current Rust `SCHEMA_HEAD`.
5. `VACUUM` once for a compact initial file.
6. Stamp `_degenbot_db_schema_version` with `SCHEMA_HEAD`.

The stable Python mirror in
[`src/degenbot/db/__init__.py`](../../src/degenbot/db/__init__.py)
delegates to the same Rust ops over the `degenbot._ffi.db_*` seam.

## Database Schema Changes

### Forward version-lock (Rust-owned)

Schema changes are Rust-owned (ADR-052 D2). A binary whose
`RUST_SCHEMA_VERSION` is ahead of the file's stamp applies the pending embedded
`ALTER` steps strictly in order at open (each step in its own transaction; a
failed step rolls back to the last-good stamp and refuses loudly). A file
stamped **newer** than the running binary is refused (`schema N > binary M`), so
an old reader never silently misreads a new database. There is no Alembic
revision to generate and no user-applied migration step.

## Configuration

The database path is configured via settings:

```python
# From degenbot.config.settings
settings.database.path  # pathlib.Path to database file
```

Default database location depends on the platform and configuration.

## Error Handling

**BackupExists**: Raised when backup file already exists. User can choose to overwrite or abort.

**Schema newer than the binary**: The open refuses with "the binary is older
than the database (schema N > binary M)" — upgrade the binary, never the file.
A stale Alembic-era file is healed at open instead (ADR-052 D1).

## Related Functions

### Database operations and typed reads

The stable Python mirror in [`src/degenbot/db/__init__.py`](../../src/degenbot/db/__init__.py) is a thin delegation to the Rust `degenbot-db` operations:

- `db_backup_database(src, dst)` - Create an online backup of a database
- `db_create_new_database(path)` - Create a new database at the Rust schema head
- `db_compact_database(path)` - Reclaim free space with `VACUUM`
- `db_heal_database(path)` - Perform an out-of-place dump-and-restore rebuild (ADR-011)
- `db_inspect_schema_state(path)` - Inspect schema ownership without writing

For a typed read, pass the database path and chain-scoped key directly to the
mirror. The result is a Rust-backed row, not an ORM instance:

```python
from degenbot.db import db_fetch_pool_row, db_inspect_schema_state

schema_state = db_inspect_schema_state(database_path)
pool_row = db_fetch_pool_row(database_path, chain_id, pool_address)
```

There is no `bot.db()` session API. The database schema upgrades itself at
open, and the `degenbot.db` mirror is the only supported Python database
interface.

## Dependencies

- **Database**: SQLite 3.x
- **Python interface**: stable `degenbot.db` typed mirror over Rust `degenbot-db`
- **Schema**: Rust `degenbot-db` (`SCHEMA_HEAD`, heal-at-open, forward version-lock)
- **CLI**: the Rust `degenbot` console (`degenbot-cli`)
- **Logging**: degenbot logging module

## Example Workflows

**Initial Setup**: Reset database, activate exchanges, then run updates.

**Regular Maintenance**: Backup before updates, then compact if database grew significantly.

**Schema Upgrade**: Nothing to run — the database upgrades itself at open (ADR-052). Use `degenbot database inspect` to read the schema state and `degenbot database heal` for an explicit repair.
