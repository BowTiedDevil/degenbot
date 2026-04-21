# Production Guide: Arbitrage Optimizers

How to use the arbitrage optimization system in production.

## Quick Reference

| Arbitrage Type | Method | Time | Speedup |
|----------------|--------|------|---------|
| V2-V2 single | Rust Möbius (via ArbSolver) | 0.19μs | **1021x** vs Brent |
| V2-V2 batch 1000 | Rust Batch Möbius | 0.09μs/path | **2155x** |
| V2 multi-hop (3+) | Rust Möbius (via ArbSolver) | ~1.5μs | **129x** |
| V3 single-range | Möbius closed-form | ~0.86μs | **453x** |
| V3 multi-range (1 V3 hop) | Piecewise-Möbius (Rust) | **~9μs** | **43x** |
| V3-V3 both single-range | V3-V3 Rust solver | **~0.19μs** | **2053x** |
| V3-V3 multi-range | V3-V3 Rust solver | **~10-50μs** | **8-39x** |
| V3-V3 complex (fallback) | Brent | ~390μs | baseline |
| Balancer N=3 | Eq.9 closed-form | ~576μs | — |

## Unified Interface (Recommended)

The `ArbSolver` automatically selects the best method via a single Rust dispatch call:

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
    print(f"Method used: {result.method}")  # MOBIUS, PIECEWISE_MOBIUS, or BRENT
```

### Dispatch Flow

```
ArbSolver.solve()
    ├── RustArbSolver.solve_raw()  ← flat int array (default, V2/single-range V3)
    │     └── Möbius + U256 integer refinement (EVM-exact)
    ├── RustArbSolver.solve()      ← object-based (V3 multi-range, V3-V3)
    │     ├── Pure V2/V3 single-range → Möbius
    │     ├── V3 multi-range (1 hop) → solve_v3_sequence
    │     └── V3-V3 (2 hops) → solve_v3_v3
    │   Then (int hops): mobius_refine_int → U256 integer refinement (EVM-exact, merged into RustArbSolver.solve())
    ├── PiecewiseMobiusSolver  ← Python fallback for V3
    ├── SolidlyStableSolver
    ├── BalancerMultiTokenSolver
    └── BrentSolver
```

Rust returns `supported=False` for unsupported hop types (Solidly, Balancer, Curve), triggering Python fallback.

For V2/single-range V3 paths (the common case), `ArbSolver` uses `RustArbSolver.solve_raw()` which accepts a flat int list, avoiding Python object construction. For V3 multi-range and V3-V3 paths, it falls back to `RustArbSolver.solve()` with `RustIntHopState` or float tuple objects. Both paths produce truly EVM-exact `optimal_input` and `profit` values for Möbius results, matching on-chain swap math exactly. Non-Möbius results (V3 multi-range, V3-V3) use `int()` conversion of the float result.

### Pool State Cache (Fastest Path)

For the fastest solve path, register pool states in a Rust-side cache at update time, then solve by pool ID reference. This eliminates all Python object construction on the solve path.

```python
from degenbot.arbitrage.optimizers.solver import ArbSolver
from fractions import Fraction

solver = ArbSolver()

# Register pools at state update time (once per block)
fee = Fraction(3, 1000)  # 0.3%
pool_id_0 = solver.register_pool(reserve_in=1_500_000_000_000, reserve_out=800_000_000_000_000_000, fee=fee)
pool_id_1 = solver.register_pool(reserve_in=1_000_000_000_000_000_000, reserve_out=2_000_000_000_000, fee=fee)

# Solve by pool ID (no Python objects on hot path)
result = solver.solve_cached([pool_id_0, pool_id_1])
if result.success:
    print(f"Optimal: {result.optimal_input}, Profit: {result.profit}")

