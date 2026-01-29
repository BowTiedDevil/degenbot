# CLI Agent Documentation

## Overview

This directory contains CLI command implementations for the degenbot tool, including Aave V3 market management and position tracking.

## Focused Documentation

See [aave.md](../../../docs/cli/aave.md) for comprehensive documentation on Aave CLI commands, including:

- `degenbot aave update` - Update positions by processing blockchain events
- `degenbot aave activate` - Activate a market for tracking
- `degenbot aave deactivate` - Deactivate a market

The aave.md file contains detailed information on:
- Aave V3 architecture and scaled balance mechanics
- Event processing algorithms (Mint, Burn, BalanceTransfer, etc.)
- Data flow diagrams
- Data model updates
- Solidity contract references
- Error handling and validation

See [pool.md](../../../docs/cli/pool.md) for comprehensive documentation on liquidity pool CLI commands, including:

- `degenbot pool update` - Update liquidity pool metadata and liquidity positions
- `degenbot exchange activate` - Activate an exchange for liquidity pool tracking
- `degenbot exchange deactivate` - Deactivate an exchange

The pool.md file contains detailed information on:
- Liquidity pool architecture across DEX versions (Uniswap versions 2, 3, and 4)
- Liquidity pool discovery events and liquidity updates
- Mock liquidity pool helpers for efficient processing
- Data flow diagrams
- Data model updates
- Solidity contract references

See [database.md](../../../docs/cli/database.md) for comprehensive documentation on database CLI commands, including:

- `degenbot database backup` - Back up the database
- `degenbot database reset` - Remove and recreate the database
- `degenbot database upgrade` - Upgrade the database schema to the latest version
- `degenbot database compact` - Compact the database to reclaim free space

The database.md file contains detailed information on:
- SQLite with Write-Ahead Logging (WAL) configuration
- Database schema organization and table relationships
- Alembic migrations for schema versioning
- Data flow diagrams for all database operations
- Configuration and performance settings
- Error handling and development notes
- Example workflows for setup, maintenance, and upgrades

## Related Files

- `aave.py` - Aave CLI command implementations
- `database.py` - Database-related CLI commands
- `exchange.py` - Exchange-related CLI commands
- `pool.py` - Liquidity pool-related CLI commands
- `utils.py` - Utility functions for CLI operations
