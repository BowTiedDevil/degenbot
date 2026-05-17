# Plan 022: Remove Backward Compatibility Shims and Aliases

**Status: READY**

## Overview

Remove backward-compat aliases and shims that no longer serve external callers. These accumulate after migrations (type relocations, I/O-free architecture, solver decomposition) and add indirection without current value. Each step is independently shippable with its own red-green cycle.

## Audit Summary

| # | Shim | Location | Risk | Step |
|---|------|----------|------|------|
| 1 | ~~`_ensure_bytes = to_bytes`~~ | `abi_adapter.py:41` | ✅ Done | — |
| 2 | `*_legacy` functions | `v4_libraries/bit_math.py`, `v3_libraries/tick_bitmap.py` | Low | ~~1~~ ✅ |
| 3 | `hop_factory` + `Hop = hop_factory` | `types/hop_types.py` | Low | ~~2~~ ✅ |
| 4 | `pool_hop_adapter.py` | `arbitrage/path/` | Low | ~~3~~ ✅ |
| 5 | `_v3_virtual_reserves` re-export | `arbitrage_path.py` | Very low | ~~4~~ ✅ |
| 6 | Hop type re-exports (`F401`) | `solver.py`, `optimizers/__init__.py` | ~~Low–medium~~ ✅ | ~~5~~ ✅ |
| 7 | `register_web3()` legacy methods | `connection_manager.py`, `async_connection_manager.py` | ~~Medium~~ ✅ | ~~6~~ ✅ |

### Not in scope

| Item | Reason |
|------|--------|
| `DatabaseSessionManager.__getattr__` | Active design pattern for test indirection; not a shim |
| `UniswapLpCycle` deprecation | Functional class; removal tracked by completed Plan 021 |
| Aave enrichment legacy/new dispatch | Feature-flagged migration; legacy impl needed until new is proven |
| V3 libraries Python/Rust fallback | CI depends on Python fallback; needs CI fix first |

---

## Step 1: Move `*_legacy` reference functions to tests

### Files modified

**Production (deletions only):**
- `src/degenbot/uniswap/v4_libraries/bit_math.py` — delete `least_significant_bit_legacy`, `most_significant_bit_legacy`
- `src/degenbot/uniswap/v3_libraries/tick_bitmap.py` — delete `next_initialized_tick_within_one_word_legacy`

**Tests:**
- `tests/uniswap/v4/libraries/test_bit_math.py` — update 2 equivalence tests
- `tests/uniswap/v3/libraries/test_tick_bitmap.py` — relocate function body, update imports and ~20 call sites

### Why

The three `*_legacy` functions are loop-based reference implementations kept alongside optimized versions. No production code calls them. The v4 bit_math tests define their own inline reference functions already; the v3 tick_bitmap tests use the legacy function as the expected-value oracle in dual-comparison assertions.

### Changes

#### `src/degenbot/uniswap/v4_libraries/bit_math.py`

Delete `least_significant_bit_legacy` (lines 17–34) and `most_significant_bit_legacy` (lines 45–63).

#### `tests/uniswap/v4/libraries/test_bit_math.py`

The file already has two inline reference implementations:

```python
def _most_significant_bit_reference(x: int):
    i = 0
    while (x := x >> 1) > 0:
        i += 1
    return i


def _least_significant_bit_reference(x: int):
    assert x > 0
    i = 0
    while (x >> i) & 1 == 0:
        i += 1
    return i
```

Change the two equivalence tests:
- `test_least_significant_bit_equivalence`: `bit_math.least_significant_bit_legacy(number)` → `_least_significant_bit_reference(number)`
- `test_most_significant_bit_equivalence`: `bit_math.most_significant_bit_legacy(number)` → `_most_significant_bit_reference(number)`

Remove `least_significant_bit_legacy` and `most_significant_bit_legacy` from any import statements.

#### `src/degenbot/uniswap/v3_libraries/tick_bitmap.py`

Delete `next_initialized_tick_within_one_word_legacy` (lines 52–120).

