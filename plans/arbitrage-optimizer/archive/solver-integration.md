# Solver Integration Plan

## Goal

Wire the unified `ArbSolver` into the production `_calculate` methods in `uniswap_2pool_cycle_testing.py`, replacing direct `scipy.optimize.minimize_scalar` calls.

## Current State

- `ArbSolver` with `MobiusSolver`, `NewtonSolver`, `BrentSolver` is implemented and tested (45 tests)
- `_calculate` in `uniswap_2pool_cycle_testing.py` has 9 inner methods (`_calculate_v2_v2`, `_calculate_v2_v3`, etc.) that each call `minimize_scalar` directly
- Each method builds a lambda that calls pool `calculate_tokens_out_from_tokens_in` / `calculate_tokens_in_from_tokens_out` to simulate swaps, then Brent optimizes that lambda
- After Brent finds the optimum, each method validates the result with actual pool swap calculations and builds `ArbitrageCalculationResult`

## Integration Strategy

### Phase 1: Add `ArbSolver` as an optimization layer alongside Brent

The current cycle methods use Brent to find the optimal forward token amount, then validate with pool methods. We'll add `ArbSolver` as a fast-path that runs first:

```
1. Build SolveInput from pool reserves/fees
2. Try ArbSolver (Mobius → Newton → Brent-fast)
3. If ArbSolver succeeds → validate with pool swap methods → return result
4. If ArbSolver fails → fall back to existing Brent path
```

This is additive — no existing code is removed. The feature flag pattern from AGENTS.md.

### Phase 2: Method-by-method integration

| Method | Pool Types | Solver | Rationale |
|--------|-----------|--------|-----------|
| `_calculate_v2_v2` | V2-V2 | MobiusSolver | 225x faster, exact closed-form |
| `_calculate_v2_v3` | V2-V3 | MobiusSolver (single-range) → Brent | Mobius if no tick crossing |
| `_calculate_v2_v4` | V2-V4 | MobiusSolver (single-range) → Brent | Same as V2-V3 |
| `_calculate_v3_v2` | V3-V2 | MobiusSolver (single-range) → Brent | Same as V2-V3 |
| `_calculate_v3_v3` | V3-V3 | Brent | Mobius can't handle both sides crossing |
| `_calculate_v3_v4` | V3-V4 | Brent | Both sides may have tick crossings |
| `_calculate_v4_v2` | V4-V2 | MobiusSolver (single-range) → Brent | Same as V2-V3 |
| `_calculate_v4_v3` | V4-V3 | Brent | Both sides may have tick crossings |
| `_calculate_v4_v4` | V4-V4 | Brent | Both sides may have tick crossings |

### Phase 3: Validation

For each method, after integration:
1. Run existing tests to verify identical profit results
2. Compare ArbSolver profit vs Brent profit — must match within 1 wei
3. Compare solve times — ArbSolver should be faster for V2-V2

## Implementation Details

### Building SolveInput from cycle pools

The cycle class knows:
- `self.swap_pools` — the two pools
- `self.input_token` — the profit token (WETH)
- Which pool is "high ROE" (buy) and "low ROE" (sell)
- `forward_token` — the intermediate token

For each `_calculate_*` method, the Hop construction is:
- Hop 1 (buy pool): input_token → forward_token, reserve_in = input_token reserves, reserve_out = forward_token reserves
- Hop 2 (sell pool): forward_token → input_token, reserve_in = forward_token reserves, reserve_out = input_token reserves

Wait — the orientation depends on which pool is "high" vs "low" ROE. Let me verify this.

### Reserve orientation

In the V2-V2 case:
- Pool_hi has higher ROE (buy forward token cheap, sell for more input token)
- Pool_lo has lower ROE (buy forward token expensive, sell for less input token)
- Strategy: deposit input_token into pool_lo → get forward_token → deposit forward_token into pool_hi → get input_token out

So the Hop sequence for the solver is:
- Hop 0 (pool_lo): input_token → forward_token (buy forward token)
  - reserve_in = pool_lo's input_token reserves
  - reserve_out = pool_lo's forward_token reserves
- Hop 1 (pool_hi): forward_token → input_token (sell forward token)
  - reserve_in = pool_hi's forward_token reserves
  - reserve_out = pool_hi's input_token reserves

This matches the existing lambda pattern: `pool_lo.calculate_tokens_out_from_tokens_in(forward_token_amount)` then `pool_hi.calculate_tokens_out_from_tokens_in(forward_token_amount)`.

Wait, looking more carefully at the code — the forward_token_amount in the lambda is the amount of forward_token, not input_token. The solver's `optimal_input` represents the input_token amount, and the path output should also be in input_token terms (profit = output - input).

Let me re-read the existing V2-V2 Brent lambda to confirm.

### Approach: Fast-path before Brent

Rather than replacing Brent, we'll add a fast-path:

```python
def _calculate_v2_v2(self, ...):
    # --- FAST PATH: Try ArbSolver first ---
    try:
        solve_input = _build_solve_input_v2_v2(pool_hi, pool_lo, input_token, forward_token)
        result = self._arb_solver.solve(solve_input)
        if result.success and result.profit > 0:
            # Validate with pool swap methods (same as current post-Brent validation)
            validated = self._validate_solver_result(pool_hi, pool_lo, result, forward_token, ...)
            if validated is not None:
                return validated
    except Exception:
        pass  # Fall through to Brent

    # --- EXISTING BRENT PATH (unchanged) ---
    ...
```

### Feature flag

Use `self._use_solver` flag on the cycle class, defaulting to `True`. Can be disabled for debugging or rollback.

## File Changes

1. `uniswap_2pool_cycle_testing.py` — Add `ArbSolver` integration to each `_calculate_*` method
2. `solver.py` — May need minor adjustments to `pool_to_hop` for state overrides
3. Tests — Add integration tests comparing solver output vs Brent output for each pool type pair

## Success Criteria

- [x] V2-V2 uses MobiusSolver (225x faster)
- [x] V2-V3/V2-V4 uses MobiusSolver when no tick crossing
- [x] All existing tests pass unchanged
- [x] Solver profit matches Brent profit within 1 wei for all V2-V2 cases
- [x] Feature flag allows disabling the fast path
- [x] Balancer multi-token pools use closed-form Eq.9 solver

## Balancer Multi-Token Integration

Added `BalancerMultiTokenHop` and `BalancerMultiTokenSolver` to the unified interface.

**Dispatch priority**: Balancer multi-token is checked after Solidly stable but before Newton/Brent.

```python
# In ArbSolver._select_method():
if all BalancerMultiTokenHop → BalancerMultiTokenSolver
```

**Pool types**: BalancerV2 weighted pools with 3+ tokens.

**Input**: `BalancerMultiTokenHop` with reserves, weights, fee, decimals, market_prices.

**Output**: `SolveResult` with `method=SolverMethod.BALANCER_MULTI_TOKEN`.
