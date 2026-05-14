# Plan 026: Replace Address-Dispatched Behaviour in CurveStableswapPool with Strategy Objects

## Status: COMPLETE ✅

Committed as `0fe5d9ed`. See also bug fix `a03b3295` (sUSD pool LendingRateStyle) and docs commit `efd56594` (provenance warnings, debugging workflow).

## Overview

Replace the 26 `if self.address in {...}` dispatches in `CurveStableswapPool` with injectable strategy objects set at construction time. The builder already detects pool variants (lending, crypto, metapool, D-variant, Y-variant) at build time — the strategy objects make those variants visible in the pool's configuration rather than hidden behind address lookups.

This is the primary deepening: it transforms a 2122-line class whose behaviour is determined by its address into one whose behaviour is determined by strategy enums injected at construction.

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — remove 26 address dispatches, 7 variant group frozensets (67 addresses), ~130 hex address literals; accept strategy objects instead
- `src/degenbot/curve/types.py` — add strategy enums and frozen dataclasses
- `src/degenbot/builders/curve_pool_builder.py` — resolve strategies from detection results and pass to pool constructor
- `src/degenbot/curve/detection/` — detection modules return richer results that map directly to strategy values

**Secondary:**
- `src/degenbot/curve/fetcher_factory.py` — strategy values drive which fetchers are created
- `tests/curve/test_curve_stableswap_pool.py` — update pool construction to pass strategy values
- `tests/curve/detection/` — detection tests now validate strategy mapping
- `src/degenbot/curve/CONTEXT.md` — document new terms

## Problem

### Deletion test

If you delete the `if self.address in {...}` blocks, complexity does NOT vanish — it reappears as "which calculation path should this pool use?" across every caller or test that needs to construct a pool with the right behaviour. The current class is *earning its keep* (it dispatches correctly), but it's doing it in a way that has zero locality and zero leverage:

- **No locality:** A bug in the cToken rate accrual lives inside a 2122-line class, dispatched by a hard-coded address in `get_dy`. Fixing it requires finding and editing the class.
- **No leverage:** The pool's public interface (`get_dy(i, j, dx)`) already hides the dispatch. Callers don't need address checks. But tests do — you can't test the cToken path without knowing which address triggers it.
- **No AI-navigability:** An LLM or new contributor reading `get_dy()` encounters 16 sequential `if self.address` blocks before reaching the general case. The function is ~650 lines long.

### Specific dispatch points

The 26 address dispatches fall into these categories:

| Category | Methods | Address sets | Behavioural difference |
|----------|---------|-------------|----------------------|
| **Rate source** | `get_dy`, `_dynamic_fee` | 16 individual/set checks | Which `_stored_rates_from_*()` method to call for lending tokens |
| **Balance source** | `get_dy`, `_dynamic_fee` | 5 address sets | Whether to use `self.balances` or live balances minus admin balances |
| **Crypto pool** | `get_dy` | 1 address | Uses `_newton_y()` + D/gamma/price_scale fetchers |
| **D-variant** | `_get_d` | 5 variant groups | Which `calc_d` / `calc_dp` formula pair |
| **Y-variant** | `_get_y` | 2 variant groups | Whether amp uses `A_PRECISION` divisor and c formula |
| **Y_D-variant** | `_get_y_d` | 1 variant group | Whether b/c formulas use `A_PRECISION` divisor |
| **Metapool special** | `get_dy`, `_get_dy_underlying` | 2 individual addresses | Redemption price rate source |
| **Fee style** | `get_dy`, `_dynamic_fee` | Varied | Whether fee is applied before or after rate conversion, offpeg multiplier |

## Solution

### Step 1: Define strategy enums

Added to `src/degenbot/curve/types.py`:

