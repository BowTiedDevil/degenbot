# Plan 017: Complete I/O-Free Migration for V2/V3/V4/Aerodrome Pools

**Status: COMPLETE**

## Overview

Remove all `ProviderAdapter`-taking methods from Uniswap V2/V3/V4 and Aerodrome pool classes. Replace `get_reserves()`, `get_immutable_pool_values()`, `get_mutable_pool_values()`, `get_tick_bitmap_at_word()`, and `get_populated_ticks_in_word()` with fetcher callback injection following the same pattern established by ADR-001 for Curve pools. Move `from_chain` classmethod I/O into the builders. Makes the I/O-free seam complete for all pool types.

## Files Involved

**Existing:**
- `src/degenbot/uniswap/v2_liquidity_pool.py` (~765 lines)
- `src/degenbot/uniswap/v3_liquidity_pool.py` (~1275 lines)
- `src/degenbot/uniswap/v4_liquidity_pool.py` (~939 lines)
- `src/degenbot/aerodrome/pools.py` (~726 lines)
- `src/degenbot/camelot/pools.py`
- `src/degenbot/builders/v2_pool_builder.py`
- `src/degenbot/builders/v3_pool_builder.py`
- `src/degenbot/builders/v4_pool_builder.py`

**Modified:**
- All pool classes above — remove provider-dependent methods, add fetcher parameters
- All builders — absorb I/O from `from_chain`, create fetcher closures for update path

**Tests:**
- `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py`
- `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py`
- All builder tests

## Problem

ADR-001 defines the I/O-free architecture and marks Phase 3 as "repeat for Uniswap V2/V3/V4, Aerodrome, etc." The Curve pools are fully migrated. But V2/V3/V4/Aerodrome pools are only half-migrated:

- **Construction** is I/O-free — builders fetch data from DB/RPC, then pass values to the pool constructor.
- **Update path** is NOT I/O-free — `V2PoolBuilder._update_uniswap_v2()` calls `pool.get_reserves(provider, block)` which takes a `ProviderAdapter`. The pool object reaches back through the provider to fetch its own state.
- **`from_chain` classmethods** on `AerodromeV2Pool` and `CamelotLiquidityPool` receive a `ProviderAdapter` and perform I/O inside the pool class.
- **Helper methods** like `get_immutable_pool_values()`, `get_mutable_pool_values()`, `get_tick_bitmap_at_word()`, `get_populated_ticks_in_word()` all take `ProviderAdapter` — they're I/O inside a class that should be I/O-free.

This split creates real problems:

1. **Tests can't be pure unit tests.** Any test exercising the update path must provide a real or mock `ProviderAdapter`.
2. **The pool's I/O-free claim is false advertising.** You can construct a V2 pool from pure data, but you can't update it without a provider — the same object has one foot in each world.
3. **Builder update methods couple to pool internals.** `V2PoolBuilder._update_uniswap_v2()` knows to call `pool.get_reserves(provider, ...)` — the I/O seam is at the builder, but the pool silently participates by providing the method.

## Solution

### Deletion test

If we delete `get_reserves()`, `get_immutable_pool_values()`, etc., complexity does NOT vanish — it reappears in the builder, which must now construct the same RPC calls itself. But the builder already does this. The pool methods are pass-throughs that let the pool participate in I/O it shouldn't know about. The real depth comes from having the builder own the full I/O choreography.

### Fetcher pattern for each pool type

**V2 pools** — single fetcher for reserves:

```python
# New fetcher protocol in degenbot/uniswap/v2_types.py
class ReservesFetcher(Protocol):
    def __call__(self, *, block_identifier: int) -> tuple[int, int]: ...

# Pool constructor gains optional fetcher
class UniswapV2Pool:
    def __init__(
        self,
        ...,
        reserves_fetcher: ReservesFetcher | None = None,
    ) -> None:
        self._reserves_fetcher = reserves_fetcher

# Builder creates the fetcher closure
class V2PoolBuilder:
    def build(self, pool_address, ...):
        chain_id = ...
        provider = self._connections.get_provider(chain_id)

        def reserves_fetcher(*, block_identifier: int) -> tuple[int, int]:
            return raw_call(
                provider,
                address=pool_address,
                calldata=encode_function_calldata("getReserves()", None),
                return_types=["uint256", "uint256"],
                block_identifier=block_identifier,
            )

        pool = UniswapV2Pool(
            ...,
            reserves_fetcher=reserves_fetcher,
        )
```

**V3 pools** — fetcher for slot0+liquidity and tick data:

