# Status

## Test Suite

**599 tests passing, 9 skipped** (verified 2026-04-15)

## Phase Completion

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | Foundation (benchmark suite, baseline metrics) | ✅ COMPLETE |
| 2 | Numerical Improvements (log-domain, scaling) | ✅ COMPLETE |
| 3 | V3/V4 Research (dual decomposition, bounded product CFMM) | ✅ COMPLETE |
| 4 | Performance Tuning (solver selection, caching, warm start) | ✅ COMPLETE |
| 5 | Parallelization Benchmark (threading, GIL findings) | ✅ COMPLETE |
| 5b | Multiprocessing Benchmark (ProcessPoolExecutor) | ✅ COMPLETE |
| 6 | Closed-Form V2 (Newton's method, 26x faster) | ✅ COMPLETE |
| 7 | Production Hardening (Newton, batch, chain rule, hybrid) | ✅ COMPLETE |
| 9 | Multi-Token Routing (dual decomposition, L-BFGS-B) | ✅ COMPLETE |
| 10 | Möbius Transformation (V2 multi-hop, 225x faster) | ✅ COMPLETE |
| 11 | Möbius V3 Generalization (V3 single-range, O(1)) | ✅ COMPLETE |
| 12 | Piecewise-Möbius (V3 multi-range with crossing, ~25μs) | ✅ COMPLETE |
| Batch | Vectorized Batch Möbius (NumPy ~0.14μs/path, Rust ~0.09μs/path) | ✅ COMPLETE |
| Rust | Rust Möbius (f64: 0.19μs, integer: 0.88μs EVM-exact) | ✅ COMPLETE |
| Solver | Unified ArbSolver interface + cycle class integration | ✅ COMPLETE |
| Balancer | Closed-form N-token Balancer weighted pool (Eq.9, ~3μs/signature) | ✅ COMPLETE |

V3 Deep Dive (tick prediction, V2V3Optimizer) also complete.

## Solver Integration Details

### Unified Interface (`solver.py`)

| Type | Description |
|------|-------------|
| `Hop` | Frozen dataclass: reserve_in, reserve_out, fee, optional V3 fields |
| `SolveInput` | Frozen: tuple of Hops + optional max_input |
| `SolveResult` | Frozen: optimal_input, profit, success, iterations, method, solve_time_ns |
| `SolverMethod` | Enum: MOBIUS, NEWTON, PIECEWISE_MOBIUS, BRENT, BALANCER_MULTI_TOKEN |
| `Solver` | ABC: `solve(input) -> SolveResult`, `supports(input) -> bool` |
| `MobiusSolver` | Zero-iteration closed-form, ~5.8μs (V2+V3 single-range) |
| `NewtonSolver` | Newton with Möbius initial guess, ~4.5μs (2-hop V2) |
| `BrentSolver` | scipy fallback, ~223μs (all pool types) |
| `ArbSolver` | Dispatcher: Mobius → Newton → Brent |

### Cycle Class Integration

- `USE_SOLVER_FAST_PATH = True` feature flag
- `_build_solve_input_v2()` — builds SolveInput from V2 pool reserves
- `_solver_fast_path_v2_v2()` — V2-V2 specific fast-path (before CVXPY)
- `_solver_fast_path_mixed()` — generic fast-path for any pool-type pair (before Brent)
- Integrated into ALL 9 `_calculate_*` methods
- Feature flag allows instant rollback if issues found

### Timing (Production Integration)

| Solver | Median Time | vs Brent | Notes |
|--------|------------|----------|-------|
| MobiusSolver | 5.8μs | **38x faster** | Zero iterations, integer neighbor check |
| NewtonSolver | 4.5μs | **49x faster** | 3-4 iterations with Möbius initial guess |
| ArbSolver (dispatch) | 6.4μs | **35x faster** | Includes dispatch overhead |
| BrentSolver | 223μs | — | scipy baseline |

### Current Limitations

- V3/V4 virtual reserves are approximate (single tick range); result validated by pool swap methods
- PiecewiseMobiusSolver not yet implemented in unified interface

### Balancer Multi-Token Solver

Based on Willetts & Harrington (2024) "Closed-form solutions for generic N-token AMM arbitrage" (arXiv:2402.06731).

| Component | Description |
|-----------|-------------|
| `BalancerMultiTokenHop` | N-token pool state: reserves, weights, fee, decimals, market_prices |
| `BalancerMultiTokenSolver` | Evaluates all trade signatures, picks best |
| `BalancerWeightedPoolSolver` | Core solver using Equation 9 (closed-form per signature) |
| `PoolInvariant.BALANCER_MULTI_TOKEN` | Distinguishes from pairwise hops |
| `SolverMethod.BALANCER_MULTI_TOKEN` | Method tag in SolveResult |

**Key implementation details**:
- `d_i = I_{s_i=1}` (indicator: 1 for deposit, 0 for withdraw) — NOT -1/+1
- All reserves upscaled to 18-decimal before applying Equation 9 (Balancing Vault convention)
- Trades descaled back to native token units for profit computation
- Market prices must be in a common numéraire

**Performance**:

| Pool Size | Single Eq.9 | Full Solver | Signatures |
|-----------|-------------|-------------|------------|
| N=3 | 3.9 μs | 576 μs | 12 |
| N=4 | 3.4 μs | 1.3 ms | 50 |
| N=5 | 3.9 μs | 2.9 ms | 180 |

Single-signature evaluation at ~3 μs is **4× faster** than the paper's reported ~12 μs.

## Pending Items

| Item | Effort | Notes |
|------|--------|-------|
| ~~Gas cost modeling~~ | ~~Medium~~ | ~~REJECTED - out of scope for optimizer~~ |
| ~~Tick bitmap caching~~ | ~~Medium~~ | ~~IMPLEMENTED - already in _get_cached_tick_ranges()~~ |
| L-BFGS-B for dual decomposition | Medium | 5-10x faster convergence |
| GPU acceleration (CuPy) | High | 100x+ for massive batch |
| ML for optimal input prediction | High | Research project |
| Slippage bounds & deadline checks | Low | MEV protection |
| Telemetry for optimizer performance | Low | Monitoring |
| Pre-compute tick transition tables | Medium | Hot V3 pools |
| V3-V3 Rust Möbius | High | Extend piecewise-Möbius to Rust | ✅ COMPLETE |
| PiecewiseMobiusSolver in unified interface | Medium | V3 multi-range with crossing | ✅ COMPLETE |
| V3 buy-pool support in fast-path | Medium | ✅ COMPLETE (via pool_state_to_hop virtual reserves) |
| MobiusSolver multi-range dispatch fix | Low | ✅ COMPLETE (rejects has_multi_range=True) |

## Implementation Priority (Revised)

| Priority | Improvement | Expected Impact | Effort | Status |
|----------|-------------|-----------------|--------|--------|
| 1 | Unified solver + cycle integration | 35-49x faster (validated) | Done | ✅ COMPLETE |
| ~~2~~ | ~~Gas cost modeling~~ | ~~REJECTED~~ | ~~Medium~~ | ~~Explicitly rejected - out of scope~~ |
| 3 | Tick bitmap caching | 100ms+ saved | Medium | Pending |
| 4 | PiecewiseMobiusSolver | ~9μs for V3 crossing | Done | ✅ COMPLETE |
| 4b | V3-V3 Rust solver | ~10-50μs for V3-V3 crossing | Done | ✅ COMPLETE |
| 5 | V3/V4 buy-pool in fast-path | All pool types both directions | Done | ✅ COMPLETE |
| 6 | L-BFGS-B for dual decomposition | 5-10x faster | Medium | Pending |
| 7 | GPU acceleration (CuPy) | 100x+ for massive batch | High | Pending |
| 8 | ML for optimal input prediction | Research | High | Pending |

### Superseded / Tested & Rejected

| Item | Reason |
|------|--------|
| Analytical quartic solution | Superseded by Möbius (exact, O(n)) |
| Piecewise V3 convex approximation | Superseded by Piecewise-Möbius |
| Adaptive initial guess (Newton) | Overhead > savings at 7.5μs scale |
| Smart bracket (Brent) | scipy ignores bracket parameter |
| CVXPY for V2 production | 7x slower than Brent; Möbius is 1021x faster |
| Thread parallelism (Python) | GIL prevents speedup; use Rust or multiprocessing |
| Serial Newton for batch | Superseded by vectorized Möbius (4.4x faster at 1000 paths) |
| Golden section integer refinement (Möbius) | Overkill: 25 iters × simulation = ~40μs. Replaced by direct ±1 neighbor check = ~5.8μs |

## Performance Results (V2-V2 Single Path)

| Optimizer | Mean Time | vs Brent | Use Case |
|-----------|-----------|----------|----------|
| Rust Möbius (f64) | 0.19μs | **1021x faster** | V2 all paths (fastest) |
| Python Möbius | 5.8μs | **38x faster** | V2 all paths (unified solver) |
| Newton (2-hop) | 4.5μs | **49x faster** | V2-V2 fallback |
| ArbSolver (dispatch) | 6.4μs | **35x faster** | Auto-selection |
| 3-hop Möbius | 11.7μs | **14x faster** | Zero iterations, O(n) recurrence |
| Brent (baseline) | 223μs | — | V2-V3, V3-V3 |
| CVXPY | 1.3ms | 7x slower | Research only |

## Performance Results (V2 Multi-Hop)

| Hops | Python Möbius | Rust Möbius | Brent | Rust vs Brent |
|------|--------------|------------|-------|--------------|
| 2 | 5.8μs | 0.19μs | ~58μs | 305x faster |
| 3 | 1.5μs | 0.21μs | ~64μs | 305x faster |
| 5 | 1.2μs | 0.31μs | ~79μs | 255x faster |

## Performance Results (Batch 1000 paths, 2-hop)

| Optimizer | Total Time | Per-Path | vs Python Serial |
|-----------|-----------|----------|-----------------|
| Rust Batch Möbius | 93μs | 0.09μs | **35x** |
| Rust Vectorized Möbius | 104μs | 0.10μs | **31x** |
| Python Vectorized Möbius | 140μs | 0.14μs | **23x** |
| Python Vectorized Newton | 528μs | 0.53μs | 6x |
| Python Serial Möbius | 3229μs | 3.2μs | — |

## Performance Results (V3)

| Optimizer | Time | Method |
|----------|------|--------|
| Möbius V3 single-range | ~5μs | Zero iterations, O(1) closed-form |
| Möbius solve_v3_candidates | ~5-15μs | Checks 1-3 ranges |
| Piecewise-Möbius (crossing) | ~25μs | ~25 iterations, golden section |
| V2V3Optimizer (Newton) | ~5-15ms | Iterative with tick prediction |
| Brent V3-V3 | ~390μs | 73 iterations with tick logic |

## Key Discoveries

1. **Möbius is optimal for V2** — Zero iterations, O(1) for single-path, O(n) for multi-hop. Python 38x faster, Rust 1021x faster than Brent
2. **Rust Möbius hits sub-microsecond** — 0.19μs for f64, 0.88μs for integer (EVM-exact)
3. **Integer Möbius gives EVM-exact results** — uint256 arithmetic, byte-perfect match with contract simulation. Not-profitable rejection at 0.32μs via exact K>M check
4. **Vectorized Möbius beats vectorized Newton** — 4.4x faster at 1000 paths (0.14 vs 0.53 μs/path)
5. **Rust batch Möbius at 0.09 μs/path** — 35x faster than Python serial
6. **Unified solver interface works** — ArbSolver dispatches Mobius→Newton→Brent with 6.4μs median for V2-V2 (35x faster than Brent)
7. **Feature flag enables safe rollout** — `USE_SOLVER_FAST_PATH` allows instant rollback to Brent
8. **Python GIL limits parallelization** — Threading doesn't help; multiprocessing has ~26ms spawn overhead
9. **Brent is still best for V3-V3 complex fallback** — V3-V3 Rust solver handles most cases; Brent remains ultimate fallback for edge cases
10. **Piecewise-Möbius handles V3 crossing** — ~9μs with Rust, 200x faster than V2V3Optimizer
11. **V3-V3 has a piecewise solution** — Enumerate (k1,k2) ending range combos + golden section per candidate. Both single-range: ~0.19μs (Möbius). Multi-range: ~10-50μs.
11. **CVXPY's value is research** — Useful for dual decomposition, parameter sweeps, problem structure
12. **V3 tick crossings are additive, not compositional** — Cannot compose into Möbius; use piecewise approach
13. **Crossing amounts are FIXED** — Independent of total input; enables piecewise-Möbius with golden section search
14. **Float64 is sufficient for V2** — Newton and Möbius find exact solutions for reserves up to uint128 scale (87 bits); ratios preserve relative precision
15. **Direct integer neighbor check is better than golden section** — ±1 check around float optimum: 5.8μs vs 47μs (25-iter golden section). Same accuracy, 8x faster.
16. **Closed-form exists for N-token Balancer weighted pools** — Equation 9 from Willetts & Harrington (2024) gives optimal basket trades per signature in ~3μs. The `d_i = I_{s_i=1}` indicator (1 for deposit, 0 for withdraw) is critical — using `d_i = signature[i]` (giving -1) inverts all trade signs.
17. **Decimal scaling is essential for Balancer formula** — Reserves with different decimal precisions (ETH=18, USDC=6) must be upscaled to 18-decimal before applying Equation 9, or the invariant product calculation produces wildly incorrect results (1e21 vs 1e12 magnitude mismatch).
18. **Trade signatures enumerate all possible deposit/withdraw patterns** — N=3 → 12 signatures, N=4 → 50, N=5 → 180. The formula naturally rejects uneconomic signatures (gives wrong-sign trades that fail validation).
