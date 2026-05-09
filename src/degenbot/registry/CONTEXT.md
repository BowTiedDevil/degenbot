# Context — Pool Registries

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Registry** | A class indexing (chain ID, pool address) → Pool across all DEX protocols; instances owned by Bot | Pool index, pool cache |
| **Token Registry** | A class indexing (chain ID, token address) → Token across all DEX protocols; instances owned by Bot | Token index, token cache |
| **Managed Pool Registry** | A sub-registry for V4-style singleton architecture pools, keyed by (chain ID, PoolManager address, Pool ID); instance owned by Bot | V4 registry |

## Relationships

- A **Pool Registry** indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Pools** by (chain ID, PoolManager address, Pool ID)
- All registry instances are owned by **Bot** — there are no module-level singletons

## Resolved Ambiguities

### Registry vs Manager

**Ruling: **Registry** = passive index. **Manager** = active controller.**

A Registry is a simple lookup table created and owned by Bot. A Pool Manager (e.g., `UniswapV2PoolManager`, `CurveStableswapPoolManager`) actively discovers, creates, and tracks pools for a specific DEX. Registries hold all pools; managers hold a subset for one DEX.