```python
from enum import Enum, auto

class SwapStyle(Enum):
    """Which computation path to use in get_dy."""
    STANDARD = auto()                    # dy = xp[j] - y - 1, fee, then rate convert
    RATE_ADJUSTED = auto()               # dy = (xp[j] - y - 1) * PRECISION // rates[j], fee on converted dy
    RATE_ADJUSTED_NO_ONE = auto()        # dy = (xp[j] - y) * PRECISION // rates[j], fee on converted dy (no -1)
    RAW_BALANCE = auto()                 # no rate conversion on dy, direct fee
    CRYPTO = auto()                      # Newton's method, dynamic fee
    LIVE_ADMIN = auto()                  # live balances minus admin, dy = xp[j] - y - 1, fee, rate convert
    LIVE_ADMIN_DYNAMIC = auto()          # live balances minus admin, dynamic offpeg fee
    LIVE_ADMIN_DYNAMIC_PRECISION = auto() # live balances minus admin, precision multipliers for xp, dynamic offpeg fee
    LIVE_ADMIN_ORACLE = auto()           # live balances minus admin, oracle rates, dy = xp[j] - y - 1, fee, rate convert
    NO_ONE_FEE_RATE = auto()             # dy = xp[j] - y (no -1), fee, then rate convert — AETH/RETH
    CYTOKEN = auto()                    # dy = xp[j] - y - 1, then (dy - fee) * PRECISION // rates[j] — fee inside rate conversion

class MetapoolRateStyle(Enum):
    """Which rates to use for the metapool branch in get_dy."""
    STANDARD = auto()                    # (rate_multipliers[0], virtual_price)
    PRECISION_VP = auto()               # (PRECISION, virtual_price)
    REDEMPTION_VP = auto()             # (redemption_price, virtual_price)

class MetapoolUnderlyingStyle(Enum):
    """Which computation path to use in _get_dy_underlying."""
    STANDARD = auto()                   # rate_multipliers with VP for base pool LP token
    REDEMPTION = auto()                 # redemption_price for first coin, VP for second
    PRECISION_VP = auto()              # (PRECISION, virtual_price) — no rate multiplier for first coin

class LendingRateStyle(Enum):
    """Which rate-fetching method to use for lending tokens."""
    NONE = auto()            # No lending tokens — use rate_multipliers directly
    CTOKEN = auto()          # Exchange rate with supply rate accrual
    YTOKEN = auto()          # Price per full share
    CYTOKEN = auto()         # cToken + yToken combined accrual
    AETH = auto()            # Lido aETH ratio inversion
    RETH = auto()            # Rocket Pool exchange rate
    ORACLE = auto()          # On-chain oracle bitmask

class DVariant(Enum):
    """Which D-calculation formula to use in _get_d."""
    STANDARD = auto()
    VARIANT_ALPHA = auto()
    VARIANT_ALPHA_DP_ALPHA = auto()  # alpha D + alpha Dp
    VARIANT_DP_ALPHA = auto()       # standard D + alpha Dp (Group 3)
    VARIANT_BETA_DP = auto()        # standard D + beta Dp
    VARIANT_GAMMA_DP = auto()       # standard D + gamma Dp

class YVariant(Enum):
    """Which Y-calculation formula to use in _get_y."""
    STANDARD = auto()              # amp WITH A_PRECISION divisor + standard c/b
    VARIANT_0 = auto()             # amp WITHOUT A_PRECISION divisor + standard c/b
    VARIANT_1 = auto()             # amp WITHOUT A_PRECISION divisor + c/b without A_PRECISION

class YDVariant(Enum):
    """Which Y_D-calculation formula to use in _get_y_d."""
    STANDARD = auto()
    VARIANT_0 = auto()             # uses A_PRECISION in b/c formulas
```

**Design decision:** The original proposal had `FeeStyle` + `BalanceSource` as two separate axes.
In practice, the balance source is not independent — it's coupled with the fee style and rate
conversion order. The implemented `SwapStyle` enum captures each observed *combination* as a
single value (11 values covering all observed computation paths), which is more accurate and
avoids invalid combinations like `FeeStyle.RATE_ADJUSTED` + `BalanceSource.LIVE_MINUS_ADMIN`.

A `DVariant.VARIANT_DP_ALPHA` was added beyond the original proposal (Group 3 uses standard
`calc_d` + variant_alpha `calc_dp`, distinct from Group 1's variant_alpha `calc_d` + variant_alpha
`calc_dp` elsewhere in the pool, and documented.
}

### Step 2: Replace class-level address frozensets with strategy mapping

Removed all 7 `D_VARIANT_GROUP_*`, `Y_VARIANT_GROUP_*`, `Y_D_VARIANT_GROUP_*` frozensets from the class body.

Created `src/degenbot/curve/_pool_strategies.py` (374 lines) with a module-level mapping from pool address → `PoolStrategies`:

```python
_POOL_STRATEGIES: dict[ChecksumAddress, PoolStrategies] = {
    "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7": PoolStrategies(
        swap_style=SwapStyle.RATE_ADJUSTED,
        lending_rate_style=LendingRateStyle.NONE,
        d_variant=DVariant.VARIANT_ALPHA,
        y_variant=YVariant.STANDARD,
        yd_variant=YDVariant.STANDARD,
        metapool_rate_style=MetapoolRateStyle.STANDARD,
        metapool_underlying_style=MetapoolUnderlyingStyle.STANDARD,
    ),
    # ... 65 more entries
}
```

