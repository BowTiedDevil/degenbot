# Plan 004: Eliminate isinstance Dispatch in Bot.update()

## Problem

`Bot.update()` is a manual type dispatch with 5 `isinstance` checks, each delegating to a private method that knows a specific pool type's on-chain read pattern:

```python
# bot.py lines 1844-1866
def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
    if isinstance(pool, UniswapV2Pool) and not isinstance(pool, AerodromeV2Pool):
        return self._update_v2_pool(pool, provider=provider, block_number=block_number)
    if isinstance(pool, AerodromeV2Pool):
        return self._update_aerodrome_v2_pool(pool, provider=provider, block_number=block_number)
    if isinstance(pool, UniswapV3Pool) and not isinstance(pool, UniswapV4Pool):
        return self._update_v3_pool(pool, provider=provider, block_number=block_number)
    if isinstance(pool, UniswapV4Pool):
        return self._update_v4_pool(pool, provider=provider, block_number=block_number)
    if isinstance(pool, CurveStableswapPool):
        return self._update_curve_pool(pool, provider=provider, block_number=block_number)
    raise TypeError(f"update() not implemented for pool type {type(pool).__name__}")
```

Each `_update_*_pool` method implements the same high-level pattern:
1. Get current block number if not provided
2. Fetch current state from chain (different per pool type)
3. Compare to cached state
4. If changed, construct an external update and push it to the pool

The problem is structural: **update behavior is a property of the pool type, but it's implemented on the session class.** No external caller can add update behavior for a new pool type without editing `bot.py`.

Note also the anti-pattern of `isinstance(pool, UniswapV2Pool) and not isinstance(pool, AerodromeV2Pool)` — this coupling exists because `AerodromeV2Pool` inherits from `UniswapV2Pool` but has a different update type. The negative isinstance is a code smell that reveals the abstraction is leaking.

## Solution

Move the "fetch current state from chain, compare, push update" behavior behind a seam. Two approaches, each building on the pool builder extraction from Plan 001:

### Approach A: Builder-owned update (recommended)

Each builder from Plan 001 already absorbs the `_update_*` method. `Bot.update()` becomes a thin dispatch to the appropriate builder:

```python
# bot.py (after Plan 001 + Plan 004)
def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
    builder = self._builder_for_pool(pool)
    return builder.update(pool, block_number=block_number)
```

The isinstance chain shrinks from 5 branches (one per pool type, with negative isinstance for Aerodrome) to a `_builder_for_pool()` helper that's a simple type→builder mapping:

```python
def _builder_for_pool(self, pool: Any) -> PoolBuilder:
    if isinstance(pool, UniswapV4Pool):
        return self._v4_builder
    if isinstance(pool, UniswapV3Pool):
        return self._v3_builder
    if isinstance(pool, CurveStableswapPool):
        return self._curve_builder
    # V2 pool types (UniswapV2Pool, AerodromeV2Pool, CamelotLiquidityPool, etc.)
    return self._v2_builder
```

This has 3–4 branches instead of 5, and no negative isinstance. Each builder's `update()` method handles its pool family — `V2PoolBuilder.update()` internally branches on whether the pool is `AerodromeV2Pool` or a standard V2:

```python
# v2_pool_builder.py
class V2PoolBuilder(PoolBuilder):
    def update(self, pool: AbstractLiquidityPool, *, block_number: BlockIdentifier | None = None) -> bool:
        if isinstance(pool, AerodromeV2Pool):
            return self._update_aerodrome_v2(pool, block_number=block_number)
        return self._update_uniswap_v2(pool, block_number=block_number)
```

This is smaller in scope and more coherent — the V2 builder knows about V2 pool variants.

### Approach B: Pool-owned update (via protocol)

Add an `UpdatablePool` protocol that pools satisfy:

```python
# degenbot/types/pool_protocols.py (extension)
@runtime_checkable
class UpdatablePool(Protocol):
    """Pool that can refresh its state from on-chain data."""

    def fetch_external_update(
        self,
        provider: ProviderAdapter,
        *,
        w3: Web3 | None = None,
        block_number: int | None = None,
    ) -> object | None:
        """Fetch current state from chain and return an ExternalUpdate, or None if unchanged."""
        ...
```

