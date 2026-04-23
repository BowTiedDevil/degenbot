# Ubiquitous Language — Pool Types & Managers

## Liquidity Pools

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool** | A DEX smart contract holding paired token reserves that enables swaps via an automated market-making invariant; **never** used as a synonym for an Aave Market | Liquidity pool, pair, market, lending pool |
| **Pool State** | A frozen snapshot of a pool's on-chain data at a specific block | Pool snapshot, pool data |
| **Pool Address** | The unique on-chain checksummed address identifying a pool contract | Contract address, pair address |
| **Pool ID** | A hash identifying a V4 managed pool within a PoolManager, used in place of an address for singleton architectures pools | Pool hash, managed pool ID |
| **Reserves** | The token balances held by a constant-product pool (V2-style); always plural to distinguish from Aave **Asset** | Balances, inventory, reserve (singular) |
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
| **Stableswap Pool** | A Curve V1-style pool optimized for swaps between price-pegged tokens | Stable pool, Curve pool |
| **Weighted Pool** | A Balancer V2-style pool with configurable token weights in the invariant | Balancer pool |
| **Volatile Pool** | An Aerodrome V2 pool using the constant-product invariant (as opposed to its stable variant) | — |

## Pool Managers

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Manager** | An off-chain helper class that discovers, creates, and tracks Pools for a specific DEX factory on a chain; **never** called "factory" | Factory manager, pool factory |
| **Pool Factory** | The on-chain contract that creates Pools for a given DEX; a distinct concept from the off-chain **Pool Manager** | Factory (when ambiguous with Pool Manager) |
| **Factory Address** | The on-chain address of the DEX factory contract | — |
| **Tracked Pool** | A pool currently monitored by a Pool Manager | Active pool |
| **Untracked Pool** | A pool known to the Pool Manager but not currently monitored | Inactive pool |

## Relationships

- A **Pool** holds exactly two tokens: **Token0** and **Token1**
- A **Pool State** belongs to exactly one **Pool** and is captured at one **State Block**
- A **Pool Manager** tracks many **Pools** for one **Exchange Deployment**

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

### Factory (on-chain) vs Pool Manager (off-chain)

**Ruling: **Factory** = on-chain contract only. **Pool Manager** = off-chain class only. Never use one to mean the other.**

These are two distinct layers. The Factory creates Pool contracts on-chain. The Pool Manager discovers and tracks Pools off-chain. The `AbstractPoolManager` attribute `pool_factory` refers to the *class* of Pool the manager creates, not the on-chain Factory — that's `factory_address`.

- ✅ "The Uniswap V2 **Factory** is at 0x5C69…"
- ✅ "The **Pool Manager** tracks 1200 **Pools** for this **Exchange Deployment**"
- ❌ "The factory tracks 1200 pools" (use **Pool Manager**)
- ❌ "The pool manager creates new pools" (the **Factory** creates them on-chain)

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
