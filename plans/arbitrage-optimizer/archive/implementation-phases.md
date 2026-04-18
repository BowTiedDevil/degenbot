# Implementation Phases

## Phase 1: Foundation ✅ COMPLETE

- [x] Create benchmark suite using existing fixtures
- [x] Establish baseline performance metrics
- [x] Implement CVXPY → Brent refinement pipeline
- [x] Add solution validation

**Deliverable**: Quantified baseline comparison

**Results**:
- Created `tests/arbitrage/test_optimizers/` with 5 modules
- Benchmark: Brent ~160μs, CVXPY ~1ms, Hybrid ~1.1ms
- 32 tests passing
- Brent is 6x faster for simple V2 arbitrage

---

## Phase 2: Numerical Improvements ✅ COMPLETE

- [x] Implement log-domain formulation
- [x] Implement unified scaling strategy
- [x] Benchmark numerical improvements
- [x] Add regression tests

**Deliverable**: Improved numerical stability and precision

**Results**:
- Created `LogDomainOptimizerBenchmark` with explicit log constraints
- Created `ScaledOptimizerBenchmark` with geometric mean normalization
- Added `TestNumericalStability` test class
- Key finding: CVXPY handles extreme values better than Brent in some cases
- **Critical Bug Fix**: Fixed Brent optimizer direction logic (ROE-based)
- After fix: both optimizers find identical profits within 0.3 bps

**Files Created**:
```
tests/arbitrage/test_optimizers/
├── __init__.py
├── benchmark_base.py        # BenchmarkResult, BenchmarkComparison, BenchmarkReport, BenchmarkSuite
├── brent_optimizer.py       # BrentOptimizerBenchmark, BrentOptimizerWithBracket
├── convex_optimizer.py      # ConvexOptimizerBenchmark, ConvexOptimizerWithRefinement
├── log_domain_optimizer.py  # LogDomainOptimizerBenchmark, ScaledOptimizerBenchmark
└── test_benchmark.py        # 32 tests
```

**Benchmark Results**:

| Optimizer | Mean Time | Relative Speed |
|-----------|-----------|----------------|
| Brent | 168 μs | 1.0x (baseline) |
| CVXPY | 1082 μs | 6.4x slower |
| Hybrid | 1250 μs | 7.4x slower |

---

## Phase 3: V3/V4 Research ✅ COMPLETE

- [x] Research piecewise approximation approaches
- [x] Prototype V3 convex relaxation
- [x] Evaluate hybrid V2/V3 approach
- [x] Benchmark approximation accuracy
- [x] Review CFMMRouter.jl paper and implementation
- [x] Implement dual decomposition method
- [x] Create numerical example walkthrough

**Deliverable**: V3/V4 feasibility assessment and prototype

**Results**:
- Piecewise V2 approximation: Each tick range becomes a virtual V2 pool
- **Key insight from paper**: V3 tick ranges are "bounded product" CFMMs with closed-form solutions
- Convex relaxation: Upper/lower bounds on V3 swap output
- Hybrid approach: CVXPY for V2, Brent for V3/V4
- **Better approach**: Dual decomposition with bounded product CFMMs

**Recommendation**: Use dual decomposition with bounded product CFMMs for V3.

See [`research/dual-decomposition.md`](research/dual-decomposition.md) for full details.

---

## Phase 4: Performance Tuning ✅ COMPLETE

- [x] Implement solver selection logic
- [x] Add warm starting for sequential solves
- [x] Optimize problem caching
- [x] Final benchmark comparison

**Deliverable**: Production-ready improved optimizer

**Results**:
- Created `performance_optimizer.py` with `select_solver()`, `ConvexProblemCache`, `ConvexOptimizerWithPerformance`
- Created `test_performance.py` with 20 tests

**Key Findings**:

| Metric | Brent | CVXPY | CVXPY+Warm Start |
|--------|-------|-------|-----------------|
| Avg solve time | 0.19ms | 1.1ms | 1.0ms (subsequent) |
| First solve | N/A | N/A | 12.5ms |
| Accuracy | Baseline | Within 0.3 bps | Within 0.3 bps |