The builder calls `resolve_pool_strategies(pool_address)` which looks up the address in this
mapping. If the address is not in the mapping, returns `PoolStrategies()` defaults.

The mapping includes a **provenance warning** in the module docstring noting that it was derived
from old frozensets and `if self.address` blocks, NOT verified against on-chain contract source.
Per-group comments identify which old frozenset each address group came from.

### Step 3: Add `PoolStrategies` frozen dataclass

```python
@dataclasses.dataclass(slots=True, frozen=True)
class PoolStrategies:
    """Resolved calculation strategies for a Curve pool instance."""
    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    swap_style: SwapStyle = SwapStyle.STANDARD
    metapool_rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD
    metapool_underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD
    lending_rate_style: LendingRateStyle = LendingRateStyle.NONE
```

All fields have defaults so that `PoolStrategies()` produces a safe default for plain pools. The
dataclass has `slots=True` and `frozen=True` — it's immutable and picklable (contains only enums).

### Step 4: Modify `CurveStableswapPool.__init__` to accept `PoolStrategies`

The constructor gains a `strategies: PoolStrategies` parameter (default=`PoolStrategies()`). The 18 `if self.address` dispatches in `get_dy()` and `_get_dy_underlying()` become `match self._strategies.swap_style` and `if self._strategies.metapool_*` checks.

Key simplifications:
- `get_dy()` dispatches on `self._strategies.swap_style` instead of `self.address`
- `_get_dy_underlying()` dispatches on `self._strategies.metapool_underlying_style` instead of `self.address`
- `_get_d()` dispatches on `self._strategies.d_variant` instead of `self.address in self.D_VARIANT_GROUP_*`
- `_get_y()` dispatches on `self._strategies.y_variant` instead of `self.address in self.Y_VARIANT_GROUP_*`
- `_get_y_d()` dispatches on `self._strategies.yd_variant` instead of `self.address in self.Y_D_VARIANT_GROUP_*`
- `_resolve_rates()` dispatches on `self._strategies.lending_rate_style` to call the `LendingRateFetcher`
- `_dynamic_fee()` is called when `SwapStyle` is `LIVE_ADMIN_DYNAMIC` or `LIVE_ADMIN_DYNAMIC_PRECISION`

### Step 5: Modify `CurvePoolBuilder.build()` to resolve strategies

The builder calls `resolve_pool_strategies(pool_address)` from `_pool_strategies.py`, which
looks up the address in the `_POOL_STRATEGIES` dict and returns the matching `PoolStrategies`.
Unlisted addresses get `PoolStrategies()` defaults.

The builder then passes `strategies=strategies` to `CurveStableswapPool.__init__()`.

### Step 6: Remove `_stored_rates_from_*()` methods from the pool class

Done in Plan 027. The pool's `_resolve_rates()` method dispatches to the injected `LendingRateFetcher`
based on `self._strategies.lending_rate_style`. The 6 `_stored_rates_from_*()` methods were replaced
by factory-created closures in `CurveFetcherFactory`.

## Implementation Order

1. ✅ **Define strategy enums** and `PoolStrategies` dataclass in `types.py` — `SwapStyle` (11 values), `MetapoolRateStyle` (3), `MetapoolUnderlyingStyle` (3), `LendingRateStyle` (7)
2. ✅ **Create `_pool_strategies.py`** with the address → `PoolStrategies` mapping (66 unique pool addresses)
3. ✅ **Add `strategies` parameter to `CurveStableswapPool.__init__`** with default `PoolStrategies()` — backwards-compatible
4. ✅ **Replace all 18 address dispatches in `get_dy()` and `_get_dy_underlying()`** with `match self._strategies.swap_style` and `if self._strategies.metapool_*` dispatch
5. ✅ **Remove class-level frozensets** — all 7 variant groups moved to `_variant_groups.py` (Plan 029)
6. ✅ **Update builder** to resolve strategies from `_pool_strategies.py` and pass to pool constructor
7. ✅ **Update tests** — 24 new tests in `tests/curve/test_pool_strategies.py`
8. ✅ **Update `CONTEXT.md`** with strategy enum and debugging workflow documentation
9. ✅ **Add provenance warnings** to `_pool_strategies.py` module docstring and per-group comments

## Bug Fixes Discovered During Implementation

