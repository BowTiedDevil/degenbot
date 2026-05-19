# Plan 002: Register Pool Classes per DEX Deployment

**Status: COMPLETE** ✅

## Problem

When `Bot.build_v2_pool()` and `Bot.build_v3_pool()` construct pools, they select the concrete subclass via hard-coded dicts embedded inside the method body:

```python
# Inside Bot.build_v2_pool() (bot.py ~line 390)
v2_pool_class_map: dict[tuple[int, str], type[UniswapV2Pool]] = {
    (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"): SushiswapV2Pool,
    (8453, "0x71524B4f93c58fcbF659783284E38825f0622859"): SushiswapV2Pool,
    # ... more entries
}

# Inside Bot.build_v3_pool() (bot.py ~line 790) — removed by Plan 059
v3_pool_class_map: dict[tuple[int, str], type[UniswapV3Pool]] = {
    (1, "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"): SushiswapV3Pool,
    # ... more entries
}
```

Every new DEX deployment or chain requires editing `bot.py`. The knowledge "this factory address uses SushiswapV2Pool" lives in the session class, not in the Sushiswap module.

The deletion test is sharp: removing the class map doesn't eliminate complexity — it just shifts the class-selection burden to every call site that needs to know which subclass to instantiate.

## Solution

Introduce a **PoolClassRegistry** owned by Bot, populated at import time by each DEX module via explicit registration. Builders (from Plan 001) or Bot's `build_*` methods consult this registry instead of embedding dicts.

### New module

```
src/degenbot/registry/
├── ...existing files...
└── pool_class.py    # PoolClassRegistry
```

### Interface

```python
# src/degenbot/registry/pool_class.py

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.types.abstract import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class PoolClassRegistry:
    """
    Maps (chain_id, factory_address) → pool class for each pool invariant family.

    Each DEX module registers its pool subclass at import time.
    Builders consult this registry to select the concrete class.
    """

    def __init__(self) -> None:
        self._v2_classes: dict[tuple[ChainId, str], type[AbstractLiquidityPool]] = {}
        self._v3_classes: dict[tuple[ChainId, str], type[AbstractLiquidityPool]] = {}
        self._default_v2_class: type[AbstractLiquidityPool] | None = None
        self._default_v3_class: type[AbstractLiquidityPool] | None = None

    # --- Registration ---

    def register_v2_pool_class(
        self,
        pool_class: type[AbstractLiquidityPool],
        *,
        chain_id: ChainId,
        factory_address: str,
    ) -> None:
        """Register a V2 pool subclass for a specific (chain_id, factory)."""
        self._v2_classes[(chain_id, factory_address)] = pool_class

    def register_v3_pool_class(
        self,
        pool_class: type[AbstractLiquidityPool],
        *,
        chain_id: ChainId,
        factory_address: str,
    ) -> None:
        """Register a V3 pool subclass for a specific (chain_id, factory)."""
        self._v3_classes[(chain_id, factory_address)] = pool_class

    def set_default_v2_class(self, pool_class: type[AbstractLiquidityPool]) -> None:
        """Set the default V2 pool class when no factory-specific mapping exists."""
        self._default_v2_class = pool_class

    def set_default_v3_class(self, pool_class: type[AbstractLiquidityPool]) -> None:
        """Set the default V3 pool class when no factory-specific mapping exists."""
        self._default_v3_class = pool_class

    # --- Lookup ---

    def get_v2_pool_class(
        self, chain_id: ChainId, factory_address: str
    ) -> type[AbstractLiquidityPool]:
        """Get the V2 pool class for (chain_id, factory), or the default."""
        return self._v2_classes.get(
            (chain_id, factory_address),
            self._default_v2_class,
        )

    def get_v3_pool_class(
        self, chain_id: ChainId, factory_address: str
    ) -> type[AbstractLiquidityPool]:
        """Get the V3 pool class for (chain_id, factory), or the default."""
        return self._v3_classes.get(
            (chain_id, factory_address),
            self._default_v3_class,
        )

    # --- Introspection ---

    @property
    def v2_registrations(self) -> dict[tuple[ChainId, str], type[AbstractLiquidityPool]]:
        return dict(self._v2_classes)

    @property
    def v3_registrations(self) -> dict[tuple[ChainId, str], type[AbstractLiquidityPool]]:
        return dict(self._v3_classes)
```

