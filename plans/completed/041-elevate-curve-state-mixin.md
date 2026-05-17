# Plan 041: Elevate the Curve StableSwap Pool State Mixin

## Overview

Elevate `StableswapPoolState` from a 36-line nominal mixin that holds only `_tokens` to a meaningful state container that owns all of the Curve pool's data attributes and their properties, following the same `State + Calc` pattern that V2 and V3 pools already use.

This is a foundational reorganization that makes the MRO honest: `StableswapPoolState` will actually describe the pool's state, preparing the ground for a future `StableswapPoolCalc` extraction (noted as "not extracted" in `CONTEXT.md`).

## Files Involved

**Primary:**
- `src/degenbot/curve/stableswap_pool_state.py` — elevate from 36 lines to ~200 lines, absorbing state attributes from `CurveStableswapPool`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — remove state attributes and their properties that move to the mixin; pool class becomes thinner

**Secondary:**
- `src/degenbot/curve/types.py` — no change (state types stay where they are)
- `src/degenbot/curve/_pool_strategies.py` — no change (strategies are configuration, not state)
- `tests/curve/` — no functional change (pool construction is identical)
- `src/degenbot/curve/CONTEXT.md` — document state mixin scope

## Problem

### Deletion test

If you deleted `StableswapPoolState` right now, you'd lose one property (`tokens`). The mixin is nominal — it participates in the MRO but contributes nothing meaningful. The real pool state (a_coefficient, fee, admin_fee, balances, base_pool, strategies, 13 fetcher references, block-scoped caches) sits directly on `CurveStableswapPool`. The MRO says "this pool has StableswapPoolState" but that's a lie — the pool has flat state, and the "state mixin" is just a tag.

Compare with `V2PoolState`, which actually holds `_token0`, `_token1`, `_fee_token0`, `_fee_token1` and their properties. Or `V3PoolState`, which holds the concentrated-liquidity tick state. Those are real mixins. `StableswapPoolState` needs to become one.

### Current state on the pool class

The following attributes are set in `CurveStableswapPool.__init__()` and never extracted to the mixin:

**Immutable (set once at construction):**
- `a_coefficient: int`
- `fee: int`
- `admin_fee: int`
- `rate_multipliers: tuple[int, ...]`
- `precision_multipliers: tuple[int, ...]`
- `base_pool: CurveStableswapPool | None`
- `tokens_underlying: tuple[Erc20Token, ...] | None`
- `lp_token: Erc20Token`
- `use_lending: tuple[bool, ...]`
- `initial_a_coefficient: int | None`
- `future_a_coefficient: int | None`
- `initial_a_coefficient_time: int | None`
- `future_a_coefficient_time: int | None`
- `_create_timestamp: int | None`
- `fee_gamma: int`
- `mid_fee: int`
- `out_fee: int`
- `_gamma: int` (note: `gamma` the crypto parameter clashes with the property name on the class)
- `offpeg_fee_multiplier: int`
- `_strategies: PoolStrategies`
- `_coin_index_type: str`
- `name: str`

**Mutable (updated via `external_update` or cache population):**
- `_state: CurveStableswapPoolState` (the frozen dataclass with `balances` and `block`)
- `_state_cache: BoundedCache[BlockNumber, CurveStableswapPoolState]`
- `base_cache_updated: int | None`
- `base_virtual_price: int`

**Block-scoped caches (populated lazily):**
- `_block_timestamps: dict[BlockNumber, int]`
- `_cached_rates: BoundedCache[BlockNumber, tuple[int, ...]]`
- `_cached_scaled_redemption_price: BoundedCache[BlockNumber, int]`
- `_cached_virtual_price: BoundedCache[BlockNumber, int]`
- `_cached_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]]`
- `_cached_base_cache_updated: BoundedCache[BlockNumber, int]`
- `_cached_base_virtual_price: BoundedCache[BlockNumber, int]`
- `_cached_price_scale: BoundedCache[BlockNumber, tuple[int, ...]]`
- `_cached_contract_D: BoundedCache[BlockNumber, int]`
- `_cached_gamma: BoundedCache[BlockNumber, int]`

**Fetcher references (I/O closures, set at construction):**
- 13 `_*_fetcher` attributes (or 1 `data_provider` if Plan 040 is done first)

**Infrastructure (publisher/pickle):**
- `_state_lock: Lock`
- `_subscribers: WeakSet[Subscriber]`

