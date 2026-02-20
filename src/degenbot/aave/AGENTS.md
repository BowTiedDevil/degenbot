# Aave Agent Documentation

## Overview

Aave V3 lending protocol implementation with processor-based architecture for market management, position tracking, and GHO stablecoin integration.

## Components

**Core**
- `deployments.py` - `AaveV3Deployment` dataclass with pool addresses by chain
- `__init__.py` - Empty (exports in `processors/__init__.py`)

**Libraries** (`libraries/`)
Math libraries ported from Aave protocol contracts:
- `wad_ray_math.py` - Ray math with floor/ceil rounding variants (27 decimal precision)
- `percentage_math.py` - 4-decimal percentage calculations
- `token_math.py` - Pool contract TokenMath library (V1, V4, V5 implementations)

**Processors** (`processors/`)
Token processors by revision with factory pattern:
- `base.py` - Protocols (`TokenProcessor`, `CollateralTokenProcessor`, `DebtTokenProcessor`, `GhoDebtTokenProcessor`)
- `factory.py` - `TokenProcessorFactory`, `PoolProcessorFactory`
- `pool.py` - `PoolProcessor` for scaled amount calculations before token operations
- `collateral/` - v1, v3, v4, v5 collateral token processors
- `debt/` - v1, v3, v4, v5 standard debt processors
- `debt/gho/` - v1, v2, v4, v5, v6 GHO debt processors

**CLI** (`../cli/aave.py`)
Market synchronization and position tracking via blockchain event processing:
- `aave update` - Sync active markets with database
- `aave activate` - Enable market tracking
- `aave deactivate` - Disable market tracking
- Options: `--chunk`, `--to-block`, `--verify`, `--one-chunk`, `--no-progress-bar`

## Database Schema

**Tables** (`../database/models/aave.py`)

- `AaveV3MarketTable` - Market configuration (chain, name, active status, last update block)
- `AaveV3ContractsTable` - Contract addresses (Pool, PoolConfigurator, PoolDataProvider) with revisions
- `AaveV3UsersTable` - User addresses with E-mode and GHO discount settings
- `AaveV3AssetsTable` - Reserve assets with aToken/vToken addresses and revision numbers
- `AaveV3CollateralPositionsTable` - Scaled collateral balances per user/asset
- `AaveV3DebtPositionsTable` - Scaled debt balances per user/asset
- `AaveGhoTokenTable` - GHO-specific configuration (discount token, discount rate strategy)

## Key Concepts

**Scaled Balance Tracking**
- Database stores scaled balances: `actual_balance = scaled * index`
- Interest accrues automatically via index updates
- Processors calculate scaled amounts using `TokenMath` protocol

**Token Revisions**
- Each aToken/vToken has a revision number (1-6) indicating contract version
- Math operations use version-specific processors via factory pattern
- Upgrades tracked via `Upgraded` events

**GHO Discount Mechanism**
- GHO borrowers receive interest discounts based on stkAAVE holdings
- Discount rate strategy contract calculates percentage
- Balance updates trigger discount recalculation
- GHO debt revisions 2+ support discount tracking

**Processor Architecture**
- Stateless calculation of balance deltas (no state modification)
- `TokenProcessor` protocols define interface for each revision
- `PoolProcessor` mirrors on-chain TokenMath calculations
- Factory pattern selects correct processor by revision

**Event Processing**
Chronological processing of blockchain events:
- `ReserveDataUpdated` - Update liquidity/borrow rates and indices
- `Mint` - Collateral deposit or debt borrow
- `Burn` - Collateral withdrawal or debt repayment
- `BalanceTransfer` - Collateral transfer between users
- `UserEModeSet` - E-mode category changes
- `Upgraded` - Token contract upgrades
- `DiscountTokenUpdated` / `DiscountRateStrategyUpdated` - GHO configuration

## Revision Mapping

- Pool revisions 1-3 → `TokenMathV1`
- Pool revision 4 → `TokenMathV4` (first explicit rounding)
- Pool revisions 5+ → `TokenMathV5` (same as V4)

## Design Patterns

- Processor architecture with factory pattern for version selection
- Frozen dataclasses for deployment immutability
- Scaled balance mechanics for efficient interest tracking
- Chain-unique GHO tokens shared across markets
- Assertion-heavy verification for position accuracy
- Block-state caching for consistent balance lookups

## Environment Variables

Debug controls for event processing:
- `DEGENBOT_VERBOSE_ALL` - Enable all verbose logging
- `DEGENBOT_VERBOSE_USERS` - Comma-separated addresses to trace
- `DEGENBOT_VERBOSE_TX` - Comma-separated transaction hashes to trace
