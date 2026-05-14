# Plan 019: Replace ArbPoolCacheAdapter getattr Chain with Protocol Methods

**Status: READY**

## Overview

Replace the `getattr`-based introspection in `ArbPoolCacheAdapter._get_reserves()` and `_get_fee()` with explicit protocol methods on pool classes. Each pool type implements `reserves_for_cache()` and `fee_for_cache()` directly, eliminating the phantom seam where the adapter claims to work with "any pool" but actually hard-codes knowledge of V2 state attributes, V3 fee denominators, and Aerodrome fee structures.

## Files Involved

**Existing:**
- `src/degenbot/arbitrage/optimizers/pool_cache_adapter.py` — the phantom seam
- `src/degenbot/types/pool_protocols.py` — existing protocol definitions
- `src/degenbot/uniswap/v2_liquidity_pool.py` — `UniswapV2Pool`
- `src/degenbot/uniswap/v3_liquidity_pool.py` — `UniswapV3Pool`
- `src/degenbot/uniswap/v4_liquidity_pool.py` — `UniswapV4Pool`
- `src/degenbot/aerodrome/pools.py` — `AerodromeV2Pool`
- `src/degenbot/types/hop_types.py` — `PoolInvariant` (hop-types enum)
- `src/degenbot/types/pool_type.py` — `PoolInvariant` (pool-type enum)

**Modified:**
- `src/degenbot/arbitrage/optimizers/pool_cache_adapter.py` — replace `getattr` chains with protocol method calls
- `src/degenbot/types/pool_protocols.py` — add protocol methods
- All pool classes — implement `reserves_for_cache()` and `fee_for_cache()`

**Tests:**
- `tests/arbitrage/test_pool_cache_adapter.py` — new or updated

## Problem

`ArbPoolCacheAdapter` uses `getattr` chains with `isinstance` fallbacks to extract data from pool objects:

```python
# Current _get_reserves()
@staticmethod
def _get_reserves(pool: Any) -> tuple[int, int] | None:
    state = getattr(pool, "state", None)
    if state is not None:
        r0 = getattr(state, "reserves_token0", None)
        r1 = getattr(state, "reserves_token1", None)
        if isinstance(r0, int) and isinstance(r1, int):
            return r0, r1
    reserves = getattr(pool, "reserves", None)
    if isinstance(reserves, tuple) and len(reserves) == 2:
        return reserves[0], reserves[1]
    return None

# Current _get_fee()
@staticmethod
def _get_fee(pool: Any) -> Fraction | None:
    fee = getattr(pool, "fee", None)
    if isinstance(fee, Fraction):
        return fee
    fee_token0 = getattr(pool, "fee_token0", None)
    if isinstance(fee_token0, Fraction):
        return fee_token0
    fee_int = getattr(pool, "fee", None)
    fee_denom = getattr(pool, "FEE_DENOMINATOR", None)
    if isinstance(fee_int, int) and isinstance(fee_denom, int):
        return Fraction(fee_int, fee_denom)
    return None
```

This is a **phantom seam** — the adapter works with type `Any` but has hard-coded knowledge of:
- V2 pool state structure (`state.reserves_token0`, `state.reserves_token1`)
- V2 directional fees (`fee_token0` as default forward direction)
- V3/V4 fee representation (`fee` + `FEE_DENOMINATOR`)
- Aerodrome fee representation (`fee` as `Fraction`)

If a new pool type is added, the adapter breaks silently (returns `None`, raised as `ValueError` at the call site). The deletion test: deleting the adapter wouldn't eliminate the knowledge of how to extract reserves/fees — it would reappear at every call site that needs to register a pool with the solver cache.

## Solution

Add `reserves_for_cache()` and `fee_for_cache()` to the `ArbitrageCapablePool` protocol (or a dedicated `CacheablePool` protocol). Each pool implements these methods. The adapter becomes a thin subscriber that calls protocol methods instead of introspecting.

### Protocol addition

```python
# src/degenbot/types/pool_protocols.py

@runtime_checkable
class CacheablePool(Protocol):
    """
    Interface for pools that can register their state in the solver cache.

    Each pool knows how to export its reserves and fee for the Rust
    solver cache. This replaces getattr-based introspection.
    """

    def reserves_for_cache(self) -> tuple[int, int]:
        """
        Return (reserve_in, reserve_out) for the forward direction
        (token0 → token1).

        Implementations may return reserves from state or calculated
        virtual reserves (for concentrated-liquidity pools).
        """
        ...

    def fee_for_cache(self) -> Fraction:
        """
        Return the trading fee as a Fraction for the forward direction
        (token0 → token1).

        For V2 pools with directional fees, returns fee_token0.
        For V3/V4 pools, returns fee / FEE_DENOMINATOR.
        """
        ...
```