# Update pool state on new block
solver.update_pool(pool_id_0, new_reserve_in, new_reserve_out, fee)
```

**Performance**: `solve_cached()` is **~2.5μs** for V2-V2 (vs ~3.9μs for standard `solve()`), a **1.5x** speedup. Rust-only computation is ~0.73μs.

**Important**: Pool states must be registered *before* calling `solve_cached()`. The cache stores one reserve orientation per pool ID. If a pool appears in different directions across paths, register it twice with different IDs.

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
- Dispatched automatically by `RustArbSolver` when 2 V3 hops detected

## Recommended Approach by Use Case

| Use Case | Recommended Approach |
|----------|---------------------|
| **Production V2** | `ArbSolver` (Rust Möbius dispatch, 0.19μs) |
| **Production V2-V3** | `ArbSolver` (Rust piecewise-Möbius, ~9μs) |
| **Production V3-V3** | `ArbSolver` (Rust V3-V3 solver, ~0.19μs–50μs), Brent fallback |
| **MEV bot** | `ArbSolver.solve_cached()` (pool cache, ~2.5μs, no Python objects on solve path) |
| **Backtesting** | Vectorized Batch Möbius for batch efficiency |
| **Balancer baskets** | `BalancerMultiTokenSolver` with Eq.9 |

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

## Feature Flags

### Solver Fast Path

The unified solver is behind a feature flag for safe rollout:

```python
# In cycle class
USE_SOLVER_FAST_PATH = True  # Enable fast path
USE_SOLVER_FAST_PATH = False  # Fallback to Brent only
```

### Merged Integer Refinement

When enabled (default), `ArbSolver._try_rust_solve` passes `RustIntHopState` objects
to `RustArbSolver.solve()`, which does float solve + U256 integer refinement in a
single Rust call. When disabled, falls back to the old two-step approach (float tuples
then separate `py_mobius_refine_int` call).

```python
# Environment variable
DEGENBOT_MERGED_INT_REFINEMENT=   # Disabled (empty string)
DEGENBOT_MERGED_INT_REFINEMENT=1 # Enabled (default)
```

```python
# Module-level constant
from degenbot.arbitrage.optimizers.solver import USE_MERGED_INT_REFINEMENT
USE_MERGED_INT_REFINEMENT = False  # Disable
```

### Raw Array Marshalling

When enabled (default), `ArbSolver._try_rust_solve` passes a flat `list[int]` to
`RustArbSolver.solve_raw()` instead of creating `RustIntHopState` Python objects.
This eliminates Python object construction overhead on the hot path (~0.3μs saved for 2 hops).
When disabled, falls back to `RustIntHopState` objects via `RustArbSolver.solve()`.

Only applies to ConstantProduct and single-range BoundedProduct hops.
V3 multi-range paths always use the object-based `RustArbSolver.solve()` path.

```python
# Environment variable
DEGENBOT_RAW_ARRAY_MARSHALLING=   # Disabled (empty string)
DEGENBOT_RAW_ARRAY_MARSHALLING=1 # Enabled (default)
```

```python
# Module-level constant
from degenbot.arbitrage.optimizers.solver import USE_RAW_ARRAY_MARSHALLING
USE_RAW_ARRAY_MARSHALLING = False  # Disable
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
| Pool cache requires manual register/update | Call `register_pool()`/`update_pool()` at state update time |
| Pool cache stores one reserve orientation per ID | Register same pool twice with different IDs for different directions |
| `solve_cached` ~1.87μs Python overhead | Item #21: return lighter-weight result or direct Rust access |

## Rejected Features

| Feature | Reason |
|---------|--------|
| ~~Gas cost modeling~~ | Out of scope for optimizer - should be handled at execution layer |

## Test Status

**694 tests passing, 9 skipped** (as of 2026-04-17)

Run tests:
```bash
uv run pytest tests/arbitrage/test_optimizers/ -x -q
```

## Source Files

| File | Purpose |
|------|---------|
| `solver.py` | `ArbSolver` thin Rust wrapper + Python fallback solvers |
| `mobius.py` | Möbius transformation optimizer |
| `batch_mobius.py` | Vectorized batch Möbius |
| `newton.py` | Newton's method for 2-hop V2 |
| `v2_v3_optimizer.py` | V2-V3 with tick prediction |
| `balancer_weighted.py` | Balancer Eq.9 solver |
| `rust/src/optimizers/mobius_int.rs` | U256 EVM-exact integer Möbius + `mobius_refine_int` |
| `rust/src/optimizers/mobius_v3_v3.rs` | V3-V3 Rust solver |
| `rust/src/optimizers/mobius_v3.rs` | V3 piecewise Rust solver |
| `rust/src/optimizers/mobius.rs` | Core Möbius Rust solver |
| `rust/src/optimizers/mobius_py.rs` | Python bindings: `RustArbSolver` (with merged int refinement + `solve_raw`), `RustPoolCache`, `py_mobius_refine_int`, etc. |

## See Also

- [NEXT_STEPS.md](NEXT_STEPS.md) — Current status and recommended next steps
- [RESEARCH_HISTORY.md](RESEARCH_HISTORY.md) — Complete research history and technical details
- [Archive](archive/) — Historical planning documents
