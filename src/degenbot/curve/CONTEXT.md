# Context — Curve StableSwap Pools

## Overview

Curve StableSwap pools are optimized AMMs for price-pegged assets (stablecoins, liquid staking derivatives, yield-bearing tokens). The degenbot implementation follows an **I/O-free architecture** where on-chain data fetching is decoupled via injected callback protocols. All pool types (stableswap, metapool, lending, and crypto) are fully I/O-free.

## Pool Types

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Stableswap Pool** | A Curve V1-style pool using the StableSwap invariant (x·y·D formula with amplification coefficient A) | Stable pool, Curve pool (use specific version) |
| **Metapool** | A Stableswap pool where one coin is another pool's LP token (typically 3Crv), enabling nested liquidity | Meta pool, nested pool |
| **Base Pool** | The underlying pool whose LP token is used as a coin in a metapool (e.g., 3Crv tripool) | Underlying pool, parent pool |
| **Lending Pool** | A pool containing interest-bearing tokens (cTokens, yTokens) requiring rate conversion to underlying | cToken pool, Compound pool |
| **Crypto Pool** | A Curve pool with volatile assets (e.g., Tricrypto) using dynamic fees and Newton's method for y-calculation | Volatile pool, crypto-stable pool |
| **Plain Pool** | A simple Stableswap pool with 2-8 plain ERC-20 tokens (no lending rates, no base pool) | Direct pool, standard pool |

## Pool State Parameters

| Term | Definition | Notes |
|------|------------|-------|
| **A Coefficient** | The amplification parameter controlling price slippage in the StableSwap invariant | Can ramp over time between A and future_A |
| **D** | The invariant value representing total pool liquidity when all tokens have equal value | For crypto pools, fetched on-chain via D_fetcher |
| **Virtual Price** | The price of pool LP token relative to underlying; increases with fees | Used by metapools to value base pool LP tokens |
| **Stored Rates** | The exchange rates for lending tokens (cTokens, yTokens) to their underlying assets | Updated per-block via fetcher callbacks |
| **Precision Multipliers** | Scaling factors for token decimals to normalize calculations (10^(18 - decimals)) | For cTokens, use **underlying token decimals**, not cToken decimals |
| **Admin Balances** | Accumulated fees held by pool admin before distribution | Accessed via `admin_balances()` or `admin_balances(uint256)` |

## Fetcher Protocols

The I/O-free architecture uses **fetcher callbacks** injected at construction. These are `Protocol` types defining callable interfaces:

| Protocol | Purpose | Called When |
|----------|---------|-------------|
| **VirtualPriceFetcher** | Fetch base pool virtual price | Pool is a metapool (is_meta is True) |
| **TimestampFetcher** | Fetch block timestamp for given block | A coefficient ramping calculations |
| **RedemptionPriceFetcher** | Fetch LSD redemption price (e.g., stETH, frxETH) | Pool wraps liquid staking derivatives |
| **AdminBalancesFetcher** | Fetch admin fee balances | Pool uses admin balance tracking |
| **DFetcher** | Fetch on-chain invariant D | Crypto pool y-calculation |
| **GammaFetcher** | Fetch on-chain gamma parameter | Crypto pool dynamic fee calculation |
| **PriceScaleFetcher** | Fetch on-chain price_scale values | Crypto pool multi-asset price normalization |

In addition to typed fetcher protocols, two low-level I/O callbacks exist:

| Callback | Purpose |
|----------|---------|
| **provider_call** | Raw `(to, data, block) -> bytes` closure wrapping `w3.eth.call()`; used by `_stored_rates_from_*()` methods for type-specific rate logic (cToken accrual, yToken PPS, aETH ratio, oracle method) |
| **block_number_fetcher** | Returns current block number when `block_identifier` is None |

**Key Principle:** Fetchers decouple on-chain I/O from pool logic. `Bot.build_curve_pool()` injects these callbacks; `CurveStableswapPool` calls them on-demand without managing connections directly.

## Crypto Pool Details

Crypto pools (e.g., Tricrypto USDT-WBTC-WETH) use a fundamentally different calculation path from stableswap pools:

