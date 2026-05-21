# Plan 069: Replace DyCalculationInputs Closures with Pre-Resolved Values

## Overview

Remove the `get_y` and `newton_y` closures from `DyCalculationInputs` and replace them with the pre-resolved values that calculators need (amp, d_variant, y_variant, yd_variant, a_precision). Calculators call the pure `stableswap_get_y()` and `stableswap_newton_y()` functions directly, eliminating the hidden back-reference from calculators to the pool.

## Problem

### Deletion test

If you delete the two closures from `DyCalculationInputs`, the calculators currently call them. But `calculations/stableswap.py` already exports the pure functions `stableswap_get_y()` and `stableswap_newton_y()` that the closures wrap. The closures add only: (1) A-ramping resolution, (2) variant enum lookup, (3) `EVMRevertError` wrapping. All three can be provided as pre-resolved values or handled at the call site.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| `get_y` closure captures `self` | `curve_stableswap_liquidity_pool.py:392–396` | `def get_y(i, j, x, xp_): return self._get_y(i, j, x, xp_)` — captures the entire pool object |
| `newton_y` closure captures `self` | `curve_stableswap_liquidity_pool.py:398–401` | Same — `def newton_y(ann, gamma, xp_, d, token_index): return self._newton_y(...)` |
| Calculators not truly pure | All calculators in `curve/calculators/` | They call `inputs.get_y()` which delegates to `self._get_y()` on the pool — this is an I/O-capable object, not pure math |
| `DyCalculationInputs` is not a value object | `curve/types.py:192–194` — `get_y` and `newton_y` are callable fields | Frozen dataclass with callable fields that capture mutable state — confusing for users and testers |
| `self._get_y()` does A-resolution + variant dispatch | `curve_stableswap_liquidity_pool.py:543–569` | The closure hides amp resolution, variant selection, and `EVMRevertError` wrapping — logic that should be explicit in the calculator |

## Solution

### Step 1: Add pre-resolved values to `DyCalculationInputs`

The calculators that call `inputs.get_y()` need: amp (already present), `d_variant`, `y_variant`, `a_precision`, and the pool's n_coins (already present). Add these as fields:

```python
# Before
@dataclass(slots=True, frozen=True)
class DyCalculationInputs:
    # ... existing fields ...
    get_y: Callable[[int, int, int, Sequence[int]], int] = field(default=None)
    newton_y: Callable[[int, int, Sequence[int], int, int], int] = field(default=None)

# After
@dataclass(slots=True, frozen=True)
class DyCalculationInputs:
    # ... existing fields ...
    # Strategy enums (for pure invariant solving)
    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    a_precision: int = 100  # A_PRECISION constant
```

### Step 2: Update calculators to call pure functions directly

```python
# Before (standard.py example)
class StandardDyCalculator:
    def calculate(self, i, j, dx, *, inputs, override_state=None):
        # ... rate/fee logic ...
        y = inputs.get_y(i, j, x, xp)  # calls closure → pool._get_y()

# After
from degenbot.calculations.stableswap import stableswap_get_y

class StandardDyCalculator:
    def calculate(self, i, j, dx, *, inputs, override_state=None):
        # ... rate/fee logic ...
        amp = inputs.amp  # already pre-resolved (includes A-ramping)
        try:
            y = stableswap_get_y(
                i, j, x=x, xp=xp, amp=amp,
                n_coins=inputs.n_coins,
                a_precision=inputs.a_precision,
                y_variant=inputs.y_variant,
                d_variant=inputs.d_variant,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e
```

### Step 3: Remove closures from `_resolve_calculation_inputs_via_io`

```python
# Before
def _resolve_calculation_inputs_via_io(self, ...):
    # ... resolve rates, xp, amp ...
    def get_y(i, j, x, xp_): return self._get_y(i, j, x, xp_)
    def newton_y(ann, gamma, xp_, d, token_index): return self._newton_y(ann, gamma, xp_, d, token_index)
    inputs = DyCalculationInputs(
        ...,
        get_y=get_y,
        newton_y=newton_y,
    )

# After
def _resolve_calculation_inputs_via_io(self, ...):
    # ... resolve rates, xp, amp (same as before) ...
    inputs = DyCalculationInputs(
        ...,
        d_variant=self._strategies.d_variant,
        y_variant=self._strategies.y_variant,
        yd_variant=self._strategies.yd_variant,
        a_precision=self.A_PRECISION,
    )
```

### Step 4: Update `CurveStableswapPool._get_y` callers

The pool's own `_get_y()` and `_newton_y()` methods are still used by `_get_d()` and `calc_withdraw_one_coin()`. These continue to call the pure functions directly (they already do) and wrap with `EVMRevertError`. No change needed — the pool methods remain as they are; only the closure-passing to calculators is removed.

### Design decisions

