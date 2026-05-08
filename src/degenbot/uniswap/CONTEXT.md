# Uniswap Module

Domain terms for Uniswap V2, V3, and V4 liquidity pools and pool managers.

## Term Table

| Term | Definition | Aliases |
|------|------------|---------|
| **Pool Invariant** | The mathematical relationship between pool reserves determining swap pricing. V2 uses constant product (x*y=k), V3/V4 use concentrated liquidity with tick-based pricing. | AMM formula, bonding curve |
| **Reserves** | Token balances held by a V2 pool contract at a given block. | Pool balances, token amounts |
| **SqrtPriceX96** | Square root of the price ratio between token0 and token1, encoded in Q64.96 fixed-point format. | sqrtPriceX96, price |
| **Tick** | Logarithmic price spacing unit. Each tick represents a 0.01% (1 basis point) price change. | price tick |
| **Liquidity** | Active capital depth at the current price in V3/V4. Represents the amount of both tokens available for swaps at the current tick. | L, active liquidity |
| **Tick Spacing** | Minimum distance between usable ticks for a given fee tier. Determines concentration granularity. | spacing |
| **Fee Tier** | Percentage fee charged on swaps. Denominator is 1,000,000 (1,000,000 = 100%). | swap fee, fee |
| **Pool Manager** | Bot-owned helper that discovers and tracks pools for a specific DEX factory on a chain. Manages pool lifecycle, from creation to state updates. | Manager |
| **Factory** | On-chain contract that creates new pools when token pairs are added. V2/V3 use separate factories per version. | DEX factory |
| **Pool Init Hash** | Keccak256 hash of pool creation init code. Used with CREATE2 to deterministically compute pool addresses from (factory, token0, token1, fee). | init code hash |
| **Factory Deployment** | Configuration for a specific DEX factory on a chain, including factory address, deployer, and pool init hash. | Exchange deployment |
| **PairCreated Event** | V2 factory event emitted when a new pool is created: `(token0, token1, pair, pairCount)`. | Pool creation event |
| **PoolCreated Event** | V3 factory event emitted when a new pool is created: `(token0, token1, fee, tickSpacing, pool)`. | V3 pool creation event |
| **Mint Event** | V3 pool event when liquidity is added: `(sender, owner, tickLower, tickUpper, amount, amount0, amount1)`. | Add liquidity |
| **Burn Event** | V3 pool event when liquidity is removed: `(owner, tickLower, tickUpper, amount, amount0, amount1)`. | Remove liquidity |
| **Tick Bitmap** | 256-bit word mapping initialized tick positions for efficient tick traversal in V3/V4. Each word tracks 256 ticks. | tick mapping, bitmap |
| **Tick Data** | Per-tick liquidity information: `liquidityNet` (delta at crossing) and `liquidityGross` (total at tick). | liquidity net/gross |
| **Concentrated Liquidity** | V3/V4 feature allowing LPs to provide capital within custom price ranges rather than the full range. | range liquidity |
| **V4 Pool Key** | Keccak256 hash of `(currency0, currency1, fee, tickSpacing, hooks)` identifying a V4 pool within a PoolManager. | pool identifier |
| **PoolManager** | V4 singleton contract managing all V4 pools. Pools don't have individual contract addresses. | V4 manager |
| **ModifyLiquidity Event** | V4 single event for adding/removing liquidity: `(poolId, sender, tickLower, tickUpper, liquidityDelta, salt)`. | V4 liquidity update |
| **Simulation** | Off-chain calculation of a swap's result without executing it on-chain. Updates virtual pool state. | swap preview, dry run |
| **Auto-Update** | Mechanism where pools subscribe to state updates (events, block changes) and refresh their state automatically. | state sync |
| **State Cache** | Ring buffer of recent pool states for rollback/replay capabilities. Used in simulation and arbitrage calculations. | state history |
| **Swap Vector** | Directional pair `(token_in, token_out)` for a swap across a pool. | swap direction |
| **Exact Input** | Swap calculation mode where the input amount is fixed and output is calculated. | exact in, token_in specified |
| **Exact Output** | Swap calculation mode where the output amount is fixed and required input is calculated. | exact out, token_out specified |

## Pool Types

### V2 Pool (Constant Product)

Simple AMM with reserves `token0` and `token1` satisfying `x * y = k`.

- **Fee**: Fixed fraction (default 3/1000 = 0.30%)
- **Price**: `reserves_token1 / reserves_token0` (in token1 per token0)
- **Address**: Deterministically derived from factory, token0, token1 via CREATE2
- **State**: `reserves_token0`, `reserves_token1`

### V3 Pool (Concentrated Liquidity)

Tick-based AMM with custom price ranges.

- **Fee**: Fixed per pool at creation (500, 3000, or 10000 bps)
- **Price**: Derived from `sqrtPriceX96` via `(sqrtPriceX96/2^96)^2`
- **Liquidity**: Active at current tick, with gross/net tracking per initialized tick
- **Address**: Derived from factory, token0, token1, fee via CREATE2 (includes fee in hash)
- **State**: `liquidity`, `sqrt_price_x96`, `tick`, `tick_bitmap`, `tick_data`

### V4 Pool (Hooks + Singleton)

