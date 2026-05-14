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
| **LendingRateFetcher** | Fetch per-token lending rates for a block | Lending pools (cToken, yToken, cyToken, aETH, rETH, oracle) |

In addition to typed fetcher protocols, two low-level I/O callbacks exist:

| Callback | Purpose |
|----------|---------|
| **block_number_fetcher** | Returns current block number when `block_identifier` is None |

**Key Principle:** Fetchers decouple on-chain I/O from pool logic. `Bot.build_curve_pool()` injects these callbacks; `CurveStableswapPool` calls them on-demand without managing connections directly.

## Variant Enums

Mainnet Curve pools use different calculation formulas depending on the pool contract version. The variant is determined by the pool address at construction time and stored as an enum on the pool instance. This replaces the former class-level address frozensets (`D_VARIANT_GROUP_*`, `Y_VARIANT_GROUP_*`, `Y_D_VARIANT_GROUP_*`) that coupled configuration data to pool behaviour.

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **DVariant** | An enum identifying which D-calculation formula pair (`calc_d` / `calc_dp`) a pool uses in `_get_d()` | D variant, D group |
| **YVariant** | An enum identifying which Y-calculation formula a pool uses in `_get_y()`; controls amp divisor and c/b formula selection | Y variant, Y group |
| **YDVariant** | An enum identifying which Y_D-calculation formula a pool uses in `_get_y_d()`; controls whether A_PRECISION appears in b/c formulas | Y_D variant, Y_D group |

**DVariant values:** `STANDARD` (standard calc_d + calc_dp), `VARIANT_ALPHA` (variant_alpha d + standard dp), `VARIANT_ALPHA_DP_ALPHA` (variant_alpha d + variant_alpha dp), `VARIANT_DP_ALPHA` (standard d + variant_alpha dp), `VARIANT_BETA_DP` (standard d + variant_beta dp), `VARIANT_GAMMA_DP` (standard d + variant_gamma dp).

**YVariant values:** `STANDARD` (amp with A_PRECISION divisor, c/b with A_PRECISION), `VARIANT_0` (amp without A_PRECISION divisor, c/b without A_PRECISION), `VARIANT_1` (amp with A_PRECISION divisor, c/b without A_PRECISION).

**YDVariant values:** `STANDARD` (b/c without A_PRECISION), `VARIANT_0` (b/c with A_PRECISION).

Variant resolution is done by `resolve_d_variant()`, `resolve_y_variant()`, `resolve_yd_variant()` in `_variant_groups.py`, called by `CurvePoolBuilder.build()` at construction time. The pool class is address-agnostic for D/Y/YD calculations — it only reads the enum values.

## Strategy Enums

Mainnet Curve pools differ in their swap computation path, rate source, and fee application. These were formerly dispatched via `if self.address` blocks in `get_dy()` and `_get_dy_underlying()`. Each pool now receives a `PoolStrategies` frozen dataclass at construction time, combining all orthogonal strategy axes into a single value object.

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **SwapStyle** | An enum identifying which computation path `get_dy()` uses for dy calculation, fee application, and rate conversion order | Fee style, swap type, calculation path |
| **MetapoolRateStyle** | An enum identifying how a metapool constructs its rate tuple in `get_dy()` | Metapool rate, meta style |
| **MetapoolUnderlyingStyle** | An enum identifying how a metapool computes `get_dy_underlying()` | Metapool underlying style |
| **LendingRateStyle** | An enum identifying which `_stored_rates_from_*()` method provides lending rates | Rate source, rate style |
| **PoolStrategies** | A frozen dataclass combining all strategy enums into a single value object passed to the pool constructor | Strategies, pool config |

