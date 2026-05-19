# Context Map

## Language

**Bot**: The central session class that owns all I/O, registries, config, and database connections; single entry point for all pool and token operations; resolves pool types automatically via `build_pool()`; delegates I/O orchestration to typed **Builders**.
_Avoid_: Bot session, session, orchestrator, bot instance

**Pool State Message**: A publisher/subscriber message notifying that a pool's state has changed.
_Avoid_: State update message, state update

**Anvil Fork**: A local forked blockchain instance running via Foundry's Anvil client for testing.
_Avoid_: Fork, local chain

## Module contexts

- [Pool Types & Trackers](src/degenbot/types/CONTEXT.md) — pool types, type resolution, trackers, fee representations, and I/O-free architecture terms
- [Uniswap](src/degenbot/uniswap/CONTEXT.md) — V2/V3/V4 pools, concentrated liquidity, tick mechanics, and event types
- [Tokens](src/degenbot/erc20/CONTEXT.md) — ERC-20 tokens, ether placeholder, and chain ID
- [Pool Registries](src/degenbot/registry/CONTEXT.md) — address-based registries and the pool type registry
- [Arbitrage, Solvers & Adapters](src/degenbot/arbitrage/CONTEXT.md) — arbitrage cycles, solvers, adapters, and swap encoding
- [Aave](src/degenbot/aave/CONTEXT.md) — lending markets, assets, collateral, debt, and liquidation
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — StableSwap pools, CurveDataProvider seam, DyCalculator, variant and strategy enums (with `make_calculator()` factory methods), CurveOnChainCache
- [Connection Management](src/degenbot/connection/CONTEXT.md) — connection managers, provider references, and subscription primitives
- [Chainlink](src/degenbot/chainlink/CONTEXT.md) — price feeds, aggregators, round data
- [Builders](src/degenbot/builders/CONTEXT.md) — pool builders, PoolIO seam (7-method protocol), BuilderContext, PoolBuilder/AsyncPoolBuilder protocols, V2BuilderBase/V3BuilderBase/V4BuilderBase shared helpers, and shared type resolution

## Instructions

1. **Terms belong to one module.** Add new terms to the `CONTEXT.md` in the module that owns the concept. Don't duplicate definitions at root.
2. **Ambiguity rulings go where the ambiguity lives.** If both terms are in the same module (e.g., Solver vs Optimizer), put the ruling in that module. Only cross-module ambiguities (e.g., Pool vs Market, Reserves vs Asset) go in root.
3. **Relationships follow the same rule.** If all terms in a relationship belong to one module, put it in that module's `## Relationships`. Only cross-module seams (where a term from one module relates to a term from another) go in root's `## Cross-module relationships`.
4. **When adding a module**, create its `CONTEXT.md` with term table, `## Relationships`, and `## Resolved ambiguities` sections as needed, then add a link to this map.
5. **Keep this map in sync.** When a module context changes (new terms, new rulings), update the bullet summary in this map to reflect it.
6. **Root contains only cross-cutting content:** shared term definitions, module index, cross-module relationships, cross-module ambiguity rulings, and the example dialogue.

## Cross-module relationships