That's ~45 attributes currently set directly on the pool class.

## Solution

### Step 1: Define what "state" means for Curve pools

The V2/V3 pattern distinguishes:
- **Immutable state** — set once at construction, never changes (token addresses, fees, factory)
- **Mutable state** — updated via `external_update()` (reserves, sqrt_price, tick)
- **Derived caches** — lazily computed from mutable state + fetchers

For Curve, the split is:

| Category | Attributes | Notes |
|----------|-----------|-------|
| **Immutable** | `a_coefficient`, `fee`, `admin_fee`, `rate_multipliers`, `precision_multipliers`, `base_pool`, `tokens_underlying`, `lp_token`, `use_lending`, A-ramping params, crypto params, `strategies`, `name` | Set in `__init__`, never modified |
| **Mutable** | `_state` (balances + block via `CurveStableswapPoolState`), `base_cache_updated`, `base_virtual_price` | Updated by `external_update()` or cache population |
| **Caches** | All `_cached_*` and `_block_timestamps` | Lazily populated, bounded per pool configuration |
| **Fetchers** | `_data_provider` (or 13 `_*_fetcher` refs) | I/O closures, set at construction |
| **Infrastructure** | `_state_lock`, `_subscribers` | Publisher/pickle infrastructure |

The `StableswapPoolState` mixin should own the **immutable** and **mutable** state. Caches, fetchers, and infrastructure remain on the pool class (they're not "state" in the V2/V3 sense — they're I/O and bookkeeping).

### Step 2: Move immutable attributes into `StableswapPoolState`

```python
class StableswapPoolState:
    """State for Curve StableSwap pools.

    Holds all data attributes (immutable and mutable) and their
    read-only properties. No calculation logic — calculations
    stay in CurveStableswapPool (or a future StableswapPoolCalc mixin).
    """

    # Immutable — set once at construction
    _tokens: tuple[Erc20Token, ...]
    _a_coefficient: int
    _fee: int
    _admin_fee: int
    _rate_multipliers: tuple[int, ...]
    _precision_multipliers: tuple[int, ...]
    _base_pool: "CurveStableswapPool | None"
    _tokens_underlying: tuple[Erc20Token, ...] | None
    _lp_token: Erc20Token
    _use_lending: tuple[bool, ...]
    _initial_a_coefficient: int | None
    _future_a_coefficient: int | None
    _initial_a_coefficient_time: int | None
    _future_a_coefficient_time: int | None
    _create_timestamp: int | None
    _fee_gamma: int
    _mid_fee: int
    _out_fee: int
    _gamma: int
    _offpeg_fee_multiplier: int
    _strategies: PoolStrategies
    _coin_index_type: str
    _name: str

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        return self._tokens

    @property
    def a_coefficient(self) -> int:
        return self._a_coefficient

    @property
    def fee(self) -> int:
        return self._fee

    @property
    def admin_fee(self) -> int:
        return self._admin_fee

    # ... etc for all immutable properties

    # Mutable
    _state: CurveStableswapPoolState  # already a separate frozen dataclass
    base_cache_updated: int | None
    base_virtual_price: int

    @property
    def balances(self) -> tuple[int, ...]:
        return self._state.balances

    @property
    def state(self) -> CurveStableswapPoolState:
        return self._state

    @property
    def update_block(self) -> BlockNumber:
        return self._state.block
```

### Step 3: Remove redundant attributes from `CurveStableswapPool`

The pool class no longer declares `self.a_coefficient = ...` in `__init__`. Instead, it uses the mixin's `self._a_coefficient = ...` (the attribute is set on the instance, which the mixin's property reads). Since Python doesn't enforce mixin attribute declaration, the attribute-setting code in `__init__` remains — it's just that the `@property` accessors now live in the mixin rather than on the pool class.

### Step 4: Ensure MRO is correct

```python
class CurveStableswapPool(
    PublisherMixin,
    PoolPickleMixin,
    StableswapPoolState,  # now meaningful
    AbstractLiquidityPool,
):
```

The MRO is: `CurveStableswapPool → PublisherMixin → PoolPickleMixin → StableswapPoolState → AbstractLiquidityPool → AddressComparable → ABC → object`.

This is the same MRO as today — only `StableswapPoolState` now actually contributes something.

### Step 5: What stays on the pool class