**SwapStyle values:** `STANDARD` (dy = xp[j] - y - 1, fee, rate convert), `RATE_ADJUSTED` (dy converted before fee), `RATE_ADJUSTED_NO_ONE` (dy converted before fee, no -1 subtraction), `RAW_BALANCE` (no rate conversion), `CRYPTO` (Newton's method, dynamic fee), `LIVE_ADMIN` (live balances minus admin), `LIVE_ADMIN_DYNAMIC` (live balances, dynamic offpeg fee), `LIVE_ADMIN_DYNAMIC_PRECISION` (live balances, precision multipliers for xp), `LIVE_ADMIN_ORACLE` (live balances, oracle rates), `NO_ONE_FEE_RATE` (dy = xp[j] - y without -1, fee, rate convert — AETH/RETH), `CYTOKEN` (dy = xp[j] - y - 1, fee inside rate conversion).

**MetapoolRateStyle values:** `STANDARD` (rate_multipliers[0], VP), `PRECISION_VP` (PRECISION, VP), `REDEMPTION_VP` (redemption_price, VP).

**MetapoolUnderlyingStyle values:** `STANDARD` (default underlying path), `PRECISION_VP` (PRECISION + VP rate tuple for base coin), `REDEMPTION` (redemption price for first coin).

**LendingRateStyle values:** `NONE` (no lending rates, use rate_multipliers), `CTOKEN` (cToken accrual rates), `YTOKEN` (yToken price-per-share rates), `CYTOKEN` (Yearn vault cToken rates), `AETH` (ankrETH ratio rates), `RETH` (rETH oracle rates), `ORACLE` (oracle-based rates).

Strategy resolution is done by `resolve_pool_strategies()` in `_pool_strategies.py`, which combines the address→strategy mapping with variant group resolution. The `PoolStrategies` dataclass is frozen (immutable after creation) and picklable (contains only enums). The pool class is fully address-agnostic — it only reads strategy enum values.

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

### 7. LendingRateFetcher vs provider_call

**Ruling:** Use **LendingRateFetcher** for all lending rate-fetching. The old `provider_call` backdoor has been removed.

Each lending rate variant (cToken, yToken, cyToken, aETH, rETH, oracle) has its own fetcher closure factory method in `CurveFetcherFactory`. The factory creates closures that capture tokens, use_lending, and precision_multipliers at construction time, and use the `ConnectionManager` for I/O when called. The `LendingRateFetcher` protocol provides a single `(block_number) -> tuple[int, ...]` interface, and the pool's `_resolve_rates()` method dispatches to the correct fetcher based on `PoolStrategies.lending_rate_style`.

### 8. Crypto pool vs Stableswap pool

**Ruling:** **Crypto pool** is a Curve pool subclass using the CryptoSwap invariant (dynamic fees, Newton's method). **Stableswap pool** uses the standard x·y·D invariant. Both are represented by `CurveStableswapPool` — the difference is internal dispatch in `get_dy()`.

Don't call crypto pools "volatile pools" (that term is used for Aerodrome constant-product pools).

## Debugging Swap Mismatches

When `get_dy()` disagrees with the on-chain contract call for a specific pool, the mismatch is almost always a wrong strategy enum — not a floating-point or arithmetic error. Use this workflow:

### Step 1: Fetch the verified contract source

```bash
cast source <pool_address> > /tmp/pool_source.vy
```

Curve V1 pools are written in Vyper. The source is the ground truth for all calculation paths.

### Step 2: Verify each strategy enum against the source

| Python enum | Contract reference | What to check |
|-------------|-------------------|---------------|
| `LendingRateStyle` | `USE_LENDING` constant | Does the contract have any `True` values? If all `False`, `LendingRateStyle` must be `NONE`. |
| `SwapStyle` | `get_dy()` function body | Compare the dy formula: presence/absence of `-1`, order of rate conversion vs fee, whether `_stored_rates()` or `_current_rates()` is called. |
| `DVariant` | `get_D()` function body | Check whether `A_PRECISION` appears in the D and D_P formulas. Standard pools divide by `A_PRECISION`; variant_alpha pools omit it. Also check whether the D_P loop has `+ 1` (variant_dp_alpha). |
| `YVariant` | `get_y()` function body | Check whether `self.A` is used directly (VARIANT_0 / VARIANT_1) or scaled by `A_PRECISION` (STANDARD). Check whether the `c` and `b` formulas include `A_PRECISION`. |
| `YDVariant` | `calc_withdraw_one_coin()` function body | Same A_PRECISION check as YVariant, but for the y_d calculation. |
| `MetapoolRateStyle` | `get_dy()` when `base_pool` exists | What rate tuple is constructed? `(PRECISION, VP)`, `(redemption_price, VP)`, or `(rate_multipliers[0], VP)`? |
| `MetapoolUnderlyingStyle` | `get_dy_underlying()` function body | Same analysis for the underlying swap path. |

### Step 3: Check the address mapping

The address→strategy mapping in `_pool_strategies.py` was **derived from old class-level frozensets**, not verified against contract source. Two common errors:

1. **Wrong `LendingRateStyle`.** A pool was grouped into a cToken/yToken frozenset because the old Python code routed it through `_stored_rates_from_ctokens()`, but the contract has `USE_LENDING = [False, ...]`. The old code worked because the `_stored_rates_from_*()` method returns `PRECISION * LENDING_PRECISION` when `use_lending` is all-False — same as `rate_multipliers`. The new code creates a fetcher that makes spurious on-chain calls, potentially returning different rates.

2. **Missing address.** A pool not in the mapping falls through to `PoolStrategies()` defaults (`SwapStyle.STANDARD`, `LendingRateStyle.NONE`). This is correct for plain 2-token stablecoin pools but wrong for unlisted lending, metapool, or crypto pools.

### Step 4: Verify with on-chain call

Compare the contract's output directly:

```bash
cast call <pool_address> "get_dy(int128,int128,uint256)" <i> <j> <dx> --block <block>
```

Then compare against the Python pool's `get_dy()` result. If they match for several block heights and token pairs, the strategy is correct.

### Common pitfalls

- **sUSD pool pattern.** Pool `0xA5407eAE` holds DAI, USDC, USDT, sUSD (no cTokens) but the old code had it in the `CTOKEN_ADDRESSES` frozenset. The contract's `USE_LENDING = [False, False, False, False]` means `_stored_rates()` just returns `PRECISION_MUL * LENDING_PRECISION`.
- **Y pool sub-variants.** Some Y pools use `(xp[j] - y - 1)` in the dy formula (`RATE_ADJUSTED`) while others use `(xp[j] - y)` without the `-1` (`RATE_ADJUSTED_NO_ONE`). The Vyper source is the only way to distinguish.
- **A_PRECISION in get_y.** The contract stores `A` as the raw value (not scaled by `A_PRECISION`). But degenbot stores `self.a_coefficient` and scales it in `_a()`. When `YVariant.VARIANT_0` divides `amp` by `A_PRECISION` before passing to `_get_d()`, the intermediate values change — this is correct, matching the contract's direct use of `self.A`. But the D calculation must also use a matching `d_variant` that omits `A_PRECISION` from the formula.