Each pool type implements `fetch_external_update()` which does the chain reads and returns the appropriate `ExternalUpdate` dataclass (or `None` if unchanged). Then:

```python
# bot.py
def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
    if isinstance(pool, UpdatablePool):
        provider = self.connections.get_provider(pool.chain_id)
        w3 = self.connections.get_web3(pool.chain_id)
        update = pool.fetch_external_update(provider, w3=w3, block_number=block_number)
        if update is not None:
            pool.external_update(update)
            return True
        return False
    raise TypeError(f"update() not implemented for pool type {type(pool).__name__}")
```

**Why Approach A is recommended over B:**

1. **I/O-free architecture (ADR-001)**: Approach B adds a method to the pool that takes a provider — violating the I/O-free principle. ADR-001 explicitly rejected putting provider references in pool classes. The builder sits at the seam between I/O-free pools and I/O-full session, which is the correct location.

2. **Pool already accepts `external_update()`**: The right pattern is: something *outside* the pool fetches the data and pushes the update via `pool.external_update()`. The builder is that "something."

3. **Curve pools don't need provider methods**: CurveStableswapPool uses fetcher callbacks and doesn't have `get_tick_bitmap_at_word()`-style methods. Approach B would need to add chain-read methods to Curve pools too, which is a larger change with no benefit.

4. **Locality**: In Approach A, the update I/O for V3 pools sits in `V3PoolBuilder` alongside the construction I/O. In Approach B, it sits in the pool class, far from construction logic and mixing I/O into the I/O-free pool.

### Approach A in detail

Each builder's `update()` method absorbs the corresponding `_update_*` methods from Bot. The exact implementation per builder:

#### V2PoolBuilder.update()

```python
def update(self, pool: AbstractLiquidityPool, *, block_number: BlockIdentifier | None = None) -> bool:
    if isinstance(pool, AerodromeV2Pool):
        return self._update_aerodrome_v2(pool, block_number=block_number)
    if isinstance(pool, CamelotLiquidityPool):
        # Camelot pools are V2 subclasses with the same update mechanism
        return self._update_uniswap_v2(pool, block_number=block_number)
    return self._update_uniswap_v2(pool, block_number=block_number)

def _update_uniswap_v2(self, pool: UniswapV2Pool, *, block_number: int | None) -> bool:
    provider = self._connections.get_provider(pool.chain_id)
    _block_number = block_number if block_number is not None else provider.get_block_number()
    reserves0, reserves1 = pool.get_reserves(provider, block_identifier=_block_number)
    if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
        return False
    update = UniswapV2PoolExternalUpdate(block_number=_block_number, reserves_token0=reserves0, reserves_token1=reserves1)
    pool.external_update(update)
    return True

def _update_aerodrome_v2(self, pool: AerodromeV2Pool, *, block_number: int | None) -> bool:
    # Same pattern with AerodromeV2PoolExternalUpdate
    ...
```

#### V3PoolBuilder.update()

```python
def update(self, pool: AbstractLiquidityPool, *, block_number: BlockIdentifier | None = None) -> bool:
    if not isinstance(pool, UniswapV3Pool) or isinstance(pool, UniswapV4Pool):
        raise TypeError(f"V3PoolBuilder cannot update {type(pool).__name__}")
    provider = self._connections.get_provider(pool.chain_id)
    _block_number = block_number if block_number is not None else provider.get_block_number()

    slot0_result = provider.call(
        to=pool.address,
        data=encode_function_calldata("slot0()", None),
        block=_block_number,
    )
    sqrt_price_x96, tick, *_ = cast(
        "tuple[int, ...]",
        eth_abi.abi.decode(types=["uint160", "int24", "uint16", "uint16", "uint16"], data=slot0_result),
    )
    (liquidity,) = cast(
        "tuple[int]",
        eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=pool.address,
                data=encode_function_calldata("liquidity()", None),
                block=_block_number,
            ),
        ),
    )

    if pool.sqrt_price_x96 == sqrt_price_x96 and pool.liquidity == liquidity and pool.tick == tick:
        return False

    update = UniswapV3PoolExternalUpdate(
        block_number=_block_number, sqrt_price_x96=sqrt_price_x96, tick=tick, liquidity=liquidity,
    )
    pool.external_update(update)
    return True
```