### Pool implementations

```python
# UniswapV2Pool
def reserves_for_cache(self) -> tuple[int, int]:
    return self.reserves_token0, self.reserves_token1

def fee_for_cache(self) -> Fraction:
    return self._fee_token0  # Forward direction (token0 → token1)

# AerodromeV2Pool
def reserves_for_cache(self) -> tuple[int, int]:
    return self.reserves_token0, self.reserves_token1

def fee_for_cache(self) -> Fraction:
    return self._fee  # Non-directional fee

# UniswapV3Pool
def reserves_for_cache(self) -> tuple[int, int]:
    """Compute virtual reserves for the Rust solver cache."""
    from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves
    reserve_in, reserve_out = v3_virtual_reserves(
        liquidity=self.liquidity,
        sqrt_price_x96=self.sqrt_price_x96,
        fee=self._fee,
        tick_lower=self._tick_lower_for_virtual_reserves,
        tick_upper=self._tick_upper_for_virtual_reserves,
    )
    return reserve_in, reserve_out

def fee_for_cache(self) -> Fraction:
    return Fraction(self._fee, self.FEE_DENOMINATOR)

# UniswapV4Pool — same as V3
```

### Simplified adapter

```python
# src/degenbot/arbitrage/optimizers/pool_cache_adapter.py

class ArbPoolCacheAdapter(Subscriber):
    """
    Subscribes to pool state updates and auto-registers them in the
    ArbSolver's Rust pool cache.

    Each pool is registered in both reserve orientations (token0→token1
    and token1→token0), since the solver's cache stores direction-specific
    reserve pairs.
    """

    def __init__(self, *, solver: ArbSolver) -> None:
        self._solver = solver
        self._pool_to_ids: dict[int, tuple[int, int]] = {}

    def register(self, pool: CacheablePool) -> int:
        pool.subscribe(self)
        reserves = pool.reserves_for_cache()
        fee = pool.fee_for_cache()
        reserve_in, reserve_out = reserves

        # Register forward orientation
        forward_id = self._solver.register_pool(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )

        # Register reverse orientation
        reverse_id = self._solver.register_pool(
            reserve_in=reserve_out,
            reserve_out=reserve_in,
            fee=fee,
        )

        self._pool_to_ids[id(pool)] = (forward_id, reverse_id)
        return forward_id

    def notify(self, publisher: Any, message: AbstractPublisherMessage) -> None:
        pool = publisher
        ids = self._pool_to_ids.get(id(pool))
        if ids is None:
            return

        if not isinstance(pool, CacheablePool):
            return

        reserves = pool.reserves_for_cache()
        fee = pool.fee_for_cache()
        reserve_in, reserve_out = reserves
        forward_id, reverse_id = ids

        self._solver.update_pool(forward_id, reserve_in, reserve_out, fee)
        self._solver.update_pool(reverse_id, reserve_out, reserve_in, fee)

    def get_pool_ids(self, pool: CacheablePool) -> tuple[int, int] | None:
        return self._pool_to_ids.get(id(pool))
```

## Implementation Steps

### Phase 1: Add protocol methods to pool classes (TDD)

1. **Red:** Write tests for `reserves_for_cache()` and `fee_for_cache()` on each pool type:
   ```python
   def test_v2_pool_reserves_for_cache():
       pool = FakeV2Pool(reserves_token0=1000, reserves_token1=2000, fee_token0=Fraction(3, 1000))
       assert pool.reserves_for_cache() == (1000, 2000)
       assert pool.fee_for_cache() == Fraction(3, 1000)

   def test_aerodrome_pool_fee_for_cache():
       pool = FakeAerodromePool(fee=Fraction(2, 1000))
       assert pool.fee_for_cache() == Fraction(2, 1000)

   def test_v3_pool_reserves_for_cache():
       pool = FakeV3Pool(liquidity=..., sqrt_price_x96=..., fee=3000)
       reserves = pool.reserves_for_cache()
       assert isinstance(reserves, tuple) and len(reserves) == 2
       assert pool.fee_for_cache() == Fraction(3000, 1_000_000)
   ```
2. **Green:** Add `reserves_for_cache()` and `fee_for_cache()` methods to `UniswapV2Pool`, `AerodromeV2Pool`, `UniswapV3Pool`, `UniswapV4Pool`.
3. Run all pool tests.

### Phase 2: Define the protocol

1. Add `CacheablePool` protocol to `src/degenbot/types/pool_protocols.py`.
2. Verify `isinstance` checks work with `@runtime_checkable`.

### Phase 3: Refactor adapter

