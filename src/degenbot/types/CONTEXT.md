# Context — Pool Types & Managers

## Liquidity Pools

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool** | A DEX smart contract holding paired token reserves that enables swaps via an automated market-making invariant; **never** used as a synonym for an Aave Market | Liquidity pool, pair, market, lending pool |
| **Pool State** | A frozen snapshot of a pool's on-chain data at a specific block | Pool snapshot, pool data |
| **Pool Address** | The unique on-chain checksummed address identifying a pool contract | Contract address, pair address |
| **Pool ID** | A hash identifying a V4 **Managed Pool** within a PoolManager, used in place of an address; see [Managed Pool](../uniswap/CONTEXT.md) in the Uniswap context | Pool hash, managed pool ID |
| **Reserves** | The token balances held by a constant-product pool; always plural to distinguish from Aave **Asset** | Balances, inventory, reserve (singular) |
| **Liquidity** | The concentrated liquidity value governing swap price impact in a V3/V4 pool | L, liquidityActive |
| **Sqrt Price** | The √price value in X96 format representing the current exchange ratio in a V3/V4 pool | sqrtPriceX96, current price |
| **Tick** | An integer index representing a specific price point in a concentrated liquidity pool's range | Price tick |
| **Tick Spacing** | The minimum interval between initialized ticks in a V3/V4 pool, set at pool creation | Tick size |
| **Tick Bitmap** | A compressed word-indexed map recording which ticks are initialized | Initialization map |
| **Tick Data** | The per-tick liquidityNet and liquidityGross values stored for every initialized tick | Liquidity map, tick liquidity |
| **Fee** | The generic concept of a swap fee deducted by a pool; when precision is needed, use **V2 Directional Fee**, **V3/V4 Pip Fee**, or **Weighted Fee Ratio** | Swap fee, trading fee, commission |
| **V2 Directional Fee** | A V2-style swap fee expressed as a Fraction, potentially different per direction (fee_token0, fee_token1 over fee_denominator) | Directional fee, fee_fraction |
| **V3/V4 Pip Fee** | A V3/V4-style swap fee expressed in pips (hundredths of 1%) over a fee denominator (e.g., fee=3000, FEE_DENOMINATOR=1_000_000 → 0.30%) | Pip fee, basis point |
| **Weighted Fee Ratio** | A Balancer-style swap fee expressed as a numerator/denominator pair applied to the weighted invariant | Balancer fee |
| **Simulation** | A calculation of swap inputs/outputs against a given pool state without modifying on-chain state | Quote, calculation, preview |
| **Simulation Result** | The output of a simulation: amount deltas, initial state, and final state | Swap result, quote result |
| **External Update** | New on-chain data pushed to a pool helper to synchronize it with the chain | State update, pool update |
| **State Block** | The block number at which a pool helper's current state was captured | Last update block |

## Pool Types (by Invariant)

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Constant-Product Pool** | A V2-style pool using the x·y=k invariant with directional fees | XYK pool, AMM pool, product pool |
| **Concentrated-Liquidity Pool** | A V3/V4-style pool where liquidity providers select active price ranges | CL pool, ranged pool |
| **Stableswap Pool** | A Curve V1-style pool optimized for swaps between price-pegged tokens; see [Curve CONTEXT.md](../curve/CONTEXT.md) for metapool, lending pool, and crypto pool subtypes | Stable pool, Curve pool |
| **Weighted Pool** | A Balancer V2-style pool with configurable token weights in the invariant | Balancer pool |
| **Volatile Pool** | An Aerodrome V2 pool using the constant-product invariant (as opposed to its stable variant) | — |

