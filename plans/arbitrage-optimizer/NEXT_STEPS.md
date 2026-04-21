# Next Steps: Arbitrage Optimizer

**Status**: **ALL TESTS PASSING** — 694 Python + 209 Rust tests. System production-ready.

**Last Updated**: 2026-04-18

---

## Recently Completed (2026-04-18) ✅

### 23. V3-V3 Range Validation Tolerance Fix — COMPLETE ✅

Fixed a float64 precision bug that caused the V3-V3 Rust solver to miss the
optimal solution for asymmetric multi-range paths. The `asymmetric_10x3ranges`
benchmark scenario was showing a 56% profit mismatch (Rust: 4.77e+15,
brute-force: 1.08e+16).

**Root cause**: The golden section search in `solve_v3_v3_piecewise` converges to
the input that maximizes profit, which for asymmetric paths is right at the
boundary where pool 2's ending tick range is nearly exhausted. At that boundary,
`estimate_v3_final_sqrt_price()` (a float64 approximation) produces a final
sqrt price ~1e-12 *relatively* above `sqrt_price_upper`, causing the range
validation in `compute_v3_v3_profit` to reject every point in the convergence
zone. The golden section reports profit=-1e30 and the solver falls back to the
inferior baseline.

**Debugging approach**: Added temporary `log::debug!` instrumentation to
`solve_v3_v3_piecewise` and `compute_v3_v3_profit`. This revealed that the
golden section converged correctly to x≈5.618e17 but the profit function
returned -1e30 at every point near convergence due to the hop 2 range
validation rejecting the estimated sqrt price as out-of-bounds by ~1.67e-14
(about 1 ULP of float64 error).

**Fix**: Changed the range validation tolerance in `compute_v3_v3_profit` and
the single-range fast path in `solve_v3_v3` from `1e-12` to `1e-10` relative
to `sqrt_price_current`. This absorbs multi-step float64 rounding errors from
`estimate_v3_final_sqrt_price` while still being 7+ orders of magnitude tighter
than genuine range violations (~1e-3 relative for adjacent tick ranges).

**Why 1e-10**: The overshoot at the boundary was ~1.06e-12 relative. The old
tolerance of `1e-12 * sqrt_price_current` was borderline — sometimes passing,
sometimes failing, depending on the ULP rounding path. The new `1e-10` tolerance
provides ~100x headroom over the observed overshoot.

**Benchmark result**: All 13 V3-V3 scenarios now match brute-force within 1%.
The `asymmetric_10x3ranges` scenario now shows Rust profit 1.08e+16 matching
brute-force 1.08e+16 (previously 56% mismatch).

**Files changed**:
- `rust/src/optimizers/mobius_v3_v3.rs` — Changed 7 tolerance constants from
  `1e-12` to `1e-10` in `solve_v3_v3` and `compute_v3_v3_profit`

---

## Recently Completed (2026-04-17) ✅

### 20. Direct Pool State to Rust — COMPLETE ✅

Added `RustPoolCache` — a Rust-side HashMap that stores pool states keyed by
`u64` ID, and `ArbSolver.solve_cached()` that solves by passing just pool IDs.
This eliminates all Python object construction on the solve path.

**Architecture**:
- **Register**: At pool state update time (once per block), call
  `solver.register_pool(reserve_in, reserve_out, fee)` → returns pool_id
  or `solver.update_pool(pool_id, reserve_in, reserve_out, fee)` to update
- **Solve**: Call `solver.solve_cached([pool_id_0, pool_id_1])` — just two
  Python integers passed to Rust. Rust looks up cached `IntHopState` via
  HashMap, assembles the solve pipeline, and returns EVM-exact results.
- **No Python objects on solve path**: No `Hop`, `SolveInput`, `RustIntHopState`,
  or flat int list construction at solve time.

**New Rust type**: `RustPoolCache`
- `insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)` —
  register/update a pool's state
- `remove(pool_id)` — remove a pool (returns bool)
- `solve(path, max_input=None)` — solve by looking up cached states
- `contains(pool_id)` / `len()` — query methods
- Internal: `HashMap<u64, IntHopState>` with O(1) lookup

**New ArbSolver methods**:
- `get_pool_cache()` — access the Rust cache
- `register_pool(reserve_in, reserve_out, fee, *, pool_id=None)` —
  register with auto-ID assignment