#### `tests/uniswap/v3/libraries/test_tick_bitmap.py`

This is the larger change. The test file uses the legacy function in two ways:

1. **`is_initialized()` helper** (line 19) — calls `next_initialized_tick_within_one_word_legacy` to check if a tick is set. Rewrite to use the new `next_initialized_tick_within_one_word`:
   ```python
   def is_initialized(tick_bitmap, tick_data, tick):
       next_tick, initialized = next_initialized_tick_within_one_word(
           tick_data=tick_data,
           tick_bitmap=tick_bitmap,
           tick=tick,
           tick_spacing=1,
           less_than_or_equal=True,
       )
       return next_tick == tick if initialized else False
   ```
   Update all ~10 call sites to pass `tick_data`.

2. **Dual-comparison assertions** (~20 occurrences) — each test asserts both legacy and new return the same value:
   ```python
   assert next_initialized_tick_within_one_word_legacy(...) == (84, True)
   assert next_initialized_tick_within_one_word(...) == (84, True)
   ```
   Replace with single assertion against the new function only:
   ```python
   assert next_initialized_tick_within_one_word(...) == (84, True)
   ```

3. Copy the legacy function body into the test file as `_next_initialized_tick_within_one_word_reference` if we want to preserve the equivalence guarantee. However, since the new function has been validated in CI for months, simply asserting against the new function is sufficient. **Recommend: just remove the legacy calls, keep single assertions against the new function.**

4. Remove `next_initialized_tick_within_one_word_legacy` from the import block.

### Test plan

- `tests/uniswap/v4/libraries/test_bit_math.py` — all existing tests pass
- `tests/uniswap/v3/libraries/test_tick_bitmap.py` — all existing tests pass
- `just lint` — no import errors for deleted functions

---

## Step 2: Remove `hop_factory` and `Hop = hop_factory`

### Files modified

**Production:**
- `src/degenbot/types/hop_types.py` — delete `hop_factory` function and `Hop = hop_factory` alias
- `src/degenbot/types/__init__.py` — remove `Hop` from import and `__all__`
- `src/degenbot/arbitrage/optimizers/solver.py` — remove `Hop` from re-import and `__all__`, update docstring example
- `src/degenbot/arbitrage/optimizers/__init__.py` — remove `Hop` from import and `__all__`

**Tests (~12 files, ~70 `Hop(...)` calls):**
- `tests/arbitrage/test_optimizers/conftest.py`
- `tests/arbitrage/test_optimizers/test_solver.py`
- `tests/arbitrage/test_optimizers/test_solver_hypothesis.py`
- `tests/arbitrage/test_optimizers/test_solver_integration.py`
- `tests/arbitrage/test_optimizers/test_solver_tagged_hops.py`
- `tests/arbitrage/test_optimizers/test_rust_int_refinement.py`
- `tests/arbitrage/test_optimizers/test_rust_merged_int_refinement.py`
- `tests/arbitrage/test_optimizers/test_rust_raw_array_marshalling.py`

### Why

`hop_factory` sniffs keyword args to guess `ConstantProductHop` vs `BoundedProductHop`. Every production caller already knows which type it needs — only test code uses `Hop(...)` as shorthand. The alias obscures the concrete type and adds a dynamic dispatch that doesn't belong in frozen-dataclass territory.

### Changes

#### `src/degenbot/types/hop_types.py`

1. Delete the `hop_factory` function (lines 255–299).
2. Delete `Hop = hop_factory` (line 303).
3. Remove `hop_factory` from any module-level `__all__` if present.

#### Import updates

Replace all `Hop(reserve_in=..., reserve_out=..., fee=...)` with `ConstantProductHop(...)`.

Replace all `Hop(reserve_in=..., reserve_out=..., fee=..., liquidity=..., sqrt_price=..., tick_lower=..., tick_upper=...)` with `BoundedProductHop(...)`.

