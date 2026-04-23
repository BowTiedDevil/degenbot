# Ubiquitous Language

## Liquidity Pools

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool** | A DEX smart contract holding paired token reserves that enables swaps via an automated market-making invariant; **never** used as a synonym for an Aave Market | Liquidity pool, pair, market, lending pool |
| **Pool State** | A frozen snapshot of a pool's on-chain data at a specific block | Pool snapshot, pool data |
| **Pool Address** | The unique on-chain checksummed address identifying a pool contract | Contract address, pair address |
| **Pool ID** | A hash identifying a V4 managed pool within a PoolManager, used in place of an address for singleton architectures pools | Pool hash, managed pool ID |
| **Reserves** | The token balances held by a constant-product pool (V2-style); always plural to distinguish from Aave **Reserve** | Balances, inventory, reserve (singular) |
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

## Tokens

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Token** | An ERC-20 compatible fungible token contract on an EVM chain; **always** use this term for ERC-20 contracts regardless of context | Coin, asset |
| **Token0** | The token with the lower address in a pool pair, as defined by the protocol | — |
| **Token1** | The token with the higher address in a pool pair, as defined by the protocol | — |
| **Ether Placeholder** | A Token-like adapter for native ETH in pools that use the zero-address or all-Es convention | ETH token, WETH placeholder |
| **Wrapped Native Token** | The WETH/WETH-like ERC-20 token that wraps the chain's native currency | Native token wrapper |
| **Chain ID** | The integer identifying an EVM-compatible blockchain (e.g., 1 = Ethereum, 8453 = Base) | Network ID, chain id |

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

## Pool Registries

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Registry** | A global singleton index mapping (chain ID, pool address) → Pool across all DEX protocols | Pool index, pool cache |
| **Token Registry** | A global singleton index mapping (chain ID, token address) → Token across all DEX protocols | Token index, token cache |
| **Managed Pool Registry** | A sub-registry for V4-style singleton architecture pools, keyed by (chain ID, PoolManager address, Pool ID) | V4 registry |

## Arbitrage

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Arbitrage Cycle** | A sequence of pools forming a closed loop where an input token is swapped through intermediate tokens and back to the original | Arb cycle, circular path, cycle |
| **Arbitrage Path** | An event-driven wrapper around a sequence of pools that validates token flow, subscribes to state updates, and delegates solving | Arb path |
| **Input Token** | The token supplied to the first swap in an arbitrage cycle | Starting token |
| **Profit Token** | The token in which arbitrage profit is measured (always equals the Input Token for a cycle) | Output token |
| **Input Amount** | The quantity of Input Token to be swapped into the first pool | Swap amount, trade size |
| **Profit Amount** | The net token gain after completing all swaps in the cycle (negative = unprofitable) | PnL, gain |
| **Rate of Exchange** | The ratio of output to input across the entire cycle; values > 1 indicate a profitable opportunity | Exchange rate, arb rate |
| **Swap Vector** | A directed pair (token_in, token_out) plus a zero_for_one flag describing the direction of a single swap within a path | Swap direction, flow |
| **Swap Amounts** | The per-pool input/output amounts and parameters needed to execute the swaps in an arbitrage cycle | Swap details |
| **Calculation Result** | The complete output of an arbitrage calculation: input amount, profit amount, per-pool swap amounts, and state block | Arb result |

## Solvers & Optimizers

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Solver** | An algorithm that finds the optimal **Input Amount** for a single arbitrage path given **Hop States**; operates on one path at a time | Optimizer, finder |
| **Optimizer** | A higher-level routine that coordinates **Solvers** across multiple paths or state configurations; finds the best path/strategy across alternatives | Solver |
| **Hop State** | The numerical representation of a single pool's state in a form suitable for solver consumption (e.g., virtual reserves) | Pool state (solver context), solver state |
| **Mobius Solver** | A solver using the Möbius transformation approach for constant-product and concentrated-liquidity pools | — |

## Pool Adapters

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Pool Adapter** | A protocol object that translates a specific pool type into solver-compatible Hop State, extracts fees, and builds Swap Amounts | Adapter, bridge |
| **Pool Compatibility** | An enum indicating whether a pool can participate in an arbitrage path (COMPATIBLE, INCOMPATIBLE_INVARIANT, INCOMPATIBLE_TOKENS) | — |