- `update_pool(pool_id, reserve_in, reserve_out, fee)` — update existing pool
- `remove_pool(pool_id)` — remove a pool
- `solve_cached(path, *, max_input=None)` — solve by pool IDs

**Performance** (10000 iterations, V2-V2):

| Method | Time | vs Standard |
|--------|------|-------------|
| `solve_cached([id0, id1])` | **~2.5μs** | **1.5x faster** |
| `solve(SolveInput(hops=...))` | ~3.7μs | baseline |
| Rust `cache.solve()` (no Python) | **~0.73μs** | — |
| Pool update cost | ~0.34μs/pool | — |

**3-hop path**:
| `solve_cached` | **~3.6μs** | **1.3x faster** |
| `solve` (standard) | ~4.7μs | baseline |

**Breakdown of ~2.5μs `solve_cached` (2-hop)**:
- Rust computation (HashMap lookup + Möbius + U256 refinement): ~0.73μs
- ArbSolver method dispatch + result processing: ~1.77μs

**Test coverage**: 18 new tests in `test_rust_pool_cache.py`:
- 16 low-level: RustPoolCache insert, solve, EVM-exact, match vs solve_raw/object,
  full-scale reserves, max_input, unprofitable, 3-hop, best-in-neighborhood,
  mixed fees, update overwrites, remove, missing pool ID, too few pools,
  reuse across paths
- 2 high-level: ArbSolver integration with cache, cache matches standard solve

**Files changed**:
- `rust/src/optimizers/mobius_py.rs` — Added `PyPoolCache` struct with
  `insert`, `remove`, `solve`, `contains`, `len`
- `src/degenbot/arbitrage/optimizers/solver.py` — Added `_pool_cache`,
  `_next_pool_id`, `get_pool_cache()`, `register_pool()`, `update_pool()`,
  `remove_pool()`, `solve_cached()`
- `tests/arbitrage/test_optimizers/test_rust_pool_cache.py` — 18 new tests

### 18. Raw Array Hop Marshalling — COMPLETE ✅

Added `RustArbSolver.solve_raw()` method that accepts a flat Python list of ints instead of `RustIntHopState` objects, eliminating Python object construction overhead on the hot path.

**Architecture change**:
- **Before**: `ArbSolver._try_rust_solve` creates `RustIntHopState` Python objects (~0.4μs for 2 hops), then passes them to `RustArbSolver.solve()` where PyO3 extracts each object's fields
- **After**: `ArbSolver._try_rust_solve` builds a flat `list[int]` (`[r_in, r_out, gamma_numer, fee_denom, ...]` per hop, ~0.1μs for 2 hops), passes it to `RustArbSolver.solve_raw()` which parses the flat array directly

**New Rust method**: `RustArbSolver.solve_raw(int_hops_flat, max_input=None)`
- `int_hops_flat`: flat list of Python ints, 4 elements per hop: `[reserve_in, reserve_out, gamma_numer, fee_denom]`
- Rust internally calls `extract_python_u256` for reserves and `u64` extraction for fee params
- Returns same `RustArbResult` with integer fields populated for Möbius results
- Validates array length is multiple of 4 and at least 2 hops

**Python dispatch**:
- `ArbSolver._try_rust_solve` checks if all hops are ConstantProduct or single-range BoundedProduct
- If yes and `USE_RAW_ARRAY_MARSHALLING` is enabled, calls `_try_rust_solve_raw()` (new helper)
- Otherwise falls back to the object-based `RustArbSolver.solve()` path (V3 multi-range paths)
- Shared `_process_rust_result()` helper handles result processing for both paths

**Performance** (10000 iterations):

| Path | Object-based | Raw Array | Speedup |
|------|-------------|-----------|--------|
| 2-hop ArbSolver | ~4.4μs | **~3.9μs** | **1.13x** |
| 3-hop ArbSolver | ~5.7μs | **~4.9μs** | **1.17x** |
| RustIntHopState construction | 0.41μs (2 hops) | 0.10μs (flat list) | **4.1x** |

**Feature flag**: `USE_RAW_ARRAY_MARSHALLING` (env: `DEGENBOT_RAW_ARRAY_MARSHALLING`, default enabled). When disabled, falls back to `RustIntHopState` objects via `RustArbSolver.solve()`.