- **sUSD pool (`0xA5407eAE`)**: Was incorrectly in the `CTOKEN_ADDRESSES` frozenset, but `cast source` revealed `USE_LENDING = [False, False, False, False]`. Changed to `LendingRateStyle.NONE` with `SwapStyle.RATE_ADJUSTED_NO_ONE` (fixed in `a03b3295`).
- **D_VARIANT_GROUP_3**: Uses `calc_d` (standard) + `calc_dp_variant_alpha`, which is different from D_VARIANT_GROUP_1 (`calc_d_variant_alpha` + `calc_dp_variant_alpha`). Added `DVariant.VARIANT_DP_ALPHA` to distinguish (fixed in Plan 029, `586579ff`).
- **Y_VARIANT_GROUP_0 ⊂ Y_VARIANT_GROUP_1**: Y_VARIANT_GROUP_0 is a proper subset of Y_VARIANT_GROUP_1. This yields 3 observed combinations (STANDARD, VARIANT_0, VARIANT_1) not 4 (fixed in Plan 029, `586579ff`).

## Testing

### Unit tests (implemented)

- 24 tests in `tests/curve/test_pool_strategies.py`:
  - Strategy enum construction and immutability
  - `PoolStrategies` frozen dataclass defaults and custom construction
  - Address→strategy mapping completeness (all addresses in old frozensets are mapped)
  - Each `SwapStyle` and `LendingRateStyle` value has at least one pool mapped to it
  - `resolve_pool_strategies()` returns correct strategies for known addresses and defaults for unknown
  - Pickle round-trip preserves strategy values

### Integration tests (existing)

- All 129 Curve tests pass (including `test_calculations()` which compares pool output against on-chain contract calls)
- 2556 total project tests pass (1 pre-existing Camelot test failure unrelated)

### Fake pool construction

```python
# Old: Must know that 0x06364f10... is a yToken pool
pool = CurveStableswapPool(address="0x06364f10...", ...)

# New: Any valid address works; strategy is explicit
pool = CurveStableswapPool(
    address="0x0000...",
    strategies=PoolStrategies(
        lending_rate_style=LendingRateStyle.YTOKEN,
        swap_style=SwapStyle.RATE_ADJUSTED,
        ...
    ),
    ...
)
```

## Benefits

- **Locality:** Adding a new pool variant requires editing `_pool_strategies.py` and/or the strategy enum, not the 1708-line pool class
- **Leverage:** The pool's public interface (`get_dy`, `calculate_tokens_out_from_tokens_in`) doesn't change. Callers and tests don't need to know about strategies. But internal tests can test each strategy independently.
- **Testability:** Tests construct pools with explicit strategies instead of relying on address-triggered dispatch paths
- **AI-navigability:** `get_dy()` shrinks from ~650 lines of sequential address checks to a targeted `match` on `self._strategies.swap_style`
- **Pool class size:** 2122 → 1708 lines (~19% reduction)
- **Address literals removed:** 67 hex addresses removed from class body, ~130 lines of frozenset definitions eliminated

## Risks

- **Mapping completeness:** ✅ Mitigated. The `_POOL_STRATEGIES` mapping covers all 66 unique addresses from the old frozensets. Unlisted addresses get `PoolStrategies()` defaults. The module docstring includes a provenance warning documenting that the mapping was derived from old frozensets, not verified against contract source.
- **Performance:** ✅ Confirmed. Strategy dispatch via `match` on enum values is equivalent to frozenset membership testing — both are O(1).
- **Partial migration risk:** ✅ Resolved. All 18 address dispatches in `get_dy()` and `_get_dy_underlying()` were replaced in a single commit. No mixed dispatch remains.
- **Wrong enum value:** ⚠️ Discovered and fixed. sUSD pool was incorrectly mapped to `LendingRateStyle.CTOKEN` (see Bug Fixes section). The provenance warning in `_pool_strategies.py` and debugging workflow in `CONTEXT.md` guide future verification against on-chain contract source.

## Relationship to Other Plans

- **Plan 027** (Lending-Rate Fetcher Protocols): Plan 027 removes `_stored_rates_from_*()` from the pool class by converting them to typed fetcher closures. This plan (026) makes the *selection* of which fetcher to use strategy-driven rather than address-driven. They are complementary: 026 defines *which* rate style to use, 027 defines *how* that rate style fetches data. Recommend doing 026 first (strategy selection), then 027 (fetcher extraction).
- **Plan 018** (Curve Builder Decomposition): Complete. The detection sub-modules already return frozen dataclasses (`CoinDiscoveryResult`, `LendingDetectionResult`, etc.). This plan adds a new detection-like step: strategy resolution. It naturally extends the builder decomposition.
- **ADR-001** (I/O-Free Pools): This plan deepens the I/O-free architecture. Strategies are pure configuration data injected at construction — they contain no I/O. The pool remains I/O-free.