1. **Red:** Write tests for the refactored `ArbPoolCacheAdapter` using `CacheablePool` protocol:
   ```python
   def test_adapter_registers_pool_via_protocol():
       pool = FakeV2Pool(reserves_token0=100, reserves_token1=200, fee=Fraction(3, 1000))
       adapter = ArbPoolCacheAdapter(solver=fake_solver)
       pool_id = adapter.register(pool)
       assert pool_id is not None

   def test_adapter_updates_on_state_change():
       pool = FakeV2Pool(reserves_token0=100, reserves_token1=200, fee=Fraction(3, 1000))
       adapter = ArbPoolCacheAdapter(solver=fake_solver)
       adapter.register(pool)
       # Simulate state change
       pool._set_reserves(300, 400)
       adapter.notify(pool, PoolStateMessage(...))
       # Verify solver cache was updated
   ```
2. **Green:** Replace `getattr` chains in `_get_reserves()` and `_get_fee()` with `pool.reserves_for_cache()` and `pool.fee_for_cache()`.
3. Remove `_get_reserves()` and `_get_fee()` static methods.
4. Run all arbitrage tests.

### Phase 4: V3 virtual reserves

1. The V3 `reserves_for_cache()` requires computing virtual reserves from liquidity and sqrt_price. This calculation already exists in `degenbot/uniswap/v3_libraries/functions.py` as `v3_virtual_reserves()`.
2. **Red:** Write test proving `v3_virtual_reserves()` returns a valid pair.
3. **Green:** Implement `UniswapV3Pool.reserves_for_cache()` using the library function.
4. Same for `UniswapV4Pool`.

### Phase 5: Verify and clean up

1. `grep -rn "getattr.*reserves\|getattr.*fee" src/degenbot/arbitrage/` — should return zero.
2. Run `just test-all`.
3. Run `just lint`.

## What Stays the Same

- `ArbPoolCacheAdapter`'s subscribe/notify lifecycle.
- The Rust solver cache integration (`register_pool`, `update_pool`).
- Double-registration (forward + reverse orientation).
- Pool's `to_hop_state()` and `extract_fee()` — different concern, not touched.

## What Changes

| Before | After |
|---|---|
| `_get_reserves(pool: Any)` with `getattr` chain | `pool.reserves_for_cache()` — explicit protocol method |
| `_get_fee(pool: Any)` with `getattr` chain | `pool.fee_for_cache()` — explicit protocol method |
| Adapter silently fails on unknown pool types (returns `None`) | `AttributeError` / `ProtocolError` at registration time — fail fast |
| Adding a pool type requires updating adapter's `getattr` chain | Adding a pool type: implement the protocol methods, no adapter changes |
| 22 lines of introspection per method | 1 line call per method |

## Metrics

| Metric | Before | After |
|---|---|---|
| `getattr` calls in pool_cache_adapter.py | 9 | 0 |
| Lines of introspection logic | ~40 | 0 |
| Pool types silently broken by adapter | Unknown (any pool without matching attrs) | 0 (checked at registration) |
| Protocol methods on pools | 0 | 2 per pool (`reserves_for_cache`, `fee_for_cache`) |
| Real seam? | No (getattr is hypothetical) | Yes (two adapters: protocol + getattr would work) |

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| V3 virtual reserves calculation may not match what the Rust solver expects | The Rust solver cache currently only supports V2/Aerodrome constant-product pools (documented in adapter docstring). V3 support is a future extension — the protocol method exists but the adapter may still skip V3 pools. |
| Adding methods to pool classes increases their interface size | These are two focused methods with clear semantics. They're more discoverable than the `getattr` fallbacks they replace. |
| Existing callers of `_get_reserves` / `_get_fee` static methods | These are private (`_`-prefixed) and only called internally. No external callers. |

## Definition of Done

- [ ] `CacheablePool` protocol defined in `pool_protocols.py`
- [ ] `UniswapV2Pool.reserves_for_cache()` implemented
- [ ] `UniswapV2Pool.fee_for_cache()` implemented
- [ ] `UniswapV3Pool.reserves_for_cache()` implemented (virtual reserves)
- [ ] `UniswapV3Pool.fee_for_cache()` implemented
- [ ] `UniswapV4Pool.reserves_for_cache()` implemented (virtual reserves)
- [ ] `UniswapV4Pool.fee_for_cache()` implemented
- [ ] `AerodromeV2Pool.reserves_for_cache()` implemented
- [ ] `AerodromeV2Pool.fee_for_cache()` implemented
- [ ] `ArbPoolCacheAdapter._get_reserves()` deleted
- [ ] `ArbPoolCacheAdapter._get_fee()` deleted
- [ ] Adapter calls `pool.reserves_for_cache()` and `pool.fee_for_cache()`
- [ ] No `getattr` calls on pool objects in adapter
- [ ] All arbitrage tests pass
- [ ] `just test-all` passes
