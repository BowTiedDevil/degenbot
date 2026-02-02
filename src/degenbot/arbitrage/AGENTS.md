# Arbitrage Agent Documentation

## Overview

Arbitrage cycle detection across DEX protocols. Identifies profitable swap paths and calculates optimal input amounts.

## Components

**Cycle Types**
- `uniswap_lp_cycle.py` - Uniswap liquidity pool arbitrage (same DEX)
- `uniswap_curve_cycle.py` - Cross-protocol arbitrage (Uniswap + Curve)
- `uniswap_2pool_cycle_testing.py` - Testing utilities for 2-pool cycles
- `uniswap_multipool_cycle_testing.py` - Testing utilities for multi-pool cycles

**Types**
- `types.py` - ArbitrageCalculationResult

## Design Patterns

- Cycles composed of sequential swaps across pools
- Profit calculation accounts for fees at each hop
- Solver-based optimization for optimal input amounts
- Publisher/subscriber for cycle state updates
