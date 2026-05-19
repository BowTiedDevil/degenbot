# Plan 056: Move Calculator Factory Functions to Enum Types

## Overview

Move the `_make_dy_calculator`, `_make_metapool_dy_calculator`, and
`_make_metapool_underlying_dy_calculator` factory functions from
`_pool_strategies.py` onto their respective enum types as instance methods.
Then remove the pool class's import of these private functions, eliminating
the locality violation where the pool reaches into the strategy resolution
module for calculator construction.

This supersedes the original Plan 056 (externalize mapping to database).
Externalizing to DB is a valid future step but provides less leverage than
fixing the locality violation first — the database just changes where the
mapping lives, while this plan removes the pool→strategy-module import
coupling entirely.

## Problem

### Deletion test

If you deleted `_pool_strategies.py`, `CurveStableswapPool` would fail to
import because it imports `_make_dy_calculator`, `_make_metapool_dy_calculator`,
and `_make_metapool_underlying_dy_calculator` from it. The pool class should
not depend on the strategy resolution module — it only needs the strategies
themselves (which it already receives as `PoolStrategies`).

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Pool imports private factory functions from strategy module | `curve_stableswap_liquidity_pool.py` line 26-29 | The pool class reaches into the strategy resolution module for calculator construction. If the factory logic changes, the pool must be updated. This is a locality violation: the pool should only depend on `PoolStrategies`, not on how strategies are resolved. |
| Calculator factory functions are free functions, not on the enum they dispatch on | `_pool_strategies.py` `_make_dy_calculator`, `_make_metapool_dy_calculator`, `_make_metapool_underlying_dy_calculator` | Each factory is a `match` statement on an enum value. This pattern belongs on the enum itself — the enum owns the dispatch, so it should own the factory. |
| Lazy calculator construction in pool is dead code | Pool class lines 621, 636, 687: `if calculator is None: _make_*(...)` | `resolve_pool_strategies()` always sets calculators on `PoolStrategies`. The `None` fallback in the pool class never fires in practice. The lazy construction was a safety net that's no longer needed. |

## Solution

### Step 1: Add `make_calculator()` method to `SwapStyle` enum

Move the `match` statement from `_make_dy_calculator()` onto `SwapStyle`
as an instance method. The enum owns its dispatch — it should own its factory.

### Step 2: Add `make_calculator()` methods to `MetapoolRateStyle` and `MetapoolUnderlyingStyle`

Same pattern — move the factory functions onto the enum types that own the dispatch.

### Step 3: Make `PoolStrategies.__post_init__` always construct calculators

Instead of relying on the builder to pass calculators, make `PoolStrategies`
construct them automatically from enum values. Remove `DyCalculator | None`
optional types — calculators are always present.

### Step 4: Remove pool class imports and lazy construction

The pool class no longer needs to import `_make_*` from `_pool_strategies.py`.
Remove the `if calculator is None` fallback branches — calculators are always
set.

### Step 5: Simplify `resolve_pool_strategies()`

The function no longer needs to construct calculators — `PoolStrategies.__post_init__`
handles that. It just looks up the mapping and returns a `PoolStrategies` with
the right enum values.

### Design decisions

- **Enum as factory**: The `make_calculator()` method on each enum is a Python
  pattern that co-locates dispatch with factory. The match statement moves from
  a free function to the enum that owns the variants. This is more navigable
  (jump-to-definition on the enum) and more testable (test the enum method
  directly).

- **Always-calculators on PoolStrategies**: Making calculators non-optional
  removes the `None` branch from every call site. The `DyCalculator | None`
  type was a temporary artifact of the migration from address-dispatch to
  strategy-dispatch. Now that all code paths set calculators, the optional
  type is unnecessary.

- **Keep `_pool_strategies.py`**: The address→strategy mapping stays in the
  Python file for now. Moving it to a database is a separate concern with
  different tradeoffs. The mapping is stable (it documents mainnet pools)
  and the file is 451 lines — not large enough to warrant a DB migration.

- **Keep `_variant_groups.py`**: Same reasoning — the variant group mappings
  are stable and small (181 lines). No DB migration needed.

## Files Involved

**Primary:**
- `src/degenbot/curve/types.py` — add `make_calculator()` to `SwapStyle`, `MetapoolRateStyle`, `MetapoolUnderlyingStyle`; update `PoolStrategies` to always construct calculators
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — remove `_make_*` imports and `if calculator is None` branches
- `src/degenbot/curve/_pool_strategies.py` — remove factory functions, simplify `resolve_pool_strategies()`