## Aave V3

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Market** | An Aave lending system comprising a **Pool contract**, its configurator, oracle, and all associated Reserves, positions, and risk parameters | Aave pool, lending pool |
| **Reserve** | The lending and borrowing state for one Token within an Aave Market; wraps three Token references (underlying, aToken, vToken) plus supply/borrow info, caps, collateral config, eMode, and isolation mode; **never** use for DEX pool balances — those are **Reserves** (plural) | Asset, Aave asset, aave reserve |
| **Collateral** | A Token deposited by a user as security for borrowing, represented by an aToken balance within a Reserve | Deposit, supply |
| **Debt** | A Token borrowed by a user, represented by a vToken balance within a Reserve | Loan, borrow |
| **aToken** | The interest-bearing Token minted to represent Collateral supplied to a Reserve | Collateral token, aToken |
| **vToken** | The variable-rate debt Token tracking a user's borrowed amount plus accrued interest within a Reserve | Debt token, variableDebtToken |
| **GHO** | Aave's native stablecoin with special discount mechanics for borrowers | — |
| **Health Factor** | The ratio of adjusted collateral value to debt value; below 1.0 the position can be liquidated | HF, safety factor |
| **Liquidation Threshold** | The percentage of collateral value usable for health factor calculation (e.g., 80% = 8000 bps) | LT |
| **Liquidation** | The forced repayment of a borrower's debt using their collateral when health factor falls below 1.0 | Liquidation event, liq |
| **Liquidation Pattern** | The on-chain event structure for multi-liquidations: SINGLE, COMBINED_BURN, or SEPARATE_BURNS | — |
| **Operation** | A user action on Aave: Supply, Withdraw, Borrow, Repay, Liquidation, etc. | Transaction, action |
| **Scaled Amount** | A token amount normalized by the current index (raw ÷ index), used for interest-accruing balance tracking | Normalized balance |
| **Raw Amount** | The actual token quantity before index-based scaling | Actual amount, wei amount |
| **Index** | The cumulative interest rate multiplier (liquidity index or borrow index) used to convert between raw and scaled amounts | Rate index, accumulator |
| **Enrichment** | The process of augmenting raw Aave events with computed scaled amounts and contextual data | — |
| **Processor** | A versioned component that calculates balance changes for a specific Aave contract revision and event type | Handler, calculator |
| **E-Mode** | Efficiency mode: higher LTV/liquidation thresholds for correlated assets within a category | High efficiency mode |
| **Isolation Mode** | A restriction where an asset can only be borrowed up to a debt ceiling, with no other assets usable as collateral | — |

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

## Infrastructure

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Anvil Fork** | A local forked blockchain instance running via Foundry's Anvil client for testing | Fork, local chain |
| **Provider** | An adapter wrapping an RPC connection for blockchain reads (sync or async) | RPC client, web3 |
| **Connection Manager** | A singleton managing provider instances keyed by chain ID | Connection |
| **Pool State Message** | A publisher/subscriber message notifying that a pool's state has changed | State update message |

## Relationships

- A **Pool** holds exactly two tokens: **Token0** and **Token1**
- A **Pool State** belongs to exactly one **Pool** and is captured at one **State Block**
- A **Pool Manager** tracks many **Pools** for one **Exchange Deployment**
- A **Pool Registry** indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Pools** by (chain ID, PoolManager address, Pool ID)
- An **Arbitrage Cycle** contains an ordered sequence of **Pools** that form a closed token loop
- An **Arbitrage Path** wraps a sequence of pools with a **Solver** and subscribes to **Pool State Messages**
- A **Swap Vector** describes the direction of a single hop within an **Arbitrage Path**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver**
- An **Aave Market** contains many **Reserves**, each wrapping three **Tokens** (underlying, aToken, vToken) plus lending state; the Market's **Pool contract** handles user-facing operations
- **Collateral** is represented by an **aToken** balance within a **Reserve**; **Debt** is represented by a **vToken** balance within a **Reserve**
- A **Health Factor** is computed from all **Collateral** and **Debt** positions of a single user
- A **Liquidation** occurs when a **Health Factor** drops below 1.0
- **GHO** debt uses a discount mechanism not present in standard **Debt**

## Example dialogue

> **Dev:** "I'm adding a new DEX pool type. Should I register it in the **Pool Registry** directly or go through a **Pool Manager**?"
>
> **Domain expert:** "Create a **Pool Manager** subclass for that DEX's **Exchange Deployment**. The **Pool Manager** handles discovery and tracking — it's the off-chain helper. The **Factory** is the on-chain contract that actually creates the **Pools**. The **Pool Registry** is just an index — **Pools** get added there automatically when they're created by the manager."
>
> **Dev:** "What about V4 pools that don't have their own contract address?"
>
> **Domain expert:** "V4 pools live inside a **PoolManager** contract and are identified by **Pool ID**, not address. The **Pool Registry** delegates V4 lookups to the **Managed Pool Registry**, which keys by (chain ID, PoolManager address, Pool ID). You'll need to set up the **Pool Adapter** so the **Solver** can convert the V4 pool into a **Hop State**."
>
> **Dev:** "And for the **Arbitrage Cycle**, I just add the V4 pool to `swap_pools`?"
>
> **Domain expert:** "Yes, but make sure the **Swap Vectors** line up — each pool's **Token Out** must equal the next pool's **Token In**, and the last pool must return the **Input Token**. The **Solver** will compute the optimal **Input Amount** for that single path, and you'll get back a **Calculation Result** with per-pool **Swap Amounts**. If you're comparing multiple paths, that's the **Optimizer**'s job — it delegates to the **Solver** per path and picks the best."
>
> **Dev:** "Got it. One more thing — the Aave **Reserve** for USDC shows a borrow rate change. Should I call that a pool update?"
>
> **Domain expert:** "No — that's an Aave **Market**, not a **Pool**. The USDC **Reserve** is one token's lending state inside that **Market**. A **Pool** is always a DEX contract. Keep the terms separate."
>
> **Dev:** "But the Aave contract is literally called Pool.sol — can I say 'Pool contract' to be specific?"
>
> **Domain expert:** "Yes — **Pool contract** is fine when you mean the on-chain contract. 'The Market's Pool contract emitted a Supply event' is perfectly clear. Just don't use **Pool** alone to mean the lending system — that's always a **Market**."

