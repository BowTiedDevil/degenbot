# Plan 003: Unify V3/V4 Tick Data Fetcher Factories

**Status: COMPLETE** ✅

## Problem

Bot contains two near-identical fetcher factory methods:

- `_make_tick_data_fetcher_v3` (lines 598–645, ~48 lines)
- `_make_tick_data_fetcher_v4` (lines 647–700, ~54 lines)

Both implement the same algorithm:
1. Look up the pool from a registry (PoolRegistry vs ManagedPoolRegistry)
2. Get a provider
3. Fetch the bitmap value at a word position
4. If non-zero, fetch populated ticks in that word
5. Build a new state via `dataclasses.replace()`
6. Push the state via `pool._state_mgr.push_state()`

The only differences are:

| Aspect | V3 | V4 |
|--------|----|----|
| Pool lookup | `self.pools.get(pool_address, chain_id)` | `self.managed_pools.get(chain_id, pool_manager_address, pool_id)` |
| Pool type | `UniswapV3Pool` | `UniswapV4Pool` |
| Bitmap-at-word type | `UniswapV3BitmapAtWord` | `UniswapV4BitmapAtWord` |
| Liquidity-at-tick type | `UniswapV3LiquidityAtTick` | `UniswapV4LiquidityAtTick` |
| `get_tick_bitmap_at_word` call | `pool.get_tick_bitmap_at_word(provider, word_position, block_identifier)` | Same signature |
| `get_populated_ticks_in_word` call | `pool.get_populated_ticks_in_word(provider, word_position, block_identifier)` | Same signature |

This is a shallow split — deleting either method doesn't eliminate complexity; the same algorithm would need to be reconstituted for the deleted variant. The interface is as complex as each implementation.

## Solution

Unify into a single parameterized fetcher factory. The factory takes a pool-lookup callable, the bitmap/tick types, and delegates to the pool's own methods (which already differ correctly between V3 and V4).

### New location

If Plan 001 is implemented: the factory moves into the V3/V4 pool builder module.
If not: it stays in `bot.py` but as a single method.

This plan assumes Plan 001 is implemented, so the factory lives in the builders.

### Interface

```python
# src/degenbot/builders/tick_data_fetcher.py

from collections.abc import Callable
from typing import TYPE_CHECKING, Any

import dataclasses

if TYPE_CHECKING:
    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
    from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
    from degenbot.types.aliases import BlockNumber, ChainId
    from eth_typing import ChecksumAddress


@dataclasses.dataclass(slots=True, frozen=True)
class TickDataTypes:
    """
    Type-params that differ between V3 and V4 tick data fetchers.

    V3 and V4 use the same algorithm but different concrete types for
    the bitmap-at-word and liquidity-at-tick values. This dataclass
    captures those differences so the algorithm can be written once.
    """
    bitmap_at_word: type  # UniswapV3BitmapAtWord or UniswapV4BitmapAtWord
    liquidity_at_tick: type  # UniswapV3LiquidityAtTick or UniswapV4LiquidityAtTick


def make_tick_data_fetcher(
    pool_lookup: Callable[[int], UniswapV3Pool | UniswapV4Pool | None],
    provider_lookup: Callable[[], Any],
    types: TickDataTypes,
) -> Callable[[int, int], None]:
    """
    Create a tick data fetcher callback for a concentrated-liquidity pool.

    Args:
        pool_lookup: Callable that takes a block number and returns the pool
            instance, or None if it's been removed from the registry.
            For V3: `lambda _: self._bot.pools.get(pool_address, chain_id)`
            For V4: `lambda _: self._bot.managed_pools.get(chain_id, pm_addr, pool_id)`
        provider_lookup: Callable that returns the ProviderAdapter.
            For both: `lambda: self._bot.connections.get_provider(chain_id)`
        types: The V3 or V4 specific types for bitmap/tick data.

    Returns:
        A callable `fetcher(word_position: int, block_number: int) -> None`
        that fetches tick bitmap and tick data for the given word position
        and pushes updated state to the pool.
    """

    def fetcher(word_position: int, block_number: int) -> None:
        pool = pool_lookup(block_number)
        if pool is None:
            return

        provider = provider_lookup()
        working_tick_bitmap = dict(pool.tick_bitmap)
        working_tick_data = dict(pool.tick_data)

        try:
            bitmap_value = pool.get_tick_bitmap_at_word(
                provider, word_position=word_position, block_identifier=block_number
            )
        except Exception:
            # If fetching fails (e.g., historical block unavailable),
            # don't update the pool state - let the caller handle the missing word
            return

        working_tick_bitmap[word_position] = types.bitmap_at_word(
            bitmap=bitmap_value, block=block_number
        )

        if bitmap_value != 0:
            populated_ticks = pool.get_populated_ticks_in_word(
                provider, word_position=word_position, block_identifier=block_number
            )
            for tick, liquidity_gross, liquidity_net in populated_ticks:
                working_tick_data[tick] = types.liquidity_at_tick(
                    liquidity_net=liquidity_net,
                    liquidity_gross=liquidity_gross,
                    block=block_number,
                )

        new_state = dataclasses.replace(
            pool.state,
            tick_bitmap=working_tick_bitmap,
            tick_data=working_tick_data,
            block=max(pool.update_block, block_number),
        )
        pool._state_mgr.push_state(new_state)

    return fetcher
```

