# Uniswap Agent Documentation

## Overview

Uniswap DEX implementations across versions 2, 3, and 4. Includes liquidity pool models, math libraries ported from Solidity, and position tracking.

## Version Implementations

**Uniswap V2**
- `v2_liquidity_pool.py` - Constant product AMM with 0.3% fee
- `v2_functions.py` - Quote and amount calculations
- `v2_types.py` - V2-specific type aliases

**Uniswap V3**
- `v3_liquidity_pool.py` - Concentrated liquidity with tick-based pricing
- `v3_functions.py` - Swap calculations and tick queries
- `v3_types.py` - V3-specific type aliases
- `v3_snapshot.py` - Position state capture for backtesting

**Uniswap V4**
- `v4_liquidity_pool.py` - Singleton architecture with hooks
- `v4_types.py` - V4-specific type aliases
- `v4_snapshot.py` - Position state capture for backtesting

## Math Libraries

Solidity-ported math functions using integer division (`//` equals Solidity `/`):

**V3 Libraries** (`v3_libraries/`)
- `sqrt_price_math.py` - SqrtPriceX96 conversions
- `tick_math.py` - Tick to price conversions
- `liquidity_math.py` - Liquidity delta calculations
- `swap_math.py` - Swap amount computations
- `full_math.py` - 512-bit multiplication and division
- `bit_math.py` - Bitwise position utilities
- `tick_bitmap.py` - Tick spacing compression
- `tick.py` - Tick state management
- `unsafe_math.py` - Unchecked arithmetic

**V4 Libraries** (`v4_libraries/`)
- Same modules as v3 with V4-specific constants
- `fixed_point_96.py` - Q96 fixed-point math

## Shared Components

- `types.py` - Common Uniswap types
- `managers.py` - Liquidity pool registry and lifecycle
- `deployments.py` - Factory and pool addresses by chain
- `abi.py` - Contract ABIs for pool interactions

## Design Patterns

- Libraries mirror Solidity contract structure
- Pools use stateful classes with update methods
- Integer math matches Solidity exactly
- Snapshots enable deterministic replay
