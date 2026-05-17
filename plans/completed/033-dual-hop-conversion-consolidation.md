# Plan 033: Consolidate Dual Pool-to-Hop Conversion

## Overview

Eliminate the duplicate pool→hop conversion path in `solver_hop_builders.py`, making each pool's own `to_hop_state()` method the single source of truth. The external `pool_to_hop()` / `pool_state_to_hop()` / `pools_to_solve_input()` functions are removed; all callers are routed through the `ArbitrageCapablePool` / `ArbitragePathPool` protocol's `to_hop_state()` method.

**Dependency**: Plan 034 (delete legacy arbitrage cycles) should be implemented first, since the only `src/` consumers of `solver_hop_builders.py` are the legacy cycle classes. Implementing 034 first turns this plan into pure deletion with no caller migration.

## Files Involved

**Primary (delete):**
- `src/degenbot/arbitrage/optimizers/solver_hop_builders.py` — delete `pool_to_hop()`, `pool_state_to_hop()`, `pools_to_solve_input()`. Retain `_v3_utils.py` imports if pool `to_hop_state()` methods still need them.

**Primary (update):**
- `src/degenbot/arbitrage/optimizers/__init__.py` — remove re-exports of `pool_state_to_hop`, `pool_to_hop`, `pools_to_solve_input`
- `src/degenbot/arbitrage/path/arbitrage_path.py` — inline the three thin free functions (`_pool_to_hop_state`, `_extract_fee`, `_check_pool_compatibility`) into their call sites

**Secondary (rewrite):**
- `tests/arbitrage/test_optimizers/test_solver_hop_builders.py` — rewrite to test `pool.to_hop_state()` directly

**No change needed:**
- Pool classes (`UniswapV2Pool`, `UniswapV3Pool`, `UniswapV4Pool`, `AerodromeV2Pool`, `CamelotLiquidityPool`, `CurveStableswapPool`, `BalancerWeightedPool`) — `to_hop_state()` already exists and is the authoritative implementation

## Problem

Two parallel paths convert a Pool into a HopType for the Solver:

1. **Protocol method**: Each pool class implements `to_hop_state(zero_for_one, state_override)` defined on the `ArbitrageCapablePool` / `ArbitragePathPool` protocols in `pool_protocols.py`.

2. **External functions**: `solver_hop_builders.py` provides `pool_to_hop(pool, input_token)` and `pool_state_to_hop(pool, input_token, state_override)` — two 200+ line functions doing `isinstance` chains that largely duplicate the pool methods.

These paths **diverge**:

- `solver_hop_builders.py` constructs `swap_fn` closures differently from the pool methods. For example, the Aerodrome stable-path logic captures different variable bindings, and the Camelot stable path wires `k_func`/`get_y_func` differently.
- The external functions use `input_token: Erc20Token` to derive `zero_for_one`, while the protocol method takes `zero_for_one: bool` directly. This is a signature mismatch that forces callers to translate.
- `pool_state_to_hop` duplicates almost every line of `pool_to_hop` with the addition of a `state_override` parameter. The pool methods handle this uniformly via `state = state_override or self.state`.

Adding a new pool variant currently means patching **both** paths and hoping they stay consistent. This is a locality failure: the knowledge of "how pool X converts to a HopType" should live in one place.

The **deletion test** confirms this: deleting `solver_hop_builders.py` would not cause complexity to reappear elsewhere — the pool methods already provide all the functionality.

### Caller audit (post-Plan 034)

The only `src/` consumers of `pool_to_hop` / `pool_state_to_hop` / `pools_to_solve_input` are the legacy cycle classes:

- `arbitrage/uniswap_lp_cycle.py` — uses `pool_state_to_hop`, `pools_to_solve_input`
- `arbitrage/uniswap_2pool_cycle_testing.py` — uses `pool_state_to_hop`