**Performance Improvements**:
- Warm start: 12x faster on subsequent solves
- Cache retrieval: 1138x faster than problem creation
- All optimizers find identical profits

**Recommendation**: Brent remains the best choice for V2 arbitrage. CVXPY is valuable for research, V3 dual decomposition, multi-token routing, and sequential solves with warm starting.

---

## Phase 5: Parallelization Benchmark ✅ COMPLETE

- [x] Implement parallel solver wrapper (threading)
- [x] Benchmark single-threaded vs parallel
- [x] Evaluate CPU scaling (2, 4, 8, 16 cores)
- [x] Identify crossover point
- [x] Add parallel benchmark tests

**Critical Discovery**: Python's GIL prevents true parallel execution with threads. Both optimizers show degraded performance with threading due to GIL contention.

**Key Findings**:

| Metric | Brent | CVXPY |
|--------|-------|-------|
| Serial per-problem | 0.15ms | 1.3ms |
| Parallel (4 workers) | 0.20ms | 19.5ms |
| Parallel efficiency | 19% | 2% |
| Speedup vs serial | 0.89x | 0.07x |

---

## Phase 5b: Multiprocessing Benchmark ✅ COMPLETE

- [x] Implement worker functions for true multiprocessing
- [x] Measure process spawn overhead
- [x] Compare CVXPY vs Brent with warm start

**Critical Findings**:

| Metric | Brent | CVXPY | CVXPY+WS |
|--------|-------|-------|----------|
| Serial per-problem | 0.15ms | 1.09ms | 1.08ms |
| Parallel (4 workers) | 0.41ms | 2.47ms | 2.41ms |
| Speedup | 0.52x | 1.14x | 1.20x |
| Efficiency | 13% | 29% | 30% |

**Process Spawn Overhead**: ~26ms for 4 workers

**Key Insight**: Brent is SO fast (0.15ms/problem) that the spawn overhead dominates. CVXPY's higher per-problem cost allows actual speedup for 500+ problems.

---

## Phase 6: Closed-Form V2 (Newton's Method) ✅ COMPLETE

- [x] Research closed-form V2 arbitrage formulas
- [x] Implement Newton's method optimizer
- [x] Compare accuracy against Brent
- [x] Benchmark performance across price ratios
- [x] Add comprehensive test suite

**Deliverable**: Analytical V2 optimizer with 26x speedup over Brent

**Theoretical Foundation**: The optimal arbitrage satisfies the first-order condition `d(profit)/dx = dz/dy * dy/dx - 1 = 0`. Newton converges in 3-4 iterations because the function is smooth and well-behaved.

**Performance Comparison**:

| Price Ratio | Newton | Brent | Speedup | Profit Match |
|-------------|--------|-------|---------|--------------|
| 1% | 7.5μs | 202μs | 26.9x | ✓ |
| 2% | 7.5μs | 156μs | 20.8x | ✓ |
| 5% | 7.5μs | 189μs | 25.2x | ✓ |
| 10% | 7.5μs | 144μs | 19.2x | ✓ |
| 20% | 7.5μs | 136μs | 18.1x | ✓ |

**Files Created**:
- `closed_form.py` — `v2_optimal_arbitrage_newton()`, `ClosedFormOptimizer`
- `newton.py` — `NewtonV2Optimizer` (production)
- `test_closed_form.py` — 29 tests
- `run_all_benchmarks.py` — Comprehensive benchmark

---

## Phase 7: Production Hardening ✅ COMPLETE

- [x] Integrate `NewtonV2Optimizer` for V2-V2 arbitrage
- [x] Add vectorized batch solver for multi-path evaluation
- [x] Implement bounded product CFMM for V3 single-range
- [x] Add chain rule Newton for triangular arbitrage
- [x] Create hybrid optimizer with automatic method selection

