# Plan 011: Unify UniswapLpCycle._calculate() Behind the ArbSolver Seam

## Overview

Make `UniswapLpCycle._calculate()` delegate to `ArbSolver.solve()` by converting
its pool sequence to `SolveInput` via the existing `pools_to_solve_input()`
builder. Delete the pool-type-specific `_arb_profit()` method. Keep
`_build_swap_amounts()` for calldata generation (different concern).

This eliminates dual maintenance: the optimization algorithm lives in one
place (the solvers), and pool-type changes need updates in one place
(`solver_hop_builders.py`).

**Status: COMPLETE** ✅

## Files Involved

- **Existing:**
  - `src/degenbot/arbitrage/uniswap_lp_cycle.py` (~768 lines)
  - `src/degenbot/arbitrage/optimizers/solver.py` (~283 lines) — `ArbSolver`, re-exports
  - `src/degenbot/arbitrage/optimizers/solver_hop_builders.py` (~382 lines) — hop builders
  - `src/degenbot/arbitrage/optimizers/_solver_utils.py` (~152 lines)
  - `src/degenbot/arbitrage/optimizers/hop_types.py` — `SolveInput`, `SolveResult`, `Solver`
  - `src/degenbot/types/hop_types.py` — `HopType`, `ConstantProductHop`, `BoundedProductHop`, `SolidlyStableHop`
- **Modified:**
  - `src/degenbot/arbitrage/uniswap_lp_cycle.py` — `_calculate()` and `_arb_profit()`
- **May need updates:**
  - `src/degenbot/arbitrage/optimizers/solver_hop_builders.py` — `pool_state_to_hop()` missing Camelot stable swap_fn and Aerodrome stable swap_fn (present in `pool_to_hop` but not `pool_state_to_hop`)

## Problem

Two parallel paths optimize arbitrage:

**Path A (old):** `UniswapLpCycle._calculate()` → scipy `minimize_scalar` →
`_arb_profit(x, state_overrides)` → match/case over every pool type →
`calculate_tokens_out_from_tokens_in()` per pool.

**Path B (new):** `ArbSolver.solve(SolveInput(hops))` → `MobiusSolver` / `BrentSolver`
→ `_simulate_path(amount, hops)` → Hop abstractions.

`_simulate_path()` and `_arb_profit()` re-implement the same loop:
- Walk pools in order
- For each pool, compute output given input
- Feed output as next input
- Return final output

The difference is only the input representation: `Pool` objects vs `Hop`
objects. `_arb_profit()` already has pool-type dispatch for AerodromeV2Pool,
UniswapV2Pool, UniswapV3Pool, UniswapV4Pool.

This is leakage across a seam. A change to Aerodrome stable pool swap math
requires edits in both `solver_hop_builders.py` and `uniswap_lp_cycle.py`.

The `ArbSolver` seam already has multiple adapters (`MobiusSolver`,
`BrentSolver`, etc.). Adding the cycle calculation as a consumer proves it's
a real seam.

## Target State

`UniswapLpCycle` delegates optimization to `ArbSolver`:

```python
class UniswapLpCycle(...):
    def _calculate(self, state_overrides=None):
        if state_overrides is None:
            state_overrides = {}

        # Convert pools → hops, with state overrides applied
        if state_overrides:
            hops = []
            current_token = self.input_token
            for pool in self.swap_pools:
                hop = pool_state_to_hop(pool, current_token, state_overrides.get(pool))
                hops.append(hop)
                current_token = pool.token1 if current_token == pool.token0 else pool.token0
            solve_input = SolveInput(hops=tuple(hops), max_input=self.max_input)
        else:
            solve_input = pools_to_solve_input(
                self.swap_pools,
                self.input_token,
                max_input=self.max_input,
            )

        # Optimize via ArbSolver
        solver = ArbSolver()
        result = solver.solve(solve_input)

        # Build swap amounts for calldata from the optimal input
        optimal_amounts = self._build_swap_amounts(
            token_in_quantity=result.optimal_input,
            state_overrides=state_overrides,
        )

        # Compute profit from swap amounts (same as before)
        input_swap, *_, output_swap = optimal_amounts
        ...  # existing profit extraction logic unchanged

        return ArbitrageCalculationResult(...)
```

`_arb_profit()` is **deleted entirely**. The `scipy` import can be removed.

## Current vs Target Flow

```
CURRENT:
  UniswapLpCycle._calculate()
    → minimize_scalar(_arb_profit)
      → match/case per pool type
        → pool.calculate_tokens_out_from_tokens_in()
    → _build_swap_amounts(result.x)

TARGET:
  UniswapLpCycle._calculate()
    → pools_to_solve_input() / pool_state_to_hop()  [existing builders]
    → ArbSolver.solve(solve_input)
      → MobiusSolver / BrentSolver / etc.
        → _simulate_path()    [uses Hop abstractions]
    → _build_swap_amounts(result.optimal_input)
```

## Gaps to Close Before Migration

### Gap 1: `pool_state_to_hop()` missing Camelot and Aerodrome stable `swap_fn`

`pool_to_hop()` includes custom `swap_fn` closures for:
- Camelot stable pools (`_camelot_stable_swap_fn`)
- Aerodrome stable pools (`_aerodrome_stable_swap_fn`)

`pool_state_to_hop()` does not include these `swap_fn` closures — it returns
bare `SolidlyStableHop` without a custom `swap_fn`. This means state-override
paths through `pool_state_to_hop()` for Camelot/Aerodrome stable pools will
use the default Solidly swap math, which may differ from the pool-specific
implementation.

**Fix:** Add `swap_fn` closures to `pool_state_to_hop()` for Camelot and
Aerodrome stable pools, matching what `pool_to_hop()` does.

