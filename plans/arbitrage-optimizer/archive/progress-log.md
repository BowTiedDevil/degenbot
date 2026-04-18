# Progress Log

| Date | Phase | Description |
|------|-------|-------------|
| 2026-04-04 | Planning | Created improvement plan |
| 2026-04-04 | Phase 1 | Created benchmark suite (20 tests passing) |
| 2026-04-04 | Phase 1 | Fixed CVXPY formulation for correct arbitrage direction |
| 2026-04-04 | Phase 2 | Created log-domain and scaled optimizer implementations |
| 2026-04-04 | Phase 2 | Added numerical stability tests (32 tests passing) |
| 2026-04-04 | Phase 2 | Initial finding: CVXPY found profits where Brent returned -1 |
| 2026-04-04 | Phase 2 | Implemented log-domain formulation with explicit log constraints |
| 2026-04-04 | Phase 2 | Implemented scaled formulation with geometric mean normalization |
| 2026-04-04 | Phase 2 | All tests pass (32 optimizer tests) |
| 2026-04-04 | Phase 2 | **BUG FIX**: Fixed Brent optimizer — was using wrong arbitrage direction |
| 2026-04-04 | Phase 2 | After fix: Brent and CVXPY find identical profits in all test cases |
| 2026-04-04 | Cleanup | Separated RPC-dependent tests to tests/arbitrage/integration/ |
| 2026-04-04 | Cleanup | Added @pytest.mark.fork for easy filtering |
| 2026-04-04 | Cleanup | Removed gas modeling from plan (too many runtime dependencies) |
| 2026-04-04 | Phase 3 | Created V3 approximation prototypes |
| 2026-04-04 | Phase 3 | Created HybridOptimizer for mixed V2/V3 |
| 2026-04-04 | Phase 3 | Reviewed Diamandis et al. paper on optimal routing through CFMMs |
| 2026-04-04 | Phase 3 | Implemented dual decomposition method with bounded product CFMMs |
| 2026-04-04 | Phase 3 | Key insight: V3 tick ranges are bounded product CFMMs with closed-form arbitrage |
| 2026-04-04 | Phase 3 | Total 51 tests passing, 5 skipped |
| 2026-04-04 | Phase 4 | Created performance_optimizer.py with solver selection, caching, warm start |
| 2026-04-04 | Phase 4 | Warm start: 12x faster on subsequent solves |
| 2026-04-04 | Phase 4 | Cache retrieval: 1138x faster than problem creation |
| 2026-04-04 | Phase 4 | Total 71 tests passing, 5 skipped |
| 2026-04-04 | Phase 5 | Key finding: Python GIL prevents true thread parallelism |
| 2026-04-04 | Phase 5 | Total 84 tests passing, 6 skipped |
| 2026-04-04 | Phase 5b | Key finding: CVXPY shows 1.14-1.20x parallel speedup, Brent 0.52x |
| 2026-04-04 | Phase 5b | Process spawn overhead (~26ms) dominates for fast operations |
| 2026-04-04 | Phase 5b | Total 99 tests passing, 6 skipped |
| 2026-04-04 | Phase 6 | Created closed_form.py with Newton's method implementation |
| 2026-04-04 | Phase 6 | Key finding: Newton is 12-19x faster than Brent |
| 2026-04-04 | Phase 6 | Total 128 tests passing |
| 2026-04-04 | Improvements | Implemented adaptive initial guess (Newton V2) and smart bracket (Brent) |
| 2026-04-04 | Improvements | Key finding: scipy bounded method ignores bracket parameter |
| 2026-04-04 | Corrections | Initial finding wrong: used tiny reserves, not full precision |
| 2026-04-04 | Corrections | With full precision, Newton and Brent find identical profits (0-1 wei diff) |
| 2026-04-04 | Corrections | Fixed all test fixtures to use full precision reserves |
| 2026-04-04 | Corrections | Added Key Lessons Learned section to plan |
| 2026-04-04 | Corrections | 149 tests passing (21 new) |
| 2026-04-04 | Summary | Final benchmark: Newton 29x faster than Brent (6.9μs vs 198μs) |
| 2026-04-04 | Summary | All phases complete, effort concluded |
| 2026-04-05 | Eval | Full test suite run: 407 passed, 6 skipped in 22.01s |
| 2026-04-05 | Multi-token | Created multi_token.py with DualDecompositionSolver, MultiTokenRouter |
| 2026-04-05 | Multi-token | Total 364 tests passing, 6 skipped |
| 2026-04-05 | V3 Deep Dive | Created v3_tick_predictor.py with tick crossing prediction |
| 2026-04-05 | V3 Deep Dive | Total 387 tests passing, 6 skipped |
| 2026-04-05 | V3 Deep Dive | Created v2_v3_optimizer.py with equilibrium estimation |
| 2026-04-05 | V3 Deep Dive | Total 407 tests passing, 6 skipped |
| 2026-04-14 | Phase 10 | Created mobius.py with MobiusV2Optimizer, ~40x faster than Brent, zero iterations |
| 2026-04-14 | Phase 10 | 447 tests passing |
| 2026-04-14 | Integration | HybridOptimizer dispatches pure V2 paths to MobiusV2Optimizer |
| 2026-04-14 | Integration | Full test suite: 675 passing, 8 skipped |
| 2026-04-14 | Phase 11 | Generalized MobiusV2Optimizer to MobiusOptimizer (V2+V3 support) |
| 2026-04-14 | Phase 11 | 38 new V3 Möbius tests in test_mobius_v3.py |
| 2026-04-14 | Phase 12 | Fixed TWO BUGS in mobius.py: to_hop_state() double-counting, estimate_v3_final_sqrt_price() missing *sqrt_p |
| 2026-04-14 | Phase 12 | V3 tick crossings are ADDITIVE, not compositional |
| 2026-04-14 | Phase 12 | Implemented piecewise-Möbius with explicit crossing + golden section search |
| 2026-04-14 | Phase 12 | Full test suite: 790 passing, 17 skipped |
| 2026-04-15 | Batch | Created batch_mobius.py with VectorizedMobiusSolver, SerialMobiusSolver, BatchMobiusOptimizer |
| 2026-04-15 | Batch | Fixed critical bug: numpy view mutation (M *= r_in overwrote input hops_array) |
| 2026-04-15 | Batch | Implemented log-domain overflow handling: log-sum-exp for N, expm1 for profit |
| 2026-04-15 | Batch | Batch Möbius ~0.15μs/path (1000 paths), 3-4x faster than Batch Newton (zero iterations) |
| 2026-04-15 | Batch | Full test suite: 841 passing, 17 skipped |
| 2026-04-15 | Rust | Created Rust Möbius optimizer (mobius.rs) — f64 at 0.19μs (1021x faster than Brent) |
| 2026-04-15 | Rust | Created Rust integer Möbius optimizer (mobius_int.rs) — u256 at 0.88μs (EVM-exact) |
| 2026-04-15 | Rust | Created Rust batch Möbius optimizer (mobius_batch.rs) — 0.09μs/path at 1000 paths |
| 2026-04-15 | Rust | Created Rust V3 Möbius optimizer (mobius_v3.rs) — V3 tick range support |
| 2026-04-15 | Rust | PyO3 bindings (mobius_py.rs) — Python-callable Rust solvers |
| 2026-04-15 | Benchmark | Full benchmark run: all optimizers vs Brent baseline |
| 2026-04-15 | Benchmark | V2-V2 single: Brent 194μs, Möbius Py 0.86μs (225x), Möbius Rust 0.19μs (1021x), Newton 7.5μs (26x) |
| 2026-04-15 | Benchmark | V2 multi-hop: Möbius scales O(n), Rust 4-7x faster than Python |
| 2026-04-15 | Benchmark | Batch 1000: Rust Batch 93μs (0.09μs/path), Py Vec Möbius 140μs (0.14μs/path), Py Vec Newton 528μs (0.53μs/path) |
| 2026-04-15 | Benchmark | CVXPY: 1.3ms serial (7x slower than Brent), cache hit 0.001ms |
| 2026-04-15 | Benchmark | Integer Möbius: 0.88μs EVM-exact, not-profitable rejection 0.32μs |
| 2026-04-15 | Plan Update | Updated all plan files with benchmark results, Rust results, and revised recommendations |
| 2026-04-15 | Solver | Created solver.py with unified interface: Hop, SolveInput, SolveResult, ArbSolver, MobiusSolver, NewtonSolver, BrentSolver |
| 2026-04-15 | Solver | MobiusSolver: zero-iteration closed-form, direct ±1 integer neighbor check (5.8μs) |
| 2026-04-15 | Solver | NewtonSolver: Newton with Möbius initial guess, 2-hop V2 (4.5μs) |
| 2026-04-15 | Solver | BrentSolver: scipy fallback for all pool types (223μs) |
| 2026-04-15 | Solver | ArbSolver dispatcher: Mobius→Newton→Brent, 6.4μs median for V2-V2 (35x faster than Brent) |
| 2026-04-15 | Solver | 45 solver unit tests + 20 integration/timing tests (916 total passing) |
| 2026-04-15 | Integration | Added USE_SOLVER_FAST_PATH feature flag to uniswap_2pool_cycle_testing.py |
| 2026-04-15 | Integration | Integrated ArbSolver fast-path into all 9 _calculate_* methods (V4-V4, V3-V4, V4-V2, V4-V3, V3-V3, V2-V3, V2-V4, V3-V2, V2-V2) |
| 2026-04-15 | Integration | _solver_fast_path_v2_v2: before CVXPY, _solver_fast_path_mixed: before Brent |
| 2026-04-15 | Optimization | Replaced golden section integer refinement (47μs) with direct ±1 neighbor check (5.8μs). Same accuracy, 8x faster |
| 2026-04-15 | Timing | Validated: Mobius 5.8μs, Newton 4.5μs, ArbSolver 6.4μs, Brent 223μs. All find identical profit. |
| 2026-04-15 | Full Support | Replaced _build_solve_input_v2, _solver_fast_path_v2_v2, _solver_fast_path_mixed with single _solver_fast_path(pools, input_token, state_overrides) |
| 2026-04-15 | Full Support | Added _v3_virtual_reserves() for V3/V4 virtual reserve computation (L/sqrt_p, L*sqrt_p) |
| 2026-04-15 | Full Support | Added pool_state_to_hop() with state override support for V2, V3, V4, Aerodrome |
| 2026-04-15 | Full Support | Fixed bug: pool_to_hop referenced state.reserves_token0 on V3/V4 (doesn't exist). Now uses _v3_virtual_reserves |
| 2026-04-15 | Full Support | Updated all 9 _calculate_* call sites to use new _solver_fast_path |
| 2026-04-15 | Full Support | 3-hop Möbius: 11.7μs (14x faster than Brent), finds 71 wei MORE profit |
| 2026-04-15 | Full Support | 11 new tests: V3 virtual reserves, pool_state_to_hop, all pool type combos, multi-hop paths |
| 2026-04-15 | Full Support | 927 tests passing, 17 skipped |
| 2026-04-15 | Balancer | Implemented closed-form N-token Balancer weighted pool solver (Equation 9 from Willetts & Harrington 2024) |
| 2026-04-15 | Balancer | Fixed two critical bugs: (1) d_i indicator must be I_{s_i=1} not signature[i], (2) reserves must be upscaled to 18-decimal before formula |
| 2026-04-15 | Balancer | Single Eq.9 eval: 3.9μs, Full solver N=3: 576μs (12 signatures) |
| 2026-04-15 | Balancer | Added BalancerMultiTokenHop with decimals field, BalancerMultiTokenSolver in ArbSolver dispatch chain |
| 2026-04-15 | Balancer | Added BALANCER_MULTI_TOKEN to PoolInvariant and SolverMethod enums |
| 2026-04-15 | Balancer | 30 passing tests in test_balancer_weighted.py, 1012 total arbitrage tests passing |