**Production Optimizer Files**:
```
src/degenbot/arbitrage/optimizers/
├── __init__.py              # Exports all optimizers
├── base.py                  # OptimizerResult, OptimizerType
├── newton.py                # NewtonV2Optimizer (7.5μs, 26x faster)
├── vectorized_batch.py      # BatchNewtonOptimizer (~0.53μs/path)
├── batch_mobius.py          # BatchMobiusOptimizer (~0.14μs/path)
├── bounded_product.py       # BoundedProductCFMM
├── chain_rule.py            # ChainRuleNewtonOptimizer (~50μs)
├── mobius.py                # MobiusOptimizer (0.86μs Python, V2+V3)
├── hybrid.py                # HybridOptimizer (auto-selects)
├── gradient_descent.py      # GradientDescentOptimizer
├── multi_pool_gradient.py   # MultiPoolGradientDescentOptimizer
├── multi_token.py            # MultiTokenRouter (dual decomposition)
├── v2_v3_optimizer.py       # V2V3Optimizer (tick prediction)
└── v3_tick_predictor.py     # V3TickPredictor
```

**Rust Optimizer Files**:
```
rust/src/optimizers/
├── mod.rs                   # Module exports
├── mobius.rs                # MobiusSolver (f64, 0.19μs)
├── mobius_int.rs            # MobiusSolver (u256, 0.88μs, EVM-exact)
├── mobius_batch.rs          # MobiusBatchSolver (0.09μs/path)
├── mobius_v3.rs             # V3 tick range support
└── mobius_py.rs             # PyO3 Python bindings
```

---

## Phase 9: Multi-Token Routing ✅ COMPLETE

- [x] Implement `MultiTokenRouter` with dual decomposition
- [x] Implement `DualDecompositionSolver` with L-BFGS-B
- [x] Closed-form single-market solutions
- [x] Shadow price initialization from pool reserves

**Performance**: 1 path ~5ms, 5 paths ~8ms, 10 paths ~12ms

See [`research/dual-decomposition.md`](research/dual-decomposition.md) for algorithm details.

---

## Phase 10: Möbius Transformation ✅ COMPLETE

- [x] Research Möbius transformation approach for multi-hop V2 paths
- [x] Implement `MobiusV2Optimizer` with closed-form O(n) solution
- [x] Verify accuracy against scipy Brent and chain rule Newton
- [x] Benchmark performance across path lengths (2-50 pools)
- [x] Add comprehensive test suite (40 tests)

**Deliverable**: Exact closed-form optimizer for V2 multi-hop paths, 5-40x faster than chain rule Newton

**Performance**:

| Pools | Möbius (μs) | Brent (μs) | Speedup |
|-------|-------------|------------|---------|
| 2 | 1.35 | 57.90 | **43x** |
| 3 | 1.53 | 64.47 | **42x** |
| 5 | 1.82 | 78.60 | **43x** |
| 10 | 2.84 | 108.36 | **38x** |
| 20 | 4.50 | 177.57 | **40x** |

See [`research/mobius-transformation.md`](research/mobius-transformation.md) for full details.

---

## Phase 11: Möbius V3 Generalization ✅ COMPLETE

- [x] Generalize MobiusV2Optimizer to MobiusOptimizer (V2+V3 support)
- [x] Add V3TickRangeHop for bounded product CFMM as Möbius hop
- [x] Add estimate_v3_final_sqrt_price for range validation
- [x] Add solve_v3_candidates for multi-range V3 optimization
- [x] MobiusV2Optimizer = MobiusOptimizer (backward-compatible alias)
- 38 new V3 Möbius tests in `test_mobius_v3.py`

---

## Phase 12: Piecewise-Möbius with Explicit Tick Crossing ✅ COMPLETE