- **Keep `_get_y` and `_newton_y` on the pool class**: They're used by `calc_token_amount`, `calc_withdraw_one_coin`, and `_get_d`. These pool-internal methods don't need `DyCalculationInputs` — they call `stableswap_get_y()` directly.
- **EVMRevertError wrapping moves to calculators**: Previously, `_get_y()` wrapped `ValueError → EVMRevertError`. After this change, calculators call `stableswap_get_y()` themselves and do the wrapping. This is appropriate — the calculator is the boundary between pure math and pool-level error handling.
- **`a_precision` as a field, not a constant**: `A_PRECISION = 100` is a class constant on the pool. Adding it as a field on `DyCalculationInputs` makes the calculator dependency explicit. The value is always 100 for Curve V1, but making it a field allows future pools with different precision.
- **`d_variant`, `y_variant`, `yd_variant` are already on `PoolStrategies`**: The pool looks them up from `self._strategies.d_variant` etc. Passing them through `DyCalculationInputs` makes the calculator's dependencies explicit — it needs these to call the right variant of `stableswap_get_y()`.

## Files Involved

**Primary:**
- `src/degenbot/curve/types.py` — add `d_variant`, `y_variant`, `yd_variant`, `a_precision` to `DyCalculationInputs`; remove `get_y` and `newton_y` callable fields
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — remove closure construction from `_resolve_calculation_inputs_via_io` and `_resolve_metapool_inputs_via_io`; pass variant enums instead
- `src/degenbot/curve/calculators/standard.py` — call `stableswap_get_y()` directly instead of `inputs.get_y()`
- `src/degenbot/curve/calculators/crypto.py` — call `stableswap_newton_y()` directly instead of `inputs.newton_y()`
- `src/degenbot/curve/calculators/live_admin.py` — call `stableswap_get_y()` directly
- `src/degenbot/curve/calculators/metapool.py` — call `stableswap_get_y()` directly

**Secondary:**
- `src/degenbot/calculations/stableswap.py` — no change (pure functions already exported)
- `tests/curve/` — update any test that constructs `DyCalculationInputs` with closure fields

**No change needed:**
- `src/degenbot/curve/_pool_strategies.py` — strategies already carry the variant enums
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — no closure involvement (per-block caches are `_cache_*` fields with `_get_cached_*` accessors, absorbed Plan 068)

## Implementation Order

### Slice 1: Add variant fields to `DyCalculationInputs`, keep closures temporarily

1. Add `d_variant: DVariant`, `y_variant: YVariant`, `yd_variant: YDVariant`, `a_precision: int` fields to `DyCalculationInputs`
2. Populate these fields in `_resolve_calculation_inputs_via_io` and `_resolve_metapool_inputs_via_io`
3. Keep `get_y` and `newton_y` closures for now (backwards compat)
4. Run: `just test-python` — expect all green

### Slice 2: Migrate `standard.py` calculator to call `stableswap_get_y()` directly

1. Update `StandardDyCalculator` to call `stableswap_get_y(inputs.amp, i, j, x=x, xp=xp, n_coins=inputs.n_coins, a_precision=inputs.a_precision, y_variant=inputs.y_variant, d_variant=inputs.d_variant)` instead of `inputs.get_y(i, j, x, xp)`
2. Add `EVMRevertError` wrapping around the call
3. Run: `just test-python` — expect green (standard calculator tests pass)

### Slice 3: Migrate remaining calculators

1. Migrate `RateAdjustedDyCalculator`, `RateAdjustedNoOneDyCalculator`, `RawBalanceDyCalculator` — they call `inputs.get_y()`
2. Migrate `CryptoDyCalculator` — it calls `inputs.newton_y()`
3. Migrate `LiveAdmin*` calculators — they call `inputs.get_y()`
4. Migrate `CytokenDyCalculator`, `NoOneFeeRateDyCalculator` — they call `inputs.get_y()`
5. Migrate metapool calculators — they call `inputs.get_y()`
6. Run: `just test-python` — expect all green

### Slice 4: Remove closure fields from `DyCalculationInputs`

1. Remove `get_y` and `newton_y` fields from `DyCalculationInputs`
2. Remove closure construction from `_resolve_calculation_inputs_via_io` and `_resolve_metapool_inputs_via_io`
3. Update any tests that construct `DyCalculationInputs` with closure fields
4. Run: `just test-python` — expect all green

### Slice 5: Validate and clean up

1. Run `just lint` + `just test-all`
2. Verify `DyCalculationInputs` is now a pure value object (all fields are ints, tuples, or enums — no callables)
3. Update `curve/CONTEXT.md` to note that calculators call pure `stableswap_*` functions directly

## Testing

### Per-slice test runs

Each slice runs `just test-python`. The calculator tests in `tests/curve/` are the primary validation.

### New unit tests

