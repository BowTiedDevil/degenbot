# Plan 028: Builder Registry on Bot

## Status: PENDING

## Overview

Replace the hard-coded builder wiring in `Bot.__init__` and `Bot.build_pool()` with a builder registry keyed by `PoolFamily`. Adding a new pool family currently requires five touch points in `Bot`; this plan reduces that to two: create the builder, register it.

## Files Involved

**Primary:**
- `src/degenbot/bot.py` — remove individual builder attributes and typed `build_*_pool` methods; add `BuilderRegistry` with `register()` and `get()` methods; simplify `build_pool()` dispatch
- `src/degenbot/builders/` — each builder implements a common `PoolBuilder` protocol

**Secondary:**
- `src/degenbot/types/pool_type.py` — `PoolFamily` enum is the registry key (no changes needed)
- `src/degenbot/registry/pool_type.py` — `pool_type_registry` singleton is a model for the builder registry pattern
- `tests/` — update Bot construction tests
- `src/degenbot/CONTEXT-MAP.md` — document builder registry

## Problem

### Deletion test

If you delete the `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool` methods from `Bot`, complexity does NOT vanish — it reappears as callers needing to know which builder to call directly. These methods are pass-throughs, but they earn their keep by providing a single-point entry. The builder pattern is correct; the wiring is just verbose and repetitive.

### Current touch points for a new pool family

1. Create the builder class in `src/degenbot/builders/`
2. Add `self._xxx_builder = XxxPoolBuilder(...)` to `Bot.__init__`
3. Add `def build_xxx_pool(...)` method to `Bot` (pass-through to builder)
4. Add a `case PoolFamily.XXX:` branch in `Bot.build_pool()`
5. Add a `isinstance(pool, XxxPool)` check in `Bot._builder_for_pool()`

Five touch points for one new pool family. This is a linear scaling problem: N pool families → 5N wiring lines in `Bot`.

### Boilerplate in Bot.__init__

```python
# Current: 5 builder constructions, each taking the same 4-5 dependencies
self._erc20_builder = Erc20Builder(
    connections=self.connections, db=self.db, tokens=self.tokens
)
self._v2_builder = V2PoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
    erc20_builder=self._erc20_builder,
)
self._v3_builder = V3PoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
    managed_pools=self.managed_pools, erc20_builder=self._erc20_builder,
)
self._v4_builder = V4PoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
    managed_pools=self.managed_pools, erc20_builder=self._erc20_builder,
)
self._curve_builder = CurvePoolBuilder(
    connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
    erc20_builder=self._erc20_builder,
)
```

Each builder takes a slightly different set of dependencies, but the core set (`connections`, `db`, `pools`, `tokens`, `erc20_builder`) is shared.

## Solution

### Step 1: Define a `PoolBuilder` protocol

```python
# In src/degenbot/builders/protocol.py (new file)
from typing import Protocol, runtime_checkable

from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.pool_type import PoolFamily

@runtime_checkable
class PoolBuilder(Protocol):
    """Protocol for pool builders that can build and update pools."""

    @property
    def family(self) -> PoolFamily:
        """The pool family this builder handles."""
        ...

    def build(self, address: str, **kwargs: object) -> AbstractLiquidityPool:
        """Build a pool from an address, fetching data from chain/DB."""
        ...

    def update(self, pool: AbstractLiquidityPool, **kwargs: object) -> bool:
        """Fetch current state from chain and push update to the pool."""
        ...
```

Each existing builder class gains a `family` class attribute:

```python
class V2PoolBuilder:
    family = PoolFamily.CONSTANT_PRODUCT
    ...

class V3PoolBuilder:
    family = PoolFamily.CONCENTRATED_LIQUIDITY
    ...

class V4PoolBuilder:
    family = PoolFamily.CONCENTRATED_LIQUIDITY
    ...

class CurvePoolBuilder:
    family = PoolFamily.STABLESWAP
    ...
```