### Usage in builders

```python
# In V3PoolBuilder.build():
from degenbot.builders.tick_data_fetcher import make_tick_data_fetcher, TickDataTypes
from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick

v3_types = TickDataTypes(
    bitmap_at_word=UniswapV3BitmapAtWord,
    liquidity_at_tick=UniswapV3LiquidityAtTick,
)
tick_fetcher = make_tick_data_fetcher(
    pool_lookup=lambda _: self._pools.get(pool_address=pool_address, chain_id=chain_id),
    provider_lookup=lambda: self._connections.get_provider(chain_id),
    types=v3_types,
)

# In V4PoolBuilder.build():
from degenbot.uniswap.v4_types import UniswapV4BitmapAtWord, UniswapV4LiquidityAtTick

v4_types = TickDataTypes(
    bitmap_at_word=UniswapV4BitmapAtWord,
    liquidity_at_tick=UniswapV4LiquidityAtTick,
)
tick_fetcher = make_tick_data_fetcher(
    pool_lookup=lambda _: self._managed_pools.get(
        chain_id=chain_id,
        pool_manager_address=pool_manager_address,
        pool_id=pool_id_bytes,
    ),
    provider_lookup=lambda: self._connections.get_provider(chain_id),
    types=v4_types,
)
```

### Why not put this on the pool itself?

The fetcher captures registry + provider + types from the Bot/builder scope. Putting `make_tick_data_fetcher` on the pool class would require the pool to know about registries, which contradicts the I/O-free architecture (ADR-001). The builder is the right seam — it sits between the I/O-free pool and the I/O-full session.

## Implementation steps

### Phase 1: Create the unified fetcher module

1. Create `src/degenbot/builders/tick_data_fetcher.py` with `TickDataTypes` and `make_tick_data_fetcher`.

### Phase 2: Replace the two Bot methods

2. In `Bot._make_tick_data_fetcher_v3`, replace the inline implementation with a call to `make_tick_data_fetcher(... v3_types)`.
3. In `Bot._make_tick_data_fetcher_v4`, replace the inline implementation with a call to `make_tick_data_fetcher(... v4_types)`.
4. The two Bot methods become thin wrappers that just wire up the pool-lookup and provider-lookup lambdas. Optionally rename them to `_make_tick_data_fetcher` with a parameter.

### Phase 3: If Plan 001 is implemented

5. Move the thin wrappers into V3PoolBuilder and V4PoolBuilder respectively.
6. Remove the two methods from Bot entirely.

### Phase 4: Tests

7. Add `tests/builders/test_tick_data_fetcher.py`:
   - Create a fake V3 pool with known tick_bitmap and tick_data.
   - Create a fake provider that returns known bitmap values.
   - Call the fetcher with a word position and block number.
   - Assert that the pool's state was updated with the new tick data.
   - Same test with V4 types.
8. Run `just test-all`.

### Phase 5: Cleanup

9. Remove `_make_tick_data_fetcher_v3` and `_make_tick_data_fetcher_v4` from `bot.py`.
10. Remove the `from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick` and `from degenbot.uniswap.v4_types import ...` from `bot.py` if no other usage remains.

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| Lines of fetcher factory code | ~102 (48 + 54) | ~55 (unified) + 2 × ~4 (wiring) = ~63 |
| Duplicate algorithm implementations | 2 | 1 |
| Bug fix surfaces for tick traversal | 2 (must fix both) | 1 (fix once) |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| The two fetchers may diverge over time (V4 gets features V3 doesn't) | The unified version handles this naturally: if V4 needs additional fetcher behavior, the `TickDataTypes` dataclass can be extended with optional callables (e.g., `post_fetch_hook`). The algorithm itself (bitmap→ticks→push state) is unlikely to diverge. |
| `pool._state_mgr.push_state()` is a private attribute | This is already the case in the current code. The fetcher is created by the builder (which is in the `degenbot` package), so accessing `_state_mgr` is acceptable. A future refactor could expose `push_state()` as a package-internal method. |
| V3 and V4 `BitmapAtWord`/`LiquidityAtTick` constructors have different field names | Actually they don't — V4's types inherit directly from V3's with no field changes (`class UniswapV4BitmapAtWord(UniswapV3BitmapAtWord): ...`). The unified code can call either type's constructor with the same kwargs. |

## Dependencies on other plans

- **Plan 001** (Pool builders): This plan can be done independently. If Plan 001 is also done, the unified fetcher naturally lives in the builders directory.
- No other dependencies.
