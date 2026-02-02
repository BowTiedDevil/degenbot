# Exceptions Agent Documentation

## Overview

Comprehensive exception hierarchy. All exceptions inherit from DegenbotError base class.

## Exception Categories

**Base**
- `base.py` - DegenbotError, DegenbotTypeError, DegenbotValueError

**Domains**
- `arbitrage.py` - ArbCalculationError, NoLiquidity, Unprofitable, InvalidSwapPathError, etc.
- `connection.py` - ConnectionTimeout, Web3ConnectionTimeout, IPCSocketTimeout
- `fetching.py` - FetchingError, BlockFetchingTimeout, LogFetchingTimeout
- `liquidity_pool.py` - BrokenPool, InvalidSwapInputAmount
- `erc20.py` - ERC20-specific errors
- `evm.py` - EVMRevertError, simulation failures
- `anvil.py` - AnvilError for fork testing
- `database.py` - Database operation errors
- `manager.py` - Registry and lifecycle errors
- `registry.py` - Token/pool lookup errors

## Design Patterns

- All exceptions inherit from DegenbotError
- Specific subtypes for distinct error categories
- Avoid broad exception catches in favor of specific types
