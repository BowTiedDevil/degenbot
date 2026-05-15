# Context Map

Module-level context files (terms, aliases, relationships, and ambiguity rulings):

- [Pool Types & Trackers](src/degenbot/types/CONTEXT.md) — pool types, type resolution, trackers, fee representations, and I/O-free architecture terms
- [Uniswap](src/degenbot/uniswap/CONTEXT.md) — V2/V3/V4 pools, concentrated liquidity, tick mechanics, and event types
- [Tokens](src/degenbot/erc20/CONTEXT.md) — ERC-20 tokens, ether placeholder, and chain ID
- [Pool Registries](src/degenbot/registry/CONTEXT.md) — address-based registries and the pool type registry
- [Arbitrage, Solvers & Adapters](src/degenbot/arbitrage/CONTEXT.md) — arbitrage cycles, solvers, adapters, and swap encoding
- [Aave](src/degenbot/aave/CONTEXT.md) — lending markets, assets, collateral, debt, and liquidation
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — StableSwap pools, fetcher protocols, variant and strategy enums
- [Infrastructure](src/degenbot/connection/CONTEXT.md) — providers, connection management, and the Bot session

## Instructions

1. **Terms belong to one module.** Add new terms to the `CONTEXT.md` in the module that owns the concept. Don't duplicate definitions at root.
2. **Ambiguity rulings go where the ambiguity lives.** If both terms are in the same module (e.g., Solver vs Optimizer), put the ruling in that module. Only cross-module ambiguities (e.g., Pool vs Market, Reserves vs Asset) go in root.
3. **Relationships follow the same rule.** If all terms in a relationship belong to one module, put it in that module's `## Relationships`. Only cross-module seams (where a term from one module relates to a term from another) go in root's `## Cross-module relationships`.
4. **When adding a module**, create its `CONTEXT.md` with term table, `## Relationships`, and `## Resolved ambiguities` sections as needed, then add a link to this map.
5. **Keep this map in sync.** When a module context changes (new terms, new rulings), update the bullet summary in this map to reflect it.
6. **Root contains only cross-cutting content:** module index, cross-module relationships, cross-module ambiguity rulings, and the example dialogue.

## Cross-module relationships

- **Bot** owns all session state: Connection Manager, Pool Registry, Token Registry, Managed Pool Registry, config, database
- **Bot.build_pool()** resolves the pool type automatically via DB `kind` column, Pool Type Registry, or on-chain probing, then dispatches to typed builders
- A **Pool Type Registry** (module-level singleton) maps (chain ID, factory address) → pool class + identity + deployment data; each DEX module self-registers at import time; auto-derives invariant, variant, and kind from the class
- A **Pool Registry** (class, owned by Bot) indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Managed Pools** by (chain ID, PoolManager address, Pool ID)
- An **Arbitrage Cycle** contains an ordered sequence of **Pools** that form a closed token loop
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver**
- A **Pool Cache Adapter** subscribes to **Pool State Messages** and auto-registers both reserve orientations in the Rust solver cache
- An **Arbitrage Path** subscribes to **Pool State Messages**
- An **Aave Market** contains many **Assets**, each wrapping an **Erc20Token** plus lending state
- A **Curve Pool Tracker** tracks **Curve StableSwap Pools** and delegates construction to **Bot**
- **Fetcher Callbacks** are injected into **Curve Pools** by **Bot.build_curve_pool()**; pools never access connections directly
- V2/V3/V4/Aerodrome **Pools** are fully I/O-free — **Builders** fetch all data from DB/RPC and pass values; no pool class imports `ProviderAdapter` or carries provider-dependent methods
- **PoolFamily** (identity enum in `types/pool_type.py`) is the sole identity enum; **Pool Invariant** (in `types/hop_types.py`) is the solver-dispatch enum
- **CacheablePool** protocol enables **Pool Cache Adapter** registration without introspection
- **Swap Amounts** carry per-pool swap parameters and self-encode into **EncodedCall**s; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **V4PoolKey** lives on `UniswapV4PoolSwapAmounts` for custom **PayloadComposers** handling V4's unlock/swap callback dispatch

Module-internal relationships are documented in each module's context file.

## Cross-module ambiguity rulings

These ambiguities span module boundaries and are resolved here so all modules stay consistent.

### 1. Pool vs Market vs Pool Contract

**Ruling: **Pool** = DEX only. **Market** = Aave lending system. **Pool contract** = the on-chain Aave contract named Pool.sol. A Market *has* a Pool contract; they can coexist in context.**

- ✅ "The WBTC/WETH **Pool** has 0.30% fee"
- ✅ "The Aave **Market** on Ethereum has 8 **Assets**"
- ✅ "The Market's **Pool contract** is at 0x8787…"
- ❌ "The Aave **pool** has 8 reserves" (use **Market**)
- ❌ "The **pool** not initialized" (use **Pool contract** if referring to the on-chain contract, or **Market** if referring to the system)

