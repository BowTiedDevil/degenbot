# Ubiquitous Language — Pool Registries

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Registry** | A global singleton index mapping (chain ID, pool address) → Pool across all DEX protocols | Pool index, pool cache |
| **Token Registry** | A global singleton index mapping (chain ID, token address) → Token across all DEX protocols | Token index, token cache |
| **Managed Pool Registry** | A sub-registry for V4-style singleton architecture pools, keyed by (chain ID, PoolManager address, Pool ID) | V4 registry |

## Relationships

- A **Pool Registry** indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Pools** by (chain ID, PoolManager address, Pool ID)