```python
# New fetcher protocols in degenbot/uniswap/v3_types.py
class Slot0Fetcher(Protocol):
    def __call__(self, *, block_identifier: int) -> tuple[int, int, int]: ...
    # Returns (sqrt_price_x96, tick, liquidity)

# Pool constructor gains optional fetcher
class UniswapV3Pool:
    def __init__(
        self,
        ...,
        slot0_fetcher: Slot0Fetcher | None = None,
    ) -> None:
        self._slot0_fetcher = slot0_fetcher
```

**V4 pools** — same pattern with V4-specific fetchers.

**Aerodrome V2 pools** — `from_chain` classmethod moves into `V2PoolBuilder.build()`. The builder already has the provider; it fetches `stable()` and `getFee()` and passes them as constructor arguments. The `from_chain` classmethod is deleted.

### Methods to remove

| Pool Class | Method | Current Signature | Replacement |
|---|---|---|---|
| `UniswapV2Pool` | `get_reserves(provider, block_identifier)` | Returns reserves via RPC | `reserves_fetcher` callback |
| `UniswapV2Pool` | `get_immutable_pool_values(provider)` | Returns factory, tokens via RPC | Remove entirely (builders fetch these) |
| `UniswapV3Pool` | `get_immutable_pool_values(provider)` | Returns factory, tokens, fee, tick_spacing via RPC | Remove entirely |
| `UniswapV3Pool` | `get_mutable_pool_values(provider, state_block)` | Returns slot0, liquidity via RPC | `slot0_fetcher` callback |
| `UniswapV3Pool` | `get_tick_bitmap_at_word(provider, word_position, block_identifier)` | Returns bitmap via RPC | `tick_data_fetcher` (already exists) |
| `UniswapV3Pool` | `get_populated_ticks_in_word(provider, ...)` | Returns tick data via RPC | `tick_data_fetcher` (already exists) |
| `UniswapV4Pool` | Same V3-like methods | Same | Same fetcher pattern |
| `AerodromeV2Pool` | `from_chain(cls, ..., provider)` | Classmethod doing I/O | Move I/O to builder, delete classmethod |
| `CamelotLiquidityPool` | `from_chain(cls, ..., provider)` | Classmethod doing I/O | Move I/O to builder, delete classmethod |

### Update path after migration

```python
# BEFORE: builder calls pool method (pool does I/O)
class V2PoolBuilder:
    def _update_uniswap_v2(self, pool, *, block_number):
        provider = self._connections.get_provider(pool.chain_id)
        reserves0, reserves1 = pool.get_reserves(provider, block_identifier=block_number)
        # ... construct ExternalUpdate, call pool.external_update()


# AFTER: builder calls fetcher, pushes update (pool is pure)
class V2PoolBuilder:
    def _update_uniswap_v2(self, pool, *, block_number):
        if pool._reserves_fetcher is None:
            raise LiquidityPoolError(message="Cannot update pool: no reserves_fetcher injected")
        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number or provider.get_block_number()
        reserves0, reserves1 = pool._reserves_fetcher(block_identifier=_block_number)
        # ... construct ExternalUpdate, call pool.external_update()
```

Wait — if the fetcher already captures the provider, the builder doesn't need to get the provider at all. The fetcher IS the I/O. The builder just calls it:

```python
# AFTER (cleaner): builder uses the pool's fetcher directly
class V2PoolBuilder:
    def _update_uniswap_v2(self, pool, *, block_number):
        if pool._reserves_fetcher is None:
            raise LiquidityPoolError(message="Cannot update pool: no reserves_fetcher injected")
        _block_number = (
            block_number or self._connections.get_provider(pool.chain_id).get_block_number()
        )
        reserves0, reserves1 = pool._reserves_fetcher(block_identifier=_block_number)

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = UniswapV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True
```

Or even simpler — the builder can create its OWN fetcher at update time instead of relying on the pool's, since the builder already has `connections`:

```python
class V2PoolBuilder:
    def _update_uniswap_v2(self, pool, *, block_number):
        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number or provider.get_block_number()
        reserves0, reserves1 = raw_call(
            provider,
            address=pool.address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=_block_number,
        )
        # ... rest unchanged
```

This is actually what the builder already does (inlined). The key question: **should the fetcher live on the pool or on the builder?**

For **construction-time fetchers** (Curve style), the fetcher lives on the pool — the pool calls it on-demand internally (e.g., `self._virtual_price_fetcher(block)`). This makes sense when the pool's internal logic needs on-demand I/O.

For **update-time fetchers**, the builder is the I/O orchestrator — it calls the provider, constructs the update, and pushes it to the pool via `external_update()`. The pool doesn't need a fetcher for this; the builder does the I/O itself.

**Decision:** Store fetchers on the pool only when the pool's *internal logic* calls them (like Curve's rate fetchers). For the update path, the builder does I/O directly — no fetcher needed on the pool. Remove `get_reserves()` from the pool and let the builder call the provider directly (as it already does in the current code).

