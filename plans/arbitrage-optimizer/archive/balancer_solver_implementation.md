# Balancer Weighted Pool Multi-Token Arbitrage Solver

## Overview

This module implements a closed-form solver for N-token Balancer weighted pool arbitrage, based on:

> "Closed-form solutions for generic N-token AMM arbitrage"  
> Willetts & Harrington, QuantAMM.fi (February 2024)  
> [arXiv:2402.06731](https://arxiv.org/abs/2402.06731)

The solver uses Equation 9 from the paper to compute optimal multi-token basket trades in closed form, eliminating the need for iterative numerical optimization.

## Key Implementation Details

### 1. The d_i Indicator (Critical Fix)

The paper defines `d_i = I_{s_i=1}`, an **indicator function** that equals:
- `1` when token i is being **deposited** (signature = +1)
- `0` when token i is being **withdrawn** (signature = -1) or not traded (signature = 0)

A previous implementation incorrectly used `d_i = signature[i]` (giving -1 for withdrawals), which produced trades with inverted signs.

### 2. Decimal Scaling

Balancing tokens have different decimal precisions (e.g., ETH = 18 decimals, USDC = 6). The formula requires all reserves to be in a **consistent scale**. We handle this by:

1. **Upscaling** all reserves to 18-decimal (matching Balancer Vault convention)
2. Applying the closed-form formula in 18-decimal space
3. **Descending** the resulting trades back to native token units

Without this scaling, the formula produces wildly incorrect results because the invariant product calculation mixes different magnitude scales.

### 3. Trade Signatures

For an N-token pool, there are `3^N - 2^(N+1) + 1` valid trade signatures:
- N=3: 12 signatures
- N=4: 50 signatures  
- N=5: 180 signatures

The solver evaluates all valid signatures and picks the one with maximum profit. Only signatures matching the economic incentive produce valid trades (the formula naturally rejects uneconomic signatures by giving trades with wrong signs).

### 4. Fee Handling

Fees are applied via `gamma = 1 - fee`. The `gamma^(d_i)` terms in Equation 9 correctly handle asymmetric fee application:
- Deposits are reduced by the fee factor (the pool receives `gamma * Phi_i`)
- Withdrawals are not reduced (the trader receives `Phi_i`)

### 5. Profit Computation

Profit is computed in numéraire units using **token-unit** amounts:
```
profit = -sum(market_price_i * Phi_i_in_tokens)
```

Since the formula produces trades in upscaled 18-decimal units, we divide by `1e18` to get token-unit amounts before multiplying by market prices.

## Performance

| Pool Size | Single Eq.9 | Full Solver | Signatures |
|-----------|-------------|-------------|------------|
| N=3       | 3 μs        | 554 μs      | 12         |
| N=4       | 3.4 μs      | 1.3 ms      | 50         |
| N=5       | 3.9 μs      | 2.9 ms      | 180        |

The single-signature evaluation (Equation 9) achieves ~3 μs, which is **4× faster** than the paper's reported ~12 μs. The full solver overhead comes from signature enumeration, validation, and integer refinement.

### Comparison with Alternatives

| Method          | N=3       | Notes                              |
|-----------------|-----------|------------------------------------|
| Closed-form     | ~554 μs   | Evaluates all 12 signatures        |
| Brent (pairwise)| ~176 μs   | Only handles 2-token hops          |
| CVXPY           | ~1.1 ms   | General convex optimization        |

The closed-form approach is competitive for N=3 and provides unique multi-token basket trading capability that pairwise solvers cannot.

## Usage

```python
from fractions import Fraction
from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
)

# Define pool state
pool = BalancerMultiTokenState(
    reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),  # 0.3%
    decimals=(18, 6, 6),  # ETH, USDC, DAI
)

# Solve with market prices
solver = BalancerWeightedPoolSolver()
result = solver.solve(pool, market_prices=(1900.0, 1.0, 1.0))

if result.success:
    print(f"Profit: ${result.profit:.2f}")
    print(f"Signature: {result.signature}")
    for i, trade in enumerate(result.trades):
        decimals = pool.decimals[i]
        token_amount = trade / 10**decimals
        print(f"  Token {i}: {token_amount:.4f} ({'deposit' if trade > 0 else 'withdraw'})")
```

### Via ArbSolver (Integration)

```python
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BalancerMultiTokenHop,
    SolveInput,
)

hop = BalancerMultiTokenHop(
    reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),
    decimals=(18, 6, 6),
    market_prices=(1900.0, 1.0, 1.0),
)

solve_input = SolveInput(hops=[hop])
solver = ArbSolver()
result = solver.solve(solve_input)
```

## File Locations

- Closed-form solver: `src/degenbot/arbitrage/optimizers/balancer_weighted.py`
- Grid-search fallback: `src/degenbot/arbitrage/optimizers/balancer_weighted_v2.py`
- Integration: `src/degenbot/arbitrage/optimizers/solver.py`
- Tests: `tests/arbitrage/test_optimizers/test_balancer_weighted.py`
- Benchmarks: `benchmarks/balancer_solver_benchmark.py`
