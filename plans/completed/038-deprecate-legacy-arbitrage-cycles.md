# Plan 038: Deprecate Legacy Arbitrage Cycle Classes

**Supersedes**: Plan 034 (Delete Legacy Arbitrage Cycle Classes)

## Overview

Move the four legacy arbitrage cycle classes into a `degenbot/arbitrage/_legacy/` sub-package, rename them with a leading underscore, emit `DeprecationWarning` on import and construction, and provide a migration guide. This preserves backward compatibility for external consumers while making the deprecation unmistakable.

Also remove `AbstractArbitrage` and `get_arbitrage_helpers()` — these are dead code that doesn't need a deprecation path (see rationale below).

## Files Involved

**Primary (move to `_legacy/`):**
- `src/degenbot/arbitrage/uniswap_lp_cycle.py` → `src/degenbot/arbitrage/_legacy/_uniswap_lp_cycle.py`
- `src/degenbot/arbitrage/uniswap_curve_cycle.py` → `src/degenbot/arbitrage/_legacy/_uniswap_curve_cycle.py`
- `src/degenbot/arbitrage/uniswap_multipool_cycle_testing.py` → `src/degenbot/arbitrage/_legacy/_uniswap_multipool_cycle_testing.py`
- `src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py` → `src/degenbot/arbitrage/_legacy/_uniswap_2pool_cycle_testing.py`

**Primary (new files):**
- `src/degenbot/arbitrage/_legacy/__init__.py` — re-exports with `DeprecationWarning`
- `docs/migration-guides/legacy-cycles-to-arbitrage-path.md` — migration guide

**Primary (delete — no deprecation path needed):**
- `src/degenbot/types/abstract/arbitrage.py` — `AbstractArbitrage` (8 lines)

**Primary (update — remove `get_arbitrage_helpers()` and `AbstractArbitrage` imports):**
- `src/degenbot/uniswap/v2_liquidity_pool.py`
- `src/degenbot/uniswap/v3_liquidity_pool.py`
- `src/degenbot/uniswap/v4_liquidity_pool.py`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py`
- `src/degenbot/types/abstract/__init__.py`

**Secondary (update re-exports):**
- `src/degenbot/arbitrage/__init__.py` — redirect imports through `_legacy/`
- `src/degenbot/__init__.py` — redirect imports through `_legacy/`

**Secondary (update tests):**
- `tests/arbitrage/integration/test_uniswap_lp_cycle.py` — update import paths
- `tests/arbitrage/integration/test_uniswap_curve_cycle.py` — update import paths
- `tests/arbitrage/integration/test_uniswap_2pool_cycle.py` — update import paths
- `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py` — update import paths
- `tests/arbitrage/integration/test_curve_equivalence.py` — update import paths
- `tests/arbitrage/integration/test_arbitrage_path_equivalence.py` — update import paths
- `tests/arbitrage/integration/test_v3_fork_equivalence.py` — update import paths
- `tests/arbitrage/test_offline_integration.py` — update import paths
- `tests/arbitrage/mock_pools.py` — update import paths
- `tests/arbitrage/test_optimizers/test_cvxpy_optimizer.py` — update import paths
- `tests/arbitrage/test_optimizers/test_cvxpy_multipool.py` — update import paths
- `tests/arbitrage/test_optimizers/test_solver_hop_builders.py` — update import paths

## Problem

The four legacy cycle classes (~6,070 lines) are deprecated but still publicly exported from `degenbot` and `degenbot.arbitrage`. External consumers may be using them. A hard delete (Plan 034's approach) would break those consumers without a transition path.

At the same time, `AbstractArbitrage` and `get_arbitrage_helpers()` are **dead code** — `ArbitragePath` doesn't inherit `AbstractArbitrage`, so the isinstance filter returns empty. The only caller of `get_arbitrage_helpers()` outside pool classes is a legacy test file. These don't need a deprecation path because they never worked with the new architecture.

## Solution

### Step 1: Create `_legacy/` sub-package and move files

Create `src/degenbot/arbitrage/_legacy/` with:

```
_legacy/
  __init__.py
  _uniswap_lp_cycle.py        (renamed from uniswap_lp_cycle.py)
  _uniswap_curve_cycle.py     (renamed from uniswap_curve_cycle.py)
  _uniswap_multipool_cycle_testing.py  (renamed from uniswap_multipool_cycle_testing.py)
  _uniswap_2pool_cycle_testing.py     (renamed from uniswap_2pool_cycle_testing.py)