In each test file, update the import:
```python
# Before
from degenbot.arbitrage.optimizers.solver import Hop

# After
from degenbot.types.hop_types import ConstantProductHop, BoundedProductHop
```

Or from whichever re-export path the file currently uses.

#### `src/degenbot/arbitrage/optimizers/solver.py`

Update the docstring example:
```python
# Before
>>> from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput
...     Hop(reserve_in=2_000_000e6, reserve_out=1_000e18, fee=Fraction(3, 1000)),
# After
>>> from degenbot.arbitrage.optimizers.solver import ArbSolver, SolveInput
>>> from degenbot.types.hop_types import ConstantProductHop
...     ConstantProductHop(reserve_in=2_000_000e6, reserve_out=1_000e18, fee=Fraction(3, 1000)),
```

#### `tests/arbitrage/test_optimizers/test_solver_tagged_hops.py`

This file has a backward-compat test (`test_backward_compat_old_hop`). Update or remove it per the new design — there is no `Hop` alias anymore, so the test verifies something that no longer exists. The test can be deleted.

### Test plan

- `tests/arbitrage/test_optimizers/` — all tests pass with `ConstantProductHop` / `BoundedProductHop`
- `just lint` — no references to `hop_factory` or `Hop` alias

---

## Step 3: Inline `pool_hop_adapter.py` into `arbitrage_path.py`

### Files modified

**Production:**
- `src/degenbot/arbitrage/path/pool_hop_adapter.py` — **delete**
- `src/degenbot/arbitrage/path/arbitrage_path.py` — inline the two delegation calls, remove adapter imports

**Tests:**
- `tests/arbitrage/test_path/test_pool_hop_adapter.py` — **delete** (tests protocol delegation already covered by `test_arbitrage_path.py` integration tests, or relocate key cases there)

### Why

`pool_hop_adapter.py` is a 2-function, 30-line module that just calls `pool.extract_fee(...)` and `pool.to_hop_state(...)`. `arbitrage_path.py` imports these as private names (`_adapter_extract_fee`, `_adapter_to_hop_state`) then wraps them again in local functions (`_extract_fee`, `_pool_to_hop_state`). Two layers of indirection for `pool.method()`.

### Changes

#### `src/degenbot/arbitrage/path/arbitrage_path.py`

1. Remove imports:
   ```python
   from degenbot.arbitrage.path.pool_hop_adapter import extract_fee as _adapter_extract_fee
   from degenbot.arbitrage.path.pool_hop_adapter import to_hop_state as _adapter_to_hop_state
   ```

2. Inline `_check_pool_compatibility`:
   ```python
   def _check_pool_compatibility(pool: ArbitragePathPool) -> PoolCompatibility:
       try:
           pool.to_hop_state(zero_for_one=True)
       except (IncompatiblePoolInvariant, AttributeError):
           return PoolCompatibility.INCOMPATIBLE_INVARIANT
       else:
           return PoolCompatibility.COMPATIBLE
   ```

3. Inline `_extract_fee`:
   ```python
   def _extract_fee(pool: ArbitragePathPool, zero_for_one: bool) -> Fraction:
       return pool.extract_fee(zero_for_one=zero_for_one)
   ```

4. Inline `_pool_to_hop_state`:
   ```python
   def _pool_to_hop_state(
       pool: ArbitragePathPool,
       zero_for_one: bool,
       state_override: AbstractPoolState | None = None,
   ) -> HopType:
       return pool.to_hop_state(zero_for_one=zero_for_one, state_override=state_override)
   ```

5. Delete `src/degenbot/arbitrage/path/pool_hop_adapter.py`.

#### `tests/arbitrage/test_path/test_pool_hop_adapter.py`

The test file tests `extract_fee` and `to_hop_state` as standalone functions, which after inlining just delegate to `pool.extract_fee()` and `pool.to_hop_state()`. These protocol methods are tested by:
- `tests/arbitrage/test_path/test_arbitrage_path.py` — integration tests exercise the full path including hop state extraction
- Individual pool test files — test each pool's `to_hop_state` / `extract_fee` directly

