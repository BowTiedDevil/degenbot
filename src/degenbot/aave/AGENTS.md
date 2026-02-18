# Aave Agent Documentation

## Overview

Aave V3 lending protocol implementation with multi-version library support for market management, position tracking, and GHO stablecoin integration.

## Components

**Core**
- `deployments.py` - `AaveV3Deployment` dataclass with pool addresses by chain
- `__init__.py` - Module exports

**Libraries** (`libraries/`)
Version-specific math libraries matching Aave protocol revisions:
- `v3_1/`, `v3_2/`, `v3_3/`, `v3_4/`, `v3_5/` - Math modules for each protocol version
  - `wad_ray_math.py` - Ray math operations (27 decimal precision)
  - `percentage_math.py` - Percentage calculations (4 decimal precision)
  - `rounding.py` (v3_5 only) - Rounding utilities

**CLI** (`../cli/aave.py`)
Market synchronization and position tracking via blockchain event processing:
- `aave update` - Sync active markets with database
- `aave activate` - Enable market tracking
- `aave deactivate` - Disable market tracking

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
- Event processing derives scaled amounts from user amounts

**Token Revisions**
- Each aToken/vToken has a revision number (1-5) indicating contract version
- Math operations use version-specific libraries
- Upgrades tracked via `Upgraded` events

**GHO Discount Mechanism**
- GHO borrowers receive interest discounts based on stkAAVE holdings
- Discount rate strategy contract calculates percentage
- Balance updates trigger discount recalculation

**Event Processing**
Chronological processing of blockchain events:
- `ReserveDataUpdated` - Update liquidity/borrow rates and indices
- `Mint` - Collateral deposit or debt borrow
- `Burn` - Collateral withdrawal or debt repayment
- `BalanceTransfer` - Collateral transfer between users
- `UserEModeSet` - E-mode category changes
- `Upgraded` - Token contract upgrades
- `DiscountTokenUpdated` / `DiscountRateStrategyUpdated` - GHO configuration

## Helper Functions

**State Management**
- `_get_or_create_user()` - Get or initialize user record
- `_get_or_create_collateral_position()` - Get or create collateral position
- `_get_or_create_debt_position()` - Get or create debt position

**Math Operations**
- `_accrue_debt_on_action()` - Calculate balance increase with discount
- `_accrue_debt_on_action_with_assertion()` - Wrapper with assertion
- `_refresh_discount_rate()` - Update user's GHO discount rate
- `_get_discounted_balance()` - Calculate balance with discount applied
- `_get_math_libraries()` - Get version-specific math modules

**Event Decoding**
- `_decode_address()` - Decode address from event topics
- `_decode_uint_values()` - Decode uint values from event data

**Verification**
- `_verify_scaled_token_positions()` - Verify positions match on-chain state
- `_verify_gho_discount_amounts()` - Verify GHO discounts match contract

## Design Patterns

- Versioned libraries match Aave protocol upgrades (v3.1 through v3.5)
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