```

The underscore prefix on the module filenames and class names sends a clear signal: these are not part of the public API.

**Internal imports within moved files**: Update relative imports. For example, `_uniswap_multipool_cycle_testing.py` currently imports `from degenbot.arbitrage.uniswap_lp_cycle import UniswapLpCycle` → change to `from degenbot.arbitrage._legacy._uniswap_lp_cycle import _UniswapLpCycle`.

**Class renaming**: Rename the public classes:
- `UniswapLpCycle` → `_UniswapLpCycle`
- `UniswapCurveCycle` → `_UniswapCurveCycle`
- `_UniswapMultiPoolCycleTesting` → stays `_UniswapMultiPoolCycleTesting` (already underscore-prefixed)
- `_UniswapTwoPoolCycleTesting` → stays `_UniswapTwoPoolCycleTesting` (already underscore-prefixed)

### Step 2: `_legacy/__init__.py` — re-export with deprecation warnings

```python
"""Legacy arbitrage cycle classes.

These classes are deprecated and will be removed in a future release.
Refer to the migration guide at docs/migration-guides/legacy-cycles-to-arbitrage-path.md
for transitioning to ArbitragePath + ArbSolver.
"""

import warnings

from degenbot.arbitrage._legacy._uniswap_lp_cycle import _UniswapLpCycle
from degenbot.arbitrage._legacy._uniswap_curve_cycle import _UniswapCurveCycle
from degenbot.arbitrage._legacy._uniswap_multipool_cycle_testing import _UniswapMultiPoolCycleTesting
from degenbot.arbitrage._legacy._uniswap_2pool_cycle_testing import _UniswapTwoPoolCycleTesting

# Backward-compatible aliases (without underscore) for gradual migration.
# These emit DeprecationWarning on first access.
# NOTE: Using module-level __getattr__ for lazy deprecation warnings.


_DEPRECATED_CLASS_NAMES = {
    "UniswapLpCycle": _UniswapLpCycle,
    "UniswapCurveCycle": _UniswapCurveCycle,
}


def __getattr__(name: str) -> object:
    if name in _DEPRECATED_CLASS_NAMES:
        warnings.warn(
            f"{name} is deprecated. Use ArbitragePath + ArbSolver instead. "
            "See docs/migration-guides/legacy-cycles-to-arbitrage-path.md",
            DeprecationWarning,
            stacklevel=2,
        )
        return _DEPRECATED_CLASS_NAMES[name]
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)


__all__ = [
    "_UniswapLpCycle",
    "_UniswapCurveCycle",
    "_UniswapMultiPoolCycleTesting",
    "_UniswapTwoPoolCycleTesting",
]
```

Using `__getattr__` at the module level means `from degenbot.arbitrage._legacy import UniswapLpCycle` triggers the warning, but `from degenbot.arbitrage._legacy import _UniswapLpCycle` (underscore name) does not — because the underscore name is a real attribute, not a lazy redirect.

### Step 3: Update `arbitrage/__init__.py` and `__init__.py` re-exports

**`src/degenbot/arbitrage/__init__.py`**:

```python
# Before:
from .uniswap_curve_cycle import UniswapCurveCycle
from .uniswap_lp_cycle import UniswapLpCycle

# After: redirect through _legacy with deprecation warning via __getattr__
# (Remove direct imports; add __getattr__ instead)

_DEPRECATED_NAMES = {
    "UniswapLpCycle": "degenbot.arbitrage._legacy._uniswap_lp_cycle:_UniswapLpCycle",
    "UniswapCurveCycle": "degenbot.arbitrage._legacy._uniswap_curve_cycle:_UniswapCurveCycle",
}

