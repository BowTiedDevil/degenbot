# Production Guide: Arbitrage Optimizers

How to use the arbitrage optimization system in production.

## Quick Reference

| Arbitrage Type | Method | Time | Speedup |
|----------------|--------|------|---------|
| V2-V2 single | Rust Möbius | 0.19μs | **1021x** vs Brent |
| V2-V2 batch 1000 | Rust Batch Möbius | 0.09μs/path | **2155x** |
| V2 multi-hop (3+) | Python Möbius | 1.5μs | **129x** |
| V3 single-range | Möbius closed-form | ~0.86μs | **453x** |
| V3 multi-range (1 V3 hop) | Piecewise-Möbius (Rust) | **~9μs** | **43x** |
| V3-V3 both single-range | V3-V3 Rust solver | **~0.19μs** | **2053x** |
| V3-V3 multi-range | V3-V3 Rust solver | **~10-50μs** | **8-39x** |
| V3-V3 complex (fallback) | Brent | ~390μs | baseline |
| Balancer N=3 | Eq.9 closed-form | ~576μs | — |

### V3 Multi-Range Optimization Details

The `PiecewiseMobiusSolver` uses 10 optimization techniques for V3 paths with tick crossings:

1. **Proper V3 crossing math** — Exact swap formulas with fee handling
2. **Golden section search** — 15x faster than Brent for bracketed problems
3. **Pre-computed Möbius coefficients** — Cache transforms for before/after hops
4. **Adaptive iterations** — Early termination when converged (<0.01% improvement)
5. **Lazy candidate filtering** — Skip implausible ranges before evaluation
6. **Tick range caching** — 128-entry LRU cache for pool data
7. **Price impact pruning** — Quick estimate to filter impossible candidates
8. **Parallel evaluation** — ThreadPoolExecutor for 2+ candidates
9. **Vectorized bracket search** — NumPy batch evaluation for initial search
10. **Rust extension** — Full `solve_v3_sequence()` in Rust (~1μs computation, ~9μs end-to-end)

### V3-V3 Solver Details

The Rust V3-V3 solver (`solve_v3_v3`) handles two-V3-hop paths where both pools may cross ticks:

- **Fast path**: Both pools single-range → standard 2-hop Möbius (~0.19μs)
- **Slow path**: One or both pools multi-range → enumerate ending range (k1, k2) combinations, golden section search per candidate (~10-50μs)
- Dispatched automatically by `PiecewiseMobiusSolver` when 2 V3 hops detected

## Recommended Approach by Use Case

| Use Case | Recommended Approach |
|----------|---------------------|
| **Production V2** | Möbius (Python for simplicity, Rust for speed) |
| **Production V2-V3** | Möbius closed-form or `V2V3Optimizer` for complex crossings |
| **Production V3-V3** | Rust V3-V3 solver (~10-50μs), Brent fallback (~390μs) |
| **MEV bot** | Rust Möbius + Integer Möbius for validation |
| **Backtesting** | Vectorized Möbius for batch efficiency |
| **Balancer baskets** | `BalancerMultiTokenSolver` with Eq.9 |

## Unified Interface (Recommended)

The `ArbSolver` automatically selects the best method:

```python
from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput

solver = ArbSolver()

# Build hops from pool data
hops = (
    Hop(reserve_in=r0_in, reserve_out=r0_out, fee=fee0),
    Hop(reserve_in=r1_in, reserve_out=r1_out, fee=fee1),
)

# Solve
result = solver.solve(SolveInput(hops=hops))
if result.success:
    print(f"Optimal input: {result.optimal_input}")
    print(f"Profit: {result.profit}")
    print(f"Method used: {result.method}")  # MOBIUS, PIECEWISE_MOBIUS, NEWTON, or BRENT
```

### Pool-to-Hop Conversion

```python
from degenbot.arbitrage.optimizers.solver import pool_to_hop, pools_to_solve_input

# Single pool
hop = pool_to_hop(pool, input_token)

# Multiple pools
solve_input = pools_to_solve_input([pool_a, pool_b], input_token)
result = solver.solve(solve_input)
```

### With Constraints