| Component | Purpose | I/O Required |
|-----------|---------|--------------|
| **D** | Current invariant value | Fetched on-chain via `DFetcher` |
| **gamma** | Curve shape parameter | Fetched on-chain via `GammaFetcher` |
| **price_scale** | Current prices of volatile assets | Fetched on-chain via `PriceScaleFetcher` |
| **_newton_y()** | Newton's method solver for y | Pure math (no I/O) |
| **_reduction_coefficient()** | Fee reduction based on imbalance | Pure math (no I/O) |
| **Dynamic fee** | Interpolation between mid_fee and out_fee using fee_gamma | Uses pool state (no I/O) |

**Dynamic fee formula:** `fee_calc = (mid_fee * f + out_fee * (10^18 - f)) / 10^18`, where `f = _reduction_coefficient(xp, fee_gamma)`.

**Crypto pool detection:** A pool with `fee_gamma > 0` is identified as a crypto pool during `Bot.build_curve_pool()`. This triggers creation of the D, gamma, and price_scale fetchers.

## Detection Heuristics

> **Note:** These detection heuristics currently live in `CurvePoolBuilder.build()`. Plan 018 proposes decomposing them into standalone detector modules (`CoinDiscovery`, `LendingDetector`, `MetapoolDetector`, `CryptoDetector`, `ARampingDetector`), each returnable as a frozen dataclass and independently testable with a fake provider.

### Metapool Detection

1. Check Registry `is_meta(pool_address)` if available
2. Check Factory `is_meta(pool_address)` as fallback
3. If neither works, check if second coin is 3Crv LP token (`0x6c3F90f043a72FA612CbAC8115EEe7f52CdE6E490`)
4. Fall back to `base_pool()` / `get_base_pool()` contract methods
5. If all fail, mark as plain pool (is_meta = False)

### Lending Token Detection

