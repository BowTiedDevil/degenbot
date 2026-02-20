# Exceptions Agent Documentation

## Overview

Comprehensive exception hierarchy. All exceptions inherit from DegenbotError base class.

## Exception Categories

**Base** (`base.py`)
- `DegenbotError` - Base exception for all degenbot errors
- `DegenbotTypeError` - Type-related errors
- `DegenbotValueError` - Value validation errors
- `ExternalServiceError` - Errors from external service calls

**Arbitrage** (`arbitrage.py`)
- `ArbitrageError` - Base exception for arbitrage module
- `ArbCalculationError` - Calculation failures
- `NoLiquidity` - Pool lacks liquidity
- `Unprofitable` - Negative profit result
- `InvalidSwapPathError` - Invalid swap path
- `RateOfExchangeBelowMinimum` - Profit threshold not met (takes `Fraction` parameter)
- `InvalidForwardAmount` - Invalid forward amount
- `NoSolverSolution` - No optimization solution found (custom pickling)

**Connection** (`connection.py`)
- `DegenbotConnectionError` - Base connection error
- `ConnectionTimeout` - Connection timeout
- `Web3ConnectionTimeout` - Web3-specific timeout
- `IPCSocketTimeout` - IPC socket timeout

**Fetching** (`fetching.py`)
- `FetchingError` - Base fetching error
- `BlockFetchingTimeout` - Block fetch timeout
- `LogFetchingTimeout` - Log fetch timeout

**Liquidity Pool** (`liquidity_pool.py`)
- `LiquidityPoolError` - Base pool error
- `BrokenPool` - Corrupted or invalid pool state
- `InvalidSwapInputAmount` - Invalid input amount for swap
- `AddressMismatch` - Address mismatch
- `LiquidityMapWordMissing` - Missing liquidity map word
- `ExternalUpdateError` - External update failure
- `IncompleteSwap` - Incomplete swap (custom pickling)
- `LateUpdateError` - Late update
- `NoPoolStateAvailable` - No pool state available
- `PossibleInaccurateResult` - Possible inaccurate result (custom pickling, includes hooks)
- `UnknownPool` - Unknown pool
- `UnknownPoolId` - Unknown pool ID

**ERC20** (`erc20.py`)
- `Erc20TokenError` - Base ERC20 error
- `NoPriceOracle` - No price oracle available

**EVM** (`evm.py`)
- `EVMRevertError` - Simulated EVM operation would revert
- `InvalidUint256` - Invalid uint256 value

**Anvil** (`anvil.py`)
- `AnvilError` - Fork testing errors

**Database** (`database.py`)
- `BackupExists` - Backup file already exists

**Manager** (`manager.py`)
- `ManagerError` - Base manager error
- `PoolNotAssociated` - Pool not associated with manager
- `PoolCreationFailed` - Pool creation failed
- `ManagerAlreadyInitialized` - Manager already initialized

**Registry** (`registry.py`)
- `RegistryError` - Base registry error
- `RegistryAlreadyInitialized` - Registry already initialized

## Module Exports

`__init__.py` exports exceptions by category. Base exceptions (`ArbitrageError`, `LiquidityPoolError`, `Erc20TokenError`, `EVMRevertError`) are not exported directly.

## Design Patterns

- All exceptions inherit from DegenbotError
- Specific subtypes for distinct error categories
- Avoid broad exception catches in favor of specific types
- Custom `__reduce__` methods for multiprocessing pickling (`NoSolverSolution`, `IncompleteSwap`, `PossibleInaccurateResult`)
- Exception attributes store contextual data (e.g., `RateOfExchangeBelowMinimum.rate`, `ConnectionTimeout.timeout_seconds`, `PossibleInaccurateResult.hooks`)