### Registration sites — each DEX module self-registers

Each DEX module registers its pool classes when imported. The registration lives in the DEX's `__init__.py` or a dedicated `registration.py`:

```python
# src/degenbot/sushiswap/__init__.py (or sushiswap/registration.py)
from degenbot.registry.pool_class import pool_class_registry

from .pools import SushiswapV2Pool, SushiswapV3Pool

# Sushiswap V2 factories
for chain_id, factory in [
    (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
    (8453, "0x71524B4f93c58fcbF659783284E38825f0622859"),
    (42161, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
]:
    pool_class_registry.register_v2_pool_class(
        SushiswapV2Pool, chain_id=chain_id, factory_address=factory
    )

# Sushiswap V3 factories
for chain_id, factory in [
    (1, "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
    (42161, "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"),
]:
    pool_class_registry.register_v3_pool_class(
        SushiswapV3Pool, chain_id=chain_id, factory_address=factory
    )
```

```python
# src/degenbot/pancakeswap/__init__.py
from degenbot.registry.pool_class import pool_class_registry
from .pools import PancakeswapV2Pool, PancakeswapV3Pool

for chain_id, factory in [
    (1, "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
    (8453, "0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"),
]:
    pool_class_registry.register_v2_pool_class(
        PancakeswapV2Pool, chain_id=chain_id, factory_address=factory
    )

for chain_id, factory in [
    (1, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
    (8453, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
]:
    pool_class_registry.register_v3_pool_class(
        PancakeswapV3Pool, chain_id=chain_id, factory_address=factory
    )
```

```python
# src/degenbot/aerodrome/__init__.py
from degenbot.registry.pool_class import pool_class_registry
from .pools import AerodromeV3Pool

pool_class_registry.register_v3_pool_class(
    AerodromeV3Pool,
    chain_id=8453,
    factory_address="0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
)
# Note: AerodromeV2Pool is NOT registered here because it has a different
# constructor signature (stable, fee) and is built directly by AerodromeV2PoolManager,
# not via the V2 class map in Bot.
```

### Default registration

```python
# src/degenbot/uniswap/__init__.py
from degenbot.registry.pool_class import pool_class_registry
from .v2_liquidity_pool import UniswapV2Pool
from .v3_liquidity_pool import UniswapV3Pool

pool_class_registry.set_default_v2_class(UniswapV2Pool)
pool_class_registry.set_default_v3_class(UniswapV3Pool)
```

### Module-level singleton vs Bot-owned instance

Two options:

**Option A: Module-level singleton** — `pool_class_registry` is a module-level instance in `registry/pool_class.py`. Each DEX module registers against it at import time. Simple, zero wiring.

**Option B: Bot-owned instance** — `PoolClassRegistry` is constructed in `Bot.__init__`, and each DEX module's pool manager registers when `add_manager()` is called. More flexible (different Bot instances could have different registrations) but more wiring.

**Recommendation: Option A.** The (chain_id, factory) → class mapping is global knowledge — it doesn't vary between Bot instances. Option A matches the existing pattern for `FACTORY_DEPLOYMENTS` (module-level dict in `uniswap/deployments.py`).

### Consumption in builders / Bot

```python
# Before (inside Bot.build_v2_pool) — removed by Plan 059:
v2_pool_class_map: dict[tuple[int, str], type[UniswapV2Pool]] = {
    (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"): SushiswapV2Pool,
    ...
}
pool_class = v2_pool_class_map.get((chain_id, factory), UniswapV2Pool)

# After (inside V2PoolBuilder.build or Bot.build_v2_pool) — removed by Plan 059:
from degenbot.registry.pool_class import pool_class_registry
pool_class = pool_class_registry.get_v2_pool_class(chain_id, factory)
```

