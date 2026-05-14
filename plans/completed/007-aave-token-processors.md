# Plan 007: Collapse Aave Token Processor Revision Matrix

**Status: COMPLETE** (2026-05-09)

## Overview

Replace the 14 concrete token processor classes (collateral V1-V5, debt V1-V5, GHO
V1-V5+) with a single `TokenProcessor` implementation parameterized by a
`RoundingStrategy` frozen dataclass. The factory becomes a config table
(revision → strategy). GHO processors become the same implementation with
`supports_discount=True` and a `DiscountStrategy`.

## Files Involved

- **Deleted (12 files):**
  - `src/degenbot/aave/processors/collateral/` (entire directory)
  - `src/degenbot/aave/processors/debt/` (entire directory)
- **Rewrote:**
  - `src/degenbot/aave/processors/factory.py`
  - `src/degenbot/aave/processors/__init__.py`
- **Created:**
  - `src/degenbot/aave/processors/processor.py` (unified implementations)
  - `src/degenbot/aave/processors/strategies.py` (rounding configs)
- **Unchanged:**
  - `src/degenbot/aave/processors/base.py` (protocols + event dataclasses)

## Problem

The processor hierarchy is shallow. Classes are distinguished only by:

1. Rounding mode for mint vs burn (half-up, floor, ceil)
2. Whether discount math applies (GHO only)
3. `revision` classattribute

`CollateralV3Processor` is literally `V1` with a different `revision`. The
factory maintains three dicts with 20 total entries. Real code variation is
maybe 30 lines; the rest is boilerplate.

## Target State

```
processors/
├── __init__.py
├── base.py           # Protocols + event dataclasses (unchanged)
├── strategies.py     # RoundingStrategy, DiscountStrategy, revision tables
├── processor.py      # Unified TokenProcessor implementation
└── factory.py        # Revision → strategy lookup (3 config tables)
```

### `strategies.py`

```python
from enum import Enum
from dataclasses import dataclass

class RoundingMode(Enum):
    HALF_UP = "half_up"
    FLOOR = "floor"
    CEIL = "ceil"

@dataclass(frozen=True, slots=True)
class RoundingStrategy:
    """Determines ray_div/ray_mul rounding for each operation type."""
    mint_rounding: RoundingMode
    burn_rounding: RoundingMode
    transfer_rounding: RoundingMode
    balance_rounding: RoundingMode   # ray_mul direction

@dataclass(frozen=True, slots=True)
class DiscountStrategy:
    """GHO-specific discount behavior."""
    supports_discount: bool
    refresh_after_balance_change: bool

# Config tables — single source of truth for all revision behavior
COLLATERAL_STRATEGIES: dict[int, RoundingStrategy] = {
    1: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
        transfer_rounding=RoundingMode.HALF_UP,
        balance_rounding=RoundingMode.HALF_UP,
    ),
    3: RoundingStrategy(...same as 1...),
    4: RoundingStrategy(
        mint_rounding=RoundingMode.FLOOR,
        burn_rounding=RoundingMode.CEIL,
        transfer_rounding=RoundingMode.CEIL,
        balance_rounding=RoundingMode.FLOOR,
    ),
    5: RoundingStrategy(...same as 4...),
}

DEBT_STRATEGIES: dict[int, RoundingStrategy] = {
    1: RoundingStrategy(mint=HALF_UP, burn=HALF_UP, ...),
    3: ...same as 1...,
    4: RoundingStrategy(mint=CEIL, burn=FLOOR, ...),
    5: ...same as 4...,
}

GHO_DISCOUNT_STRATEGIES: dict[int, DiscountStrategy] = {
    1: DiscountStrategy(supports_discount=True, refresh_after_balance_change=True),
    2: DiscountStrategy(...same...),
    4: DiscountStrategy(supports_discount=False, refresh_after_balance_change=False),
    5: DiscountStrategy(...same...),
}
```

### `processor.py`

A single `TokenProcessor` class implementing all three protocols
(`CollateralTokenProcessor`, `DebtTokenProcessor`, `GhoDebtTokenProcessor`).
The `__init__` takes `rounding: RoundingStrategy` and optional
`discount: DiscountStrategy | None`.