This means:
- `get_reserves()` is deleted from `UniswapV2Pool`. The builder already calls `raw_call()` directly.
- `get_immutable_pool_values()` and `get_mutable_pool_values()` are deleted. Only the builder uses them.
- `get_tick_bitmap_at_word()` and `get_populated_ticks_in_word()` are deleted. The V3 builder doesn't use these for the update path (it just fetches `slot0()` and `liquidity()`).
- `from_chain` classmethods' I/O moves into the builder. The builder already has the code for most of this; `from_chain` was a shortcut that predated the builder pattern.

### Pickle implications

The `PoolPickleMixin` already handles unpicklable closures (like the thread lock and subscriber set). Fetcher closures are similarly unpicklable. The existing `_pickle_drops` / `_pickle_reconstructs` pattern from Curve pools should be replicated:

```python
class UniswapV2Pool(PoolPickleMixin, ...):
    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
        # No fetchers on V2 pool — deletions are sufficient
    })
```

V2 pools don't need fetcher drops since we're not storing fetchers on them. But if future internal fetchers are added (e.g., a reserves fetcher the pool calls during simulation), they'd need the same treatment.

## Implementation Steps

### Phase 1: V2 pools (TDD)

1. **Red:** Write tests proving `get_reserves()` and `get_immutable_pool_values()` methods exist and work.
2. **Green:** Verify existing tests pass.
3. **Red:** Write tests for `V2PoolBuilder._update_uniswap_v2()` that call `raw_call()` directly instead of `pool.get_reserves(provider, ...)`.
4. **Green:** Modify `V2PoolBuilder._update_uniswap_v2()` to call `raw_call()` directly (it already does this, but via `pool.get_reserves()`).
5. **Red:** Write tests confirming `get_reserves()` is no longer callable on `UniswapV2Pool`.
6. **Green:** Delete `get_reserves()` from `UniswapV2Pool`.
7. **Green:** Delete `get_immutable_pool_values()` from `UniswapV2Pool`.
8. Run all V2 tests.

### Phase 2: Aerodrome V2 pools

1. **Red:** Write tests for `V2PoolBuilder.build()` that fetch `stable()` and `getFee()` via provider directly, not via `AerodromeV2Pool.from_chain()`.
2. **Green:** Move the `from_chain` I/O into `V2PoolBuilder.build()` for the Aerodrome case:
   ```python
   # In V2PoolBuilder.build(), after resolving pool_class:
   if issubclass(pool_class, AerodromeV2Pool):
       stable_result = provider.call(
           to=pool_address,
           data=encode_function_calldata("stable()", None),
           block=state_block,
       )
       (stable,) = eth_abi.abi.decode(types=["bool"], data=stable_result)
       fee_result = provider.call(
           to=factory,
           data=encode_function_calldata("getFee(address,bool)", [pool_address, stable]),
           block=state_block,
       )
       (fee_raw,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)
       fee = Fraction(fee_raw, AerodromeV2Pool.FEE_DENOMINATOR)
       pool = pool_class(
           address=pool_address,
           token0=token0,
           token1=token1,
           factory=factory,
           fee=fee,
           stable=stable,
           reserves_token0=reserves0,
           reserves_token1=reserves1,
           chain_id=chain_id,
           deployer_address=deployer,
           state_block=state_block,
       )
   ```
3. **Green:** Delete `AerodromeV2Pool.from_chain()`.
4. Run all Aerodrome tests.

### Phase 3: Camelot pools

1. Same approach as Phase 2 — move `CamelotLiquidityPool.from_chain()` I/O into `V2PoolBuilder.build()`.
2. Delete `CamelotLiquidityPool.from_chain()`.
3. Run all Camelot tests.

### Phase 4: V3 pools

1. **Red:** Write tests proving `get_immutable_pool_values()`, `get_mutable_pool_values()`, `get_tick_bitmap_at_word()`, `get_populated_ticks_in_word()` exist.
2. **Green:** Verify existing tests pass.
3. **Green:** Verify `V3PoolBuilder.update()` does NOT call any of these methods — it should already fetch `slot0()` and `liquidity()` directly via provider.
4. **Red:** Write tests confirming these methods are no longer callable.
5. **Green:** Delete all four methods from `UniswapV3Pool`.
6. Delete the `ProviderAdapter` import from `v3_liquidity_pool.py`.
7. Run all V3 tests.

### Phase 5: V4 pools

1. Same approach as Phase 4.
2. Delete any `ProviderAdapter`-taking methods from `UniswapV4Pool`.
3. Run all V4 tests.

### Phase 6: Cleanup and verification