#### V4PoolBuilder, CurvePoolBuilder

Same pattern — absorb the current `_update_v4_pool` and `_update_curve_pool` logic.

## Implementation steps

### Phase 1: Move update methods into builders (requires Plan 001)

1. Move `Bot._update_v2_pool()` and `Bot._update_aerodrome_v2_pool()` into `V2PoolBuilder._update_uniswap_v2()` and `V2PoolBuilder._update_aerodrome_v2()`.
2. Move `Bot._update_v3_pool()` into `V3PoolBuilder.update()`.
3. Move `Bot._update_v4_pool()` into `V4PoolBuilder.update()`.
4. Move `Bot._update_curve_pool()` into `CurvePoolBuilder.update()`.

### Phase 2: Replace Bot.update() with builder dispatch

5. Replace the 5-branch isinstance chain in `Bot.update()` with a `_builder_for_pool()` dispatch:

```python
def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
    builder = self._builder_for_pool(pool)
    return builder.update(pool, block_number=block_number)

def _builder_for_pool(self, pool: Any) -> PoolBuilder:
    if isinstance(pool, UniswapV4Pool):
        return self._v4_builder
    if isinstance(pool, UniswapV3Pool):
        return self._v3_builder
    if isinstance(pool, CurveStableswapPool):
        return self._curve_builder
    if isinstance(pool, (UniswapV2Pool, AerodromeV2Pool)):
        return self._v2_builder
    raise TypeError(f"update() not implemented for pool type {type(pool).__name__}")
```

### Phase 3: Remove old methods from Bot

6. Remove `Bot._update_v2_pool()`, `Bot._update_aerodrome_v2_pool()`, `Bot._update_v3_pool()`, `Bot._update_v4_pool()`, `Bot._update_curve_pool()`.

### Phase 4: Tests

7. Existing `tests/aerodrome/test_aerodrome_pools.py` calls `bot.update(lp)` — should pass unchanged.
8. Existing `tests/curve/test_curve_stableswap_pool.py` calls `bot.update(tripool)` — should pass unchanged.
9. Add builder-level update tests:
   - `tests/builders/test_v2_pool_builder_update.py` — test with fake provider, fake pool state.
   - Same for V3, V4, Curve.
10. Run `just test-all`.

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| isinstance branches in `Bot.update()` | 5 (including negative isinstance) | 4 (simple type→builder map) |
| Update I/O code location | `bot.py` (session class) | Builders (per-pool-type) |
| Negative isinstance (`not isinstance(pool, AerodromeV2Pool)`) | 1 | 0 |
| Places to edit when adding a new pool type's update | `bot.py` | The pool's builder only |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Builders need provider access for update | Builders already receive `connections: ConnectionManager` from Plan 001. No new wiring. |
| AerodromeV2Pool dispatch is moved into V2PoolBuilder | The builder's `update()` method knows about Aerodrome — this is an internal detail of the V2 builder, not a public interface. |
| `pool._state_mgr.push_state()` is accessed from builders | Same as current code — the fetcher closures in Bot already access `_state_mgr`. This is an internal implementation detail within the package. |

## Dependencies on other plans

- **Plan 001** (Pool builders): This plan is **superseded** by Plan 001 — if Plan 001 is implemented, the `_update_*` methods naturally move into the builders as part of the extraction. This plan describes the shape of that migration and the dispatch simplification.
- **Plan 005** (Curve fetcher factory): The Curve builder from Plan 001 absorbs the update logic described here and the fetcher factories described in Plan 005 — they're all Curve-specific I/O that moves out of `bot.py`.
