# Plan 029: Externalize Curve Variant Group Addresses from Pool Class to Configuration

Committed as `586579ff`.

## Overview

Move the 7 class-level frozenset variant groups (67 hardcoded addresses) out of `CurveStableswapPool` and into a configuration module. The builder resolves variant group membership at construction time and passes the result as strategy enums to the pool constructor. This plan is a prerequisite sub-task of Plan 026 — it handles the *data* extraction (addresses → configuration), while Plan 026 handles the *behavioural* extraction (address dispatch → strategy dispatch).

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — removed 7 frozenset class variables, removed `oracle_method` attribute
- `src/degenbot/curve/_variant_groups.py` (new) — module containing the address-to-variant mappings and 3 resolver functions
- `src/degenbot/builders/curve_pool_builder.py` — resolves variant memberships from the mapping

**Secondary:**
- `src/degenbot/curve/types.py` — added `DVariant` (6 values), `YVariant` (3 values), `YDVariant` (2 values) enums
- `tests/curve/test_variant_groups.py` (new) — 23 unit tests for variant resolution

## Problem

### Deletion test

If you delete the 7 frozenset class variables, complexity does NOT vanish — it reappears as "which D-variant formula should this pool use?" The variant groups are configuration data that happens to be hardcoded on the class. Moving the configuration out of the class doesn't change the dispatch logic but it does separate the data from the behaviour.

### Former state (before implementation)

The pool class defined these class-level frozensets:

| Variable | Addresses | Used in |
|----------|-----------|---------|
| `D_VARIANT_GROUP_0` | 6 | `_get_d()` — selects `calc_d_variant_alpha` |
| `D_VARIANT_GROUP_1` | 4 | `_get_d()` — selects `calc_d_variant_alpha` + `calc_dp_variant_alpha` |
| `D_VARIANT_GROUP_2` | 17 | `_get_d()` — selects `calc_dp_variant_beta` |
| `D_VARIANT_GROUP_3` | 3 | `_get_d()` — selects `calc_dp_variant_alpha` |
| `D_VARIANT_GROUP_4` | 10 | `_get_d()` — selects `calc_dp_variant_gamma` |
| `Y_VARIANT_GROUP_0` | 5 | `_get_y()` — amp without A_PRECISION divisor |
| `Y_VARIANT_GROUP_1` | 10 | `_get_y()` — different c formula |
| `Y_D_VARIANT_GROUP_0` | 2 | `_get_y_d()` — A_PRECISION in b/c formulas |

Total: 67 addresses across 7 frozensets, all defined as class variables on `CurveStableswapPool`. New pools that fall into a variant group required editing the class body.

## Solution

### Step 1: Create `_variant_groups.py` module ✅

Created `src/degenbot/curve/_variant_groups.py` (181 lines) containing:

- 7 `frozenset` address groups (same addresses as the old class variables)
- `resolve_d_variant(pool_address)` → `DVariant`
- `resolve_y_variant(pool_address)` → `YVariant`
- `resolve_yd_variant(pool_address)` → `YDVariant`

### Step 2: Define `DVariant`, `YVariant`, `YDVariant` enums ✅

Added to `src/degenbot/curve/types.py`:

```python
class DVariant(Enum):
    STANDARD = auto()
    VARIANT_ALPHA = auto()           # Group 0
    VARIANT_ALPHA_DP_ALPHA = auto()  # Group 1
    VARIANT_DP_ALPHA = auto()        # Group 3 (standard d + variant_alpha dp)
    VARIANT_BETA_DP = auto()         # Group 2
    VARIANT_GAMMA_DP = auto()        # Group 4

class YVariant(Enum):
    STANDARD = auto()    # amp WITH A_PRECISION divisor + standard c/b
    VARIANT_0 = auto()   # amp WITHOUT A_PRECISION divisor + standard c/b
    VARIANT_1 = auto()   # amp WITHOUT A_PRECISION divisor + c/b without A_PRECISION

class YDVariant(Enum):
    STANDARD = auto()
    VARIANT_0 = auto()   # A_PRECISION in b/c formulas
```

**Key discovery:** `DVariant.VARIANT_DP_ALPHA` was added beyond the original proposal.
D_VARIANT_GROUP_3 uses `calc_d` (standard) + `calc_dp_variant_alpha`, which differs from
D_VARIANT_GROUP_1 (`calc_d_variant_alpha` + `calc_dp_variant_alpha`). This required a new
enum value to distinguish the two.

**Key discovery:** `Y_VARIANT_GROUP_0 ⊂ Y_VARIANT_GROUP_1`. The two groups overlap completely,
yielding 3 observed combinations (STANDARD, VARIANT_0, VARIANT_1) rather than the 4 you'd
expect from 2 independent flags.

### Step 3: Add variant enum parameters to pool constructor ✅