Both are deleted by Plan 034. After Plan 034, the only remaining references are:
- `arbitrage/optimizers/__init__.py` — re-exports (remove)
- `tests/arbitrage/test_optimizers/test_solver_hop_builders.py` — tests for the old functions (rewrite)

`ArbitragePath` already calls `pool.to_hop_state()` via its own thin wrapper `_pool_to_hop_state`. No migration needed.

## Solution

### Step 1: Audit divergence between the two paths

Compare each `isinstance` branch in `pool_to_hop()` and `pool_state_to_hop()` against the corresponding `to_hop_state()` method on each pool class. Document any differences in:

- `swap_fn` closure construction (Aerodrome stable, Camelot stable)
- Fee extraction
- Reserve orientation
- `BoundedProductHop` fields (tick ranges, virtual reserves)

Expected result: the pool methods are the authoritative implementation; any logic only present in `solver_hop_builders.py` that is correct must be ported to the pool method before deletion.

This step is read-only and zero-risk. If both paths produce identical results, proceed directly to deletion. If they diverge, the pool method is assumed authoritative unless a test proves otherwise — any missing logic in the pool method is ported there.

### Step 2: Inline thin free functions in `arbitrage_path.py`

Replace the three one-line free functions with inline calls to the protocol methods:

**`_pool_to_hop_state(pool, zero_for_one)`** → `pool.to_hop_state(zero_for_one=zero_for_one)` at each call site:

- `__init__` list comprehension: `_pool_to_hop_state(pool, self._swap_vectors[i].zero_for_one)` → `pool.to_hop_state(zero_for_one=self._swap_vectors[i].zero_for_one)`
- `_refresh_hop_states`: same pattern
- `notify`, `_resolve_state_overrides`: same pattern with `state_override` argument

**`_extract_fee(pool, zero_for_one)`** → `pool.extract_fee(zero_for_one=zero_for_one)` at the single call site in `build_swap_amounts`.

**`_check_pool_compatibility(pool)`** → inline the try/except directly in `_validate_pools`:

```python
# Before:
def _check_pool_compatibility(pool):
    try:
        pool.to_hop_state(zero_for_one=True)
    except (IncompatiblePoolInvariant, AttributeError):
        return PoolCompatibility.INCOMPATIBLE_INVARIANT
    else:
        return PoolCompatibility.COMPATIBLE


# In _validate_pools:
for i, pool in enumerate(self._pools):
    compat = _check_pool_compatibility(pool)
    if compat != PoolCompatibility.COMPATIBLE:
        ...

# After:
for i, pool in enumerate(self._pools):
    try:
        pool.to_hop_state(zero_for_one=True)
    except (IncompatiblePoolInvariant, AttributeError):
        msg = f"Pool {i} ({type(pool).__name__}) is not Mobius-compatible"
        raise PathValidationError(msg)
```

This also lets us remove the `PoolCompatibility` enum from `arbitrage/path/types.py` — it only existed as the return type of `_check_pool_compatibility`.

### Step 3: Delete `solver_hop_builders.py` functions

Remove `pool_to_hop()`, `pool_state_to_hop()`, and `pools_to_solve_input()`. The file may be deleted entirely or reduced to just `_v3_utils.py` imports if pool `to_hop_state()` methods still depend on `_v3_virtual_reserves` / `_get_cached_tick_ranges`.

Remove re-exports from `src/degenbot/arbitrage/optimizers/__init__.py`:

```python
# Remove these lines:
from degenbot.arbitrage.optimizers.solver_hop_builders import (
    pool_state_to_hop,
    pool_to_hop,
    pools_to_solve_input,
)

("pool_state_to_hop",)
("pool_to_hop",)
("pools_to_solve_input",)
```

### Step 4: Rewrite tests