**Recommend: delete the adapter test file.** The tests verify delegation, not pool behavior. If any test cases cover edge cases not tested elsewhere (e.g., `_NoAttrsPool` raising `AttributeError`), relocate those to `test_arbitrage_path.py`.

### Test plan

- `tests/arbitrage/test_path/test_arbitrage_path.py` — all pass
- `just lint` — no dangling imports to deleted module

---

## Step 4: Remove `_v3_virtual_reserves` re-export from `arbitrage_path.py`

### Files modified

**Production:**
- `src/degenbot/arbitrage/path/arbitrage_path.py` — remove the re-export import

**Tests:**
- `tests/arbitrage/test_path/test_arbitrage_path.py` — update import to source module

### Why

The import exists solely so the test file can import `_v3_virtual_reserves` from `arbitrage_path` instead of from its actual home. This puts a test convenience into production code.

### Changes

#### `src/degenbot/arbitrage/path/arbitrage_path.py`

Remove:
```python
from degenbot.uniswap.v3_libraries.functions import (
    v3_virtual_reserves as _v3_virtual_reserves,  # noqa: F401 re-export for tests
)
```

#### `tests/arbitrage/test_path/test_arbitrage_path.py`

Replace:
```python
from degenbot.arbitrage.path.arbitrage_path import _v3_virtual_reserves,
```
With:
```python
from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves as _v3_virtual_reserves
```

### Test plan

- `tests/arbitrage/test_path/test_arbitrage_path.py` — all pass
- `just lint` — no import of `_v3_virtual_reserves` in production code

---

## Step 5: Remove hop type re-exports from `solver.py` and `optimizers/__init__.py`

### Files modified

**Production:**
- `src/degenbot/arbitrage/optimizers/solver.py` — remove re-exported hop types
- `src/degenbot/arbitrage/optimizers/__init__.py` — remove re-exported hop types

**Tests (~10 files):**
- `tests/arbitrage/verify_legacy_equivalence.py`
- `tests/arbitrage/test_path/test_swap_amounts.py`
- `tests/arbitrage/test_path/test_event_driven.py`
- `tests/arbitrage/test_path/test_arbitrage_path.py`
- `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py`
- `tests/arbitrage/integration/test_curve_equivalence.py`
- `tests/arbitrage/integration/test_v3_only_equivalence.py`
- `tests/arbitrage/integration/test_v3_fork_equivalence.py`
- `tests/arbitrage/integration/test_curve_legacy_equivalence.py`
- `tests/arbitrage/integration/test_calculate_with_pool.py`
- `tests/arbitrage/test_optimizers/test_solver_integration.py`
- `tests/arbitrage/test_optimizers/test_solver_hypothesis.py`
- `tests/arbitrage/test_optimizers/test_pool_cache_adapter.py`
- `tests/arbitrage/test_optimizers/test_solver_hop_builders.py`

### Why

Hop types (`ConstantProductHop`, `BoundedProductHop`, `HopType`, `PoolInvariant`, etc.) were moved from `degenbot.arbitrage.optimizers.hop_types` to `degenbot.types.hop_types` to break a circular dependency. They're still re-exported at the old paths so existing imports keep working. The actual types now live in `degenbot.types.hop_types` and are also available via `degenbot.types` and `degenbot.arbitrage.optimizers.hop_types` (which imports from `types.hop_types` and adds `SolveInput`/`SolveResult`/`Solver`/`SolverMethod`).

### Changes

#### `src/degenbot/arbitrage/optimizers/solver.py`

Remove the hop type re-export block:
```python
from degenbot.types.hop_types import (  # noqa: F401 — re-exported for backward compatibility
    BalancerMultiTokenHop,
    BoundedProductHop,
    ConstantProductHop,
    Hop,
    HopType,
    PoolInvariant,
    SolidlyStableHop,
    V3TickRangeInfo,
)
```

Remove corresponding entries from `__all__`.

