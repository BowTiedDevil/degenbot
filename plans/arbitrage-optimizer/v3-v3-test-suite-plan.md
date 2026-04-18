# V3-V3 Synthetic Test Suite Plan

**Status**: ✅ Complete  
**Priority**: High  
**Effort**: Medium (completed in 2 sessions)  
**Created**: 2026-04-15  
**Completed**: 2026-04-16

---

## Problem

The V3-V3 Rust solver (`solve_v3_v3`) has 10 tests that cover smoke testing (no-panic, basic profit) but no accuracy validation. The core question unanswered: **does the golden section search on the piecewise profit function find the true optimum?**

The existing single-V3 accuracy tests (`test_mobius_v3_accuracy.py`) validate against V3 integer `compute_swap_step` — the gold standard. The V3-V3 solver needs the same rigor.

## Why Synthetic > Real Data

Synthetic tests are *better* for solver validation because:
1. **Analytical ground truth**: For single-range V3-V3, the Möbius closed-form gives exact answers we can compare against
2. **Edge case control**: We can construct adversarial inputs (extreme liquidity asymmetry, boundary ticks, near-zero spreads) that are rare on-chain but must be handled correctly
3. **Reproducibility**: No RPC dependency, no chain state changes between runs
4. **Brute-force reference**: We can use V3 integer swap math to build a brute-force V3-V3 profit scanner as the reference implementation

Real data only catches data-format/parsing bugs (wrong tick_bitmap deserialization, etc.) — that's integration testing, a separate concern.

## Coverage (Updated 2026-04-16)

| Category | Tests | Status |
|----------|-------|--------|
| Single-range fast path | 3 original + 6 new (incl. range bounds) | ✅ Complete |
| Profit function (Layer 1) | 6 tests | ✅ Complete |
| vs Brent reference (Layer 2) | 7 tests | ✅ Complete |
| vs V3 integer math (Layer 3) | 6 tests | ✅ Complete |
| Edge cases | 9 tests | ✅ Complete |
| End-to-end ArbSolver dispatch | 3 tests | ⚠️ Only dispatch, not accuracy |
| Rust unit tests (range bounds) | 6 new tests in mobius_v3_v3.rs | ✅ Complete |

