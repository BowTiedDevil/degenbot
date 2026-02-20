# Uniswap Agent Documentation

## Overview

Uniswap DEX implementations across versions 2, 3, and 4. Includes liquidity pool models, math libraries ported from Solidity, and position tracking.

## Version Implementations

**Uniswap V2**
- `v2_liquidity_pool.py` - Constant product AMM with 0.3% fee, split-fee pools support
- `v2_functions.py` - Quote calculations, pool address generation, constant product math
- `v2_types.py` - V2-specific type aliases
- State caching with configurable depth, simulation methods

**Uniswap V3**
- `v3_liquidity_pool.py` - Concentrated liquidity with tick-based pricing, liquidity range caching
- `v3_functions.py` - Swap calculations, path decoding, sqrtPriceX96 conversions
- `v3_types.py` - V3-specific type aliases
- `v3_snapshot.py` - Position state capture for backtesting (JSON and database)
- Performance: swap step caching, sparse vs complete liquidity mapping

**Uniswap V4**
- `v4_liquidity_pool.py` - Singleton architecture with hooks, protocol fees, dynamic LP fees
- `v4_types.py` - V4-specific type aliases (`PoolKey`, `SwapDelta`, `ProtocolFee`)
- `v4_snapshot.py` - Position state capture for backtesting (by pool ID)
- Uses `PoolManager` contract, identified by pool ID (keccak hash of PoolKey)
- Supports 14 hook types for customizing pool behavior
- Protocol fees + LP fees = total swap fee

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
- `tick.py` - Tick state management (minimal)
- `unsafe_math.py` - Unchecked arithmetic
- `functions.py` - Helper functions
- `constants.py` - Math constants
- `_config.py` - Library configuration

**V4 Libraries** (`v4_libraries/`)
- Similar structure to V3 with V4-specific math
- Different constants (e.g., `MIN_SQRT_PRICE` vs `MIN_SQRT_RATIO`)
- `fixed_point_96.py` - Q96 fixed-point math
- `constants.py` - V4-specific constants
- `functions.py` - V4 helper functions

## Shared Components

- `types.py` - Common Uniswap types
- `managers.py` - Liquidity pool registry and lifecycle
- `deployments.py` - Factory and pool addresses by chain
- `abi.py` - Contract ABIs for pool interactions

## Type Aliases

- `Liquidity`, `SqrtPriceX96`, `Tick`, `Pip`
- `InitializedTickMap`, `LiquidityMap` (V3 and V4 variants)
- V4-specific: `FeeToProtocol`, `SwapFee`

## Design Patterns

- Libraries mirror Solidity contract structure
- Pools use stateful classes with update methods
- Integer math matches Solidity exactly
- Snapshots enable deterministic replay
- State caching with configurable depth for reorg handling
- Publisher/subscriber pattern for arbitrage helpers
- Thread-safe updates with locks
- Batch RPC requests for immutable values