def __getattr__(name):
    if name in _DEPRECATED_NAMES:
        import warnings
        warnings.warn(
            f"{name} is deprecated. Use ArbitragePath + ArbSolver instead. "
            "See docs/migration-guides/legacy-cycles-to-arbitrage-path.md",
            DeprecationWarning,
            stacklevel=2,
        )
        module_path, attr = _DEPRECATED_NAMES[name].rsplit(":", 1)
        import importlib
        return getattr(importlib.import_module(module_path), attr)
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
```

**`src/degenbot/__init__.py`**:

Same pattern — remove the direct imports, add `__getattr__` for lazy deprecation warnings.

This ensures existing code like `from degenbot import UniswapLpCycle` still works but emits `DeprecationWarning`.

### Step 4: Delete `AbstractArbitrage` and `get_arbitrage_helpers()`

These are dead code and don't warrant a deprecation path:

- **`AbstractArbitrage`** — `ArbitragePath` doesn't inherit it, so the isinstance filter in `get_arbitrage_helpers()` returns empty for all new-style paths. The only callers are legacy test files (which we're keeping in `_legacy/`). **Delete immediately.**
- **`get_arbitrage_helpers()`** — only called by `tests/arbitrage/integration/test_uniswap_lp_cycle.py` (legacy test). After moving legacy files to `_legacy/`, this test needs an update anyway. **Delete immediately from pool classes.**

Changes:
1. Delete `src/degenbot/types/abstract/arbitrage.py`
2. Remove `AbstractArbitrage` import and `get_arbitrage_helpers()` method from V2, V3, V4, Curve pool classes
3. Remove `AbstractArbitrage` from `src/degenbot/types/abstract/__init__.py`

The legacy `_UniswapLpCycle` and `_UniswapCurveCycle` classes currently inherit from `AbstractArbitrage`. After deleting it, they'll inherit from `PublisherMixin` only (the only other base), which is already a correct base. Update the class definitions.

### Step 5: Update test files

For legacy test files that reference the old class names, update imports:

```python
# Before:
from degenbot.arbitrage import UniswapLpCycle

# After:
from degenbot.arbitrage._legacy import _UniswapLpCycle
```

Or continue using the deprecated name (which still works with a warning) during the transition.

### Step 6: Write migration guide

Create `docs/migration-guides/legacy-cycles-to-arbitrage-path.md`:

The guide maps the legacy API surface to the new architecture:

| Legacy (deprecated) | Replacement |
|---|---|
| `UniswapLpCycle(pools, input_token)` | `ArbitragePath(pools, input_token, solver=ArbSolver())` |
| `cycle.calculate()` | `path.calculate()` → `SolveResult` |
| `cycle.calculate(state_overrides=...)` | `path.calculate_with_state_override(...)` |
| `cycle.calculate_with_pool(...)` | `path.calculate_with_pool(...)` |
| `cycle.generate_payloads(...)` | `path.build_swap_amounts(result)` then `generate_payloads(swap_amounts, ...)` |
| `cycle.id` | `path.id` |
| `ArbitrageCalculationResult` from calculate | `SolveResult` from `calculate()` then `ArbitrageCalculationResult` from `build_swap_amounts()` |
| `cvxpy` optimizer | `ArbSolver` (Rust-accelerated mobius solver) |
| `_UniswapMultiPoolCycleTesting` | `ArbitragePath` + `ArbSolver` (mobius solver handles multi-pool) |
| `_UniswapTwoPoolCycleTesting` | `ArbitragePath` + `ArbSolver` (2-pool fast path in mobius solver) |
| `UniswapCurveCycle` | `ArbitragePath` + `ArbSolver` (CurveStableswapHop support) |

The guide should include concrete code examples showing before/after usage.

### Step 7: Remove cvxpy from core dependencies

After moving to `_legacy/`, `cvxpy` is only imported by `_legacy/` code. Move `cvxpy` from core dependencies to an optional dependency group:

```toml
# pyproject.toml
[project.optional-dependencies]
legacy-cycles = ["cvxpy"]
```

This way, users who haven't migrated yet can `pip install degenbot[legacy-cycles]` to keep the old cycle classes working. Users who have migrated see a smaller install.

`scipy` stays as a core dependency — `brent_solver.py` uses it.

## Implementation Order

1. **Step 1**: Create `_legacy/` sub-package, move + rename files (one commit)
2. **Step 2**: Add `_legacy/__init__.py` with deprecation warnings (same commit)
3. **Step 4**: Delete `AbstractArbitrage` + `get_arbitrage_helpers()`, update `_legacy/` class bases (one commit)
4. **Step 3**: Update `arbitrage/__init__.py` and `__init__.py` re-exports with `__getattr__` (one commit)
5. **Step 5**: Update test import paths (one commit)
6. **Step 6**: Write migration guide (one commit)
7. **Step 7**: Move `cvxpy` to optional dependency (one commit)

Run `just test-python` after each commit.

## Testing

### Backward compatibility

After Steps 1–3, verify that existing import paths still work:

```python
# These should still work, emitting DeprecationWarning:
from degenbot import UniswapLpCycle, UniswapCurveCycle
from degenbot.arbitrage import UniswapLpCycle