```python
class CurveStableswapPool:
    def __init__(
        self,
        # ... existing parameters ...
        d_variant: DVariant = DVariant.STANDARD,
        y_variant: YVariant = YVariant.STANDARD,
        yd_variant: YDVariant = YDVariant.STANDARD,
    ):
        self._d_variant = d_variant
        self._y_variant = y_variant
        self._yd_variant = yd_variant
```

### Step 4: Replace address dispatches ✅

- `_get_d()` uses `match self._d_variant` instead of `self.address in self.D_VARIANT_GROUP_*`
- `_get_y()` uses `match self._y_variant` instead of `self.address in self.Y_VARIANT_GROUP_*`
- `_get_y_d()` uses `match self._yd_variant` instead of `self.address in self.Y_D_VARIANT_GROUP_*`

### Step 5: Remove class-level frozensets ✅

All 7 frozensets removed from the class body (~130 lines of address literals).

### Step 6: Update builder ✅

The builder calls `resolve_d_variant()`, `resolve_y_variant()`, `resolve_yd_variant()` from
`_variant_groups.py` and passes the results to the pool constructor. These functions are also
used by `_pool_strategies.py` (Plan 026) to populate the `PoolStrategies.d_variant`,
`PoolStrategies.y_variant`, and `PoolStrategies.yd_variant` fields.

## Implementation Order

1. ✅ **Create `_variant_groups.py`** with the 7 address sets and 3 resolver functions
2. ✅ **Define `DVariant`, `YVariant`, `YDVariant` enums** in `types.py` — including the unexpected `VARIANT_DP_ALPHA` for Group 3
3. ✅ **Add variant parameters to pool constructor** with `STANDARD` defaults — backwards-compatible
4. ✅ **Replace `_get_d()` dispatch** — `match self._d_variant`
5. ✅ **Replace `_get_y()` dispatch** — `match self._y_variant`
6. ✅ **Replace `_get_y_d()` dispatch** — `match self._yd_variant`
7. ✅ **Remove class-level frozensets** — 67 addresses, ~130 lines removed
8. ✅ **Update builder** to call resolver functions and pass variants
9. ✅ **Add 23 unit tests** in `tests/curve/test_variant_groups.py`

## Testing

### Unit tests (implemented)

23 tests in `tests/curve/test_variant_groups.py`:

- Each resolver returns the correct variant for known addresses
- Each resolver returns `STANDARD` for unknown addresses
- All variant group addresses are covered (no address falls through to default when it shouldn't)
- Y_VARIANT_GROUP_0 ⊂ Y_VARIANT_GROUP_1 relationship verified
- D_VARIANT_GROUP_3 maps to `DVariant.VARIANT_DP_ALPHA` (not `VARIANT_ALPHA_DP_ALPHA`)

### Integration tests

All 129 Curve tests pass. The variant dispatch produces the same results as the old address
dispatch for all known pools.

## Benefits

- **Locality:** New pools that need a specific variant formula are added to `_variant_groups.py` — not the pool class. The pool class is address-agnostic for D/Y/YD calculations.
- **Testability:** Variant resolution can be tested independently (does address X map to variant Y?). Pool construction can be tested with explicit variant values (does a pool with `DVariant.VARIANT_ALPHA` use the correct formula?).
- **Data-behaviour separation:** The pool class holds only computation logic. The mapping from "which pool uses which formula" is configuration data in a separate module.
- **Smaller pool class:** Removing 7 frozensets (~130 lines of addresses) from the class body makes the remaining code easier to navigate.

## Risks

- **Mapping completeness:** ⚠️ Mitigated. All 67 variant group addresses are mapped. However, new Curve pools may use variant formulas without being in the mapping — they'd silently use `STANDARD`. The provenance warning in `_pool_strategies.py` documents that the mapping was derived from old frozensets, not verified against contract source.
- **Subsumed by Plan 026:** ✅ The variant group resolvers are now used by `_pool_strategies.py` to populate `PoolStrategies` fields. The two modules work together: `_variant_groups.py` handles the address→variant mapping, `_pool_strategies.py` combines those variants with `SwapStyle` and `LendingRateStyle` into a complete `PoolStrategies` object.

## Relationship to Other Plans

- **Plan 026** (Strategy Objects): Plan 029 is a subset of Plan 026. Plan 026 handles *all* address dispatches (fee style, lending rate style, balance source, plus D/Y/YD variants). This plan handles only the D/Y/YD variant groups. Both are now complete and work together: `_pool_strategies.py` calls the `_variant_groups.py` resolvers to populate `PoolStrategies`.
- **Plan 027** (Lending-Rate Fetchers): Independent. This plan doesn't touch the `_stored_rates_from_*()` methods.
- **Plan 013** (Curve I/O-Free Architecture): Complete. This plan continues the clean-up started by Plan 013.