**Test coverage**: 17 new tests in `test_rust_raw_array_marshalling.py`:
- 12 low-level: `RustArbSolver.solve_raw()` basic, EVM-exact, matches object-based solve, full-scale reserves, max_input, unprofitable, 3-hop, best-in-neighborhood, mixed fees, invalid length, too few hops, V3 single-range
- 5 high-level: `ArbSolver` end-to-end EVM-exact for V2-V2, large reserves, 3-hop, mixed fee tiers, best-in-neighborhood

**Remaining bottleneck**: Per-item PyO3 extraction from the flat list (~0.77μs for 2 hops at Rust level). Further gains would require passing data as a binary buffer (bytes) to avoid Python int extraction entirely, or having Rust access pool state directly.

**Files changed**:
- `rust/src/optimizers/mobius_py.rs` — Added `RustArbSolver.solve_raw()` method
- `src/degenbot/arbitrage/optimizers/solver.py` — Added `USE_RAW_ARRAY_MARSHALLING` feature flag, `_try_rust_solve_raw()`, `_process_rust_result()`, refactored `_try_rust_solve()` to dispatch to raw array path
- `tests/arbitrage/test_optimizers/test_rust_raw_array_marshalling.py` — 17 new tests

### 17. Merge Integer Refinement into RustArbSolver — COMPLETE ✅

Extended `RustArbSolver.solve()` to accept `RustIntHopState` objects alongside float tuples. When all hops are `RustIntHopState`, the solver does float Möbius solve + U256 integer refinement in a single Rust call, eliminating the second Python→Rust conversion.

**Architecture change**:
- **Before**: `ArbSolver._try_rust_solve` does two Python→Rust conversions:
  1. Convert `Hop` → `(f64, f64, f64)` tuples → call `RustArbSolver.solve()` (~1μs conversion + 0.2μs solve)
  2. Convert `Hop` → `RustIntHopState` → call `py_mobius_refine_int()` (~2μs conversion + 0.2μs refinement)
- **After**: `ArbSolver._try_rust_solve` converts `Hop` → `RustIntHopState` once → call `RustArbSolver.solve()` which does both float solve and U256 refinement internally

**New/changed Rust types**:
- `RustArbResult` — added `optimal_input_int` (Option<U256>) and `profit_int` (Option<U256>) fields
- `RustArbSolver.solve()` — now recognizes `RustIntHopState` objects; derives float `HopState` from integer reserves for the float solve, then calls `mobius_refine_int` internally for Möbius results
- Helper functions: `parse_hops()`, `parse_v3_sequences()`, `solve_mobius()`, `not_supported_result()`, `u256_to_f64()`

**Performance** (V2-V2, 10000 iterations):
- Direct Rust call with int hops: **~0.6μs** (float solve + U256 refinement merged)
- Direct Rust call with float tuples: ~0.2μs (float solve only, no refinement)
- Old two-step at Rust level: ~2.4μs (0.2μs float + ~2μs conversion + 0.2μs refinement)
- **Merged Rust speedup**: **4x** vs old two-step at Rust level
- `ArbSolver` end-to-end: **~3.8μs** (was ~4.2μs, ~10% faster)

**Feature flag**: `USE_MERGED_INT_REFINEMENT` (env: `DEGENBOT_MERGED_INT_REFINEMENT`, default enabled). When disabled, falls back to old float-tuple + separate `py_mobius_refine_int` path.

**Test coverage**: 14 new tests in `test_rust_merged_int_refinement.py`:
- 9 low-level: `RustArbSolver.solve()` with int hops, EVM-exact verification, match vs two-step, full-scale reserves, max_input, unprofitable, 3-hop, best-in-neighborhood
- 5 high-level: `ArbSolver` end-to-end EVM-exact for V2-V2, large reserves, 3-hop, mixed fee tiers, best-in-neighborhood

