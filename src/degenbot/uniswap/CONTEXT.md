# Uniswap Module

Domain terms for Uniswap V2, V3, and V4 liquidity pools and pool trackers.

## Term Table

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Reserves** | See [Reserves](../types/CONTEXT.md) in the types context | Pool balances, token amounts |
| **SqrtPriceX96** | The √price ratio between token0 and token1, encoded in Q64.96 fixed-point format | sqrtPriceX96, price |
| **Tick** | A logarithmic price spacing unit representing a 0.01% price change | Price tick |
| **Liquidity** | Active capital depth at the current price in a V3/V4 pool | L, active liquidity |
| **Tick Spacing** | The minimum distance between usable ticks for a given fee tier | Spacing |
| **Fee Tier** | A fixed swap fee percentage for a V3/V4 pool, expressed in pips over 1,000,000 | Swap fee, fee |
| **Pool Tracker** | An off-chain Bot helper that discovers and tracks Pools for a specific DEX factory; distinct from on-chain **PoolManager** | Manager, Pool Manager |
| **Factory** | The on-chain contract that creates new pools for a given DEX version | DEX factory |
| **Pool Init Hash** | The keccak256 hash of pool creation init code, used with CREATE2 to compute pool addresses | Init code hash |
| **Factory Deployment** | Configuration for a specific DEX factory on a chain (factory address, deployer, pool init hash); stored exclusively in **Pool Type Registry** via `register()` — the former `FACTORY_DEPLOYMENTS` dict has been removed | Exchange deployment |
| **PairCreated Event** | The V2 factory event emitted when a new pool is created | Pool creation event |
| **PoolCreated Event** | The V3 factory event emitted when a new pool is created | V3 pool creation event |
| **Mint Event** | The V3 pool event emitted when liquidity is added | Add liquidity |
| **Burn Event** | The V3 pool event emitted when liquidity is removed | Remove liquidity |
| **Managed Pool** | A V4 pool that acts like a standalone pool contract but lives inside a **PoolManager** instead of having its own contract address; identified by **Pool ID** | V4 pool, singleton pool |
| **Concentrated Liquidity** | The V3/V4 feature allowing LPs to provide capital within custom price ranges | Range liquidity |
| **PoolManager** | The V4 singleton on-chain contract that manages user positions, assets, swaps, and hook callbacks for all of its **Managed Pools**; distinct from off-chain **Pool Tracker** | V4 manager |
| **V4 Pool Key** | See [V4PoolKey](../types/CONTEXT.md) in the types context | Pool identifier |
| **Simulation** | See [Simulation](../types/CONTEXT.md) in the types context | Swap preview, dry run |
| **Swap Vector** | See [Swap Vector](../arbitrage/CONTEXT.md) in the arbitrage context | Swap direction |
| **Exact Input** | A swap calculation mode where the input amount is fixed and output is calculated | Exact in |
| **Exact Output** | A swap calculation mode where the output amount is fixed and required input is calculated | Exact out |
| **amountSpecified** | The swap amount parameter in V3/V4 `swap()`: sign convention differs between V3 (positive = exact input, negative = exact output) and V4 (negative = exact input, positive = exact output). Verified in `v3_simulator.py:93` — `exact_input = amount_specified > 0` | swap amount, specified amount |
| **StateCache** | See [StateCache](../types/CONTEXT.md) in the types context | Pool state cache |
| **ConcentratedLiquidityStateManager** | A manager class for V3/V4 that composes with `StateCache` internally, exposing CL-specific convenience properties (`liquidity`, `sqrt_price_x96`, `tick`, etc.) | State manager |
| **V4BlockEngine** | A pure-Rust engine that owns V4 pool state, tick-range construction, and solver dispatch for V4 pools behind PoolManager; mirrors `V3BlockEngine` but identifies pools by `(pool_manager, pool_id)` instead of contract address; reuses `IntV3TickRangeSequence` (same CL math as V3) | V4 engine, V4 block processor |
| **V4SwapEvent** | A decoded V4 Swap event (`Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`) from PoolManager, carrying pool_id, amounts, sqrtPriceX96, liquidity, tick, and fee | V4 swap log |
| **AMOUNT_MODIFYING_HOOK_MASK** | `0xCC` — bitmask for the 4 V4 hook flags that can modify swap amounts (`BEFORE_SWAP` 0x80, `AFTER_SWAP` 0x40, `BEFORE_SWAP_RETURNS_DELTA` 0x08, `AFTER_SWAP_RETURNS_DELTA` 0x04); pools with any of these flags are rejected at engine registration time | hook filter mask |
| **V4_DYNAMIC_FEE_FLAG** | `0x100000` — the fee value indicating a V4 pool has dynamic (swap-dependent) fees; pools with this flag are rejected at engine registration | dynamic fee flag |
| **unlockCallback** | The V4 callback fired by `PoolManager.unlock()` inside which all V4 swap/settle/take operations execute; the executor's `unlockCallback` handler resumes payload delivery from the queue | V4 unlock callback |

## Contract Reference

Verified Solidity sources for all Uniswap versions are in `contract_reference/uniswap/`:

| File | Contents |
|------|----------|
| `V2/UniswapV2Factory.sol` | Full V2 core: Factory, Pair, ERC20, SafeMath, Math, UQ112x112 |
| `V3/UniswapV3Factory.sol` | Full V3 core: Factory, Pool, Oracle, Tick, TickBitmap, SqrtPriceMath, SwapMath, TickMath, FullMath, Position, etc. |
| `V4/PoolManager.sol` | Full V4 core: PoolManager, Pool, Hooks, TickBitmap, SqrtPriceMath, SwapMath, TickMath, ProtocolFeeLibrary, LPFeeLibrary, ERC6909, etc. |