Implementation delegates to `self._math_lib` methods selected by rounding mode:

```python
def _ray_div(self, a: int, b: int, mode: RoundingMode) -> int:
    match mode:
        case RoundingMode.HALF_UP:
            return wad_ray_math.ray_div(a, b)
        case RoundingMode.FLOOR:
            return wad_ray_math.ray_div_floor(a, b)
        case RoundingMode.CEIL:
            return wad_ray_math.ray_div_ceil(a, b)
```

`process_mint_event` / `process_burn_event` / `accrue_debt_on_action` become
single implementations parameterized by the strategy. No subclassing.

### `factory.py`

```python
def get_collateral_processor(revision: int) -> CollateralTokenProcessor:
    strategy = COLLATERAL_STRATEGIES[revision]
    return UnifiedProcessor(rounding=strategy)


# etc.
```

## Migration Steps

1. **Create `strategies.py`** with config tables and frozen dataclasses. ✓
2. **Create `processor.py`** with unified implementation. ✓
   - UnifiedCollateralProcessor for aToken operations
   - UnifiedDebtProcessor for vToken operations
   - UnifiedGhoProcessor for GHO with discount support
3. **Rewrite `factory.py`** to use config tables + unified processor. ✓
4. **Delete 12 processor files** (and 2 empty subdirectories). ✓
5. **Update `__init__.py`** with new exports. ✓
6. **Run tests.** All tests pass (199 Aave tests, 473 total). ✓

## Test Strategy

**Red phase:** Before touching code, write a parametric test proving every
revision's behavior is captured by the strategy table. This is the contract
we're preserving.

**Green phase:** The 14 existing processor unit tests become 1 parametric test
that iterates `(revision, event_type, input, expected)`. Key cases:

- Collateral V1 mint: half-up scaling
- Collateral V4/V5 burn: ceil scaling (was in V4/V5 overrides)
- Debt V4/V5 mint: ceil scaling
- Debt V4/V5 burn: floor scaling
- GHO V2: discount accrual with `get_discounted_balance`
- GHO V5: discount returns 0, uses floor/ceil correctly

**Regression:** `test_position_analysis.py` exercises processors through the
full pipeline — it should pass unchanged since factory interface is stable.

## Risks

| Risk | Mitigation |
|------|------------|
| Revision behavior subtly diverges in existing code (e.g., V1 vs V3 have a bug that's currently compensated elsewhere) | Before migration, capture all existing test outputs as golden files; diff after |
| Factory interface changes break callers | Keep `get_collateral_processor` / `get_debt_processor` / `get_gho_debt_processor` signatures identical |
| GHO discount calculations are more complex than rounding alone | Keep `accrue_debt_on_action` and `get_discounted_balance` as methods on unified processor, not inlined into strategy config |

## Implementation Notes

### Actual Strategy Design

The final `RoundingStrategy` uses only two fields (simpler than planned):
- `mint_rounding`: Rounding mode for mint operations
- `burn_rounding`: Rounding mode for burn operations

The `DiscountStrategy` uses:
- `supports_discount`: Whether discount mechanism is active
- `has_discounted_balance_method`: V2+ has getDiscountedBalance, V1 does not

### Processor Classes

Three separate unified classes were created instead of one:
- `UnifiedCollateralProcessor` - Implements CollateralTokenProcessor
- `UnifiedDebtProcessor` - Implements DebtTokenProcessor
- `UnifiedGhoProcessor` - Implements GhoDebtTokenProcessor

This preserves the protocol separation while eliminating the revision matrix.

## Rollback

If the unified processor fails a test, the strategy table makes rollback trivial:
revert `factory.py` to class-based dispatch, keep `strategies.py` as reference
for the correct behavior per revision.

## Definition of Done

- [x] 14 processor files deleted
- [x] `processors/` directory has ≤ 4 Python files (base, strategies, processor, factory)
- [x] All existing processor tests pass
- [x] Single parametric test covers all revision × operation combinations
- [x] Factory public API unchanged (drop-in replacement)
- [x] No references to deleted classes anywhere in codebase
