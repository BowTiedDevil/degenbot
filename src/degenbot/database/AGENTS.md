# Database Agent Documentation

## Overview

SQLite database with Write-Ahead Logging (WAL) for persistence. SQLAlchemy ORM with Alembic migrations.

## Components

**Models** (`models/`)
- SQLAlchemy ORM models for liquidity pools, tokens, positions
- Type-annotated for mypy strict mode
- Single-table inheritance with `kind` discriminator for pool types

**Operations** (`operations.py`)
- `backup_sqlite_database()` - Creates .bak file with WAL checkpoint
- `create_new_sqlite_database()` - Initializes with WAL, auto_vacuum, creates tables
- `compact_sqlite_database()` - Runs VACUUM
- `upgrade_existing_sqlite_database()` - Runs Alembic migrations
- `get_scoped_sqlite_session()` - Returns scoped_session factory
- `get_alembic_config()` - Returns Alembic Config

**Initialization** (`__init__.py`)
- Version check and migration warnings
- Alembic integration for schema versioning

**Migrations** (`../migrations/`)
- Alembic migrations located in `src/degenbot/migrations/`
- Schema versioning tracked via Alembic

## Custom Types

- `IntMappedToString` - 78-char VARCHAR for large EVM integers
- `Address` - ChecksumAddress mapped to String(42)
- `BigInteger` - Type alias using IntMappedToString

## Table Inventory

**ERC20**
- `Erc20TokenTable` - Token metadata

**Exchanges**
- `ExchangeTable` - Exchange configurations

**Liquidity Pools**
- Base `LiquidityPoolTable` with single-table inheritance
- Concrete tables: Uniswap V2/V3/V4, Aerodrome, Camelot, Pancakeswap, Sushiswap, Swapbased

**Managed Pools**
- `ManagedLiquidityPoolTable`, `PoolManagerTable`
- `ManagedPoolInitializationMapTable` - V4 initialization maps

**Liquidity Positions**
- `LiquidityPositionTable` - V3 tick positions
- `ManagedPoolLiquidityPositionTable` - V4 positions

**Aave V3**
- `AaveV3MarketTable`, `AaveV3ContractsTable`, `AaveV3UsersTable`
- `AaveV3AssetsTable`, `AaveV3CollateralPositionsTable`, `AaveV3DebtPositionsTable`
- `AaveGhoTokenTable` - GHO-specific configuration

## Indexes

- `ix_liquidity_pool_address_chain` - unique on (address, chain)
- `ix_erc20_tokens_address_chain` - unique on (address, chain)
- `ix_liquidity_positions_pool_id_tick` - unique on (pool_id, tick)
- Aave-specific indexes for user/asset lookups

## Design Patterns

- Scoped sessions for thread-safe database access
- Single-table inheritance with `kind` discriminator
- Cascade delete relationships (e.g., V3 pools delete their liquidity positions)
- Bidirectional relationships with back_populates
- Foreign key constraints with indexes
- Alembic migrations track schema changes
- Version mismatch detection on module import
- Connection uses `URL.create()` with absolute path from `settings.database.path`

## Exceptions

- `BackupExists` - Raised when backup file already exists
