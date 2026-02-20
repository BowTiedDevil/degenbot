# Arbitrage Agent Documentation

## Overview

Arbitrage cycle detection across DEX protocols. Identifies profitable swap paths and calculates optimal input amounts.

## Components

**Cycle Types**
- `uniswap_lp_cycle.py` - `UniswapLpCycle` class for same-DEX arbitrage (Uniswap V2/V3/V4, Aerodrome V2/V3)
- `uniswap_curve_cycle.py` - `UniswapCurveCycle` class for cross-protocol arbitrage (Uniswap + Curve)
- `uniswap_2pool_cycle_testing.py` - Internal testing class for 2-pool optimization (private)
- `uniswap_multipool_cycle_testing.py` - Internal testing class for multi-pool CVXPY optimization (private)

**Types** (`types.py`)
- `ArbitrageCalculationResult` - Generic calculation result
- `UniswapV2PoolSwapAmounts` - V2 swap amounts (amounts_in, amounts_out)
- `UniswapV3PoolSwapAmounts` - V3 swap amounts (amount_in, amount_out, sqrt_price_limit)
- `UniswapV4PoolSwapAmounts` - V4 swap amounts (includes pool_id)
- `CurveStableSwapPoolSwapAmounts` - Curve swap amounts (token indices, underlying flag)
- `CurveStableSwapPoolVector` - Curve token pair direction

**Supported Pool Types**
- Uniswap V2 (constant product)
- Uniswap V3 (concentrated liquidity)
- Uniswap V4 (singleton with hooks)
- Aerodrome V2 (Solidly-style)
- Aerodrome V3 (concentrated liquidity)
- Curve StableSwap V1 (stablecoin pools)

## Optimization Strategies

- **2-pool cycles**: Scipy's `minimize_scalar` with Brent method (`method="bounded"`)
- **Multi-pool V2 cycles**: CVXPY convex optimization with DPP (Disciplined Parametrized Programming)
- Bounds: Input amount from 1.0 to `max_input` (default: 100 WETH)
- Tolerance: `xatol=1.0` for convergence

## Key Concepts

- **Swap Vectors**: Internal tracking of token flow direction through pools
- **Pool Viability**: Pre-calculation checks ensuring pools can execute swaps
- **Rate of Exchange**: Minimum profit threshold validation
- **V4 Pool Key**: Identifies V4 pools by (currency0, currency1, fee, tick_spacing, hooks)
- **Curve Discount Factor**: 0.9999 multiplier masking get_dy() vs exchange() differences
- **Native ETH**: Supported via EtherPlaceholder for WETH pairs

## Design Patterns

- Cycles composed of sequential swaps across pools
- Profit calculation accounts for fees at each hop
- Solver-based optimization for optimal input amounts
- Publisher/subscriber for cycle state updates
- Custom pickling support for multiprocessing (`NoSolverSolution`, `IncompleteSwap`, `PossibleInaccurateResult`)