Each file is a single concatenated bundle of all core contracts and libraries. When porting on-chain logic to Python, include a `See: contract_reference/uniswap/...` comment pointing to the exact source.

## Relationships

- A **Pool Tracker** tracks many **Pools** for one **Factory** on a chain
- A **Factory** creates **Pools** on-chain (V2/V3 deploy contracts; V4 **PoolManager** creates entries)
- **Token0** is paired with **Token1** in each pool, ordered by address (token0 < token1)
- A **Tick** belongs to a **Tick Bitmap** (each tick maps to a bit position in a 256-bit word)

## Resolved Ambiguities

### 1. "Pool" vs "Pool Tracker" vs "PoolManager" vs "Managed Pool" (V4)

**Pool** = any DEX liquidity pool. **Pool Tracker** = the off-chain Bot helper that discovers and tracks pools. **PoolManager** = the V4 singleton on-chain contract that manages user positions, assets, swaps, and hook callbacks for all its **Managed Pools**. A **Managed Pool** acts like a standalone pool contract but many are wrapped by a single **PoolManager** instead of being separate contracts like V2/V3 pools.

- ✅ "The **Pool Tracker** found a new Uniswap V3 pool"
- ✅ "The **PoolManager** contract emitted a ModifyLiquidity event"
- ✅ "This **Managed Pool** is identified by its Pool ID within the **PoolManager**"
- ✅ "Build a V4 pool with `build_managed_pool(address, pool_id)` where `address` is the PoolManager"
- ❌ "The pool manager found a new pool" (use **Pool Tracker** to avoid confusion with **PoolManager**)
- ❌ "V4 pools have their own contract address" (they're **Managed Pools** inside a **PoolManager**)
- ❌ "Call `build_pool(address, pool_id=...)` for V4" (use **build_managed_pool()** instead)

### 2. Fee representations

Uniswap uses different denominators per version. See [Fee representations](../types/CONTEXT.md) for the full ruling.

- ✅ "V3 fee tier of 3000 (0.30%)"
- ❌ "Fee is 0.003" (ambiguous)

### 3. Token0 vs Token1 ordering

Token addresses are ordered numerically: `token0 < token1` by address bytes.

- ✅ "Token0 is WETH (0xC02...), Token1 is USDC (0xA0b8...)"
- ❌ "TokenA and TokenB" or "Base and quote" (use Token0/Token1)

### 4. "Price" vs "Exchange Rate"

**Price** = relative value, expressed with direction and units. **Exchange Rate** = output per input for a specific swap (includes fees).

- ✅ "The pool price is 2000 USDC per WETH"
- ✅ "The exchange rate for WETH→USDC is 1994.2 USDC per WETH"
- ❌ "The price is 2000" (missing units and direction)

### 5. V4 Pool ID vs Pool Address

V4 pools don't have contract addresses. They have **Pool IDs** (32-byte keccak256 hashes of the V4 Pool Key).

- ✅ "The V4 pool ID is 0xabcd..."
- ❌ "The V4 pool address is 0xabcd..."

### 6. V3 vs V4 amountSpecified sign convention

V3 and V4 use **opposite** sign conventions for `amountSpecified`. In V3: positive (> 0) = exact INPUT, negative (< 0) = exact OUTPUT. In V4, the convention is reversed: negative = exact INPUT, positive = exact OUTPUT. This is verified in `v3_simulator.py:93` — `exact_input = amount_specified > 0`. When building swap calldata for V3 arbitrage, always use positive `amountSpecified` for exact-input mode.

- ✅ "V3 exact-input swap with `amountSpecified = 1000000` (positive)"
- ✅ "V4 exact-input swap with `amountSpecified = -1000000` (negative)"
- ❌ "Exact input uses negative amountSpecified" (wrong for V3, correct for V4 — always qualify which version)

## Example dialogue

> **Dev:** "The **PoolManager** updated its state after the swap."
> **Domain expert:** "Which one? **PoolManager** is the V4 on-chain singleton contract. **Pool Tracker** is the off-chain Bot helper that discovers and tracks pools. Completely different things — and now the names make that obvious."
>
> **Dev:** "So a V4 pool is just a regular pool that happens to live inside a **PoolManager**?"
> **Domain expert:** "Yes — it's a **Managed Pool**. It acts like a standalone pool contract — it has its own liquidity, ticks, and fee logic — but many Managed Pools are wrapped by a single **PoolManager** instead of being separate contracts like V2 and V3 pools. That's why V4 pools are identified by **Pool ID**, not address."
>
> **Dev:** "And the **PoolManager** handles everything for its Managed Pools?"
> **Domain expert:** "Right — user positions, asset transfers, swaps, and hook callbacks all flow through the **PoolManager** contract. Each **Managed Pool** is logically independent but physically lives inside the same singleton."
>
> **Dev:** "The V4 **Pool Key** includes the fee — is that the same as V3's fee tier?"
> **Domain expert:** "Similar but V4 fees can be dynamic, not fixed at creation. And watch the representation: V3/V4 fees are **Pip Fees** — integer hundredths of 1% over 1,000,000. V2 uses a **Directional Fee** as a Fraction. Always specify which."
>
> **Dev:** "One more thing — the pool price is 2000."
> **Domain expert:** "2000 what? Always specify direction and units. 'The pool **Price** is 2000 USDC per WETH' — that's clear. Or 'The **Exchange Rate** for WETH→USDC is 1994.2 USDC per WETH' if you're including fees."