- [x] Validate Mobius V3 accuracy against V3 `compute_swap_step` integer math
- [x] Fix bugs: `to_hop_state()` double-counting, `estimate_v3_final_sqrt_price()` missing `* sqrt_p`
- [x] Research V3 tick crossing algebraic structure (additive vs compositional)
- [x] Implement explicit crossing computation: `TickRangeCrossing`, `V3TickRangeSequence`
- [x] Implement piecewise V3 swap: `piecewise_v3_swap()`
- [x] Implement piecewise optimizer: `MobiusOptimizer.solve_piecewise()` with golden section search
- [x] Add comprehensive test suite (39 tests across 3 files)

**Performance**:

| Optimizer | Typical Time | Method |
|----------|-------------|--------|
| V2V3Optimizer (Newton) | ~5-15ms | Iterative with tick prediction |
| Piecewise-Möbius (golden section) | ~25μs | ~25 iterations, bracketed search |
| Single-range Möbius (closed form) | ~5μs | Zero iterations, O(1) |

See [`research/mobius-transformation.md`](research/mobius-transformation.md) for full details.

---

## V3 Deep Dive: Tick Prediction & V2V3Optimizer ✅ COMPLETE

- [x] V3 tick crossing prediction via equilibrium estimation
- [x] Bounded product CFMM with closed-form optimal swaps
- [x] Price bounds filtering (eliminates 90%+ of tick ranges)
- [x] V2V3Optimizer with equilibrium estimation

See [`research/v3-bounded-product.md`](research/v3-bounded-product.md) for full details.

---

## Phase 8: Batch Möbius ✅ COMPLETE

- [x] Create batch_mobius.py with VectorizedMobiusSolver, SerialMobiusSolver, BatchMobiusOptimizer
- [x] Fix critical numpy view mutation bug (M *= r_in overwrote input hops_array)
- [x] Implement log-domain overflow handling (log-sum-exp for N, expm1 for profit)
- [x] Benchmark: 0.14μs/path Python vectorized (1000 paths), 4.4x faster than Batch Newton
- [x] Full test suite: 841 passing, 17 skipped

**Performance**:

| Paths | Serial Möbius | Vec Möbius | Vec Newton | Vec Möbius vs Newton |
|-------|--------------|------------|------------|---------------------|
| 100 | 320μs | 28μs | 53μs | 1.9x faster |
| 1000 | 3229μs | 140μs | 528μs | **3.8x faster** |
| 10000 | 32290μs (est) | 1400μs (est) | 5280μs (est) | **3.8x faster** |

---

## Phase 9: Rust Möbius ✅ COMPLETE

- [x] Create Rust f64 Möbius optimizer (mobius.rs) — 0.19μs (1021x faster than Brent)
- [x] Create Rust integer Möbius optimizer (mobius_int.rs) — 0.88μs (EVM-exact, uint256)
- [x] Create Rust batch Möbius optimizer (mobius_batch.rs) — 0.09μs/path at 1000 paths
- [x] Create Rust V3 Möbius optimizer (mobius_v3.rs) — V3 tick range support
- [x] PyO3 Python bindings (mobius_py.rs)
- [x] Not-profitable rejection: 0.32μs via exact K>M check
- [x] EVM simulation validation: integer Möbius profit matches contract exactly

**Performance**:

| Variant | Time | vs Brent |
|---------|------|----------|
| Rust Möbius (f64) | 0.19μs | **1021x faster** |
| Python Möbius | 0.86μs | **225x faster** |
| Rust Integer Möbius | 0.88μs | **220x faster** (EVM-exact) |
| Rust Batch (1000×2-hop) | 93μs | 0.09μs/path |
| Python Vec Batch (1000×2-hop) | 140μs | 0.14μs/path |

---

## Phase 10: Unified Solver Interface ✅ COMPLETE

- [x] Design `Hop`, `SolveInput`, `SolveResult` core types
- [x] Implement `MobiusSolver` with direct integer neighbor refinement (5.8μs)
- [x] Implement `NewtonSolver` with Möbius initial guess (4.5μs)
- [x] Implement `BrentSolver` scipy fallback (223μs)
- [x] Implement `ArbSolver` dispatcher (Mobius→Newton→Brent, 6.4μs)
- [x] Add `pool_to_hop()` and `pools_to_solve_input()` conversion utilities
- [x] 45 unit tests + 20 integration/timing tests