All pools live within a single PoolManager contract, identified by Pool Key hash.

- **Fee**: Dynamic (0 to 1,000,000 bps), can change per pool
- **Hooks**: Customizable callbacks at pool boundaries (before/after swap, modify liquidity)
- **Pool ID**: `keccak256(currency0, currency1, fee, tickSpacing, hooks)`
- **State**: Similar to V3 but with additional hook state and fee protocol tracking

## Relationships

| From | Relationship | To | Notes |
|------|-------------|-----|-------|
| Pool Manager | tracks → | Pool | One manager per (chain, factory). Manages many pools. |
| Factory | creates → | Pool | On-chain contract. V2/V3 factories deploy pool contracts. V4 PoolManager creates pool entries. |
| Bot | owns → | Pool Manager | Bot manages pool managers per chain. |
| Bot | indexes → | Pool | Pool Registry holds all pools. |
| DEX | deployed on → | Chain | Exchange deployment specifies factory addresses per chain. |
| Token0 | paired with → | Token1 | Each pool has exactly two tokens. Token0 < Token1 by address. |
| LP | provides → | Liquidity | V2: full range. V3/V4: concentrated within tick range. |
| Tick | belongs to → | Tick Bitmap | Each tick maps to a bit position in a 256-bit word. |
| Pool | has → | State Cache | Ring buffer of historical states for rollback. |
| Pool | subscribes to → | Subscriber | Auto-update pushes state changes to subscribers. |

## Resolved Ambiguities

### 1. "Pool" vs "Pool Manager" vs "PoolManager" (V4)

**Pool** = the liquidity pool itself (has reserves/pricing). **Pool Manager** (off-chain, capitalized as two words) = the Bot's helper class that discovers and tracks pools. **PoolManager** (on-chain, one word) = V4's singleton contract.

- ✅ "The **Pool Manager** found a new Uniswap V3 pool"
- ✅ "The **PoolManager** contract emitted a ModifyLiquidity event"
- ❌ "The poolmanager updated its state" (use **Pool Manager** for off-chain, **PoolManager** for on-chain)
- ❌ "The Pool emitted a PoolCreated event" (factories emit this, not pools)

### 2. Fee representations

Uniswap uses different denominators per version:

- **V2**: Fraction like `Fraction(3, 1000)` = 0.30%
- **V3/V4**: Integer in basis points (1/1,000,000). 3000 = 0.30%
- **UI display**: Always percentage (e.g., "0.05%", "0.30%", "1.00%")

When documenting, mention the version and format:
- ✅ "V3 fee tier of 3000 (0.30%)"
- ❌ "Fee is 0.003" (ambiguous: 0.003%? 0.30%?)

### 3. Token0 vs Token1 ordering

Token addresses are ordered numerically: `token0 < token1` by address bytes.

- ✅ "Token0 is WETH (0xC02...), Token1 is USDC (0xA0b8...)"
- ❌ "TokenA and TokenB" (use Token0/Token1 everywhere)
- ❌ "Base and quote" (use Token0/Token1)

### 4. "Price" vs "Exchange Rate"

- **Price** = relative value, can be expressed either direction
- **Exchange Rate** = output amount per input amount for a specific swap

For pools:
- ✅ "The pool price is 2000 USDC per WETH" (directional)
- ✅ "The exchange rate for WETH→USDC is 1994.2 USDC per WETH" (includes fees)
- ❌ "The price is 2000" (always specify units and direction)

### 5. V4 Pool ID vs Pool Address

**V4 pools don't have addresses.** They have **Pool IDs** (32-byte hashes).

- ✅ "The V4 pool ID is 0xabcd..."
- ❌ "The V4 pool address is 0xabcd..." (use **Pool ID** or **Pool Key**)
- ✅ "Get the V4 pool by its PoolManager and Pool ID"

## Pool Creation Patterns

### V2 Pool Creation

```python
# via Bot (recommended)
pool = bot.build_v2_pool("0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc")

# via Pool Manager
from degenbot.uniswap.managers import UniswapV2PoolManager
manager = UniswapV2PoolManager(
    factory_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
    bot=bot,
)
pool = manager.get_pool("0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc")
```

### V3 Pool Creation

```python
# via Bot (recommended)
pool = bot.build_v3_pool("0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8")

# via Pool Manager
from degenbot.uniswap.managers import UniswapV3PoolManager
manager = UniswapV3PoolManager(
    factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
    bot=bot,
)
pool = manager.get_pool("0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8")
```

### V4 Pool Creation

```python
# V4 requires pool manager address and pool ID
pool = bot.build_v4_pool(
    pool_id="0x00...",
    pool_manager_address="0x...",
    currency0="0x...",
    currency1="0x...",
    fee=3000,
    tick_spacing=60,
)
```

## Notes for AI Agents

- Always create pools through `bot.build_*_pool()` or `manager.get_pool()` — never instantiate `UniswapV2Pool`, `UniswapV3Pool`, or `UniswapV4Pool` directly
- State caches contain historical states: `pool._state_cache[-1]` is previous state
- Tick bitmaps are sparse — only initialized ticks are stored
- V4 Pool Keys uniquely identify pools within a PoolManager contract
- Pool Managers handle pool lifecycle: discovery, creation, tracking, auto-update