Note: V3 and V4 share `PoolFamily.CONCENTRATED_LIQUIDITY`. The registry must handle this: the builder registry maps `PoolFamily` → a *list* of builders (checked in order), or a secondary discriminator is needed. Alternatively, V3 and V4 could be a single builder with a `pool_id` discriminator (matching `Bot.build_pool`'s current `pool_id` fast path).

### Step 2: Handle V3/V4 cohabitation

Two options:

**Option A: Separate builders, ordered list.** The registry maps `PoolFamily.CONCENTRATED_LIQUIDITY` → `[V3PoolBuilder, V4PoolBuilder]`. The `build()` method is called with `pool_id` as a keyword argument; V4PoolBuilder checks for `pool_id` and returns `NotImplemented` or raises if it's not a V4 pool; V3PoolBuilder always attempts to build. This is clunky.

**Option B: Single V3V4 builder.** Merge V3PoolBuilder and V4PoolBuilder into a single builder that handles both. The `pool_id` parameter discriminates V3 from V4 (as it does in `build_pool()` today). The builder internally delegates to V3 or V4 construction paths.

**Option C: Keep dispatch outside the registry.** The registry maps `PoolFamily` → builder, but V4 is handled separately via the `pool_id` fast path in `build_pool()` (already exists). The registry only maps `CONSTANT_PRODUCT` → V2, `CONCENTRATED_LIQUIDITY` → V3, `STABLESWAP` → Curve. V4 bypasses the registry entirely because it's identified by `pool_id`, not by family.

**Recommendation: Option C** — simplest, matches the current architecture where `build_pool()` has a V4 fast path before the registry/family dispatch. The builder registry handles the three main families (V2, V3, Curve). V4 dispatch remains in `build_pool()` as a special case routed by `pool_id`.

### Step 3: Create `BuilderRegistry` on Bot

```python
class Bot:
    def __init__(self, config: DegenbotConfig) -> None:
        # ... existing initialization (connections, db, registries) ...

        self._erc20_builder = Erc20Builder(
            connections=self.connections, db=self.db, tokens=self.tokens
        )

        # Builder registry
        self._builders: dict[PoolFamily, PoolBuilder] = {}

        # Register builders
        self._register_builder(V2PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        ))
        self._register_builder(V3PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            managed_pools=self.managed_pools, erc20_builder=self._erc20_builder,
        ))
        self._register_builder(CurvePoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        ))

        # V4 builder kept separate (pool_id-driven dispatch)
        self._v4_builder = V4PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            managed_pools=self.managed_pools, erc20_builder=self._erc20_builder,
        )

    def _register_builder(self, builder: PoolBuilder) -> None:
        self._builders[builder.family] = builder

    def _builder_for_pool(self, pool: AbstractLiquidityPool) -> PoolBuilder:
        """Select the appropriate builder for the pool type."""
        if isinstance(pool, UniswapV4Pool):
            return self._v4_builder

        # Try family-based lookup
        for family, builder in self._builders.items():
            if isinstance(pool, builder.pool_class):
                return builder

        msg = f"update() not implemented for pool type {type(pool).__name__}"
        raise TypeError(msg)
```

### Step 4: Simplify `build_pool()`

```python
def build_pool(self, address: str, *, pool_id: str | bytes | None = None, **kwargs) -> AbstractLiquidityPool:
    address = get_checksum_address(address)
    chain_id = kwargs.get('chain_id') or self.connections.default_chain_id

    # V4 fast path (unchanged)
    if pool_id is not None:
        return self._v4_builder.build(pool_id=pool_id, pool_manager_address=address, **kwargs)

    # Check registry for existing pool
    existing = self.pools.get(chain_id=chain_id, pool_address=address)
    if existing is not None:
        return existing

    # Resolve pool type
    pool_type = self._resolve_pool_type(address, chain_id=chain_id)

    # Dispatch via builder registry
    builder = self._builders.get(pool_type.family)
    if builder is None:
        raise DegenbotValueError(message=f"No builder for pool family {pool_type.family.value!r}")

    return builder.build(address, chain_id=chain_id, **kwargs)
```

### Step 5: Remove `build_v2_pool`, `build_v3_pool`, `build_curve_pool` pass-through methods

These become convenience methods that delegate to the registry. They can be deprecated immediately and removed in a future release, or kept as thin wrappers for API stability.

**Recommendation:** Keep as thin wrappers for one release cycle with deprecation warnings, then remove. This preserves the public API while encouraging callers to use `build_pool()`.

### Step 6: Store builder reference on pool for `update()` dispatch

Instead of `_builder_for_pool()` doing `isinstance` checks, each pool can carry a reference to its builder:

```python
class AbstractLiquidityPool:
    _builder: PoolBuilder | None = None  # Set by builder after construction
```

The builder sets `pool._builder = self` after construction. `Bot.update()` uses `pool._builder` directly:

```python
def update(self, pool: AbstractLiquidityPool, **kwargs) -> bool:
    builder = pool._builder
    if builder is None:
        msg = f"No builder registered for pool {pool.address}"
        raise TypeError(msg)
    return builder.update(pool, **kwargs)
```

This eliminates all `isinstance` dispatch in the update path. The `Erc20Token` class doesn't have a builder reference (it uses `Erc20Builder` which is always available), so `_builder` defaults to `None`.

## Implementation Order

1. **Define `PoolBuilder` protocol** in `src/degenbot/builders/protocol.py` — no behaviour change
2. **Add `family` class attribute** to each builder — no behaviour change
3. **Create `_builders` dict on Bot** alongside existing builder attributes — backwards-compatible, both paths work
4. **Update `build_pool()` to dispatch via registry** — remove `match` statement, use `_builders.get(family)`
5. **Add `_builder` attribute to pools** — set by builders after construction
6. **Update `Bot.update()`** to use `pool._builder` — remove `_builder_for_pool()` isinstance chain
7. **Remove individual builder attributes** from `Bot.__init__` — use `_builders` dict exclusively
8. **Deprecate convenience `build_*_pool` methods** — thin wrappers for one release
9. **Update tests**
10. **Update `CONTEXT-MAP.md`** with builder registry documentation

## Testing

### Unit tests

```python
def test_builder_registry_dispatch():
    bot = Bot.from_config_file()
    pool = bot.build_pool(UNISWAP_V2_POOL_ADDRESS)
    assert isinstance(pool, UniswapV2Pool)
    assert pool._builder is bot._builders[PoolFamily.CONSTANT_PRODUCT]

def test_builder_update_dispatch():
    bot = Bot.from_config_file()
    pool = bot.build_pool(UNISWAP_V2_POOL_ADDRESS)
    updated = bot.update(pool, block_number=some_block)
    # No isinstance chain used — pool._builder dispatches directly
    assert isinstance(updated, bool)

def test_builder_registry_unknown_family():
    bot = Bot.from_config_file()
    with pytest.raises(DegenbotValueError, match="No builder"):
        bot._builders.get(PoolFamily.WEIGHTED)  # Not registered
```

### Integration tests

- All existing `test_build_pool` tests pass unchanged (the public API doesn't change)
- `test_bot_update` tests pass with the new dispatch path

## Benefits

- **Locality:** Adding a pool family requires creating the builder class and registering it in `Bot.__init__`. Two touch points instead of five. No changes to `build_pool()`, `_builder_for_pool()`, or convenience methods.
- **Leverage:** The `PoolBuilder` protocol is the same interface for all builders. Bot only needs to know this interface to dispatch. Each builder's `build()` and `update()` methods are the seam.
- **Simpler update path:** `pool._builder` eliminates the `isinstance` chain in `_builder_for_pool()`. The pool knows which builder created it.

## Risks

- **V3/V4 cohabitation:** V3 and V4 share `PoolFamily.CONCENTRATED_LIQUIDITY`. If both are in the registry under the same key, `dict` semantics mean one wins. Option C (V4 outside the registry) avoids this by keeping V4 dispatch in `build_pool()` where `pool_id` provides the discriminator.
- **`_builder` attribute on pool:** Adding mutable state to pools (`_builder` reference) is a minor deviation from the I/O-free principle. The builder reference is not I/O — it's a dispatch hint. It could be a `WeakReference` to avoid circular references. Alternatively, the `update()` dispatch could remain `isinstance`-based if the pool reference is considered too coupled.
- **Public API stability:** Removing `build_v2_pool()`, `build_v3_pool()`, `build_curve_pool()` is a breaking change for callers who use these methods directly. The deprecation cycle mitigates this.

## Relationship to Other Plans

- **Plan 001** (Pool Builders): Complete. This plan builds on the builder extraction from Plan 001. The builders exist; this plan just changes how Bot wires them.
- **Plan 006** (Universal `build_pool`): Complete. This plan simplifies the dispatch inside `build_pool()` without changing its interface.
- **Plan 026** (Strategy Objects): Independent. The builder registry and strategy objects are orthogonal improvements.
