# Context — Pool Registries

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Registry** | A class indexing (chain ID, pool address) → Pool across all DEX protocols; instances owned by Bot | Pool index, pool cache |
| **Token Registry** | A class indexing (chain ID, token address) → Token across all DEX protocols; instances owned by Bot | Token index, token cache |
| **Managed Pool Registry** | A sub-registry for V4-style singleton architecture pools, keyed by (chain ID, PoolManager address, Pool ID); instance owned by Bot | V4 registry |
| **Pool Type Registry** | A module-level singleton that maps (chain ID, factory address) → pool class + identity + deployment data in a single registration; replaces the old Pool Class Registry, FACTORY_DEPLOYMENTS lookups, _KIND_TO_DESCRIPTOR, and _variant_from_class | Pool class registry, type registry |

## Relationships

- A **Pool Registry** indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Pools** by (chain ID, PoolManager address, Pool ID)
- A **Pool Type Registry** provides the (chain ID, factory) → class mapping used by Bot's type resolver to select the concrete pool subclass; each DEX module self-registers at import time
- The **Pool Type Registry** holds a default V2 class and a default V3 class (both `UniswapV2Pool` / `UniswapV3Pool`) as fallbacks for unrecognized factories
- All registry instances except **Pool Type Registry** are owned by **Bot** — the **Pool Type Registry** is a module-level singleton because the factory→class mapping is global knowledge that does not vary between Bot instances
- Each DEX module (uniswap, sushiswap, pancakeswap, aerodrome, camelot, swapbased) self-registers its pool classes in its `__init__.py` against the **Pool Type Registry** via `pool_type_registry.register()`
- The **Pool Type Registry** auto-derives pool identity from the class hierarchy (`PoolFamily`) and class attribute (`variant`), producing the `kind` string (e.g. `"sushiswap_v2"`) used for DB polymorphic identity
- The **Pool Type Registry** stores deployment data (deployer, pool_init_hash) alongside the class, so adding a new DEX requires only one registration call
- Pool classes with non-standard constructors (e.g. Camelot's `stableSwap`/`fee_denominator`, AerodromeV2's `stable`/`fee`) previously provided a `from_chain` classmethod that `build_v2_pool` delegated to; this is being removed — builders handle variant-specific I/O directly (Plan 017)

## Resolved Ambiguities

### Registry vs Manager

**Ruling: **Registry** = passive index. **Manager** = active controller.**

A Registry is a simple lookup table. A Pool Manager (e.g., `UniswapV2PoolManager`, `CurveStableswapPoolManager`) actively discovers, creates, and tracks pools for a specific DEX. Registries hold all pools; managers hold a subset for one DEX.

### Bot-owned registry vs module-level Pool Type Registry

**Ruling: Bot-owned registries hold *instances*. The Pool Type Registry holds *class mappings + deployment data*.**

The Pool, Token, and Managed Pool registries index live pool/token *objects* within a Bot session. The Pool Type Registry maps factory addresses to *classes* plus identity and deployment data — this mapping is static, global, and independent of any Bot instance. It replaces the old `PoolClassRegistry` (class-only), `FACTORY_DEPLOYMENTS` lookups (deployment data), `_KIND_TO_DESCRIPTOR` (DB kind → descriptor), and `_variant_from_class()` (class → variant string).

- ✅ "The **Pool Registry** contains the UniswapV2Pool instance for 0xBb2b…"
- ✅ "The **Pool Type Registry** maps SushiSwap's factory to the `SushiswapV2Pool` class, with variant `sushiswap` and kind `sushiswap_v2`"
- ❌ "The Pool Registry maps the factory to the class" (that's the **Pool Type Registry**)

### Pool Class Registry (deprecated) vs Pool Type Registry

**Ruling: Use **Pool Type Registry** always. The Pool Class Registry has been removed.**

The old `PoolClassRegistry` (`pool_class_registry`) mapped (chain_id, factory) → class only, with deployment data in a separate `FACTORY_DEPLOYMENTS` dict and variant/kind mappings scattered across `_variant_from_class()` and `_KIND_TO_DESCRIPTOR`. The `PoolTypeRegistry` (`pool_type_registry`) consolidates all of these into a single registration:

```python
pool_type_registry.register(
    SushiswapV2Pool,
    chain_id=1,
    factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
    pool_init_hash="0xe18a34eb0e7...",
)
```

- ✅ "Register the pool class with `pool_type_registry.register()`"
- ❌ "Register with `pool_class_registry`" (removed)
