---
title: Database CLI Commands
category: cli
tags:
  - state-management
  - database
  - cli
related_files:
  - ../../src/degenbot/cli/database.py
  - ../../src/degenbot/database/operations.py
  - ../../src/degenbot/database/__init__.py
  - ../../src/degenbot/database/models/base.py
  - ../../src/degenbot/migrations/env.py
complexity: standard
---

# Database CLI Commands

## Overview

The Database CLI provides commands for managing the SQLite database used by degenbot to store pool metadata, liquidity positions, Aave market data, and other blockchain-derived information. Commands are available for creating, backing up, compacting, upgrading, and resetting the database.

## Background: Database Architecture

### SQLite with Write-Ahead Logging (WAL)

Degenbot uses SQLite with **WAL mode** for improved concurrency and performance:

- **WAL mode**: Allows simultaneous reads and writes
- **Journal file**: Stores changes before committing to main database
- **Auto vacuum**: Enabled to reclaim free space automatically
- **Checkpointing**: WAL checkpoint performed before backups for consistency

### Database Schema

The database contains multiple tables organized by domain:

- **ERC20 tokens**: `Erc20TokenTable` - Token metadata for all tracked assets
- **Pools**: `LiquidityPoolTable` and subclass tables for V2/V3/V4 pool metadata
- **Liquidity positions**: `LiquidityPositionTable` and subclass tables for tick-level data
- **Initialization maps**: `InitializationMapTable` for tick bitmaps
- **Aave V3**: `AaveV3MarketTable`, `AaveV3AssetsTable`, `AaveV3UsersTable`, `AaveV3CollateralPositionsTable`, `AaveV3DebtPositionsTable`, `AaveV3ContractsTable`
- **Exchanges**: `ExchangeTable` for tracking active DEX deployments
- **Pool managers**: `PoolManagerTable` for Uniswap V4 pool managers

All database models are defined in [`src/degenbot/database/models/`](../../src/degenbot/database/models/).

### Alembic Migrations

Database schema changes are managed through **Alembic migrations**:

- **Version tracking**: `alembic_version` table stores current schema revision
- **Migration scripts**: Located in [`src/degenbot/migrations/versions/`](../../src/degenbot/migrations/versions/)
- **Upgrade path**: Migrations can be applied incrementally to the latest version
- **Head revision**: Latest migration marked as `head`

## Commands

All CLI commands are implemented in [`src/degenbot/cli/database.py`](../../src/degenbot/cli/database.py).

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
5. **Stamp migrations**: Marks database with latest Alembic revision
6. **Initial vacuum**: Performs VACUUM to optimize storage

#### Example Usage

```bash
degenbot database reset
```

### `degenbot database upgrade`

Upgrade the database schema to the latest version.

```bash
degenbot database upgrade [--force]
```

#### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--force` | `False` | Skip confirmation prompt |

#### Behavior

1. **Check versions**: Compares current database version with latest migration
2. **Confirmation**: Prompts unless `--force` is specified
3. **Apply migrations**: Runs all pending Alembic migrations sequentially
4. **Update version**: Updates `alembic_version` table to head revision

#### Example Usage

```bash
degenbot database upgrade          # Interactive with confirmation
degenbot database upgrade --force  # Skip confirmation
```

#### Migration Versioning

The system tracks two versions:

- **Current version**: Revision stored in `alembic_version` table
- **Latest version**: Head revision in migration scripts

A warning is logged on startup if versions don't match:
```
The current database revision (abc123) does not match the latest (def456) for degenbot version X.Y.Z!
Database-related features may raise exceptions if you continue. Perform database migrations with 'degenbot database upgrade'.
```

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

When a new database is created (via `reset` or programmatically), the following operations are performed:

```python
# 1. Create engine and connect
engine = create_engine("sqlite:///path/to/database.db")

# 2. Enable WAL mode for concurrent reads/writes
connection.execute(text("PRAGMA journal_mode=WAL;"))

# 3. Enable auto vacuum to reclaim space
connection.execute(text("PRAGMA auto_vacuum=FULL;"))

# 4. Create all tables from SQLAlchemy models
Base.metadata.create_all(bind=engine)

# 5. Perform initial vacuum for optimization
connection.execute(text("VACUUM;"))

# 6. Stamp with latest Alembic revision
command.stamp(get_alembic_config(), "head")
```

## Database Schema Changes

### Creating New Migrations

Generate and apply migrations using Alembic:

```bash
alembic revision --autogenerate -m "description of changes"
degenbot database upgrade
```

Migration files are stored in `src/degenbot/migrations/versions/`.

## Configuration

The database path is configured via settings:

```python
# From degenbot.config.settings
settings.database.path  # pathlib.Path to database file
```

Default database location depends on the platform and configuration.

## Error Handling

**BackupExists**: Raised when backup file already exists. User can choose to overwrite or abort.

**Version Mismatch**: Logged on startup if database version doesn't match code version. Run `degenbot database upgrade` to apply pending migrations.

## Related Functions

### Database Operations

All database operations are defined in [`src/degenbot/database/operations.py`](../../src/degenbot/database/operations.py):

- `backup_sqlite_database(db_path)` - Create backup of database
- `create_new_sqlite_database(db_path)` - Create new database with schema
- `compact_sqlite_database(db_path)` - Reclaim free space with VACUUM
- `upgrade_existing_sqlite_database()` - Apply pending Alembic migrations
- `get_scoped_sqlite_session(database_path)` - Get thread-safe SQLAlchemy session
- `get_alembic_config()` - Get Alembic configuration object

### Database Session

The global database session is available in [`src/degenbot/database/__init__.py`](../../src/degenbot/database/__init__.py):

```python
db_session = get_scoped_sqlite_session(database_path=settings.database.path)

# Usage:
from degenbot.database import db_session

with db_session() as session:
    result = session.execute(query)
```

## Dependencies

- **Database**: SQLite 3.x
- **ORM**: SQLAlchemy
- **Migrations**: Alembic
- **CLI**: Click
- **Logging**: degenbot logging module

## Example Workflows

**Initial Setup**: Reset database, activate exchanges, then run updates.

**Regular Maintenance**: Backup before updates, then compact if database grew significantly.

**Schema Upgrade**: Run `degenbot database upgrade --force` after pulling code with new migrations.