## Implementation steps

### Phase 1: Create PoolClassRegistry

1. Create `src/degenbot/registry/pool_class.py` with the `PoolClassRegistry` class.
2. Create module-level instance: `pool_class_registry = PoolClassRegistry()`.
3. Export from `src/degenbot/registry/__init__.py`.

### Phase 2: Add registration calls to each DEX module

4. Add default registration to `src/degenbot/uniswap/__init__.py` (UniswapV2Pool, UniswapV3Pool defaults).
5. Add Sushiswap registrations to `src/degenbot/sushiswap/__init__.py`.
6. Add Pancakeswap registrations to `src/degenbot/pancakeswap/__init__.py`.
7. Add Aerodrome V3 registration to `src/degenbot/aerodrome/__init__.py`.
   - Note: AerodromeV2Pool is excluded (different constructor signature, built directly by its manager).

### Phase 3: Replace class maps in Bot / builders

8. Replace the `v2_pool_class_map` dict in `Bot.build_v2_pool()` (or `V2PoolBuilder.build()`) — removed by Plan 059 — with `pool_class_registry.get_v2_pool_class(chain_id, factory)`.
9. Replace the `v3_pool_class_map` dict in `Bot.build_v3_pool()` (or `V3PoolBuilder.build()`) — removed by Plan 059 — with `pool_class_registry.get_v3_pool_class(chain_id, factory)`.
10. Remove the now-unused imports from `bot.py` (SushiswapV2Pool, SushiswapV3Pool, PancakeswapV2Pool, PancakeswapV3Pool, AerodromeV3Pool).

### Phase 4: Tests

11. Add `tests/test_pool_class_registry.py`:
    - Test registration and lookup.
    - Test default fallback when no factory-specific registration exists.
    - Test that importing a DEX module populates the registry.
    - Test that `get_v2_pool_class` returns `UniswapV2Pool` for unknown factories.
12. Verify existing `tests/test_pool_subclass_selection.py` still passes.
13. Run `just test-all`.

### Phase 5: Cleanup

14. Remove the `v2_pool_class_map` and `v3_pool_class_map` dicts from `bot.py`.
15. Remove the DEX-specific pool class imports from `bot.py` (they're now only needed in the DEX modules themselves).

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| Lines of class-map code in `bot.py` | ~20 (2 dicts) | 0 |
| DEX subclass imports in `bot.py` | 6 (SushiV2, SushiV3, PancakeV2, PancakeV3, AeroV2, AeroV3) | 0 |
| Places to edit when adding a new DEX | 1 (bot.py) | 1 (the DEX module's `__init__.py`) |
| Where "this factory → this class" knowledge lives | `bot.py` | The DEX module that defines the class |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Registry not populated if DEX module isn't imported | The registration is in `__init__.py` — just `import degenbot.sushiswap` populates it. But the anonymous default path (`UniswapV2Pool`) is always registered as the fallback. If a user forgets to import a DEX module, they'll get the default class — same as the current behavior when a factory isn't in the dict. |
| AerodromeV2Pool can't use the registry | AerodromeV2Pool has a different constructor signature (stable, fee) so it was already excluded from the class map. It's built directly by `AerodromeV2PoolManager`. This is unchanged. |
| Global mutable state makes testing fragile | The registry is append-only in practice (no unregistration). Tests can call `pool_class_registry._reset()` in fixtures. Alternatively, `PoolClassRegistry` could support a `copy()` method for test isolation. |

## Dependencies on other plans

- **Plan 001** (Pool builders): This plan can be done independently. If Plan 001 is also implemented, the registry is consumed by the builders, not directly by Bot. The builder delegates class-selection to the registry.
- No other dependencies.