# These should work without warning:
from degenbot.arbitrage._legacy import _UniswapLpCycle
```

### Deprecation warnings

Add a test that verifies `DeprecationWarning` is emitted:

```python
def test_legacy_import_emits_deprecation_warning():
    with pytest.warns(DeprecationWarning, match="UniswapLpCycle is deprecated"):
        from degenbot.arbitrage._legacy import UniswapLpCycle
```

### Existing tests

All legacy integration tests should pass with updated import paths. The functionality hasn't changed — only the file locations and class names.

### No reference to old paths

After Step 5, verify no file imports from the old module paths:

```bash
grep -r "from degenbot.arbitrage.uniswap_lp_cycle\|from degenbot.arbitrage.uniswap_curve_cycle\|from degenbot.arbitrage.uniswap_multipool_cycle_testing\|from degenbot.arbitrage.uniswap_2pool_cycle_testing" src/ tests/
```

Should return zero matches (all moved to `_legacy._uniswap_*` imports).

## Benefits

- **Backward compatibility preserved**: `from degenbot import UniswapLpCycle` still works, with a clear deprecation signal.
- **Clear API boundary**: `_legacy/` directory + underscore class names make it unmistakable that these classes are not part of the public API. Anyone browsing the source tree immediately sees the separation.
- **Migration path**: The migration guide gives concrete before/after examples. `cvxpy` in an optional dependency group means minimal-install users aren't affected.
- **Dead code removed**: `AbstractArbitrage` and `get_arbitrage_helpers()` are deleted immediately — they served no one and don't need a deprecation path.
- **Prerequisite for Plan 033**: After this plan, the legacy cycles no longer consume `solver_hop_builders.py` from `src/` (they're in `_legacy/`).

## Risks

- **Deprecation fatigue**: The classes still exist and work. Users may ignore the warnings indefinitely. Setting a concrete removal version in the deprecation message (e.g., "will be removed in 0.9.0") helps. The final deletion can be tracked as a separate plan.
- **Test maintenance burden**: Legacy tests remain in the test suite, exercising deprecated code. They should be tagged with `@pytest.mark.filterwarnings("ignore::DeprecationWarning")` to avoid noise, and eventually deleted when the legacy classes are removed.
- **`cvxpy` optional dependency**: Users who `pip install degenbot` without `[legacy-cycles]` will get `ImportError` when the legacy code tries to import `cvxpy`. This is the intended behavior — it forces migration. But the error message could be confusing. The `_legacy/` code should catch the `ImportError` and re-raise with a clear message: `"cvxpy is required for legacy cycle classes. Install with: pip install degenbot[legacy-cycles]"`.
- **Two-step deprecation**: The deprecation is split across `arbitrage/__init__.py` and `_legacy/__init__.py`. Both use `__getattr__` for lazy warnings. This is a minor complexity cost, but ensures warnings fire regardless of which import path the consumer uses.

## Relationship to Other Plans

- **Plan 034** (Delete Legacy Arbitrage Cycles): **Superseded by this plan.** Plan 034 is a hard delete; this plan is a gradual deprecation with the same end state. Mark 034 as REJECTED.
- **Plan 033** (Consolidate Dual Pool-to-Hop Conversion): This plan is still a **prerequisite** for 033. After moving legacy cycles to `_legacy/`, they no longer import from `solver_hop_builders.py` in `src/` — the only remaining consumers are inside `_legacy/` itself. Plan 033 can proceed to delete `solver_hop_builders.py` from `src/`.
- **Plan 011** (Unify UniswapLpCycle._calculate() Behind ArbSolver Seam): Complete. Created `ArbitragePath` as the replacement. This plan starts the deprecation of the replaced code.
- **Plan 021** (Extract SwapEncoder from UniswapLpCycle): Complete. Created `encoding.py` with `generate_payloads()`. The legacy classes still have inline encoding — that's acceptable for deprecated code.

## Future Work

- **Final deletion**: After a suitable deprecation period (at least one release with warnings), create a plan to delete `_legacy/` entirely: remove the directory, remove `cvxpy` from optional dependencies, delete legacy test files, and remove `__getattr__` redirect from `__init__.py` files.
