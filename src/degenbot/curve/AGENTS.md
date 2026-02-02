# Curve Agent Documentation

## Overview

Curve stableswap AMM implementation with specialized math for low-slippage stablecoin swaps.

## Components

**Pool Model**
- `curve_stableswap_liquidity_pool.py` - Stableswap pool with invariant-based pricing
- `types.py` - CurveStableswapPoolState, CurveStableSwapPoolStateUpdated, CurveStableswapPoolSimulationResult

**Configuration**
- `deployments.py` - Curve V1 registry, factory, and metaregistry addresses
- `abi.py` - Contract ABIs for pool interactions

## Design Patterns

- Stableswap invariant minimizes slippage for correlated assets
- Pool state updates via external events
- Supports both standard and underlying (wrapped) token swaps
