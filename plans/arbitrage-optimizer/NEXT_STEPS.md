# Next Steps: Arbitrage Optimizer

**Status**: **ALL RUST TESTS PASSING** — 161 unit tests + doc-tests clean. System production-ready.

**Last Updated**: 2026-04-16

---

## Recently Completed (2026-04-16) ✅

### 8. Rust Test Suite Fixes — COMPLETE ✅

7 of 161 Rust unit tests were failing. Three root causes found and fixed:

**Bug 1: Symmetric two-hop test reserves (mobius.rs)**
For a two-hop cycle where pool 2 mirrors pool 1's reserves (r₂=s₁, s₂=r₁), the profitability ratio K/M = γ² < 1. The pools *agree* on price, so there's no arbitrage opportunity — fees on both hops always outweigh any marginal rate advantage. Three tests used such reserves.
- `test_two_hop_profitable` — changed to asymmetric reserves where pools disagree on price
- `test_simulate_path_matches_mobius_output` — same fix
- `test_profitability_check_free` — same fix

**Bug 2: V3 range capacity exceeded in test inputs (mobius_v3.rs, mobius_v3_v3.rs)**
Test inputs exceeded V3 tick range capacity, pushing final sqrt price out of bounds.
- `test_estimate_v3_final_sqrt_price_zero_for_one` — reduced input from 1e15 to 5e13 (within ~45% of max capacity)
- `test_compute_v3_v3_profit_rejects_out_of_range` — reduced "small" input from 1e10 to 1e6 (1e10 produced ~1e16 output from hop 1, overwhelming hop 2's range)

**Bug 3: Float64 boundary precision in range validation (mobius_v3_v3.rs)**
Two production code issues:
1. `compute_range_constrained_max_input` lacked `(1 - 1e-12)` shrink factor (already present in `compute_v3_candidates_range_max`). At boundary, float64 arithmetic in `estimate_v3_final_sqrt_price` produced values just barely outside range.
2. Range validation in `solve_v3_v3` and `compute_v3_v3_profit` used strict `contains_sqrt_price()` without tolerance.
- Added shrink factor to `compute_range_constrained_max_input`
- Added tolerance-based validation (`eps = 1e-12 * sqrt_price_current`) to `solve_v3_v3` fast path, baseline, and `compute_v3_v3_profit` — consistent with `solve_v3_candidates`

**Convergence fix (mobius_v3.rs, mobius_v3_v3.rs)**:
Golden section search convergence was impractically slow when x_min=0 with large search intervals — fixed `MIN_ABS_INTERVAL` of 1e-6 required ~96 iterations for a 1e14 range. Scaled `abs_tol = max(1e-6, initial_interval * 1e-10)` to converge in ~48 iterations.

**Doc-test fix**: Wrapped 5 Unicode math formulas in ` ```text ` code blocks to prevent Rust doc-test runner from compiling them as code.

**Files changed**:
- `rust/src/optimizers/mobius.rs` — 3 test fixes
- `rust/src/optimizers/mobius_v3.rs` — 1 test fix, convergence improvement
- `rust/src/optimizers/mobius_v3_v3.rs` — shrink factor, tolerance validation, 1 test fix, convergence improvement
- `rust/src/optimizers/mobius_int.rs` — doc-test fix

### 7. V3-V3 Synthetic Test Suite — COMPLETE ✅
**Plan**: [`v3-v3-test-suite-plan.md`](v3-v3-test-suite-plan.md)

Three-layer validation (28 Python tests + 6 Rust tests):
1. **Profit function unit tests** — `compute_v3_v3_profit` validated against hand-computed expected values
2. **vs Brent reference** — Rust solver vs scipy `minimize_scalar` on same inputs
3. **vs V3 integer math** — Brute-force `compute_swap_step` scanner (gold standard)

Plus edge cases: tick boundaries, narrow ranges, liquidity asymmetry, fee tiers, empty ranges.

**Bugs found**:
1. `simulate_v3_hop_with_crossings()` had reversed tick crossing direction
2. **Range bounds violation** — `solve_v3_v3` returned inputs exceeding tick range capacity (up to 52% profit overestimate). Fixed with constrained `max_input` and profit validation. See [`v3-v3-test-suite-plan.md`](v3-v3-test-suite-plan.md) for details.

**Benchmark**: `benchmarks/v3_v3_solver_benchmark.py` — 13 scenarios, all matching brute-force within 1%.

**Tick crossing extension**: `max_candidates` increased from hardcoded 3 to configurable (default 10). Linear scaling confirmed.

**Files changed**:
- `rust/src/optimizers/mobius_v3.rs` — Added `max_gross_input_in_range()`
- `rust/src/optimizers/mobius_v3_v3.rs` — Range bounds fix, profit validation, configurable max_candidates, 6 new Rust tests
- `rust/src/optimizers/mobius_py.rs` — Exposed `max_gross_input_in_range()`, `max_candidates` param
- `src/degenbot/arbitrage/optimizers/solver.py` — Updated to pass `max_candidates=10`
- `tests/arbitrage/test_optimizers/test_v3_v3_accuracy.py` — 28 new Python tests
- `benchmarks/v3_v3_solver_benchmark.py` — 13-scenario benchmark
- `pyproject.toml` — Benchmark lint exemptions

---

## Previously Completed (2026-04-15) ✅

### 6. V3-V3 Rust Solver - COMPLETE ✅
Two-V3-hop arbitrage with simultaneous tick crossings on both pools.

**Previous state**: V3-V3 paths fell back to Brent (~390μs) because PiecewiseMobiusSolver only handled one V3 hop at a time.

**Implementation**: `rust/src/optimizers/mobius_v3_v3.rs`

**Algorithm**:
- **Fast path** (both pools single-range): Standard 2-hop Möbius (~0.19μs)
- **Slow path** (one or both pools multi-range): Enumerate all (k1, k2) ending range combinations, solve each with golden section search on the piecewise profit function

**Three sub-cases for multi-range**:
1. Hop 1 crosses, hop 2 stays in current range
2. Hop 1 stays, hop 2 crosses
3. Both hops cross simultaneously

**Profit function** (`compute_v3_v3_profit`):
```
profit(x) = output2(x) - x
output1 = crossing1.output + mobius(remaining1, ending_range1)  [if hop1 crosses]
output2 = crossing2.output + mobius(remaining2, ending_range2)  [if hop2 crosses]
```

**Python dispatch**: `PiecewiseMobiusSolver._try_rust_v3_v3()` detects 2-hop V3-V3 paths and calls Rust before falling through to single-V3 logic or Brent.

**Performance**:

| Scenario | Before (Brent) | After (Rust V3-V3) | Speedup |
|----------|---------------|-------------------|---------|
| Both single-range | ~390μs | ~0.19μs | **2053x** |
| One or both multi-range | ~390μs | ~10-50μs | **8-39x** |

**Test Coverage**: 10 new V3-V3 tests covering single-range, multi-range, profit correctness, max_input constraint

**Files Changed**:
- `rust/src/optimizers/mobius_v3_v3.rs` - New V3-V3 solver
- `rust/src/optimizers/mod.rs` - Module export
- `rust/src/optimizers/mobius_py.rs` - Python binding (`solve_v3_v3`)
- `src/degenbot/arbitrage/optimizers/solver.py` - V3-V3 dispatch (`_try_rust_v3_v3`)
- `tests/arbitrage/test_optimizers/test_v3_v3_solver.py` - 10 new tests

---

### 5. PiecewiseMobiusSolver - FULL IMPLEMENTATION ✅
Complete integration of multi-range V3 solver with 10 optimization techniques and Rust bindings.

**Performance Achieved** (benchmarked with real data):

| Scenario | Before (Brent) | Python Only | With Rust | Rust Speedup |
|----------|---------------|-------------|-----------|--------------|
| Profitable arbitrage | ~390μs | ~50μs | **~9μs** | **5.5x** |
| All candidates rejected | ~390μs | ~500μs | **~140μs** | **3.5x** |
| Raw computation | N/A | ~100μs | **~1μs** | **100x** |

---

### MobiusSolver Multi-Range Dispatch Fix ✅
`MobiusSolver.supports()` now rejects V3 hops with `has_multi_range=True`, causing `ArbSolver` to fall through to `PiecewiseMobiusSolver`. Previously, MobiusSolver accepted all bounded-product hops, silently ignoring multi-range data and producing wrong answers.

---

## Previously Completed ✅

### 1. Update Test Counts - DONE
### 2. Verify Solidly Solver Integration - DONE (20 tests)
### 3. Gas Cost Modeling - REJECTED (out of scope for optimizer)
### 4. Consolidate Overlapping Tests - DONE

---

## Recommended Next Steps (Prioritized)

### 7. ~~V3-V3 Synthetic Test Suite~~ — COMPLETE ✅
Moved to Recently Completed above.

### 8. V3-V3 Rust Performance Optimization
**Priority**: Medium  
**Effort**: Medium

**Current**: ~10-50μs for multi-range (golden section search per candidate)  
**Target**: ~5-10μs

**Approaches**:
- **Reduce candidate combinations**: Skip (k1, k2) pairs where crossing k1 doesn't improve on the baseline (single-range Möbius)
- **Tighten search bounds**: Use Möbius solution as bracket center instead of wide [x_min, x_max]. Now bounded by range capacity — tighter default brackets possible.
- **Reduce golden section iterations**: Current 30 iterations is conservative; 15-20 may suffice
- **Profile and reduce Python→Rust marshalling**: Same bottleneck as single-V3 (~6μs overhead)

**Note**: Range bounds fix (session 2) already tightened search bounds by constraining x_max to range capacity, which may improve convergence.

### 9. Reduce Python-Rust Marshalling Overhead
**Priority**: Medium
**Effort**: Medium

**Current bottleneck**: ~6μs of the ~9μs solve time is Python→Rust data conversion.

**Approaches**:
- **Pre-allocate Rust objects**: Keep Rust optimizer + sequences in a hot cache instead of rebuilding per call
- **Flat buffer protocol**: Pass hop data as a flat float64 array instead of individual RustHopState objects
- **Move dispatch logic to Rust**: Let Rust handle the full pipeline (including V3-V3 detection) to avoid Python dispatch overhead

**Potential**: If marshalling is halved, end-to-end drops from ~9μs to ~6μs.

### 10. Tick Transition Table Pre-Computation
**Priority**: Medium
**Effort**: Medium

For hot V3 pools, pre-compute tick crossing data at pool update time instead of at solve time.

**Current**: Crossing data computed on every `solve()` call
**Target**: Crossing data cached at pool state update time

**Benefit**: Moves ~2μs of sequence building out of the critical path.

### 11. L-BFGS-B for Dual Decomposition
**Priority**: Low-Medium
**Effort**: Medium

If `multi_token.py` (archived) is revived for multi-path routing:

**Current**: Custom gradient descent (~5-12ms)
**Target**: L-BFGS-B (5-10x faster convergence)

**Reference**: `plans/arbitrage-optimizer/archive/research/dual-decomposition.md`

### 12. V2-V3 Rust Solver
**Priority**: Low
**Effort**: Medium-High

V2-V3 paths (one V2 hop, one V3 hop with crossings) currently use `solve_v3_sequence` which handles this well. A dedicated V2-V3 Rust solver could eliminate per-hop RustHopState construction overhead.

**Potential**: ~5-10% speedup for V2-V3 paths specifically.

---

## Optional / Research

### 13. GPU Acceleration (CuPy)
**Priority**: Low
**Effort**: High

100x+ for massive batch operations. Use case: backtesting thousands of paths simultaneously.

**Blockers**: CuPy dependency, CUDA requirement, diminishing returns for typical batch sizes.

### 14. ML for Optimal Input Prediction
**Priority**: Low
**Effort**: High

Train model to predict optimal input without solving. ~0.01μs inference vs 0.19μs Möbius solve.

**Risk**: Approximation errors in financial context. Not recommended for production without extensive validation.

---

## Architecture (Current)

```
ArbSolver.solve()
    ├── 1. MobiusSolver (~0.86μs)
    │       └── V2 + V3 single-range, zero-iteration closed-form
    ├── 2. PiecewiseMobiusSolver (~9μs Rust, ~50μs Python)
    │       ├── V3-V3? → _try_rust_v3_v3() [NEW]
    │       │       ├── Both single-range → standard Möbius
    │       │       └── Multi-range → enumerate (k1,k2) + golden section
    │       ├── Single V3 multi-range → _try_rust_multi_range()
    │       │       └── solve_v3_sequence (Rust)
    │       └── Python fallback: golden section per candidate
    ├── 3. SolidlyStableSolver (~15-25μs)
    ├── 4. BalancerMultiTokenSolver (~576μs)
    ├── 5. NewtonSolver (~4.5μs)
    └── 6. BrentSolver (~223μs)
```

---

## Solver Coverage Matrix

| Path Type | Solver | Time | Status |
|-----------|--------|------|--------|
| V2-V2 | Rust Möbius | 0.19μs | ✅ Complete |
| V2-V2 (batch) | Rust Batch Möbius | 0.09μs/path | ✅ Complete |
| V2 multi-hop | Python Möbius | 1.5μs | ✅ Complete |
| V3 single-range | Möbius | ~0.86μs | ✅ Complete |
| V2-V3 (1 V3 crossing) | Piecewise-Möbius Rust | ~9μs | ✅ Complete |
| V3-V2 (1 V3 crossing) | Piecewise-Möbius Rust | ~9μs | ✅ Complete |
| V3-V3 (both single-range) | V3-V3 Rust solver | ~0.19μs | ✅ Complete |
| V3-V3 (one or both crossing) | V3-V3 Rust solver | ~10-50μs | ✅ Complete |
| Solidly stable | SolidlyStableSolver | ~15-25μs | ✅ Complete |
| Balancer weighted | Eq.9 closed-form | ~576μs | ✅ Complete |
| General fallback | Brent | ~223-390μs | ✅ Complete |

---

## Cross-Session Context

When resuming work:

1. **Read this file first** - Check current status
2. **Verify tests pass** - `uv run pytest tests/arbitrage/ -x -q`
3. **Check for drift** - Compare this file with actual state
4. **Update on completion** - Mark items done, add new discoveries

**Key principle**: Preserve the consolidated structure. Add new docs to `archive/` if superseded, don't create new top-level files.