- **Delete** `tests/arbitrage/test_optimizers/test_solver_hop_builders.py` — it tests functions that no longer exist.
- **Ensure** `tests/arbitrage/test_path/test_arbitrage_path.py` covers `to_hop_state()` directly (it already tests `_pool_to_hop_state`, which after inlining will call the protocol method directly — no change needed to test logic, just remove the import of the deleted free function).
- **Add** `to_hop_state()` tests per pool type to `tests/uniswap/` and `tests/aerodrome/` if they don't already exist. Each test verifies the returned HopType has correct:
  - `reserve_in` / `reserve_out` orientation
  - `fee` value
  - `invariant` tag
  - `swap_fn` closure produces correct output (for SolidlyStableHop and CurveStableswapHop)
  - `tick_ranges` / `current_range_index` (for BoundedProductHop)

## Implementation Order

1. **Step 1**: Audit divergence (read-only, zero risk)
2. **Step 2**: Inline thin free functions in `arbitrage_path.py` (one file, self-contained)
3. **Step 3**: Delete `solver_hop_builders.py` functions and `__init__.py` re-exports
4. **Step 4**: Rewrite tests

Steps 2 and 3 can be a single commit. Step 4 is a separate commit.

**Precondition**: Plan 034 (delete legacy arbitrage cycles) should be implemented first, since the only `src/` callers are the legacy cycle classes. If Plan 034 is not yet done, Step 3 must also remove the imports from the legacy files — but those files are going away anyway, so it's cleaner to do 034 first.

## Testing

### Per-step verification

- After Step 1: document differences; no code change
- After Step 2: `just test-python` passes (inlining doesn't change behavior)
- After Step 3: `just test-python` passes with external functions removed
- After Step 4: `just test-python` passes with tests targeting `to_hop_state()` directly

### New test coverage

- For each pool type with a `to_hop_state()` method, add a direct test that verifies the returned HopType. See Step 4 for the full checklist.

## Benefits

- **Locality**: Pool→hop conversion defined in one place (the pool's own method). Bugs in hop conversion are found by reading one method per pool, not two parallel implementations.
- **Leverage**: Adding a new pool type means implementing `to_hop_state()` on the pool class. No need to touch an external module or remember two parallel isinstance chains.
- **Testability**: Each pool's `to_hop_state()` is tested directly, not through an external dispatcher that hides which code path was taken.
- **Code reduction**: ~486 lines removed from `solver_hop_builders.py` (the pool methods already exist and are not being added). ~20 lines removed from `arbitrage_path.py` (inlined free functions). `PoolCompatibility` enum removed from `path/types.py`.

## Risks

- **swap_fn closure equivalence**: The `swap_fn` closures in `solver_hop_builders.py` may capture different variable bindings than the pool method's closures. Thorough comparison in Step 1 mitigates this. If they differ in observable behavior, the test suite should catch it.
- **V3 utils coupling**: `_v3_utils.py` (`_v3_virtual_reserves`, `_get_cached_tick_ranges`) is imported by both `solver_hop_builders.py` and pool `to_hop_state()` methods. Ensure the pool methods already import these directly — if so, deleting `solver_hop_builders.py` doesn't affect the import chain.
- **Implementation order dependency**: If Plan 034 is not done first, Step 3 must also handle the imports in `uniswap_lp_cycle.py` and `uniswap_2pool_cycle_testing.py`. These files are deprecated and slated for deletion, so patching them is wasted effort. Implementing Plan 034 first avoids this.

## Relationship to Other Plans

- **Plan 034** (Delete Legacy Arbitrage Cycle Classes): Should be implemented first. The legacy cycle classes are the only `src/` consumers of `solver_hop_builders.py`. After 034, this plan is pure deletion.
- **Plan 028** (Builder Registry & Pool Class Restructuring): Complete. The pool `to_hop_state()` methods were created as part of that plan. This plan removes the pre-028 legacy path.
- **Plan 011** (Unify UniswapLpCycle._calculate() Behind ArbSolver Seam): Complete. `ArbitragePath` already uses the protocol method. This plan removes the last remnant of the old path.
- **ADR-001** (I/O-Free Pools): The `to_hop_state()` methods are pure calculation — no I/O. Consistent with the I/O-free architecture.