## Pool Invariant & Type Resolution

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Invariant** | A solver-dispatch enum identifying which mathematical invariant governs a pool's swap pricing for the arbitrage solver: `CONSTANT_PRODUCT`, `BOUNDED_PRODUCT`, `SOLIDLY_STABLE`, `CURVE_STABLESWAP`, `BALANCER_WEIGHTED`, `BALANCER_MULTI_TOKEN`; see [Arbitrage CONTEXT.md](../arbitrage/CONTEXT.md) | Pool family, pool category |
| **Pool Type Descriptor** | A frozen dataclass carrying the resolved pool identity: `PoolFamily` + variant name (e.g., `"sushiswap"`) + factory address | Type descriptor, pool descriptor |
| **Pool Variant** | A string identifying the DEX-specific subclass within an invariant family (e.g., `"sushiswap"`, `"camelot"`, `"aerodrome"`); `None` for the canonical Uniswap variant | DEX variant, subclass name |
| **Type Resolution** | The process of determining a pool's `PoolTypeDescriptor` from its address, consulting DB `kind` column → Pool Type Registry → on-chain probing; sync/async top-level functions are thin wrappers delegating to shared pure functions `_build_descriptor_from_db_result` and `_descriptor_from_probing_result` (Plan 066) | Pool discovery, type detection |
| **Kind** | The polymorphic identity string stored in the database `kind` column (e.g., `"uniswap_v2"`, `"sushiswap_v3"`, `"camelot_v2"`); derived from `derive_kind(family, variant)` — the family adds the `_v2`/`_v3` suffix | Polymorphic type, DB type |
| **Builder variant method** | A method on a pool builder that fetches class-specific state from chain and constructs variant pools | Variant builder, chain constructor |

## Pool Trackers

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Tracker** | An off-chain helper class that discovers, creates, and tracks Pools for a specific DEX factory on a chain; **never** called "manager" or "factory" | Pool Manager, factory manager, pool factory |
| **Pool Factory** | The on-chain contract that creates Pools for a given DEX; a distinct concept from off-chain **Pool Tracker** | Factory (when ambiguous with Pool Tracker) |
| **Factory Address** | The on-chain address of the DEX factory contract | — |
| **Tracked Pool** | A pool currently monitored by a Pool Tracker | Active pool |
| **Untracked Pool** | A pool known to the Pool Tracker but not currently monitored | Inactive pool |

## State Cache Terms

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **StateCache** | A generic temporal state cache (`StateCache[T: CacheableState]`) owning a deque and a lock; replaces per-pool deque+lock+navigation duplication | State cache, pool cache |
| **CacheableState** | A protocol requiring a `block: int | None` attribute; the type bound for `StateCache`'s type parameter | Cacheable, state protocol |
| **Caller-holds-lock** | Design where `StateCache` mutation methods are unlocked; the caller (pool) acquires `cache.lock()` for compound operations | External locking, explicit locking |
| **ConcentratedLiquidityStateManager** | A manager class for V3/V4 that composes with `StateCache` internally, exposing CL-specific convenience properties (`liquidity`, `sqrt_price_x96`, `tick`, etc.) | State manager, CL manager |

## Relationships

- A **Pool** holds paired tokens for swapping
- A **Pool State** belongs to exactly one **Pool** and is captured at one **State Block**
- A **Pool Tracker** tracks many **Pools** for one **Exchange Deployment**
- A **Pool State** may be updated via **data_provider** (e.g., Curve pools call `CurveDataProvider` methods on-demand)
- A **StateCache** stores a temporal sequence of **Pool State** snapshots for one **Pool**
- A **ConcentratedLiquidityStateManager** wraps a **StateCache** and adds CL-specific read conveniences

## I/O-Free Architecture Terms

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **DataProvider** | A protocol (e.g., `CurveDataProvider`) injected at pool construction that provides on-chain data on-demand via named methods | Data provider, fetcher |
| **Provider Method** | A single method on a DataProvider (e.g., `CurveDataProvider.D()`) called lazily when data is needed | — |
| **PoolFamily** | An enum identifying a pool's mathematical invariant family for type resolution: `CONSTANT_PRODUCT`, `CONCENTRATED_LIQUIDITY`, `STABLESWAP`, `WEIGHTED` | Pool family (lowercase) |
| **CacheablePool** | A protocol for pools that register with the Rust solver cache, requiring `reserves_for_cache()` and `fee_for_cache()` methods | Cacheable adapter |
| **Pair Selection** | The `token_in`/`token_out` keyword-only kwargs on `to_hop_state()` (both `ArbitrageCapablePool` and `ArbitragePathPool` protocols) that allow N-token pools to select an arbitrary coin pair; both-or-neither, resolve against `self.tokens`, override `zero_for_one` when both provided (Plan 071) | Token pair kwargs |
| **V4PoolKey** | A frozen dataclass carrying the V4 pool identification struct (currency0, currency1, fee, tick_spacing, hooks) | Pool key, V4 key |

