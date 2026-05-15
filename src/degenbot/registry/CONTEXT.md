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

## Resolved Ambiguities

### Registry vs Manager

**Ruling: **Registry** = passive index. **Manager** = active controller.**

A Registry is a simple lookup table. A Pool Tracker (e.g., `UniswapV2PoolManager`, `CurveStableswapPoolManager`) actively discovers, creates, and tracks pools for a specific DEX. Registries hold all pools; trackers hold a subset for one DEX.

### Bot-owned registry vs module-level Pool Type Registry

**Ruling: Bot-owned registries hold *instances*. The Pool Type Registry holds *class mappings + deployment data*.**

- ✅ "The **Pool Registry** contains the UniswapV2Pool instance for 0xBb2b…"
- ✅ "The **Pool Type Registry** maps SushiSwap's factory to the SushiswapV2Pool class"
- ❌ "The Pool Registry maps the factory to the class" (that's the **Pool Type Registry**)

### Pool Class Registry (removed) vs Pool Type Registry

**Ruling: Use **Pool Type Registry** always. The Pool Class Registry has been removed.**

- ✅ "Register the pool class with `pool_type_registry.register()`"
- ❌ "Register with `pool_class_registry`" (removed)

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