**Performance**:

| Solver | Median Time | vs Brent |
|--------|------------|----------|
| MobiusSolver | 5.8μs | 38x faster |
| NewtonSolver | 4.5μs | 49x faster |
| ArbSolver | 6.4μs | 35x faster |
| BrentSolver | 223μs | — |

---

## Phase 11: Cycle Class Integration ✅ COMPLETE

- [x] Add `USE_SOLVER_FAST_PATH` feature flag
- [x] Add `_build_solve_input_v2()` for V2 pool→Hop conversion
- [x] Add `_solver_fast_path_v2_v2()` before CVXPY
- [x] Add `_solver_fast_path_mixed()` before Brent
- [x] Integrate into all 9 `_calculate_*` methods
- [x] V2 buy-pool support + V3 sell-pool virtual reserves
- [x] 14 integration tests, all passing

**Key Design Decisions**:
- Feature flag allows instant rollback to Brent
- Solver optimizes in input_token space, converts to forward_token via buy pool simulation
- V3 sell-pool uses virtual reserves (approximate; validated by pool swap methods)
- V3 buy-pool conservatively skipped (falls back to Brent)

---

## Phase 12+: Future (Pending)

- [ ] PiecewiseMobiusSolver in unified interface (~25μs for V3 multi-range with crossing)
- [ ] V3 buy-pool support in solver fast-path
- [ ] Gas cost modeling in objective function
- [ ] Tick bitmap caching (production) — 100ms+ saved per V3 optimization
- [ ] Pre-compute tick transition tables for hot V3 pools
- [ ] L-BFGS-B for dual decomposition — 5-10x faster convergence
- [ ] Slippage bounds and deadline checks — MEV protection
- [ ] Telemetry for optimizer performance monitoring
- [ ] GPU acceleration for massive batch processing (CuPy) — 100x+ at 10K+ paths
- [ ] ML for optimal input prediction — research
- [ ] V3-V3 Rust Möbius — extend piecewise-Möbius to Rust

---

## Phase 13: Closed-Form N-Token Balancer Weighted Pool ✅ COMPLETE

Based on "Closed-form solutions for generic N-token AMM arbitrage" by Willetts & Harrington (2024), arXiv:2402.06731.

- [x] Implement Equation 9 closed-form solution per trade signature
- [x] Fix critical `d_i` indicator bug (`I_{s_i=1}` = 1 for deposit, 0 for withdraw, NOT -1/+1)
- [x] Implement 18-decimal reserve upscaling for mixed-decimal tokens
- [x] Add `BalancerMultiTokenState` with decimals field and upscaling/descaling
- [x] Add `BalancerMultiTokenHop` to unified solver interface
- [x] Add `BalancerMultiTokenSolver` to ArbSolver dispatch chain
- [x] Add `PoolInvariant.BALANCER_MULTI_TOKEN` and `SolverMethod.BALANCER_MULTI_TOKEN`
- [x] Implement integer refinement with token-unit profit computation
- [x] 30 passing tests in `test_balancer_weighted.py`
- [x] Correctness verified: zero-fee equilibrium → zero trades, direction matches economic incentive, invariant preserved

**Performance**:

| Pool Size | Single Eq.9 | Full Solver | Signatures |
|-----------|-------------|-------------|------------|
| N=3 | 3.9 μs | 576 μs | 12 |
| N=4 | 3.4 μs | 1.3 ms | 50 |
| N=5 | 3.9 μs | 2.9 ms | 180 |

**Key Implementation Details**:
1. `d_i = I_{s_i=1}` — indicator function (1 for deposit, 0 for withdraw)
2. Reserves upscaled to 18-decimal before formula (Balancing Vault convention)
3. Trades descaled to native token units; profit in numéraire using token-unit amounts
4. Market prices in common numéraire (e.g., USD)

See [`docs/balancer_solver_implementation.md`](../../docs/balancer_solver_implementation.md) for full details.
