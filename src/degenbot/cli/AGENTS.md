# CLI Agent Documentation

## Overview

CLI command implementations for Aave V3, liquidity pools, exchanges, and database operations.

## Commands

**Aave V3**
- `aave.py` - Commands: `update`, `activate`, `deactivate`
- See `../../../docs/cli/aave.md` for detailed docs
- Environment variables for debug: `DEGENBOT_VERBOSE_ALL`, `DEGENBOT_VERBOSE_USERS`, `DEGENBOT_VERBOSE_TX`

**Liquidity Pools**
- `pool.py` - Commands: `update`
- `exchange.py` - Commands: `activate`, `deactivate`
- See `../../../docs/cli/pool.md` for detailed docs

**Database**
- `database.py` - Commands: `backup`, `reset`, `upgrade`, `compact`
- See `../../../docs/cli/database.md` for detailed docs

## Related Files

- `aave.py` - Aave CLI commands
- `database.py` - Database commands
- `exchange.py` - Exchange commands
- `pool.py` - Liquidity pool commands
- `utils.py` - CLI utilities
