# Current State

## Brent Optimizer (scipy `minimize_scalar`)

**Location**:
- `src/degenbot/arbitrage/uniswap_lp_cycle.py` — Generic LP cycle base class
- `src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py` — Two-pool cycle implementation

**Characteristics**:
- Scalar optimization via Brent's method (derivative-free)
- Works with all pool types: V2, V3, V4, mixed
- Each iteration calls pool calculation methods (`calculate_tokens_out_from_tokens_in`, etc.)
- Typical convergence: 15-30 iterations to reach `xatol=1.0`
- Slow threshold: `SLOW_ARB_CALC_THRESHOLD = 0.25s`

**Current Benchmark Times**:
- V2-V2: 194μs (33 iterations)
- V3-V3: 390μs (73 iterations with tick logic)

**Limitations**:
- Iterative approach requires multiple pool calculations
- No global optimality guarantee for non-convex problems
- Performance degrades with complex pool types (V3/V4 tick calculations)
- Superseded by Möbius for V2 paths (225-1021x slower)

## Möbius Optimizer (Production Default for V2)

**Location**:
- `src/degenbot/arbitrage/optimizers/mobius.py` — Python `MobiusOptimizer`
- `src/degenbot/arbitrage/optimizers/batch_mobius.py` — Python `BatchMobiusOptimizer`
- `rust/src/optimizers/mobius.rs` — Rust f64 `MobiusSolver`
- `rust/src/optimizers/mobius_int.rs` — Rust u256 `MobiusSolver`
- `rust/src/optimizers/mobius_batch.rs` — Rust batch `MobiusBatchSolver`

**Characteristics**:
- Closed-form O(1) for single-path, O(n) for multi-hop
- Zero iterations regardless of path length
- Free profitability check (K/M > 1) without simulation
- Handles V2, V3 single-range, V3 multi-range (piecewise)
- Vectorized batch: 0.14μs/path (Python), 0.09μs/path (Rust) at 1000 paths

**Current Benchmark Times**:

| Variant | Time | vs Brent | Notes |
|---------|------|----------|-------|
| Rust Möbius (f64) | 0.19μs | **1021x faster** | Fastest overall |
| Python Möbius | 0.86μs | **225x faster** | Best pure-Python |
| Rust Integer Möbius | 0.88μs | **220x faster** | EVM-exact (uint256) |
| Rust Batch (1000×2-hop) | 93μs total | — | 0.09μs/path |
| Python Vectorized (1000×2-hop) | 140μs total | — | 0.14μs/path |

**Limitations**:
- V2 and V3 single-range only for closed-form
- V3 multi-range with tick crossings requires piecewise approach (~25μs)
- V3-V3 complex cases still need Brent

## Newton V2 Optimizer

**Location**:
- `src/degenbot/arbitrage/optimizers/newton.py` — `NewtonV2Optimizer`
- `src/degenbot/arbitrage/optimizers/vectorized_batch.py` — `BatchNewtonOptimizer`

**Characteristics**:
- Newton's method for V2 constant product, 3-4 iterations
- 26x faster than Brent
- Superseded by Möbius for speed, but useful as fallback

**Current Benchmark Times**:
- Single path: 7.5μs
- Batch (1000 paths): 528μs total (0.53μs/path)

## CVXPY Convex Optimizer

**Location**:
- `src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py` — Two-pool V2 implementation
- `src/degenbot/arbitrage/uniswap_multipool_cycle_testing.py` — Multi-pool V2 implementation
- `tests/test_cvxpy.py` — Experimental tests

**Characteristics**:
- Convex optimization via interior point method (CLARABEL solver)
- DPP-compliant: problem compiled once, parameters updated per-instance
- Uses geometric mean for constant product invariant: `geo_mean(reserves) >= k`
- Reserve compression to `[0.0, 1.0]` range for numerical stability
- Single solve call, no iteration overhead

**Current Benchmark Times**:

| Mode | Time |
|------|------|
| No warm start | 1.2ms |
| Warm start | 1.1ms |
| Problem creation | 1.4ms |
| Cache hit | 0.001ms (974x faster than creation) |

**Limitations**:
- V2-only (constant product AMM)
- 7x slower than Brent for V2 serially
- GIL prevents parallel speedup in Python
- Numerical precision issues with disparate reserve magnitudes
- No gas cost modeling
- Superseded by Möbius for V2 production (Möbius is 1021x faster than Brent)

**CVXPY's Remaining Value**:
- Research and prototyping
- V3/V4 dual decomposition approach
- Multi-token routing problems
- Problem structure analysis
- Parameter sweeps

## Benchmark Comparison (V2-V2 Single Path)

| Metric | Brent | Möbius (Py) | Möbius (Rust f64) | Möbius (Rust u256) | Newton | CVXPY |
|--------|-------|-------------|-------------------|---------------------|--------|-------|
| Solve time | 194μs | 0.86μs | 0.19μs | 0.88μs | 7.5μs | 1300μs |
| vs Brent | — | 225x faster | 1021x faster | 220x faster | 26x faster | 7x slower |
| Profit match | Baseline | Identical | Identical | EVM-exact | Identical | <0.3 bps |
| Iterations | 33 | 0 | 0 | 0 | 3-4 | N/A |

## Benchmark Comparison (Batch 1000 paths, 2-hop)

| Metric | Python Serial | Python Vec Möbius | Python Vec Newton | Rust Batch Möbius |
|--------|--------------|-------------------|-------------------|-------------------|
| Total time | 3229μs | 140μs | 528μs | 93μs |
| Per-path | 3.2μs | 0.14μs | 0.53μs | 0.09μs |
| vs serial | — | 23x faster | 6x faster | 35x faster |
