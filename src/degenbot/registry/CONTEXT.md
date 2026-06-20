# Context — Pool Registries

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Registry** | A class indexing (chain ID, pool address) → Pool across all DEX protocols; instances owned by Bot | Pool index, pool cache |
| **Token Registry** | A class indexing (chain ID, token address) → Token across all DEX protocols; instances owned by Bot | Token index, token cache |
| **Managed Pool Registry** | A sub-registry for V4 **Managed Pools**, keyed by (chain ID, PoolManager address, Pool ID); instance owned by Bot; see [Managed Pool](../uniswap/CONTEXT.md) | V4 registry |
| **Pool Type Registry** | A module-level singleton mapping (chain ID, factory address) → pool class + identity + deployment data | Pool class registry, type registry |
| **AbstractAddressRegistry** | An abstract generic base class for all address-based registries | Registry base, generic registry |
| **AddressRegistry** | A concrete generic class for single-address-keyed registries | Single-key registry |
| **MultiKeyAddressRegistry** | A concrete generic class for multi-field-keyed registries | Multi-key registry |

## Relationships

- A **Pool Registry** indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Pools** by (chain ID, PoolManager address, Pool ID)
- A **Pool Type Registry** maps factory addresses to pool classes; each DEX module self-registers at import time
- All registry instances except **Pool Type Registry** are owned by **Bot**; the **Pool Type Registry** is a module-level singleton

## Removed terms

- **`pool_registry` / `token_registry` / `managed_pool_registry`** (Removed under ADR-006 slice 8b): formerly module-level singletons; now Bot-owned class instances `PoolRegistry` / `TokenRegistry` / `ManagedPoolRegistry`. See CONTEXT-MAP.md ambiguity #4.
- **`Pool Class Registry`** (Removed, superseded by **Pool Type Registry**): former factory→class-only map with deployment data scattered elsewhere; consolidated into the single `pool_type_registry.register()` call.
- **`FACTORY_DEPLOYMENTS` dict** (Removed, superseded by **Pool Type Registry**): former `(chain_id, factory) → (deployer, pool_init_hash)` lookup; each DEX `__init__.py` now passes hardcoded values directly to `pool_type_registry.register()`, and trackers resolve via `pool_type_registry.get_deployment()`.

## Resolved Ambiguities

### Registry vs Manager

**Ruling: **Registry** = passive index. **Manager** = active controller.**

A Registry is a simple lookup table. A Pool Tracker (e.g., `UniswapV2PoolTracker`, `CurveStableswapPoolTracker`) actively discovers, creates, and tracks pools for a specific DEX. Registries hold all pools; trackers hold a subset for one DEX.

### Bot-owned registry vs module-level Pool Type Registry

**Ruling: Bot-owned registries hold *instances*. The Pool Type Registry holds *class mappings + deployment data*.**

- ✅ "The **Pool Registry** contains the LiquidityPool instance for 0xBb2b…"
- ✅ "The **Pool Type Registry** maps SushiSwap's factory to a `LiquidityPool` registration carrying the `sushiswap-v2` `DexIdentity` preset + `variant="sushiswap"` (post slice-7 collapse — the per-DEX `SushiswapV2Pool` subclass is deleted)"
- ❌ "The Pool Registry maps the factory to the class" (that's the **Pool Type Registry**)

### Use Pool Type Registry, not the removed Pool Class Registry or FACTORY_DEPLOYMENTS

**Ruling: `pool_type_registry` is the sole source of truth for `(chain_id, factory) → (class, identity, deployment data)`.** The former `Pool Class Registry` (factory→class only) and `FACTORY_DEPLOYMENTS` dict are removed (see Removed terms above).

- ✅ "Register the pool class with `pool_type_registry.register()`"
- ✅ "Resolve deployer from `pool_type_registry.get_deployment()`"
- ❌ "Register with `pool_class_registry`" (removed)
- ❌ "Look up the deployer in `FACTORY_DEPLOYMENTS`" (removed)
- ❌ "Call `register_exchange()` to populate `FACTORY_DEPLOYMENTS`" (removed)

## Example dialogue

> **Dev:** "I'm adding a new DEX. Should I register its pool class in the **Pool Registry**?"
> **Domain expert:** "No — the **Pool Registry** indexes live pool *instances* within a Bot session. You want the **Pool Type Registry** — it maps factory addresses to pool *classes* plus identity and deployment data."
>
> **Dev:** "Wait, is the Pool Type Registry another Bot-owned registry?"
> **Domain expert:** "No — it's a module-level singleton. The factory→class mapping is global knowledge that doesn't vary between Bot instances. All other registries (Pool, Token, Managed Pool) are Bot-owned class instances."
>
> **Dev:** "And the difference between a **Registry** and a **Manager**?"
> **Domain expert:** "A **Registry** is a passive index — it just stores and retrieves things. A **Pool Tracker** actively discovers, creates, and tracks pools for a specific DEX. The **Pool Registry** holds all pools; a **Pool Tracker** holds a subset for one DEX."
>
> **Dev:** "What about the old **Pool Class Registry**?"
> **Domain expert:** "Removed. It only mapped factory→class, with deployment data scattered elsewhere. The **Pool Type Registry** consolidates all of that into a single `register()` call."