Also remove the private helper re-exports if they are no longer needed by any test:
```python
from degenbot.arbitrage.optimizers._solver_utils import (  # noqa: F401 ...)
from degenbot.arbitrage.optimizers._v3_utils import (  # noqa: F401 ...)
```

Check each `_solver_utils` / `_v3_utils` name against test imports before removing.

#### `src/degenbot/arbitrage/optimizers/__init__.py`

Remove hop type re-exports:
```python
from degenbot.types.hop_types import (
    BalancerMultiTokenHop,
    BoundedProductHop,
    ConstantProductHop,
    Hop,
    HopType,
    PoolInvariant,
    SolidlyStableHop,
    V3TickRangeInfo,
)
```

Remove corresponding entries from `__all__`.

#### Test files

Update imports from `degenbot.arbitrage.optimizers.solver` or `degenbot.arbitrage.optimizers` to `degenbot.types.hop_types` or `degenbot.arbitrage.optimizers.hop_types` (which still exports `SolveInput`/`SolveResult`/`SolverMethod` plus the hop types via its own re-import).

### Test plan

- `tests/arbitrage/` — all pass
- `just lint` — no import errors for removed re-exports

---

## Step 6: Remove `register_web3()` legacy methods

### Files modified

**Production:**
- `src/degenbot/connection/connection_manager.py` — delete `register_web3()`
- `src/degenbot/connection/async_connection_manager.py` — delete `register_web3()`

**Consumers:**
- Grep codebase for `register_web3(` calls and update to `register_provider(ProviderAdapter.from_web3(w3))`

### Why

`register_web3()` is explicitly documented as a legacy method that wraps `register_provider()`. It exists for callers who haven't migrated from `cm.register_web3(w3)` to `cm.register_provider(ProviderAdapter.from_web3(w3))`.

### Risk

**Medium.** External consumer codebases may use `register_web3()`. Before removing, grep all known consumers. If any exist, this step should be deferred or gated behind a deprecation cycle.

### Changes

#### `src/degenbot/connection/connection_manager.py`

1. Add `DeprecationWarning` in a prior commit (if external callers exist).
2. In this step, delete `register_web3()`.

#### `src/degenbot/connection/async_connection_manager.py`

Same pattern for `async def register_web3()`.

### Test plan

- `just test-python` — all pass
- `just lint` — no references to deleted method

---

## Implementation Order

| Step | Dependencies | Scope | Test files touched |
|------|-------------|-------|--------------------|
| 1 | None | Delete production code, update 2 test files | 2 |
| 4 | None | Delete 1 import, update 1 test import | 1 |
| 3 | None | Delete 1 file, inline 3 calls, delete 1 test file | 2 |
| 2 | None | Delete factory/alias, ~70 mechanical replacements in ~8 test files | 8 |
| 5 | Step 2 (removes `Hop` re-export which Step 2 also removes) | Remove re-exports from 2 production files, update ~14 test imports | 14 |
| 6 | External consumer audit | Delete 2 methods | 0 (if no internal callers) |

Steps 1 and 4 are the safest — minimal surface area, no test behavior change. Step 3 is also low-risk. Step 2 is mechanical but high-volume. Step 5 depends on Step 2 being done first (both touch `Hop` re-exports). Step 6 requires an external audit.

## Completion Criteria

- [x] Step 1: No `*_legacy` functions in production `v3_libraries/` or `v4_libraries/`
- [x] Step 2: No `hop_factory` or `Hop` alias; all test uses replaced with concrete types
- [x] Step 3: No `pool_hop_adapter.py`; `arbitrage_path.py` calls pool protocol methods directly
- [x] Step 4: No `_v3_virtual_reserves` import in `arbitrage_path.py`
- [x] Step 5: No hop type re-exports in `solver.py` or `optimizers/__init__.py`; tests import from canonical location; private helper and solver_hop_builder re-exports also removed
- [x] Step 6: No `register_web3()` in connection managers
- [ ] `just lint` and `just test-all` pass after each step