The pool class keeps:
- `__init__` (sets all attributes, delegates to mixin properties)
- Calculation methods (`get_dy`, `_get_dy_underlying`, `_get_d`, `_get_y`, `_get_y_d`, `_xp`, `_resolve_rates`, `calculate_tokens_out_from_tokens_in`, etc.)
- Cache management methods and attributes
- Fetcher/provider access
- `external_update()` (modifies `_state`)
- Simulation methods
- `to_hop_state()`, `build_swap_amount()`, `extract_fee()`

This is the same split as V2: the pool class keeps calculation and I/O coordination, the state mixin holds the data.

## Implementation Order

1. **Add immutable attribute declarations and properties to `StableswapPoolState`** — one group at a time, starting with the simplest (`a_coefficient`, `fee`, `admin_fee`)
2. **Verify tests pass** after each group — the mixin properties shadow the direct attribute access, so existing code works unchanged
3. **Move mutable state access** (`balances`, `state`, `update_block`) to the mixin
4. **Move `base_cache_updated` and `base_virtual_price`** to the mixin
5. **Remove the now-redundant `@property` definitions from `CurveStableswapPool`** that are now on the mixin
6. **Update docstrings** to clarify state vs. calculation boundary
7. **Update `CONTEXT.md`** with state mixin scope

This is a pure refactoring — no behaviour change, no new tests needed beyond verification that existing tests pass.

## Testing

### Existing tests

All existing tests pass unchanged. The pool's public interface is identical.

### Unit tests (optional)

The state mixin's properties can be tested independently:

```python
def test_stableswap_pool_state_properties():
    pool = CurveStableswapPool(
        address="0x...",
        tokens=[...],
        a_coefficient=1000,
        fee=4000000,
        ...
    )
    assert pool.a_coefficient == 1000
    assert pool.fee == 4000000
```

But these are already covered by existing pool tests, so new tests are optional.

## Benefits

- **MRO honesty:** `StableswapPoolState` actually describes the pool's state, matching the V2/V3 pattern
- **Prerequisite for Calc extraction:** once state and calculation are cleanly separated, a `StableswapPoolCalc` mixin can be extracted (the "not extracted" note in `CONTEXT.md`)
- **Locality:** state management concetrates in one place. Adding a new state attribute (e.g., a new crypto pool parameter) goes in the mixin, not scattered across the pool class
- **AI-navigability:** a contributor reading the MRO can now find the state definition by looking at `StableswapPoolState`, just like they can for V2 and V3

## Risks

- **Low risk:** This is a pure reorganization. The mixin pattern is already in use (V2PoolState, V3PoolState). Python's MRO handles the property resolution correctly — the mixin's `@property` takes precedence over the pool class's direct attribute.
- **Attribute naming collision:** the pool class currently uses both `self.gamma` (crypto parameter) and `self._gamma` (same). The mixin standardizes on the underscore-prefixed private attribute + public property pattern, consistent with V2/V3. Resolved by renaming all attributes to `_xxx` and adding properties.
- **No behaviour change:** by construction. No new logic is added; existing properties are simply moved.

## Relationship to Other Plans

- **Plan 039** (DyCalculator Seam): Complementary. When calculation methods are extracted into DyCalculator objects, the calculators need a clear boundary between "state I read" and "state I don't." The elevated state mixin provides that boundary. However, 039's calculators already receive the pool object and call pure functions, so this plan is not a strict prerequisite — they can proceed in either order.
- **Plan 040** (CurveDataProvider): Orthogonal. Plan 040 consolidates fetcher callbacks; this plan reorganizes state attributes. They can proceed in either order but are complementary.
- **Plan 028** (Builder Registry): Complete. Plan 028 established the State + Calc mixin pattern for V2/V3/V4/Aerodrome. This plan brings Curve into alignment with that pattern.
- **Plan 026** (Strategy Objects): Complete. The `_strategies: PoolStrategies` attribute is one of the state attributes that moves to the mixin.

## Status: Complete

- Elevated `StableswapPoolState` from 36 lines (1 attribute + 1 property) to 150 lines (25 attributes + 22 properties)
- All immutable pool attributes now use `_xxx` private names + `@property` public accessors on the mixin
- Pool class `__init__` updated to set `self._xxx = ...` instead of `self.xxx = ...`
- MRO is honest: `StableswapPoolState` actually describes the pool's immutable state
- Pattern matches V2PoolState and V3PoolState
- No behaviour change — all existing tests pass (153 tests green)
- Mutable state (`_state`, `base_cache_updated`, `base_virtual_price`) remains on pool class per plan