1. `grep -rn "ProviderAdapter" src/degenbot/uniswap/` — should find zero matches in pool classes.
2. `grep -rn "from_chain" src/degenbot/` — should find zero matches on pool classes (only builder references).
3. `grep -rn "get_reserves\|get_immutable_pool_values\|get_mutable_pool_values\|get_tick_bitmap_at_word\|get_populated_ticks_in_word" src/degenbot/uniswap/ src/degenbot/aerodrome/` — should find zero matches.
4. Run `just test-all`.
5. Run `just lint`.

## What Stays the Same

- `external_update()` on all pool classes — this is the inbound update path, pure logic.
- `simulate_swap()`, `calculate_tokens_out_from_tokens_in()`, etc. — pure logic, no I/O.
- The `tick_data_fetcher` callback on V3/V4 pools — already I/O-free (created by builder, stores closure).
- Builder classes' public API — same `build()` and `update()` signatures.
- Bot's public API — same `build_v2_pool()`, `build_v3_pool()`, etc.

## What Changes

| Before | After |
|---|---|
| `pool.get_reserves(provider, block)` | Deleted; builder calls `raw_call()` directly |
| `pool.get_immutable_pool_values(provider)` | Deleted; builder fetches this data during `build()` |
| `pool.get_mutable_pool_values(provider, block)` | Deleted; builder fetches this data during `build()` |
| `pool.get_tick_bitmap_at_word(provider, ...)` | Deleted; builder fetches tick bitmap during `build()` |
| `pool.get_populated_ticks_in_word(provider, ...)` | Deleted; builder fetches tick data during `build()` |
| `AerodromeV2Pool.from_chain(..., provider)` | Deleted; builder fetches `stable` + `fee` from chain |
| `CamelotLiquidityPool.from_chain(..., provider)` | Deleted; builder fetches variant-specific data from chain |
| `V2PoolBuilder` checks `hasattr(pool_class, "from_chain")` | Removed; builder handles Aerodrome/Camelot explicitly |

## Metrics

| Metric | Before | After |
|---|---|---|
| Pool classes importing `ProviderAdapter` | 5 (V2, V3, V4, AerodromeV2, Camelot) | 0 |
| `get_reserves`-style methods on pool classes | 6 | 0 |
| `from_chain` classmethods on pool classes | 2 | 0 |
| I/O-free pool types | 1 (Curve) | 6 (Curve, V2, V3, V4, AerodromeV2, Camelot) |
| ADR-001 Phase 3 completion | 0% | 100% |

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| External callers of `pool.get_reserves()` | `grep` for all call sites before deletion. Current code shows only builders call these methods. |
| External callers of `from_chain()` | Same grep approach. The `hasattr(pool_class, "from_chain")` dispatch in the builder is the only caller. |
| Losing convenience of `from_chain` for quick scripts | Users should call `bot.build_v2_pool()` instead — same result, full I/O orchestration. |
| V3 builder update path already uses direct RPC | Verify this — if `V3PoolBuilder.update()` already fetches `slot0()` directly, deletion is safe. |
| Pickle: no fetchers on V2 pools means no `_pickle_drops` changes needed | Correct — V2 pools don't need the Curve-style fetcher drops pattern. |

## Dependencies on Other Plans

- **Plan 001** (Pool Builders) ✅ — foundational. The builders already exist and own the I/O orchestration. This plan removes residual I/O from the pool classes.
- **Plan 003** (Unified tick fetcher) ✅ — the `tick_data_fetcher` callback is the V3/V4 equivalent of Curve's fetcher pattern. Already implemented.
- **Plan 013** (Curve I/O-free) ✅ — established the fetcher pattern that this plan generalizes.

## Definition of Done

- [x] `UniswapV2Pool.get_reserves()` deleted
- [x] `UniswapV2Pool.get_immutable_pool_values()` deleted
- [x] `UniswapV3Pool.get_immutable_pool_values()` deleted
- [x] `UniswapV3Pool.get_mutable_pool_values()` deleted
- [x] `UniswapV3Pool.get_tick_bitmap_at_word()` deleted
- [x] `UniswapV3Pool.get_populated_ticks_in_word()` deleted
- [x] `UniswapV4Pool` same V3-like methods deleted
- [x] `AerodromeV2Pool.from_chain()` deleted
- [x] `CamelotLiquidityPool.from_chain()` deleted
- [x] No pool class imports `ProviderAdapter`
- [x] All builders handle variant-specific I/O directly
- [x] `V2PoolBuilder` no longer checks `hasattr(pool_class, "from_chain")`
- [x] All V2/V3/V4/Aerodrome tests pass
- [x] `grep -rn "ProviderAdapter" src/degenbot/uniswap/` returns zero matches in pool classes
- [x] `just test-all` passes
- [x] ADR-001 Phase 3 marked complete
