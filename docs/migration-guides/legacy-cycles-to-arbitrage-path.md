# Legacy Cycles → ArbitragePath Migration Guide

The legacy arbitrage cycle classes (`UniswapLpCycle`, `UniswapCurveCycle`, `_UniswapMultiPoolCycleTesting`, `_UniswapTwoPoolCycleTesting`) are deprecated and will be removed in a future release. Use `ArbitragePath` + `ArbSolver` instead.

## API Mapping

| Legacy (deprecated) | Replacement |
|---|---|
| `UniswapLpCycle(pools, input_token)` | `ArbitragePath(pools, input_token, solver=ArbSolver())` |
| `cycle.calculate()` | `path.calculate()` → `SolveResult` |
| `cycle.calculate(state_overrides=...)` | `path.calculate_with_state_override(...)` |
| `cycle.calculate_with_pool(...)` | `path.calculate_with_pool(...)` |
| `cycle.generate_payloads(...)` | `path.build_swap_amounts(result)` then `generate_payloads(swap_amounts, ...)` |
| `cycle.id` | `path.id` |
| `ArbitrageCalculationResult` from calculate | `SolveResult` from `calculate()` then `ArbitrageCalculationResult` from `build_swap_amounts()` |
| `cvxpy` optimizer | `ArbSolver` (Rust-accelerated mobius solver) |
| `_UniswapMultiPoolCycleTesting` | `ArbitragePath` + `ArbSolver` (mobius solver handles multi-pool) |
| `_UniswapTwoPoolCycleTesting` | `ArbitragePath` + `ArbSolver` (2-pool fast path in mobius solver) |
| `UniswapCurveCycle` | `ArbitragePath` + `ArbSolver` (CurveStableswapHop support) |

## Before / After

### 2-pool V2-V2 arbitrage

```python
# Before (deprecated)
from degenbot.arbitrage import UniswapLpCycle

cycle = UniswapLpCycle(
    input_token=usdc,
    swap_pools=[pool_a, pool_b],
)
result = cycle.calculate()
payloads = cycle.generate_payloads(result)

# After
from degenbot.arbitrage import ArbitragePath, ArbitrageCalculationResult
from degenbot.arbitrage.optimizers import ArbSolver

path = ArbitragePath(
    input_token=usdc,
    swap_pools=[pool_a, pool_b],
    solver=ArbSolver(),
)
solve_result = path.calculate()
swap_amounts = path.build_swap_amounts(solve_result)
payloads = generate_payloads(swap_amounts, ...)
```

### Multi-pool paths

```python
# Before (deprecated)
from degenbot.arbitrage import _UniswapMultiPoolCycleTesting

cycle = _UniswapMultiPoolCycleTesting(
    input_token=usdc,
    swap_pools=[pool_a, pool_b, pool_c],
)

# After
path = ArbitragePath(
    input_token=usdc,
    swap_pools=[pool_a, pool_b, pool_c],
    solver=ArbSolver(),
)
```

### Curve + Uniswap mixed paths

```python
# Before (deprecated)
from degenbot.arbitrage import UniswapCurveCycle

cycle = UniswapCurveCycle(
    input_token=weth,
    swap_pools=[curve_pool, uniswap_pool],
)

# After
path = ArbitragePath(
    input_token=weth,
    swap_pools=[curve_pool, uniswap_pool],
    solver=ArbSolver(),
)
```

## Key Differences

1. **Separation of concerns**: `ArbitragePath` computes the optimal solution; `generate_payloads()` handles encoding. The legacy classes mixed both.

2. **Rust-accelerated**: `ArbSolver` uses Rust for the core optimization. The legacy classes used Python-only or cvxpy-based solvers.

3. **Swap amounts**: `build_swap_amounts()` returns typed `SwapAmounts` subclasses with `input_amount()`, `output_amount()`, and `encode()` methods. The legacy classes returned raw tuples.

4. **No cvxpy dependency**: The new architecture doesn't require cvxpy. If you need legacy cycle classes, install with `pip install degenbot[legacy-cycles]`.