### 2. Reserves (DEX) vs Asset (Aave)

**Ruling: **Reserves** (plural) = DEX token balances. **Asset** = Aave lending state for one token.**

- ✅ "The **Reserves** are 1000 WBTC and 2000 WETH"
- ✅ "The USDC **Asset** has a liquidity index of 1.02e27"
- ❌ "The reserve is 1000 WBTC" (use **Reserves**)
- ❌ "The USDC reserves on Aave" (use **Asset**)

### 3. Asset vs Token

**Ruling: **Token** for all ERC-20 contracts. **Asset** for an ERC20 token plus its Aave lending state.**

A **Token** is just the ERC-20 contract (address, symbol, decimals) — no lending context. An **Asset** is the Token plus its lending state within an Aave Market.

- ✅ "The USDC **Token** address is 0xA0b8…" (the ERC-20 contract)
- ✅ "The USDC **Asset** has a borrow rate of 3%" (Aave lending state)
- ❌ "The asset address is 0xA0b8…" (use **Token** address)

### 4. Singleton (removed) vs Class instance

**Ruling: All former module-level singletons have been removed. Always refer to class instances owned by Bot.**

The following were previously module-level singletons; they are now classes instantiated and owned by Bot:
- `ConnectionManager` / `AsyncConnectionManager` — formerly `connection_manager` / `async_connection_manager`
- `PoolRegistry` / `TokenRegistry` / `ManagedPoolRegistry` — formerly `pool_registry` / `token_registry` / `managed_pool_registry`
- `DatabaseSessionManager` — formerly `db_session`
- `Config` — formerly `config` (LazyConfig proxy)

**Exception:** The `PoolTypeRegistry` (`pool_type_registry`) is a module-level singleton. This is intentional: the (chain ID, factory address) → class + identity + deployment data mapping is global knowledge that does not vary between Bot instances.

- ✅ "Create a `ConnectionManager` instance and pass it to Bot"
- ✅ "Bot's `connections` attribute is a `ConnectionManager`"
- ✅ "The `pool_type_registry` maps SushiSwap's factory to `SushiswapV2Pool`"
- ❌ "Import the connection_manager" (the module-level singleton no longer exists)

## Example dialogue

> **Dev:** "I'm adding a new DEX pool type. Should I register it in the **Pool Registry** directly or go through a **Pool Tracker**?"
>
> **Domain expert:** "Create a **Pool Tracker** subclass for that DEX's **Exchange Deployment**. The **Pool Tracker** handles discovery and tracking — it's the off-chain helper. The **Factory** is the on-chain contract that actually creates the **Pools**. The **Pool Registry** is just an index owned by **Bot** — **Pools** get added there automatically when they're discovered by the tracker."
>
> **Dev:** "And when someone calls `bot.build_pool(address)`, how does it know which subclass to use?"
>
> **Domain expert:** "The type resolver checks three sources in order. First, the database `kind` column — if the pool was persisted, it knows the exact subclass. Second, the **Pool Type Registry** — each DEX module registers its factory addresses at import time, so the resolver maps factory → class. Third, if neither source matches, it probes the contract on-chain — tries `slot0()` for concentrated-liquidity, `getReserves()` for constant-product. This all happens inside `build_pool` automatically."
>
> **Dev:** "What about V4 pools that don't have their own contract address?"
>
> **Domain expert:** "V4 **Managed Pools** live inside a **PoolManager** contract and are identified by **Pool ID**, not address. Call `build_pool(address, pool_id=...)` — the `pool_id` argument is the V4 discriminator. Without it, `address` is treated as a pool contract."
>
> **Dev:** "And for the **Arbitrage Cycle**, I just add the V4 pool to `swap_pools`?"
>
> **Domain expert:** "Yes, but make sure the **Swap Vectors** line up — each pool's **Token Out** must equal the next pool's **Token In**, and the last pool must return the **Input Token**. The **Solver** will compute the optimal **Input Amount** for that single path, and you'll get back a **Calculation Result** with per-pool **Swap Amounts**. If you're comparing multiple paths, that's the **Optimizer**'s job — it delegates to the **Solver** per path and picks the best."
>
> **Dev:** "Got it. One more thing — the Aave **Asset** for USDC shows a borrow rate change. Should I call that a pool update?"
>
> **Domain expert:** "No — that's an Aave **Market**, not a **Pool**. The USDC **Asset** is one token's lending state inside that **Market**. A **Pool** is always a DEX contract. Keep the terms separate."
>
> **Dev:** "But the Aave contract is literally called Pool.sol — can I say 'Pool contract' to be specific?"
>
> **Domain expert:** "Yes — **Pool contract** is fine when you mean the on-chain contract. 'The Market's Pool contract emitted a Supply event' is perfectly clear. Just don't use **Pool** alone to mean the lending system — that's always a **Market**."