## DEX Protocols (Supported)

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Exchange Deployment** | A named, chain-specific DEX deployment identified by its factory contract | Exchange, DEX deployment |
| **Uniswap V2** | A constant-product AMM with directional fees and a factory-based pool creation model | — |
| **Uniswap V3** | A concentrated-liquidity AMM with tick-based positions and a single fee per pool | — |
| **Uniswap V4** | A singleton-architecture concentrated-liquidity AMM with hook contracts and a PoolManager | — |
| **Aerodrome** | A Solidly-fork DEX on Base with V2 (volatile/stable) and V3 (concentrated) variants | — |
| **PancakeSwap** | A BSC-originating DEX with V2 and V3 variants on Ethereum and Base | — |
| **SushiSwap** | A DEX with V2 and V3 variants on Ethereum and Base | — |
| **Camelot** | A DEX on Arbitrum with a V2 variant | — |
| **SwapBased** | A DEX on Base with a V2 variant | — |
| **Curve V1** | A stableswap AMM optimized for pegged-asset exchanges | Curve |
| **Balancer V2** | A weighted-pool AMM with configurable token weights | Balancer |
| **Chainlink** | A decentralized oracle network providing price data via aggregator contracts | Oracle |

## Resolved ambiguities

### Factory (on-chain) vs Pool Tracker (off-chain)

**Ruling: **Factory** = on-chain contract only. **Pool Tracker** = off-chain class only. Never use one to mean the other.**

These are two distinct layers. The Factory creates Pool contracts on-chain. The Pool Tracker discovers and tracks Pools off-chain. The `AbstractPoolTracker` attribute `pool_factory` refers to the *class* of Pool the tracker handles, not the on-chain Factory — that's `factory_address`.

- ✅ "The Uniswap V2 **Factory** is at 0x5C69…"
- ✅ "The **Pool Tracker** tracks 1200 **Pools** for this **Exchange Deployment**"
- ❌ "The factory tracks 1200 pools" (use **Pool Tracker**)
- ❌ "The pool manager creates new pools" (use **Pool Tracker** — the **Factory** creates them on-chain)

### Fee representations

**Ruling: Use **Fee** generically. Qualify with the specific representation when precision matters.**

The three fee representations are fundamentally different data types and must not be conflated:
- **V2 Directional Fee**: `Fraction` (e.g., `Fraction(3, 1000)`), potentially different per direction
- **V3/V4 Pip Fee**: integer pips over an integer denominator (e.g., `fee=3000, FEE_DENOMINATOR=1_000_000`)
- **Weighted Fee Ratio**: numerator/denominator pair for Balancer's weighted invariant

When discussing fee values in code, always specify which representation. When discussing the concept abstractly ("this pool charges a fee"), **Fee** alone is fine.

- ✅ "The V2 **Directional Fee** is 0.30% for token0, 0.25% for token1"
- ✅ "The V3 **Pip Fee** is 3000"
- ✅ "The **Fee** makes this swap unprofitable" (conceptual usage is fine)
- ❌ "The fee is 3" (ambiguous — is that a fraction numerator? pips? basis points?)

## Example dialogue

> **Dev:** "The **Pool Tracker** created a new pool for SushiSwap."
> **Domain expert:** "The **Pool Tracker** *tracks* pools — it discovers and monitors them off-chain. The on-chain **Factory** *creates* them. The Pool Tracker is a Bot-owned helper; the Factory is the smart contract."
>
> **Dev:** "And **PoolFamily** vs **Pool Invariant** — aren't those the same thing?"
> **Domain expert:** "They map 1:1 for V2/V3, but they're different concepts. **PoolFamily** is the identity enum — it says what a pool *is* (constant-product, concentrated-liquidity, stableswap, weighted). **Pool Invariant** is the solver dispatch enum — it says which math the solver uses. They diverge for Curve and Balancer, where one PoolFamily maps to multiple Pool Invariants."
>
> **Dev:** "When I call `bot.build_pool()`, how does it know the PoolFamily?"
> **Domain expert:** "**Type Resolution** checks three sources: the database **Kind** column first, then the **Pool Type Registry** (factory→class mapping), then on-chain probing as a fallback. The Kind string like `sushiswap_v3` is derived from PoolFamily plus the **Pool Variant**."
>
> **Dev:** "What about the fee — V2 says `Fraction(3,1000)`, V3 says 3000. Which is right?"
> **Domain expert:** "Both are right for their version — they're different representations. V2 uses a **Directional Fee** as a Fraction; V3/V4 uses a **Pip Fee** as an integer over 1,000,000. Just say '**Fee**' when you mean the concept; qualify the representation when precision matters."