**Remaining bottleneck**: Per-item PyO3 extraction from flat list costs ~0.1μs/item. Further gains need binary buffer or direct pool→Rust access (Items #19-20).

**Files changed**:
- `rust/src/optimizers/mobius_py.rs` — Extended `PyArbResult` with int fields, extended `RustArbSolver.solve()` to accept `RustIntHopState`, added helpers
- `src/degenbot/arbitrage/optimizers/solver.py` — Rewrote `_try_rust_solve` to use `RustIntHopState` with feature flag, kept fallback
- `tests/arbitrage/test_optimizers/test_rust_merged_int_refinement.py` — 14 new tests

### 10. Move Integer Refinement to Rust — COMPLETE ✅

Moved integer refinement from Python (`_simulate_path` with float arithmetic, ~3.7μs) to Rust (`mobius_refine_int` with U256 EVM-exact arithmetic).

**Architecture**:
- New Rust function `mobius_refine_int(x_approx, hops, max_input)` in `mobius_int.rs` — takes float optimum from `RustArbSolver`, searches ±N using `IntHopState::swap()` with U256 arithmetic
- New Python binding `py_mobius_refine_int` exposed via `mobius` module
- `ArbSolver._rust_integer_refinement()` converts Python `Hop` → `RustIntHopState` (extracting gamma_numer/gamma_denom from `Fraction` fee), calls Rust, returns `(int, int)`
- Falls back to Python `_integer_refinement` if Rust extension unavailable

**Key fix**: Previous Python integer refinement used `_simulate_path` with **float arithmetic**, which produced non-EVM-exact profits (up to ~5e8 wei error for 3-hop paths). Rust U256 simulation produces truly EVM-exact results.

**Performance** (V2-V2 median, 10000 iterations):
- ArbSolver: **~4.2μs** end-to-end (was ~5.8μs, 28% faster)

**Note**: This two-step architecture (float solve → separate refinement call) was subsequently replaced by Item #17's merged approach, which eliminated the second Python→Rust conversion entirely.

**Files changed**:
- `rust/src/optimizers/mobius_int.rs` — Added `mobius_refine_int` + 8 Rust unit tests
- `rust/src/optimizers/mobius_py.rs` — Added `py_mobius_refine_int` binding
- `src/degenbot/arbitrage/optimizers/solver.py` — Added `_rust_integer_refinement`, replaced `_integer_refinement` call in `_try_rust_solve`
- `tests/arbitrage/test_optimizers/test_rust_int_refinement.py` — 15 new Python tests

### 9. ArbSolver Rust Dispatch — COMPLETE ✅

Transformed `ArbSolver` from a Python multi-solver dispatcher into a thin wrapper that defers to Rust `RustArbSolver` for all supported path types.

**Architecture change**:
- **Before**: `ArbSolver.solve()` → Python `MobiusSolver` → Python `PiecewiseMobiusSolver` → Python `SolidlyStableSolver` → Python `BrentSolver` (6 Python dispatch steps)
- **After**: `ArbSolver.solve()` → Rust `RustArbSolver.solve()` (single Rust dispatch) → Python fallback for unsupported types (Solidly, Balancer, Brent only)

**New Rust types**:
- `RustArbSolver` — unified solver with `solve(hops, v3_sequences=None, max_input=None, max_candidates=10)`
- `RustArbResult` — returns `(optimal_input, profit, iterations, success, method, supported)` as floats
- Method tags: 0=MOBIUS, 1=PIECEWISE_MOBIUS, 2=V3_V3, 255=NOT_SUPPORTED

**Python `ArbSolver` dispatch flow** (new):
1. Convert hops to Rust format (tuples for V2/V3, `RustV3TickRangeSequence` for multi-range V3)
2. Call `RustArbSolver.solve()` — single Rust call handles method selection
3. If `supported=False`, fall back to Python `PiecewiseMobiusSolver` (handles Q96 scale mismatches)
4. Then Python `SolidlyStableSolver`, `BalancerMultiTokenSolver`, `BrentSolver`
5. Integer refinement for Möbius results done in Rust (U256 EVM-exact arithmetic)

**Performance** (V2-V2 median, 1000 iterations):
- Rust solve only: 0.2μs
- ArbSolver (end-to-end): 4.2μs
- Old ArbSolver (Python dispatch): 6.1μs

**Note**: Subsequent Item #10 added Rust integer refinement, and Item #17 merged it into the single Rust dispatch call, bringing end-to-end to ~3.8μs.

**Files changed**:
- `rust/src/optimizers/mobius_py.rs` — Added `RustArbSolver`, `RustArbResult`, `mobius_solve_int`
- `src/degenbot/arbitrage/optimizers/solver.py` — Rewrote `ArbSolver` to be thin Rust wrapper
- `tests/arbitrage/test_optimizers/conftest.py` — Fixed import path (`degenbot._rs` → `degenbot.degenbot_rs`)
- `tests/arbitrage/test_optimizers/test_rust_*.py` — Fixed import path
- `benchmarks/v3_v3_solver_benchmark.py` — Fixed import path
- `AGENTS.md` — Added note about auto-rebuild of Rust extension

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
```text
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

### 19. Binary Buffer Hop Marshalling — SUPERSEDED by #20
**Priority**: ~~Medium~~ Closed

Item #20 (RustPoolCache) supersedes this. The pool cache eliminates per-item
extraction entirely by storing state in Rust at registration time. The
`solve_cached()` path passes only integer IDs, making binary buffer
marshalling unnecessary.

### ~~20. Direct Pool State to Rust~~ — COMPLETE ✅
See "Recently Completed" section above.

### 21. Reduce ArbSolver Method Dispatch Overhead
**Priority**: Medium
**Effort**: Medium

**Current**: `solve_cached()` costs ~2.5μs but Rust-only is ~0.73μs. The ~1.77μs
ArbSolver overhead comes from Python method dispatch and SolveResult
construction.

**Target**: ~1.5μs end-to-end

**Approaches**:
- Return a lighter-weight result from `solve_cached()` (skip full SolveResult
  construction — just return (optimal_input, profit) tuple)
- Inline the Rust cache call more tightly
- Provide a `solve_cached_raw()` that returns the Rust result directly

### 22. Pool Cache Auto-Registration
**Priority**: Medium
**Effort**: Medium-High

**Current**: Users must manually call `register_pool()`/`update_pool()` at
state update time.

**Target**: Pool objects auto-register their state in the cache when they
update (via the existing PublisherMixin notification system).

**Approach**:
- Add an `ArbPoolCacheAdapter` subscriber that listens to `PoolStateMessage`
- On state update, automatically call `cache.insert()` with the pool's ID
- Pool objects get a `.arb_pool_id` attribute assigned lazily
- Requires coordinating the pool's reserve orientation (which token is
  reserve_in vs reserve_out depends on the arbitrage direction)

**Challenge**: A single pool may appear in multiple paths with different
reserve orientations. The cache stores one orientation per ID, so we'd
need either two entries per pool (one per direction) or store both
reserves and let Rust choose orientation at solve time.

### 11. V3-V3 Rust Performance Optimization
**Priority**: Medium
**Effort**: Medium

**Current**: ~10-50μs for multi-range (golden section search per candidate)
**Target**: ~5-10μs

**Approaches**:
- **Reduce candidate combinations**: Skip (k1, k2) pairs where crossing k1 doesn't improve on the baseline (single-range Möbius)
- **Tighten search bounds**: Use Möbius solution as bracket center instead of wide [x_min, x_max]. Now bounded by range capacity — tighter default brackets possible.
- **Reduce golden section iterations**: Current 30 iterations is conservative; 15-20 may suffice
- **Profile and reduce Python→Rust marshalling**: Same bottleneck as single-V3 (~6μs overhead)

**Note**: Range bounds fix (Item #7) already tightened search bounds by constraining x_max to range capacity, which may improve convergence.

### 12. Tick Transition Table Pre-Computation
**Priority**: Medium
**Effort**: Medium

For hot V3 pools, pre-compute tick crossing data at pool update time instead of at solve time.

**Current**: Crossing data computed on every `solve()` call
**Target**: Crossing data cached at pool state update time

**Benefit**: Moves ~2μs of sequence building out of the critical path.

### 13. L-BFGS-B for Dual Decomposition
**Priority**: Low-Medium
**Effort**: Medium

If `multi_token.py` (archived) is revived for multi-path routing:

**Current**: Custom gradient descent (~5-12ms)
**Target**: L-BFGS-B (5-10x faster convergence)

**Reference**: `plans/arbitrage-optimizer/archive/research/dual-decomposition.md`

### 14. V2-V3 Rust Solver
**Priority**: Low
**Effort**: Medium-High

V2-V3 paths (one V2 hop, one V3 hop with crossings) currently use `solve_v3_sequence` which handles this well. A dedicated V2-V3 Rust solver could eliminate per-hop RustHopState construction overhead.

**Potential**: ~5-10% speedup for V2-V3 paths specifically.

---

## Optional / Research

### 15. GPU Acceleration (CuPy)
**Priority**: Low
**Effort**: High

100x+ for massive batch operations. Use case: backtesting thousands of paths simultaneously.

**Blockers**: CuPy dependency, CUDA requirement, diminishing returns for typical batch sizes.

### 16. ML for Optimal Input Prediction
**Priority**: Low
**Effort**: High

Train model to predict optimal input without solving. ~0.01μs inference vs 0.19μs Möbius solve.

**Risk**: Approximation errors in financial context. Not recommended for production without extensive validation.

---

## Architecture (Current)

```
ArbSolver.solve()                          ← Python thin wrapper
    ├── _try_rust_solve_raw()               ← Raw int array (default for SolveInput)
    │     └── RustArbSolver.solve_raw()     ← Flat list → Möbius + U256 refinement
    │           ~3.9μs end-to-end (V2-V2, EVM-exact)
    ├── _try_rust_solve()                   ← Object path (V3 multi-range)
    │     └── RustArbSolver.solve()         ← RustIntHopState / float tuples
    │           ├── V3 multi-range (1 hop) → solve_v3_sequence (~9μs)
    │           └── V3-V3 (2 hops) → solve_v3_v3
    │                   ├── Both single-range → Möbius (~0.19μs)
    │                   └── Multi-range → enumerate (k1,k2) + golden section
    ├── PiecewiseMobiusSolver (~9μs Rust, ~50μs Python)
    ├── SolidlyStableSolver (~15-25μs)
    ├── BalancerMultiTokenSolver (~576μs)
    └── BrentSolver (~223μs, ultimate fallback)

ArbSolver.solve_cached([id0, id1])        ← Fastest path (no Python objects)
    └── RustPoolCache.solve()              ← HashMap lookup → Möbius + U256
            ~2.5μs end-to-end (V2-V2, EVM-exact)
            ~0.73μs Rust-only
```

### Performance Evolution

| Milestone | V2-V2 Time | Change |
|-----------|-----------|--------|
| Brent baseline | ~223μs | — |
| Python Möbius | ~0.86μs (compute) | 259x |
| Python Möbius + ArbSolver | ~6.1μs (end-to-end) | 37x |
| Rust dispatch (Item #9) | ~4.2μs (end-to-end) | 1.5x over Python |
| Rust int refinement (Item #10) | ~4.2μs (EVM-exact) | Same speed, exact results |
| Merged int refinement (Item #17) | ~3.8μs (EVM-exact) | 1.1x, single Rust call |
| Raw array marshalling (Item #18) | ~3.9μs (EVM-exact) | 1.13x, eliminates Python objects |
| Pool cache solve_cached (Item #20) | **~2.5μs** (EVM-exact) | **1.5x**, no Python objects on solve path |
| Pool cache Rust-only (Item #20) | **~0.73μs** | **3.3x** over ArbSolver, 305x over Brent |

---

## Solver Coverage Matrix

| Path Type | Solver | Time | Status |
|-----------|--------|------|--------|
| V2-V2 | Rust Möbius + U256 refinement | ~3.9μs | ✅ Complete |
| V2-V2 (cached) | RustPoolCache → Möbius + U256 | **~2.5μs** | ✅ Complete |
| V2-V2 (batch) | Rust Batch Möbius | 0.09μs/path | ✅ Complete |
| V2 multi-hop | Rust Möbius + U256 refinement | ~3.9μs | ✅ Complete |
| V3 single-range | Möbius | ~3.9μs | ✅ Complete |
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
3. **Build Rust extension** - `just dev` (required after Rust changes)
4. **Run V3-V3 benchmark** - `uv run python benchmarks/v3_v3_solver_benchmark.py` — all 13 scenarios should match ✓
5. **Check for drift** - Compare this file with actual state
6. **Update on completion** - Mark items done, add new discoveries

**Key principle**: Preserve the consolidated structure. Add new docs to `archive/` if superseded, don't create new top-level files.