**File**: `tests/arbitrage/test_optimizers/test_v3_v3_accuracy.py` (28 tests)
**File**: `rust/src/optimizers/mobius_v3_v3.rs` (6 new #[test] functions)
**Benchmark**: `benchmarks/v3_v3_solver_benchmark.py` (13 scenarios)

**Bugs found and fixed**:
1. `simulate_v3_hop_with_crossings()` had reversed tick crossing direction — fixed.
2. **Range bounds violation**: `solve_v3_v3` returned inputs exceeding tick range capacity, causing up to 52% profit overestimate. Fixed by constraining max_input to range capacity and validating final sqrt prices. See [Range Bounds Bug Fix](#range-bounds-bug-fix) below.

**All 474 optimizer tests pass. Lint clean.**

## Architecture: Three-Layer Validation

### Layer 1: `compute_v3_v3_profit` Unit Tests
The profit function is the atomic building block. If it's wrong, everything built on it is wrong.

**Approach**: For known inputs, compute expected profit by hand (using V3 swap formulas), compare against `compute_v3_v3_profit`.

**Test cases**:
- No crossings: `profit(x) = mobius2(x) - x` — verify against `simulate_path`
- Crossing on hop 1 only: `profit(x) = mobius2(crossing1.output + mobius1(x - crossing1.gross_input, ending_range1)) - x`
- Crossing on hop 2 only: `profit(x) = mobius2(crossing2.output + mobius1(output_of_hop1 - crossing2.gross_input, ending_range2)) - x`
- Both crossings: chain both

**Implementation**: These are pure Rust `compute_v3_v3_profit` tests or Python tests calling the Rust optimizer's `simulate_path` to build expected values.

### Layer 2: V3-V3 vs Brent Reference
Brent is the existing fallback that handles V3-V3 correctly (slowly). It serves as an independent reference implementation.

**Approach**:
1. Construct V3-V3 paths with tick crossings using `BoundedProductHop` + `V3TickRangeInfo`
2. Solve with both `solve_v3_v3` (Rust) and `BrentSolver` (Python)
3. Assert: optimal_input within 1% relative, profit within 0.5% relative

**Key insight**: Brent operates on the *full* profit function (including tick crossings at arbitrary points), while `solve_v3_v3` uses the piecewise approximation (enumerate discrete ending ranges). They should agree for well-structured inputs but may diverge for:
- Paths where the true optimum falls between tick ranges (rare but possible)
- Paths with 4+ tick crossings (V3-V3 only checks 3 candidates per side)

**Test matrix**:
| Scenario | Pools | Expected Agreement |
|----------|-------|--------------------|
| Both single-range | V3 + V3 | Exact match |
| One pool crossing 1 tick | V3(2 ranges) + V3(1 range) | Within 0.5% |
| Both crossing 1 tick | V3(2 ranges) + V3(2 ranges) | Within 0.5% |
| One pool crossing 2 ticks | V3(3 ranges) + V3(1 range) | Within 1% |
| Both crossing 2 ticks | V3(3 ranges) + V3(3 ranges) | Within 1% |
| No arbitrage | Same-price V3 + V3 | Both return 0 |
| Very narrow spread | V3 + V3 (0.1% price diff) | Both find same (or both fail) |

### Layer 3: V3-V3 vs V3 Integer Math (Gold Standard)
The ultimate validation: build a brute-force V3-V3 profit scanner using `compute_swap_step` with integer arithmetic, and compare against the Rust solver.

**Approach** (mirrors `test_mobius_v3_accuracy.py` pattern):
1. Build two V3 pools with integer tick data (tick, liquidity, fee)
2. For a range of input amounts, simulate the full 2-hop path using `compute_swap_step`
3. Find the brute-force optimal input and profit
4. Compare with Rust `solve_v3_v3` output

**This is the hardest layer** because V3 integer swap math handles tick crossings naturally (it's what the contract does), while the Rust solver approximates by enumerating discrete ending ranges. The comparison surfaces any approximation error from the piecewise decomposition.

**Test cases**:
- WETH/USDC-like pools (tick ≈ -83000, spacing=60)
- Stablecoin-like pools (tick ≈ 0, spacing=1 or 10)
- High-liquidity pools (1e20+)
- Low-liquidity pools (1e10)
- Asymmetric crossings (hop 1 crosses 3 ticks, hop 2 crosses 0)

---

## Edge Cases to Cover

### Tick Boundary Edge Cases
| Edge Case | Why It Matters |
|-----------|---------------|
| Current price at exact tick boundary | `compute_crossing(k=1)` returns crossing_input=0 |
| Crossing into empty range (L=0) | `to_hop_state()` returns zero reserves → division by zero? |
| Very narrow range (1 tick spacing) | reserves are tiny, price impact is massive |
| Adjacent ranges with same liquidity | crossing happens but output changes are small |

### Numerical Edge Cases
| Edge Case | Why It Matters |
|-----------|---------------|
| Very small spread (fees > profit) | Solver should return failure, not negative profit |
| Very large spread | Optimal input may exceed range, need max_input |
| Massive liquidity asymmetry (1e30 vs 1e10) | Float64 precision limits |
| Near-zero liquidity in one range | Division by near-zero in Möbius formula |
| Extreme prices (sqrt_price < 1e-10 or > 1e10) | Float64 underflow/overflow |

### Golden Section Search Edge Cases
| Edge Case | Why It Matters |
|-----------|---------------|
| Profit function has multiple local maxima | Golden section finds one, may miss the global |
| Profit function is flat (no clear optimum) | Convergence tolerance matters |
| x_min ≈ x_max (tiny search interval) | Early exit should trigger |
| x_min > x_max (no valid search region) | Should return (0, 0) |
| Crossing cost exceeds all potential profit | Should return (0, 0) |

### Fee Tier Edge Cases
| Fee | Use Case |
|-----|----------|
| 500 (0.05%) | Stablecoin pairs |
| 3000 (0.3%) | Standard pairs |
| 10000 (1%) | Exotic pairs |
| Mixed (0.05% + 1%) | Cross-tier arbitrage |

---

## Test Implementation Plan

### File: `tests/arbitrage/test_optimizers/test_v3_v3_accuracy.py`

```
class TestV3V3ProfitFunction:           # Layer 1
    test_profit_no_crossing_matches_simulate_path
    test_profit_crossing1_only
    test_profit_crossing2_only
    test_profit_both_crossings
    test_profit_below_crossing_cost_returns_negative_infinity
    test_profit_at_exact_crossing_boundary

class TestV3V3VsBrent:                  # Layer 2
    test_single_range_matches_brent
    test_one_pool_crossing_matches_brent
    test_both_pools_crossing_matches_brent
    test_no_arbitrage_matches_brent
    test_narrow_spread_matches_brent
    test_max_input_respected_by_both
    test_different_fee_tiers

class TestV3V3VsV3IntegerMath:          # Layer 3
    test_v3_v3_arbitrage_vs_brute_force_weth_usdc
    test_v3_v3_arbitrage_vs_brute_force_stablecoin
    test_v3_v3_profit_gradient_zero_at_optimum
    test_v3_v3_high_liquidity_accuracy
    test_v3_v3_low_liquidity_accuracy
    test_v3_v3_asymmetric_crossings

class TestV3V3EdgeCases:                # Edge cases
    test_empty_range_zero_liquidity
    test_current_price_at_tick_boundary
    test_very_narrow_tick_range
    test_massive_liquidity_asymmetry
    test_near_zero_liquidity
    test_multiple_local_maxima
    test_fee_exceeds_profit
    test_mixed_fee_tiers
```

### Supporting Infrastructure

**`v3_v3_brute_force_solver()`**: A Python function that takes two V3 pool descriptions (integer tick data + liquidity) and finds the optimal input by:
1. Scanning input amounts from 0 to max
2. For each input, simulating the 2-hop path using `compute_swap_step`
3. Tracking the maximum profit
4. Returning `(optimal_input, profit)`

This mirrors the `v3_swap_within_range_integer()` helper in `test_mobius_v3_accuracy.py` but chains two V3 pools.

**`v3_v3_brent_solver()`**: A wrapper around the existing `BrentSolver` that takes `SolveInput` with two `BoundedProductHop` (V3 multi-range) hops and returns the result. This is simpler than the brute-force approach since Brent already works.

**`make_v3_v3_scenario()`**: A factory function that takes scenario parameters (price spread, tick spacings, liquidities, fee tiers) and returns:
- `seq1`, `seq2` (Rust `V3TickRangeSequence` objects)
- `solve_input` (Python `SolveInput` with `BoundedProductHop` objects for Brent)
- Expected profit range (from analytical or brute-force computation)

---

## Implementation Order

1. **Layer 1** (profit function unit tests) — 30 min
   - Pure Rust/Python unit tests for `compute_v3_v3_profit`
   - No external dependencies beyond existing `simulate_path`
   - Validates the atomic building block

2. **Layer 2** (vs Brent) — 1-2 hours
   - Build `make_v3_v3_scenario()` factory
   - Run `BrentSolver` on V3-V3 paths
   - Requires `BoundedProductHop` with tick range data (existing `FakeV3PoolWithTicks` + `_get_cached_tick_ranges`)
   - 7-8 test methods

3. **Layer 3** (vs V3 integer math) — 2-3 hours
   - Build `v3_v3_brute_force_solver()` with chained `compute_swap_step`
   - Handle tick crossings in brute-force (loop through ranges until input exhausted)
   - 5-6 test methods
   - Hardest part: getting the brute-force V3-V3 simulation right

4. **Edge cases** — 1 hour
   - Use existing infrastructure from Layers 1-3
   - 8-10 test methods
   - Many may reveal bugs in the solver that need fixing

**Total estimate**: 4-7 hours across 2-3 sessions.

---

## Success Criteria

| Criterion | Threshold |
|-----------|-----------|
| Single-range V3-V3 vs Möbius | Exact match (0 relative error) |
| Multi-range V3-V3 vs Brent | < 0.5% relative profit difference |
| Multi-range V3-V3 vs integer math | < 1% relative profit difference |
| All edge cases handled | No panics, no negative profits returned as success |
| No-arbitrage detection | Both solver and reference return failure |
| max_input constraint | Always respected |

## Risks

| Risk | Mitigation | Status |
|------|------------|--------|
| V3-V3 golden section misses global optimum (multiple local maxima) | Compare against Brent which uses a different search strategy; if discrepancy found, add candidate refinement | ✅ Tested — no discrepancies |
| Brute-force V3-V3 simulation is wrong | Validate brute-force against known V3 integer test cases first (reuse `test_mobius_v3_accuracy.py` patterns) | ✅ Validated |
| Tick crossing math in `compute_crossing` is wrong | Layer 1 profit function tests will surface this; fix `compute_crossing` if needed | ✅ Bug found & fixed (reversed direction) |
| Float64 precision insufficient for extreme values | Add precision warnings and skip tests where precision is known to be inadequate | ✅ No issues found |
| **Möbius optimum exceeds V3 range bounds** | Constrain max_input to range capacity; validate final sqrt price | ✅ Bug found & fixed (see below) |

---

## Session Start Checklist

When picking up this work:
1. `uv run pytest tests/arbitrage/test_optimizers/test_v3_v3_solver.py -x -q` — verify current 10 tests pass
2. `uv run pytest tests/arbitrage/test_optimizers/test_mobius_v3_accuracy.py -x -q` — verify V3 accuracy tests pass (reference pattern)
3. Read this plan
4. Start with Layer 1 (profit function tests) — they're the fastest to implement and most likely to surface bugs

---

## Range Bounds Bug Fix (Session 2)

**Discovered**: Benchmark revealed 52% profit mismatch on narrow tick ranges.

**Root cause**: `solve_v3_v3` used `mobius_solve` with unbounded constant-product reserves. The Möbius closed-form finds the optimal input assuming infinite liquidity, but V3 tick ranges are bounded — the price can't move past `sqrt_price_lower`/`sqrt_price_upper`. For narrow ranges, the unconstrained optimum fell 2.4× outside the range capacity.

**Example**: Stablecoin-like pool with 100-tick range width. The unconstrained Möbius optimum was 5.97e15, but the range capacity was only 2.49e15. V3 integer math capped at the range boundary, yielding actual profit of 2.38e13 vs the solver's claimed 7.2e13 (3× overestimate).

**Three-layer fix** in `rust/src/optimizers/mobius_v3_v3.rs`:

1. **Constrain `max_input` to range capacity** — Two new helper functions:
   - `compute_range_constrained_max_input()`: For single-range fast path, computes min(hop1 capacity, hop2 capacity inverted through Möbius formula, user max_input)
   - `compute_piecewise_range_max()`: Same logic for piecewise search, including crossing costs
   - Both pass constrained `max_input` to `mobius_solve` and golden section search

2. **Validate `mobius_solve` results** — After `mobius_solve` returns, the single-range fast path and k=0 baseline now verify that the final sqrt price stays within both ranges using `estimate_v3_final_sqrt_price()`. Out-of-range results are rejected.

3. **Validate inside `compute_v3_v3_profit`** — Profit function now takes optional `V3TickRangeHop` refs. When a hop has no crossing, it checks `estimate_v3_final_sqrt_price()` against range bounds. Out-of-range inputs return `-1e30` (which golden section search naturally avoids).

**New method**: `V3TickRangeHop::max_gross_input_in_range()` — computes the hard capacity limit:
- zfo: `L * (1/√P_lower - 1/√P_current) / γ`
- ofz: `L * (√P_upper - √P_current) / γ`

**New Rust tests** (6):

| Test | What it validates |
|------|------------------|
| `test_v3_v3_narrow_range_stays_in_bounds` | Regression: narrow range where old code returned 2.4× over-capacity input |
| `test_max_gross_input_in_range_zfo` | zfo formula correctness |
| `test_max_gross_input_in_range_ofz` | one-for-zero formula correctness |
| `test_max_gross_input_in_range_exhausted` | Zero capacity when price is at boundary |
| `test_compute_v3_v3_profit_rejects_out_of_range` | Profit function returns -1e30 for out-of-range inputs |

**Benchmark results after fix**: All 13 scenarios match brute-force within 1%. Previous mismatches (stablecoin 1-range: 52%, asymmetric 10×3: 15%) eliminated.

**Tick crossing extension** (same session): `max_candidates` increased from hardcoded 3 to configurable parameter (default 10). Benchmark confirms linear scaling (0.2μs → 4.7μs for 1→10 candidates on 10-range scenario).

**Files changed**:
- `rust/src/optimizers/mobius_v3.rs` — Added `max_gross_input_in_range()`
- `rust/src/optimizers/mobius_v3_v3.rs` — Range-constrained max_input, profit validation, 6 new tests
- `rust/src/optimizers/mobius_py.rs` — Exposed `max_gross_input_in_range()` to Python
- `benchmarks/v3_v3_solver_benchmark.py` — 13-scenario benchmark comparing Rust/Brent/brute-force
- `pyproject.toml` — Added benchmark lint exemptions

---

## Rust Test Suite Fixes (Session 3)

**Discovered**: 7 of 161 Rust unit tests failing after session 2 changes.

**Three root causes**:

1. **Symmetric two-hop test reserves** — `test_two_hop_profitable`, `test_simulate_path_matches_mobius_output`, `test_profitability_check_free` used mirrored reserves (r₂=s₁, s₂=r₁) where K/M = γ² < 1. Such paths are never profitable because the pools agree on price. Fixed with asymmetric reserves.

2. **Test inputs exceeding V3 range capacity** — `test_estimate_v3_final_sqrt_price_zero_for_one` used input 1e15 (max capacity ~1.1e14). `test_compute_v3_v3_profit_rejects_out_of_range` used "small" input 1e10 that produced ~1e16 output from hop 1, overwhelming hop 2's range. Fixed by reducing inputs.

3. **Float64 boundary precision** — The range bounds fix from session 2 was incomplete:
   - `compute_range_constrained_max_input` lacked `(1 - 1e-12)` shrink factor (already in `compute_v3_candidates_range_max`)
   - Range validation in `solve_v3_v3` and `compute_v3_v3_profit` used strict `contains_sqrt_price()` without tolerance
   - Added `eps = 1e-12 * sqrt_price_current` tolerance, consistent with `solve_v3_candidates`

**Convergence fix**: Golden section search with x_min=0 and large intervals was impractically slow (fixed MIN_ABS_INTERVAL=1e-6 requires ~96 iterations for 1e14 range). Scaled `abs_tol = max(1e-6, initial_interval * 1e-10)` in both `solve_piecewise` and `solve_v3_v3_piecewise`.

**Doc-test fix**: Wrapped 5 Unicode math formulas in ` ```text ` blocks to prevent Rust doc-test runner compilation.

**All 161 Rust unit tests passing, doc-tests clean, clippy clean.**