## Resolved ambiguities

### 1. Pool vs Market vs Pool Contract

**Ruling: **Pool** = DEX only. **Market** = Aave lending system. **Pool contract** = the on-chain Aave contract named Pool.sol. A Market *has* a Pool contract; they can coexist in context.**

The Aave V3 contract is formally named "Pool" (Pool.sol), but this collides with the much more frequent DEX usage. The CLI already uses "Market" (`degenbot aave market show`, `AaveV3Market` in the database). Adopting **Market** for the lending system eliminates the collision. When referring specifically to the on-chain contract, say **Pool contract** to clarify you mean the Aave contract, not a DEX pool.

- ✅ "The WBTC/WETH **Pool** has 0.30% fee"
- ✅ "The Aave V3 **Market** on Ethereum has 8 **Reserves**"
- ✅ "The Market's **Pool contract** is at 0x8787…"
- ✅ "Fetch events from the **Pool contract** for this Market"
- ❌ "The Aave **pool** has 8 reserves" (use **Market**)
- ❌ "The **pool** not initialized" (use **Pool contract** if referring to the on-chain contract, or **Market** if referring to the system)

### 2. Reserves (DEX) vs Reserve (Aave)

**Ruling: **Reserves** (plural) = DEX token balances. **Reserve** (singular, capitalized) = Aave lending configuration.**

The plural/singular distinction mirrors the actual domain: a DEX pool holds multiple reserve *balances* (reserves_token0, reserves_token1), while an Aave Reserve is a single asset's lending state within a Market.

- ✅ "The **Reserves** are 1000 WBTC and 2000 WETH"
- ✅ "The USDC **Reserve** has a liquidity index of 1.02e27"
- ❌ "The reserve is 1000 WBTC" (use **Reserves**)
- ❌ "The USDC reserves on Aave" (use **Reserve**)

### 3. Asset vs Token vs Reserve

**Ruling: **Token** for all ERC-20 contracts. **Reserve** for Aave lending state. Drop "asset" entirely.**

The official Aave documentation defines a strict hierarchy: a **Market** contains many **Reserves**; each **Reserve** wraps three **Tokens** (underlyingToken, aToken, vToken) plus lending state (APYs, caps, collateral config, eMode, isolation mode). A Token is just the ERC-20 contract (address, symbol, decimals). A Reserve is the lending state for one Token within a Market. "Asset" is ambiguous — it conflates the ERC-20 contract with its lending wrapper. Every "asset" in Aave context is either a **Token** (the ERC-20 contract) or a **Reserve** (the lending configuration wrapping that Token). Use the specific term.

- ✅ "The USDC **Token** address is 0xA0b8…" (the ERC-20 contract)
- ✅ "The USDC **Reserve** in the Market has a borrow rate of 3%" (the lending state)
- ✅ "Each **Reserve** references three **Tokens**: underlying, aToken, and vToken"
- ❌ "The USDC asset has a borrow rate of 3%" (use **Reserve**)
- ❌ "The asset address is 0xA0b8…" (use **Token** address)

### 4. Factory (on-chain) vs Pool Manager (off-chain)

**Ruling: **Factory** = on-chain contract only. **Pool Manager** = off-chain class only. Never use one to mean the other.**

These are two distinct layers. The Factory creates Pool contracts on-chain. The Pool Manager discovers and tracks Pools off-chain. The `AbstractPoolManager` attribute `pool_factory` refers to the *class* of Pool the manager creates, not the on-chain Factory — that's `factory_address`.

- ✅ "The Uniswap V2 **Factory** is at 0x5C69…"
- ✅ "The **Pool Manager** tracks 1200 **Pools** for this **Exchange Deployment**"
- ❌ "The factory tracks 1200 pools" (use **Pool Manager**)
- ❌ "The pool manager creates new pools" (the **Factory** creates them on-chain)

### 5. Fee representations

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

### 6. Solver vs Optimizer

**Ruling: **Solver** = single-path input optimization. **Optimizer** = multi-path coordination. Never substitute.**

The codebase enforces this hierarchy: `Solver` / `SolverProtocol` receives a sequence of **Hop States** and returns a result for one path. `ArbitrageOptimizer` coordinates across multiple paths, calling Solvers internally. An Optimizer decides *which* path is best; a Solver decides *how much* to input on a given path.

- ✅ "The **Mobius Solver** found an optimal **Input Amount** of 5 ETH for this path"
- ✅ "The **Optimizer** compared 12 paths and selected the best one"
- ❌ "The solver compared 12 paths" (that's an **Optimizer**)
- ❌ "The optimizer found the input amount" (that's a **Solver**)