```python
# tests/curve/calculators/test_pure_invariant_solving.py


def test_standard_calculator_calls_pure_stableswap_get_y():
    """StandardDyCalculator resolves dy via stableswap_get_y, not a closure."""
    inputs = DyCalculationInputs(
        PRECISION=10**18,
        FEE_DENOMINATOR=10**10,
        fee=4_000_000,
        n_coins=2,
        balances=(1000000000, 1000000000),
        # ... other required fields ...
        amp=1000 * 100,  # pre-resolved
        d_variant=DVariant.STANDARD,
        y_variant=YVariant.STANDARD,
        a_precision=100,
        # NO get_y closure!
    )
    calc = StandardDyCalculator()
    result = calc.calculate(0, 1, 1000, inputs=inputs)
    assert result > 0
```

### Integration tests

Existing `tests/curve/test_pool_strategies.py` and `tests/curve/test_curve_io_free_example.py` exercise the full pool → calculator pipeline.

## Benefits

- **Locality**: Invariant-solving logic concentrates in `calculations/stableswap.py` and the calculator modules — not hidden behind closures on a data class
- **Leverage**: `DyCalculationInputs` is now a pure value object — no closures, no pool capture. It can be serialized, compared, and tested without any live object.
- **Depth**: The calculator interface deepens — callers provide values, not closures. The calculator owns its error wrapping and variant dispatch.
- **Deletion test**: Delete the closures → calculators call the pure functions that already exist. No unique behavior is lost.

## Risks

- **More fields on `DyCalculationInputs`**: Adding `d_variant`, `y_variant`, `yd_variant`, `a_precision` increases the field count from ~22 to ~26. This is acceptable — they're all primitive types (enums and ints), not callables. The dataclass is large but pure.
- **`EVMRevertError` wrapping moves to calculators**: Each calculator must now wrap `stableswap_get_y()` calls. This is 3 lines per call site. There are ~7 calculator files. The wrapping was previously centralized in `pool._get_y()`, but this centralization was misleading — the pool method was called through a closure, making the error path invisible. Explicit wrapping is clearer.
- **Calculator import dependency on `EVMRevertError`**: Calculators in `curve/calculators/` will import from `degenbot.exceptions.pool`. This is a new cross-module dependency. `EVMRevertError` is a domain exception, so this is appropriate — the calculator is the boundary between pure math and pool-level errors.

## Relationship to Other Plans

- **Plan 068** (Absorb CurveOnChainCache): Complementary — after absorbing the cache, the pool's I/O resolution is all in one place, making the closure elimination simpler (no cache object between pool and provider).
- **Plan 039** (DyCalculator seam): This plan continues the trajectory of Plan 039 — making calculators pure. Removing closures is the final step in making `DyCalculationInputs` a true value object.
- **Plan 045** (Calculator explicit data): This plan extends Plan 045, which replaced `pool` parameter with `DyCalculationInputs`. Now `DyCalculationInputs` itself is cleaned of its closure fields.

## Status

[x] Slice 1: Add variant fields to `DyCalculationInputs`, keep closures temporarily
    - Added `d_variant`, `y_variant`, `yd_variant`, `a_precision` fields to `DyCalculationInputs`
    - Pool passes these in `_resolve_calculation_inputs_via_io`
    - Amp resolution now y_variant-aware (VARIANT_0 divides by A_PRECISION, others keep raw)
[x] Slice 2: Migrate `standard.py` calculator to call `stableswap_get_y()` directly
[x] Slice 3: Migrate remaining calculators
    - `standard.py`: 6 calls — all call `stableswap_get_y()` + `EVMRevertError` wrapping
    - `metapool.py`: 6 calls — all call `stableswap_get_y()` + `EVMRevertError` wrapping
    - `crypto.py`: 1 call — calls `stableswap_newton_y()` + `EVMRevertError` wrapping
    - `live_admin.py`: 4 calls — all call `stableswap_get_y()` + `EVMRevertError` wrapping
[x] Slice 4: Remove closure fields from `DyCalculationInputs`
    - Removed `get_y` and `newton_y` callable fields
    - Removed `Callable`/`Sequence` imports from `types.py` (no longer needed)
    - Removed closure construction from `_resolve_calculation_inputs_via_io` (10 lines)
    - Removed `get_y`/`newton_y` from `DyCalculationInputs` constructor call
    - Updated `DyCalculator` protocol docstring: "closures" → "variant enums"
[x] Slice 5: Validate and clean up
    - `just test-python`: 3022 passed
    - `just test-rust`: all passed
    - `ruff check`: no new errors (only pre-existing ones)
    - `DyCalculationInputs` is now a pure value object (all fields are ints, tuples, enums, or None — zero callables)
    - Updated `curve/CONTEXT.md`: DyCalculationInputs description, Relationships section, and example dialogue
