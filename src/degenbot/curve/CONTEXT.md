# Context — Curve StableSwap Pools

## Overview

Curve StableSwap pools are optimized AMMs for price-pegged assets (stablecoins, liquid staking derivatives, yield-bearing tokens). The degenbot implementation follows an **I/O-free architecture** where on-chain data fetching is decoupled via injected callback protocols.

## Pool Types

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Stableswap Pool** | A Curve V1-style pool using the StableSwap invariant (x·y·D formula with amplification coefficient A) | Stable pool, Curve pool (use specific version) |
| **Metapool** | A Stableswap pool where one coin is another pool's LP token (typically 3Crv), enabling nested liquidity | Meta pool, nested pool |
| **Base Pool** | The underlying pool whose LP token is used as a coin in a metapool (e.g., 3Crv tripool) | Underlying pool, parent pool |
| **Lending Pool** | A pool containing interest-bearing tokens (cTokens, yTokens) requiring rate conversion to underlying | cToken pool, Compound pool |
| **Crypto Pool** | A Stableswap pool with volatile assets (e.g., Tricrypto) using dynamic fees (fee_gamma, mid_fee, out_fee) | Volatile pool, crypto-stable pool |
| **Plain Pool** | A simple Stableswap pool with 2-8 plain ERC-20 tokens (no lending rates, no base pool) | Direct pool, standard pool |

## Pool State Parameters

| Term | Definition | Notes |
|------|------------|-------|
| **A Coefficient** | The amplification parameter controlling price slippage in the StableSwap invariant | Can ramp over time between A and future_A |
| **D** | The invariant value representing total pool liquidity when all tokens have equal value | Used in `_get_y()` calculations |
| **Virtual Price** | The price of pool LP token relative to underlying; increases with fees | Used by metapools to value base pool LP tokens |
| **Stored Rates** | The exchange rates for lending tokens (cTokens, yTokens) to their underlying assets | Updated per-block via fetcher callbacks |
| **Precision Multipliers** | Scaling factors for token decimals to normalize calculations (10^(18 - decimals)) | For cTokens, use **underlying token decimals**, not cToken decimals |
| **Admin Balances** | Accumulated fees held by pool admin before distribution | Accessed via `admin_balances()` or `admin_balances(uint256)` |

## Fetcher Protocols

The I/O-free architecture uses **fetcher callbacks** injected at construction. These are `Protocol` types defining callable interfaces:

| Protocol | Purpose | Called When |
|----------|---------|-------------|
| **RateFetcher** | Fetch lending token rates (cToken/yToken → underlying) | Pool has cTokens (isCToken) or yTokens |
| **VirtualPriceFetcher** | Fetch base pool virtual price | Pool is a metapool (is_meta is True) |
| **TimestampFetcher** | Fetch block timestamp for given block | A coefficient ramping calculations |
| **RedemptionPriceFetcher** | Fetch LSD redemption price (e.g., stETH, frxETH) | Pool wraps liquid staking derivatives |
| **AdminBalancesFetcher** | Fetch admin fee balances | Pool uses admin balance tracking |

**Key Principle:** Fetchers decouple on-chain I/O from pool logic. `Bot.build_curve_pool()` injects these callbacks; `CurveStableswapPool` calls them on-demand without managing connections directly.

## Detection Heuristics

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
| **Plain Token** | Neither check succeeds | No rate conversion needed |

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
| **price_scale** | Current price of volatile assets | Pool contract `price_scale()` or `price_oracle()` |
| **price_oracle** | Exponential moving average price | Pool contract `price_oracle()` |

**Note:** Dynamic fee calculation uses `fee_gamma` to interpolate between `mid_fee` and `out_fee` based on price deviation from `price_scale`.

## Error Types

| Exception | When Raised |
|-----------|-------------|
| **CurveError** | Base class for all Curve-specific errors |
| **MissingCurveData** | Required on-chain data unavailable via fetchers |
| **BrokenPool** | Pool has < 2 tokens or returns invalid data |
| **InvalidSwapInputAmount** | Swap amount exceeds available liquidity |
| **NoLiquidity** | Pool has zero reserves for the requested direction |

## Relationships

- A **Metapool** has exactly one **Base Pool** (via base_pool_address)
- A **Pool** has 2-8 **Tokens** (coins[] array)
- A **Lending Pool** has **Stored Rates** for each lending token
- A **Pool State** includes balances, rates, and virtual price at a specific block
- A **Fetcher** is injected per-pool by **Bot.build_curve_pool()**
- **A Coefficient** ramping uses **TimestampFetcher** to calculate time-weighted values

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
