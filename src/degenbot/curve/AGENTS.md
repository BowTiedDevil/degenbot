# Curve Agent Documentation

## Overview

Curve stableswap AMM implementation with specialized math for low-slippage stablecoin swaps.

## Components

**Pool Model**
- `curve_stableswap_liquidity_pool.py` - Stableswap pool with:
  - Invariant-based pricing with D/y calculation variants
  - Metapool support (pools holding base pool LP tokens)
  - A coefficient ramping (time-based parameter changes)
  - Publisher/Subscriber pattern for state updates
  - Address-specific calculation variants for different pool types

**Types** (`types.py`)
- `CurveStableswapPoolState` - Pool state with balances and optional base pool state
- `CurveStableswapPoolSimulationResult` - Simulation output
- `CurveStableSwapPoolStateUpdated` - State update message
- `CurveStableSwapPoolAttributes` - Pool metadata (address, coins, metapool flag, etc.)

**Configuration**
- `deployments.py` - Registry/factory/metaregistry addresses and broken pool blacklist
- `abi.py` - V1 and V2 pool ABIs, plus registry/factory/metaregistry ABIs

**Exports** (`__init__.py`)
- `CurveStableswapPool` class
- State types and simulation result
- `abi` module (excluded from `__all__`)

## Key Features

**Pool Variants**
- Address-based variant groups determine D and y calculation methods
- Different rate sources: cTokens, yTokens, aETH, rETH, oracles
- Off-peg fee multiplier support for dynamic fees

**Metapools**
- Hold LP tokens from base pools
- Support underlying token swaps via `get_dy_underlying()`
- Virtual price caching with 10-minute expiration

**State Management**
- Bounded cache for historical states
- Lock-protected state updates
- Auto-update mechanism fetches on-chain balances

## Design Patterns

- Stableswap invariant minimizes slippage for correlated assets
- Pool state updates via external events
- Supports both standard and underlying (wrapped) token swaps
