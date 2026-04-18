# Optimizer Recommendations

## By Arbitrage Type

| Type | Recommended Method | Time | Notes |
|------|-------------------|------|-------|
| **V2-V2 (single)** | Möbius (Py 0.86μs / Rust 0.19μs) | Sub-μs | Zero iterations, O(1) |
| **V2-V2 (EVM-exact)** | Rust Integer Möbius | 0.88μs | uint256, byte-perfect match |
| **V2 multi-hop (2+ pools)** | Möbius (Py 0.9-1.5μs / Rust 0.2-0.3μs) | Sub-μs to ~1.5μs | Zero iterations, O(n) |
| **V2-V2 (batch 100+)** | Vectorized Möbius (Py 0.14μs / Rust 0.09μs per path) | 0.09-0.14μs/path | Batch processing |
| **V2-V3 (known range)** | Möbius closed-form | ~5μs | Zero iterations |
| **V2-V3 (unknown range)** | Möbius solve_v3_candidates | ~5-15μs | Checks 1-3 ranges |
| **V2-V3 (tick crossing)** | Möbius solve_piecewise | ~25μs | Golden section search |
| **V2-V3 (complex)** | V2V3Optimizer or Brent | ~5-15ms | Multiple tick crossings |
| **V3 single range** | Möbius closed-form | ~5μs | Zero iterations |
| **V3 multi-range (with crossing)** | Möbius solve_piecewise | ~25μs | Golden section search |
| **V3-V3** | Brent | ~390-500μs | Both pools have ticks |
| **Triangular (3 pools)** | Möbius | ~5μs | O(n) closed-form |
| **Multi-hop (4-6 pools)** | Möbius | ~5μs | O(n) closed-form |
| **Multi-hop (10+ pools)** | Möbius or Dual decomposition | ~5-500μs | Depends on pool types |
| **Multi-path simultaneous** | MultiTokenRouter | ~5-12ms | Dual decomposition |
| **Balancer weighted (3+ tokens)** | Closed-form Eq.9 | ~576μs (N=3) | Willetts & Harrington 2024 |
| **Fixed pools, HFT** | Lookup table | ~0.1μs | Pre-compute |
| **EVM-exact validation** | Rust Integer Möbius | 0.88μs | uint256 arithmetic |
| **Balancer 3-token basket** | Closed-form Eq.9 | 576μs | 12 signatures, ~3μs each |
| **Balancer 4-token basket** | Closed-form Eq.9 | 1.3ms | 50 signatures |
| **Balancer 5-token basket** | Closed-form Eq.9 | 2.9ms | 180 signatures |

## By Use Case

| Use Case | Approach |
|----------|----------|
| **Research & prototyping** | Any (compare results) |
| **Production V2** | Möbius (Py for simplicity, Rust for speed) |
| **Production V2-V3** | Möbius (known/unknown range) or V2V3Optimizer (complex) |
| **Production V3-V3** | Brent (handles both sides) |
| **MEV bot** | Rust Möbius + Integer Möbius for validation + lookup tables for hot paths |
| **Backtesting** | Vectorized Möbius for batch efficiency |
| **Gas-sensitive** | Include gas in objective function (pending) |
| **EVM-exact required** | Rust Integer Möbius (0.88μs, exact match) |
| **Balancer multi-token** | Closed-form Eq.9 (576μs N=3, 1.3ms N=4) |

## Performance Hierarchy (V2-V2 Single Path)

```
Rust Möbius (f64)                0.19μs   ████████████████████████████████ (fastest)
Python Möbius                    0.86μs   ████████████████████████████████
Rust Integer Möbius              0.88μs   ████████████████████████████████
Newton (V2-V2 single)            7.5μs    ██████████████████████████████
Brent (baseline)               194μs     █████
CVXPY                          1300μs     █
```

## Performance Hierarchy (Batch 1000 paths, 2-hop)

```
Rust Batch Möbius               93μs  (0.09μs/path)  ████████████████████████████████
Rust Vectorized Möbius         104μs  (0.10μs/path)  ████████████████████████████████
Python Vectorized Möbius       140μs  (0.14μs/path)  ████████████████████████████████
Python Vectorized Newton       528μs  (0.53μs/path)  ████████████████████
Python Serial Möbius          3229μs  (3.2 μs/path)  ████
```

## Quick Start (Production Code)

```python
# Unified interface (recommended)
from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput

solver = ArbSolver()

# V2-V2 single path
hops = (
    Hop(reserve_in=pool_lo_reserves_in, reserve_out=pool_lo_reserves_out, fee=pool_lo.fee),
    Hop(reserve_in=pool_hi_reserves_in, reserve_out=pool_hi_reserves_out, fee=pool_hi.fee),
)
result = solver.solve(SolveInput(hops=hops))
if result.success:
    optimal_input = result.optimal_input  # int wei
    profit = result.profit              # int wei
    method = result.method               # SolverMethod.MOBIUS/NEWTON/BRENT

# Multi-hop V2 (3+ pools)
hops = (hop_a, hop_b, hop_c)
result = solver.solve(SolveInput(hops=hops))

# With max_input constraint
result = solver.solve(SolveInput(hops=hops, max_input=10**18))

# Pool-to-Hop conversion
from degenbot.arbitrage.optimizers.solver import pool_to_hop, pools_to_solve_input
hop = pool_to_hop(pool, input_token)
solve_input = pools_to_solve_input([pool_a, pool_b], input_token)

# Legacy interface (still available)
from degenbot.arbitrage.optimizers import MobiusOptimizer, NewtonV2Optimizer

# Balancer multi-token basket arbitrage
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver, BalancerMultiTokenHop, SolveInput,
)
from fractions import Fraction

hop = BalancerMultiTokenHop(
    reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),
    decimals=(18, 6, 6),
    market_prices=(1900.0, 1.0, 1.0),
)
result = solver.solve(SolveInput(hops=[hop]))
if result.success:
    print(f"Profit: ${result.profit:.2f}")
    print(f"Method: {result.method}")  # BALANCER_MULTI_TOKEN
```

## Fallback Chain

The `ArbSolver` implements this automatically:

```python
# ArbSolver.solve() tries in order:
# 1. MobiusSolver (5.8μs, zero iterations, V2 + V3 single-range)
# 2. NewtonSolver (4.5μs, 2-hop V2 fallback)
# 3. BrentSolver (223μs, handles all pool types)
# Returns first successful result
```

## Not-Profitable Early Exit

```python
# Möbius free profitability check: K/M > 1
# No simulation needed — instant rejection at 0.32μs (integer)
if not mobius.is_profitable(path):
    return None  # Skip unprofitable paths without solving
```
