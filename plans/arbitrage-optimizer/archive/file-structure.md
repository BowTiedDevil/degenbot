# File Structure & Test Commands

## Source Code

```
src/degenbot/arbitrage/
├── optimizers/                      # Production optimizer implementations
│   ├── __init__.py
│   ├── base.py                      # Abstract optimizer interface
│   ├── batch_mobius.py              # Vectorized batch Möbius (NumPy, log-domain overflow handling)
│   ├── bounded_product.py           # Bounded product CFMM for V3 single-range
│   ├── chain_rule.py                # Chain rule Newton for triangular arbitrage
│   ├── gradient_descent.py          # Gradient descent optimizer
│   ├── hybrid.py                    # HybridOptimizer: auto-selects solver by pool type
│   ├── mobius.py                    # MobiusOptimizer: V2+V3, single & multi-range, piecewise
│   ├── multi_pool_gradient.py       # Multi-pool gradient descent
│   ├── multi_token.py               # MultiTokenRouter: dual decomposition multi-path
│   ├── newton.py                    # NewtonV2Optimizer: V2-V2 single path
│   ├── v2_v3_optimizer.py           # V2V3Optimizer: V2-V3 with tick prediction
│   ├── v3_tick_predictor.py         # V3TickPredictor: tick crossing prediction
│   ├── vectorized_batch.py          # BatchNewtonOptimizer: vectorized batch Newton
│   ├── balancer_weighted.py          # Closed-form N-token Balancer weighted pool (Eq.9)
│   ├── balancer_weighted_v2.py        # Grid-search Balancer fallback (superseded)
│   └── solver.py                     # Unified ArbSolver + Hop/SolveInput/SolveResult types
│
├── uniswap_lp_cycle.py              # Modified: Use optimizer interface
├── uniswap_2pool_cycle_testing.py   # Modified: Use optimizer interface
└── uniswap_multipool_cycle_testing.py

rust/src/optimizers/                  # Rust optimizer implementations (PyO3)
├── mod.rs
├── mobius.rs                        # MobiusSolver (f64, 0.19μs)
├── mobius_int.rs                    # MobiusSolver (u256, 0.88μs, EVM-exact)
├── mobius_batch.rs                  # MobiusBatchSolver (0.09μs/path)
├── mobius_v3.rs                     # V3 tick range support
└── mobius_py.rs                     # PyO3 Python bindings
```

## Tests

```
tests/arbitrage/
├── integration/                     # RPC-dependent tests (slow)
│   ├── __init__.py
│   ├── test_uniswap_2pool_cycle.py  # Fork tests (@pytest.mark.fork)
│   ├── test_uniswap_curve_cycle.py
│   └── test_uniswap_lp_cycle.py
│
├── test_optimizers/                 # Unit tests (fast, no RPC)
│   ├── __init__.py
│   ├── benchmark_base.py            # Base classes for benchmarks
│   ├── brent_optimizer.py           # Brent optimizer
│   ├── closed_form.py              # Phase 6: Newton's method
│   ├── convex_optimizer.py         # CVXPY optimizer
│   ├── log_domain_optimizer.py     # Log-domain and scaled optimizers
│   ├── test_mobius_optimizer.py    # Phase 10: Mobius tests + benchmarks
│   ├── test_mobius_v3.py           # Phase 11: V3 tick range hop tests
│   ├── test_mobius_v3_accuracy.py  # Phase 12: V3 accuracy vs compute_swap_step
│   ├── test_piecewise_mobius.py    # Phase 12: Piecewise-Mobius crossing + optimizer tests
│   ├── test_batch_mobius.py        # Batch Möbius: vectorized, serial, overflow, benchmarks
│   ├── performance_optimizer.py    # Phase 4: Performance optimizations
│   ├── parallel_benchmark.py       # Phase 5: Parallelization benchmark
│   ├── multiprocess_benchmark.py   # Phase 5b: Multiprocessing benchmark
│   ├── v3_approximation.py         # V3 piecewise approximation
│   ├── dual_decomposition.py       # Dual decomposition method
│   ├── numerical_example.py        # Step-by-step dual decomposition example
│   ├── final_benchmark.py          # Phase 4: Comprehensive benchmark
│   ├── run_parallel_benchmark.py   # Phase 5: Parallel benchmark script
│   ├── run_all_benchmarks.py       # Phase 6: All optimizers benchmark
│   ├── vectorized_batch.py         # Vectorized batch evaluation (NumPy)
│   ├── run_vectorized_benchmark.py # Vectorized benchmark script
│   ├── test_benchmark.py           # Benchmark tests
│   ├── test_closed_form.py         # Phase 6: Newton method tests
│   ├── test_performance.py         # Phase 4: Performance tests
│   ├── test_parallel.py            # Phase 5: Parallelization tests
│   ├── test_multiprocess.py        # Phase 5b: Multiprocessing tests
│   ├── test_v3_approximation.py    # V3 approximation tests
│   ├── test_dual_decomposition.py  # Dual decomposition tests
│   └── test_vectorized_batch.py    # Vectorized batch tests
│   ├── test_balancer_weighted.py    # Balancer closed-form: signature gen, Eq.9, validation, profit
│
└── fixtures/                        # Existing test fixtures
```

## Running Tests

```bash
# All tests
uv run pytest tests/arbitrage/

# Fast unit tests only (no RPC)
uv run pytest tests/arbitrage/ -m "not fork"

# Fork tests only
uv run pytest tests/arbitrage/ -m fork

# Optimizer tests only
uv run pytest tests/arbitrage/test_optimizers/

# Closed-form tests
uv run pytest tests/arbitrage/test_optimizers/test_closed_form.py

# Vectorized tests
uv run pytest tests/arbitrage/test_optimizers/test_vectorized_batch.py

# Mobius tests
uv run pytest tests/arbitrage/test_optimizers/test_mobius_optimizer.py
uv run pytest tests/arbitrage/test_optimizers/test_mobius_v3.py
uv run pytest tests/arbitrage/test_optimizers/test_piecewise_mobius.py

# Batch Möbius tests
uv run pytest tests/arbitrage/test_optimizers/test_batch_mobius.py

# Balancer closed-form tests
uv run pytest tests/arbitrage/test_optimizers/test_balancer_weighted.py

# Run final benchmark
python -m tests.arbitrage.test_optimizers.final_benchmark

# Run parallel benchmark
python -m tests.arbitrage.test_optimizers.run_parallel_benchmark

# Run all benchmarks
python -m tests.arbitrage.test_optimizers.run_all_benchmarks

# Run vectorized benchmark
python -m tests.arbitrage.test_optimizers.run_vectorized_benchmark
```

## Research Documents

```
plans/arbitrage-optimizer/research/
├── dual-decomposition.md       # Dual decomposition algorithm, bounded product CFMMs
├── v3-bounded-product.md       # V3 tick prediction, bounded product CFMM, V2V3Optimizer
├── mobius-transformation.md    # Phases 10-12: Mobius for V2 multi-hop & V3 single/multi-range
├── vectorized-batch.md         # NumPy vectorized batch, precision analysis, large reserves
├── batch-mobius.md             # Vectorized batch Möbius: log-domain, overflow, benchmarks
└── rust-opportunities.md       # Rust implementation: SIMD, Rayon, integer arithmetic, PyO3
```