- **Bot** owns all session state: Connection Manager, Pool Registry, Token Registry, Managed Pool Registry, config, database
- **Bot.build_pool()** resolves the pool type automatically via DB `kind` column, Pool Type Registry, or on-chain probing, then dispatches to the **Builder Registry** (`dict[type, PoolBuilder]`) keyed by concrete pool class
- A **Pool Type Registry** (module-level singleton) maps (chain ID, factory address) → pool class + identity + deployment data; each DEX module self-registers at import time; auto-derives invariant, variant, and kind from the class
- A **Pool Registry** (class, owned by Bot) indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Managed Pools** by (chain ID, PoolManager address, Pool ID)
- An **Arbitrage Cycle** (deprecated) was an ordered sequence of **Pools** that form a closed token loop; replaced by **Arbitrage Path**
- An **Arbitrage Path** contains an ordered sequence of **Pools** that form a closed token loop, wraps them with a **Solver**, and subscribes to **Pool State Messages**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver** (inline in `ArbitragePath`; `solver_hop_builders.py` deleted)
- A **Pool Cache Adapter** subscribes to **Pool State Messages** and auto-registers both reserve orientations in the Rust solver cache
- An **Arbitrage Path** subscribes to **Pool State Messages**
- **Swap Amounts** carry per-pool swap parameters and self-encode into **EncodedCall**s; `input_amount()`/`output_amount()` provide generic extraction; `build_swap_amount()` on pool classes replaces instanceof-chain factory; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **Pool → Hop conversion** flows through each pool's `to_hop_state()` method (single source of truth; `solver_hop_builders.py` deleted)
- An **Aave Market** contains many **Assets**, each wrapping an **Erc20Token** plus lending state
- A **Curve Pool Tracker** tracks **Curve StableSwap Pools** and delegates construction to **Bot**
- A **CurveDataProvider** is injected into **Curve Pools** by the **Curve Pool Builder** (invoked via `Bot.build_pool()`); pools never access connections directly. The builder creates a `CurveDataProviderImpl` (structured class with real methods, Plan 049) that wraps a `ProviderAdapter` — the former 850-line `CurveFetcherFactory` closure bag and 13 individual fetcher callbacks have been replaced
- A **CurveOnChainCache** consolidates all per-block on-chain data caches for a **Curve Pool** into a single object with the try-cache→call-provider→store→return pattern; replaces the former 10 individual `BoundedCache` fields scattered across the pool class (Plan 054)
- Strategy enums (`SwapStyle`, `MetapoolRateStyle`, `MetapoolUnderlyingStyle`) provide `make_calculator()` factory methods returning the matching `DyCalculator` instance; `PoolStrategies` auto-constructs calculators from enum values (Plan 056)
- V2/V3/V4/Aerodrome **Pools** are fully I/O-free — **Builders** fetch all data from DB/RPC and pass values; no pool class imports `ProviderAdapter` or carries provider-dependent methods
- **PoolIO** is the builder-facing I/O seam — a 7-method protocol (`call`, `call_raw`, `get_block_number`, `get_block`, `get_block_timestamp`, `get_code`, `get_balance`) with sync (`SyncPoolIO`) and async (`AsyncPoolIO`) adapters wrapping `ProviderAdapter`/`AsyncProviderAdapter`; **Bot** and **AsyncBot** create the appropriate adapter and pass `io=` to all builder calls; `BuilderContext` no longer carries a `connections` field — all I/O flows through `io: PoolIO` at call sites
- **Type Resolution** (`type_resolution.py`) provides shared pure-logic functions for pool class resolution, replacing ~330 lines of duplicated resolution code in Bot and AsyncBot; I/O-bearing steps come in sync/async pairs that accept `PoolIO`/`AsyncPoolIO`
- **DyCalculationInputs** is a frozen dataclass constructed by `CurveStableswapPool.get_dy()` that carries pre-resolved data for a single dy calculation; **DyCalculator** implementations receive this instead of the pool object, eliminating all private member access (Plan 045)
- **PoolFamily** (identity enum in `types/pool_type.py`) is the sole identity enum; **Pool Invariant** (in `types/hop_types.py`) is the solver-dispatch enum
- **CacheablePool** protocol enables **Pool Cache Adapter** registration without introspection
- **Swap Amounts** carry per-pool swap parameters and self-encode into **EncodedCall**s; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **V4PoolKey** lives on `UniswapV4PoolSwapAmounts` for custom **PayloadComposers** handling V4's unlock/swap callback dispatch
- A **Subscription** is a Rust-backed async iterator over push events from `eth_subscribe` with double-buffer drain for GIL-free accumulation; created by `AsyncProviderAdapter.subscribe_*()`; requires WS/IPC transport; raises **SubscriptionNotSupported** on HTTP providers; sync adapters and `_AsyncWeb3Adapter` inherit subscription stubs from **SyncSubscriptionSupport** / **AsyncSubscriptionSupport** mixins (Plan 058)
- A **LogListener** is a pure Python dispatch registry mapping `(address, topic0)` → handler set; receives raw log dicts via `dispatch(log)`, calls handlers sequentially; created by the user, not owned by Bot
- **LogSubscriptionFilter** carries `addresses` + `topics` only (no block range) for log subscriptions
- **LOG_HANDLERS** is a `ClassVar[dict[str, Callable]]` on pool types mapping event topic0 → decoder function; each decoder takes a log dict and returns a closure that applies the update to a pool instance; the user wires LOG_HANDLERS to a LogListener after `build_pool()`
- **Bot.start_listening()** creates newHeads + unfiltered logs subscriptions for a chain, returns the subscription pair; stores the AsyncProviderAdapter per chain in `_async_adapters`

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
> **Dev:** "And for the **Arbitrage Path**, I just add the V4 pool to `swap_pools`?"
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