### Gap 2: Behavioral change on unprofitable paths

**`minimize_scalar`** returns a result even when profit is negative.
`_calculate()` returns an `ArbitrageCalculationResult` with `profit_amount` that
may be ≤ 0. **`ArbSolver.solve()`** raises `OptimizationError` if no sub-solver
finds a profitable solution (profit > 0).

This is an **improvement, not a regression.** The only internal caller of
`_calculate()` is `calculate()`, and its consumer in `arbitrage_path.py` already
catches `OptimizationError` and dispatches `_StateUpdatedNoProfit`. Returning
a negative-profit result was never useful — you can't execute a negative-profit
arb.

External callers who used `calculate()` and checked `profit_amount > 0` themselves
will now get `OptimizationError` instead. This is a cleaner contract: the
exception means "don't bother."

**Fix:** Document this behavioral change in the migration. No code change needed.
`ArbSolver` already handles the `(EVMRevertError, LiquidityPoolError)` case from
`_arb_profit()` — sub-solvers catch these internally, and `ArbSolver` falls
through to the next solver.

### Gap 3: No Camelot pools in `UniswapLpCycle.Pool` type alias

`UniswapLpCycle`'s `Pool` type alias is:
```python
type Pool = AerodromeV2Pool | AerodromeV3Pool | UniswapV2Pool | UniswapV3Pool | UniswapV4Pool
```

Camelot pools are handled by the solver/hop builders but not by `UniswapLpCycle`.
This is fine — not a gap, just noting the scope.

### Gap 4: AerodromeV3Pool not in `pool_to_hop()` dispatch

`AerodromeV3Pool(UniswapV3Pool)` is in the `Pool` type alias but has no
explicit case in `_arb_profit()`, `_build_swap_amounts()`, or `pool_to_hop()`.
It's handled by the `UniswapV3Pool()` / `isinstance(pool, UniswapV3Pool)`
match arms due to inheritance. No issue — just documenting.

## Migration Steps

### Step 1: Close Gap 1 — add `swap_fn` to `pool_state_to_hop()` (TDD)

Add Camelot and Aerodrome stable `swap_fn` closures to `pool_state_to_hop()`,
matching `pool_to_hop()`. Write tests that verify `pool_state_to_hop()` with
state overrides produces the same `SolidlyStableHop` (with `swap_fn`) as
`pool_to_hop()`.

### Step 2: Write parity test

Before changing `_calculate()`, write a test that proves `ArbSolver.solve()`
produces the same optimal input as `minimize_scalar(_arb_profit)` for a set
of pool configurations:

```python
def test_arb_solver_matches_legacy_for_v2_pair():
    """ArbSolver and _arb_profit must agree on optimal input for a V2 pair."""
    cycle = UniswapLpCycle(input_token=FAKE_WETH, swap_pools=[FAKE_POOL_0, FAKE_POOL_1])

    # Legacy path
    legacy_result = cycle._calculate()

    # New path
    solve_input = pools_to_solve_input(
        list(cycle.swap_pools), cycle.input_token, max_input=cycle.max_input
    )
    solver = ArbSolver()
    solver_result = solver.solve(solve_input)

    # Tolerance: solvers may differ by rounding; assert within 1% or N wei
    assert abs(legacy_result.input_amount - solver_result.optimal_input) < TOLERANCE
```

Run this for V2-V2, V2-V3, V3-V3, and mixed paths.

### Step 3: Replace `_calculate()` body

Replace `minimize_scalar(_arb_profit)` with `ArbSolver.solve()` delegation.
The `_build_swap_amounts()` call and profit extraction remain unchanged.

### Step 4: Delete `_arb_profit()`

Remove the method and the `scipy` import.

### Step 5: Run full arbitrage test suite

## What Stays the Same

- `_build_swap_amounts()` — not touched. This is calldata generation, a
  different concern from optimization.
- `calculate()` public API — same signature, same return type.
- `generate_payloads()` — not touched.
- `notify()` / `_pre_calculation_check()` — not touched.
- `ArbitrageCalculationResult` — same shape.

## Risks

| Risk | Mitigation |
|------|------------|
| `_build_swap_amounts()` and `_simulate_path` use different math precision (int vs float) | The solver tests already exercise this; integer refinement exists in `_rust_integer_refinement()` |
| `state_overrides` not fully supported by `pool_state_to_hop()` for all pool types | Gap 1 fix closes this before migration |
| `ArbSolver` doesn't find profitable solution where scipy did | Parity test (Step 2) catches this before migration |
| Solvers return `int` optimal_input vs scipy's `float` | `SolveResult.optimal_input` is already `int` — `_build_swap_amounts()` already takes `int` |
| `OptimizationError` from solver when scipy would have found a near-zero profit | `ArbSolver` tries multiple sub-solvers; if all fail, `OptimizationError` propagates as the documented behavior |

## Definition of Done

- [x] `pool_state_to_hop()` has `swap_fn` for Camelot and Aerodrome stable pools
- [x] Parity tests prove `ArbSolver.solve()` matches legacy `_calculate()` for V2-V2, V2-V3, V3-V3 paths
- [x] `_arb_profit()` deleted from `uniswap_lp_cycle.py`
- [x] `scipy` import removed from `uniswap_lp_cycle.py`
- [x] `UniswapLpCycle._calculate()` delegates to `ArbSolver.solve()`
- [x] `_build_swap_amounts()` still produces correct swap amounts from solver result
- [x] State overrides work through `pool_state_to_hop()`
- [x] All arbitrage cycle tests pass
- [x] No match/case on pool types in `_calculate()` or `_arb_profit()` (the latter deleted)