| Token Type | Detection Method | Why This Method |
|------------|------------------|-----------------|
| **cToken** | `isCToken()` returns True | Avoids false positives from `exchangeRateStored()` (WETH responds but isn't lending) |
| **yToken** | `token()` method returns underlying | More reliable than `getPricePerFullShare()` which WETH also responds to |
| **cyToken** | `isCToken()` + `token()` both return underlying | Yearn vault tokens that are also Compound tokens |
| **aETH** | Detected by `rate()` method returning ETH ratio | Lido-style staking wrappers |
| **rETH** | Detected by specific oracle method check | Rocket Pool token |
| **Plain Token** | None of the above checks succeed | No rate conversion needed |

### Coin Indexing

1. Try `coins(uint256)` first (modern pools)
2. Fall back to `coins(int128)` (legacy pools using older Solidity int types)
3. Map coins[] indices to underlying_coins[] if underlying differs

### Precision Multipliers for cTokens

**Critical:** cTokens have different decimals than their underlying (e.g., cDAI = 8 decimals, DAI = 18 decimals). The precision multiplier must use **underlying token decimals**:

```
cToken_decimals = 8
underlying_decimals = 18
multiplier = 10^(18 - underlying_decimals) = 1  (not 10^10)
```

This is fetched via `underlying()` contract call + `decimals()` on the underlying token.

## Crypto Pool Parameters

| Parameter | Purpose | Source |
|-----------|---------|--------|
| **fee_gamma** | Fee curve parameter adjusting fees based on imbalance | Pool contract `fee_gamma()` |
| **mid_fee** | Mid-range fee percentage | Pool contract `mid_fee()` |
| **out_fee** | Outlier fee percentage at extreme imbalance | Pool contract `out_fee()` |
| **gamma** | Curve shape parameter for CryptoSwap invariant | Pool contract `gamma()` |
| **price_scale** | Current price of volatile assets | Pool contract `price_scale(uint256)` |
| **offpeg_fee_multiplier** | Fee multiplier for off-peg swaps in some lending pools | Pool contract `offpeg_fee_multiplier()` |

## Error Types

| Exception | When Raised |
|-----------|-------------|
| **CurveError** | Base class for all Curve-specific errors |
| **MissingCurveData** | Required on-chain data unavailable via fetchers (e.g., D_fetcher is None for a crypto pool) |
| **BrokenPool** | Pool has < 2 tokens or returns invalid data |
| **InvalidSwapInputAmount** | Swap amount exceeds available liquidity |
| **NoLiquidity** | Pool has zero reserves for the requested direction |

## Pool Manager

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **CurveStableswapPoolManager** | A pool manager that tracks Curve StableSwap pools and delegates construction to Bot | Curve manager |

## Relationships

- A **Metapool** has exactly one **Base Pool** (via base_pool_address)
- A **Pool** has 2-8 **Tokens** (coins[] array)
- A **Lending Pool** has **Stored Rates** for each lending token
- A **Pool State** includes balances, rates, and virtual price at a specific block
- A **Fetcher** is injected per-pool by **Bot.build_curve_pool()**
- **A Coefficient** ramping uses **TimestampFetcher** to calculate time-weighted values
- A **Crypto Pool** uses **DFetcher**, **GammaFetcher**, and **PriceScaleFetcher** for y-calculation and dynamic fees
- A **CurveStableswapPoolManager** tracks Curve pools and delegates construction/query to **Bot**

## Resolved Ambiguities

### 1. Coin vs Token vs Underlying

**Ruling:** Use **coin** for Curve pool terminology; use **token** for ERC-20 contracts; use **underlying** for the base asset of a lending token.

- ✅ "The pool has 3 **coins**: DAI, USDC, USDT"
- ✅ "The **token** at address 0xA0b8… is cDAI"
- ✅ "The **underlying** for cDAI is DAI"
- ❌ "The pool has 3 tokens" (use **coins** for Curve terminology)
- ❌ "The underlying token" (redundant — use **underlying** or **underlying coin**)

### 2. Rate Fetcher Unit

**Ruling:** Rates are always returned as `PRECISION` (10^18) scaled integers, regardless of token decimals. Non-lending tokens return exactly `PRECISION`.

- ✅ "The rate for cDAI is 1.02e18"
- ❌ "The rate for cDAI is 1.02" (missing PRECISION scaling)

### 3. Base Pool Detection Priority

**Ruling:** Registry check first, Factory check second, 3Crv LP token check third, contract methods last.

This order ensures newest pools (in Factory) are correctly identified even if Registry has stale data.

### 4. Virtual Price Source for Metapools

**Ruling:** Use **base pool** virtual price, not the metapool's own virtual price, when valuing the base pool LP token coin.

The metapool's coins include the base pool LP token; the virtual price of that LP token comes from its base pool, not the metapool.

### 5. Lending Token Detection Methods

**Ruling:** Prefer `isCToken()` and `token()` over `exchangeRateStored()` and `getPricePerFullShare()`.

**Why:** WETH and other non-lending tokens respond to `exchangeRateStored()` and `getPricePerFullShare()` due to fallback behaviors, causing false positives. `isCToken()` and `token()` are explicit markers.

### 6. A Coefficient Timing

**Ruling:** A ramping uses **block timestamps**, not block numbers, for time calculations.

The pool contract stores `initial_A_time` and `future_A_time` as Unix timestamps. Use `TimestampFetcher` to get the current block's timestamp for interpolation.

### 7. provider_call vs typed fetchers

**Ruling:** Use **provider_call** for low-level rate-fetching logic that needs pool-type-specific contract decoding. Use **typed fetcher protocols** (DFetcher, etc.) for direct valued returns.

`provider_call` exists because the `_stored_rates_from_*()` methods each have unique decoding logic (cToken supply rate accrual, yToken PPS, aETH ratio inversion, oracle bitmask). A generic typed fetcher can't handle all these cases. For straightforward single-value fetches (D, gamma, price_scale), typed protocols are preferred.

### 8. Crypto pool vs Stableswap pool

**Ruling:** **Crypto pool** is a Curve pool subclass using the CryptoSwap invariant (dynamic fees, Newton's method). **Stableswap pool** uses the standard x·y·D invariant. Both are represented by `CurveStableswapPool` — the difference is internal dispatch in `get_dy()`.

Don't call crypto pools "volatile pools" (that term is used for Aerodrome constant-product pools).