**Secondary:**
- `src/degenbot/builders/curve_pool_builder.py` — no change (already passes strategies to pool)
- `tests/curve/test_pool_strategies.py` — update tests for simplified `resolve_pool_strategies()`
- `tests/curve/test_variant_groups.py` — no change

**No change needed:**
- `src/degenbot/curve/calculators/` — calculators are unchanged
- `src/degenbot/curve/data_provider_impl.py` — no change
- `src/degenbot/curve/detection/` — no change

## Implementation Order

### Slice 1: Add `make_calculator()` to enums

1. Add `make_calculator()` instance method to `SwapStyle` enum in `types.py`
2. Add `make_calculator()` instance method to `MetapoolRateStyle` enum
3. Add `make_calculator()` instance method to `MetapoolUnderlyingStyle` enum
4. Run: `uv run pytest tests/curve/ -q --no-cov` — all tests green (no callers yet)

### Slice 2: Simplify `PoolStrategies` — always compute calculators

1. Change `dy_calculator` field from `DyCalculator | None` to `DyCalculator`
   with a `__post_init__` that constructs from `swap_style`
2. Change `metapool_dy_calculator` from optional to always-set (using `swap_style` if metapool, else `StandardDyCalculator` as placeholder)
3. Change `metapool_underlying_dy_calculator` similarly
4. Run: `uv run pytest tests/curve/ -q --no-cov` — verify struct-level tests

### Slice 3: Remove pool class factory imports and lazy construction

1. Remove the `from degenbot.curve._pool_strategies import (_make_dy_calculator, ...)` import from pool class
2. Remove `if calculator is None` branches — calculators are always set
3. Update `resolve_pool_strategies()` to not construct calculators
4. Delete `_make_dy_calculator`, `_make_metapool_dy_calculator`, `_make_metapool_underlying_dy_calculator` from `_pool_strategies.py`
5. Run: `uv run pytest tests/ -x -q --no-cov -k "not test_create_camelot_v2_pool"`

### Slice 4: Validate and clean up

1. Run `just lint`
2. Run full test suite
3. Update `src/degenbot/curve/CONTEXT.md` if needed
4. Git commit

## Testing

### Per-slice test runs

Slices 1-2 add behavior without changing callers. Slice 3 removes dead code.
Each slice runs the test suite.

### New/updated tests

- Update `test_pool_strategies.py` to verify `PoolStrategies()` always has
  calculators set (no `None` values)
- Verify existing pool tests pass with always-calculators

## Benefits

- **Locality**: Pool class no longer imports from `_pool_strategies.py`. It
  only depends on `PoolStrategies` (which it already received).
- **Depth**: The enum types become deeper — `SwapStyle.STANDARD.make_calculator()`
  replaces the free function `_make_dy_calculator(SwapStyle.STANDARD)`.
- **Simpler type**: `PoolStrategies.dy_calculator` goes from optional to required.
  Callers never need to handle `None`.
- **Dead code removal**: The `if calculator is None` branches in the pool class
  are deleted.

## Risks

- **Enum import overhead**: Adding `make_calculator()` to `SwapStyle` requires
  importing all calculator classes in `types.py`. This creates a circular import
  risk if any calculator imports from `types.py`. Mitigation: calculators
  already import from `types.py` (for `DyCalculationInputs`). Check for cycles.
  If circular, use `if TYPE_CHECKING` for calculator type hints and construct
  calculators inside the method body where imports are evaluated lazily.
- **`__post_init__` on frozen dataclass**: Frozen dataclasses can't set fields
  in `__post_init__`. Mitigation: use `object.__setattr__()` or restructure
  as a classmethod factory. Alternatively, keep calculators as optional and
  add a `get_dy_calculator()` property that lazily constructs on access.

## Relationship to Other Plans

- **Plan 026** (Curve Strategy Objects): Completed. Established `PoolStrategies`
  and the `swap_style` enum. This plan deepens the enum by adding factory methods.
- **Plan 039** (DyCalculator Seam): Completed. Created the calculator protocol.
  This plan makes the calculator construction local to the enum.
- **Plan 053–055, 057**: Completed. Orthogonal refactors in Curve and arbitrage
  modules.

## Status

[x] Slice 1: Add `make_calculator()` to enums
[x] Slice 2: Simplify `PoolStrategies` — always compute calculators
[x] Slice 3: Remove pool class factory imports and lazy construction
[x] Slice 4: Validate and clean up