```python
# Maximum input constraint
result = solver.solve(SolveInput(hops=hops, max_input=10**18))
```

## Fallback Chain

`ArbSolver.solve()` tries in order:

1. **MobiusSolver** (~0.86μs, zero iterations) — V2 + V3 single-range
2. **PiecewiseMobiusSolver** (~9μs Rust, ~50μs Python) — V3 multi-range with tick crossings
   - V3-V3 paths → Rust `solve_v3_v3` (~0.19μs single-range, ~10-50μs multi-range)
   - Single V3 crossing → Rust `solve_v3_sequence` (~9μs)
3. **SolidlyStableSolver** (~15-25μs) — Aerodrome/Camelot stable pools
4. **BalancerMultiTokenSolver** (~576μs) — N-token weighted pools
5. **NewtonSolver** (~4.5μs) — 2-hop V2 fallback
6. **BrentSolver** (~223μs) — All pool types, ultimate fallback

## Balancer Multi-Token Arbitrage

For N-token weighted pool basket trades:

```python
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver, BalancerMultiTokenHop, SolveInput
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
```

## Not-Profitable Early Exit

Skip solving for unprofitable paths:

```python
from degenbot.arbitrage.optimizers.mobius import compute_mobius_coefficients

# Free profitability check: K/M > 1
coeffs = compute_mobius_coefficients(hops)
if not coeffs.is_profitable:
    return None  # Skip at 0.32μs (integer), no simulation needed
```

## Feature Flag

The unified solver is behind a feature flag for safe rollout:

```python
# In cycle class
USE_SOLVER_FAST_PATH = True  # Enable fast path
USE_SOLVER_FAST_PATH = False  # Fallback to Brent only
```

## Cycle Class Integration

The unified solver is integrated into all 9 `_calculate_*` methods:
- V4-V4, V3-V4, V4-V2, V4-V3, V3-V3, V2-V3, V2-V4, V3-V2, V2-V2

Fast-paths are attempted before CVXPY/Brent for compatible pool types.

## Current Limitations

| Limitation | Workaround |
|------------|------------|
| V3/V4 virtual reserves are approximate | Results validated by pool swap methods |
| V3-V3 golden section may miss global optimum | Brent fallback handles edge cases |
| Curve stableswap not supported | Use Brent fallback |

## Rejected Features

| Feature | Reason |
|---------|--------|
| ~~Gas cost modeling~~ | Out of scope for optimizer - should be handled at execution layer |

## Implemented Features (No Longer Pending)

| Feature | Status |
|---------|--------|
| ~~Tick bitmap caching~~ | Implemented in `_get_cached_tick_ranges()` |
| ~~V3-V3 Rust Möbius~~ | Implemented in `rust/src/optimizers/mobius_v3_v3.rs` |
| ~~PiecewiseMobiusSolver in unified interface~~ | Integrated via `ArbSolver` dispatch |

## Test Status

**599 tests passing, 9 skipped** (as of 2026-04-15)

Run tests:
```bash
uv run pytest tests/arbitrage/test_optimizers/ -x -q
```

## Source Files

| File | Purpose |
|------|---------|
| `solver.py` | Unified `ArbSolver` interface + all solvers |
| `mobius.py` | Möbius transformation optimizer |
| `batch_mobius.py` | Vectorized batch Möbius |
| `newton.py` | Newton's method for 2-hop V2 |
| `v2_v3_optimizer.py` | V2-V3 with tick prediction |
| `balancer_weighted.py` | Balancer Eq.9 solver |
| `rust/src/optimizers/mobius_v3_v3.rs` | V3-V3 Rust solver |
| `rust/src/optimizers/mobius_v3.rs` | V3 piecewise Rust solver |
| `rust/src/optimizers/mobius.rs` | Core Möbius Rust solver |
| `rust/src/optimizers/mobius_py.rs` | Python bindings for all Rust solvers |

## See Also

- [NEXT_STEPS.md](NEXT_STEPS.md) — Current status and recommended next steps
- [RESEARCH_HISTORY.md](RESEARCH_HISTORY.md) — Complete research history and technical details
- [Archive](archive/) — Historical planning documents
