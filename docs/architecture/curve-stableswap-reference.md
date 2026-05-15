# Curve StableSwap — Implementation Reference

Operational details for Curve StableSwap pools that don't belong in the domain glossary (`src/degenbot/curve/CONTEXT.md`).

## Variant Enum Values

Mainnet Curve pools use different calculation formulas depending on the pool contract version. The variant is determined by the pool address at construction time and stored as an enum on the pool instance.

### DVariant

Identifies which D-calculation formula pair (`calc_d` / `calc_dp`) a pool uses in `_get_d()`.

| Value | `calc_d` | `calc_dp` |
|-------|----------|-----------|
| `STANDARD` | Standard formula | Standard formula |
| `VARIANT_ALPHA` | Variant alpha formula | Standard formula |
| `VARIANT_ALPHA_DP_ALPHA` | Variant alpha formula | Variant alpha formula |
| `VARIANT_DP_ALPHA` | Standard formula | Variant alpha formula |
| `VARIANT_BETA_DP` | Standard formula | Variant beta formula |
| `VARIANT_GAMMA_DP` | Standard formula | Variant gamma formula |

Resolution: `resolve_d_variant()` in `_variant_groups.py`, called by `CurvePoolBuilder.build()`.

### YVariant

Identifies which Y-calculation formula a pool uses in `_get_y()`. Controls amp divisor and c/b formula selection.

| Value | Amp divisor | c/b formula |
|-------|------------|-------------|
| `STANDARD` | With `A_PRECISION` | With `A_PRECISION` |
| `VARIANT_0` | Without `A_PRECISION` | Without `A_PRECISION` |
| `VARIANT_1` | With `A_PRECISION` | Without `A_PRECISION` |

Resolution: `resolve_y_variant()` in `_variant_groups.py`.

### YDVariant

Identifies which Y_D-calculation formula a pool uses in `_get_y_d()`. Controls whether `A_PRECISION` appears in b/c formulas.

| Value | b/c formula |
|-------|-------------|
| `STANDARD` | Without `A_PRECISION` |
| `VARIANT_0` | With `A_PRECISION` |

Resolution: `resolve_yd_variant()` in `_variant_groups.py`.

## Strategy Enum Values

Mainnet Curve pools differ in their swap computation path, rate source, and fee application. Each pool receives a `PoolStrategies` frozen dataclass at construction time.

### SwapStyle

Identifies which computation path `get_dy()` uses for dy calculation, fee application, and rate conversion order.

| Value | dy formula | Fee | Rate |
|-------|-----------|-----|------|
| `STANDARD` | `xp[j] - y - 1` | After dy | After fee |
| `RATE_ADJUSTED` | Converted before fee | After conversion | Before fee |
| `RATE_ADJUSTED_NO_ONE` | Converted before fee (no `-1`) | After conversion | Before fee |
| `RAW_BALANCE` | No rate conversion | — | — |
| `CRYPTO` | Newton's method | Dynamic fee | — |
| `LIVE_ADMIN` | Live balances minus admin | — | — |
| `LIVE_ADMIN_DYNAMIC` | Live balances, dynamic offpeg fee | Dynamic | — |
| `LIVE_ADMIN_DYNAMIC_PRECISION` | Live balances, precision multipliers for xp | Dynamic | — |
| `LIVE_ADMIN_ORACLE` | Live balances, oracle rates | — | Oracle |
| `NO_ONE_FEE_RATE` | `xp[j] - y` (no `-1`) | After dy | After fee |
| `CYTOKEN` | `xp[j] - y - 1` | Inside rate conversion | — |

### MetapoolRateStyle

Identifies how a metapool constructs its rate tuple in `get_dy()`.

| Value | Rate tuple |
|-------|-----------|
| `STANDARD` | `(rate_multipliers[0], VP)` |
| `PRECISION_VP` | `(PRECISION, VP)` |
| `REDEMPTION_VP` | `(redemption_price, VP)` |

### MetapoolUnderlyingStyle

Identifies how a metapool computes `get_dy_underlying()`.

| Value | Underlying path |
|-------|---------------|
| `STANDARD` | Default underlying path |
| `PRECISION_VP` | `PRECISION` + VP rate tuple for base coin |
| `REDEMPTION` | Redemption price for first coin |

### LendingRateStyle

Identifies which `_stored_rates_from_*()` method provides lending rates.

| Value | Rate source |
|-------|-----------|
| `NONE` | No lending rates; uses `rate_multipliers` |
| `CTOKEN` | cToken accrual rates |
| `YTOKEN` | yToken price-per-share rates |
| `CYTOKEN` | Yearn vault cToken rates |
| `AETH` | ankrETH ratio rates |
| `RETH` | rETH oracle rates |
| `ORACLE` | Oracle-based rates |

Strategy resolution is done by `resolve_pool_strategies()` in `_pool_strategies.py`, which combines the address→strategy mapping with variant group resolution. The `PoolStrategies` dataclass is frozen (immutable after creation) and picklable (contains only enums).

## Crypto Pool Internals

Crypto pools (e.g., Tricrypto USDT-WBTC-WETH) use a fundamentally different calculation path from stableswap pools.

### Components

| Component | Purpose | I/O Required |
|-----------|---------|--------------|
| `D` | Current invariant value | Fetched on-chain via `DFetcher` |
| `gamma` | Curve shape parameter | Fetched on-chain via `GammaFetcher` |
| `price_scale` | Current prices of volatile assets | Fetched on-chain via `PriceScaleFetcher` |
| `_newton_y()` | Newton's method solver for y | Pure math (no I/O) |
| `_reduction_coefficient()` | Fee reduction based on imbalance | Pure math (no I/O) |
| Dynamic fee | Interpolation between mid_fee and out_fee using fee_gamma | Uses pool state (no I/O) |

### Dynamic fee formula

```
fee_calc = (mid_fee * f + out_fee * (10^18 - f)) / 10^18
where f = _reduction_coefficient(xp, fee_gamma)
```

### Crypto pool detection

A pool with `fee_gamma > 0` is identified as a crypto pool during `the Curve Pool Builder (invoked via Bot.build_pool())`. This triggers creation of the D, gamma, and price_scale fetchers.

### Contract parameters

| Parameter | Purpose | Source |
|-----------|---------|--------|
| `fee_gamma` | Fee curve parameter adjusting fees based on imbalance | `pool.fee_gamma()` |
| `mid_fee` | Mid-range fee percentage | `pool.mid_fee()` |
| `out_fee` | Outlier fee percentage at extreme imbalance | `pool.out_fee()` |
| `gamma` | Curve shape parameter for CryptoSwap invariant | `pool.gamma()` |
| `price_scale` | Current price of volatile assets | `pool.price_scale(uint256)` |
| `offpeg_fee_multiplier` | Fee multiplier for off-peg swaps in some lending pools | `pool.offpeg_fee_multiplier()` |

## Detection Heuristics

These detection heuristics currently live in `CurvePoolBuilder.build()`. Plan 018 proposes decomposing them into standalone detector modules (`CoinDiscovery`, `LendingDetector`, `MetapoolDetector`, `CryptoDetector`, `ARampingDetector`), each returnable as a frozen dataclass and independently testable with a fake provider.

### Metapool Detection

1. Check Registry `is_meta(pool_address)` if available
2. Check Factory `is_meta(pool_address)` as fallback
3. If neither works, check if second coin is 3Crv LP token (`0x6c3F90f043a72FA612CbAC8115EEe7f52CdE6E490`)
4. Fall back to `base_pool()` / `get_base_pool()` contract methods
5. If all fail, mark as plain pool (is_meta = False)

### Lending Token Detection

| Token Type | Detection Method | Why This Method |
|------------|------------------|-----------------|
| cToken | `isCToken()` returns True | Avoids false positives from `exchangeRateStored()` (WETH responds but isn't lending) |
| yToken | `token()` method returns underlying | More reliable than `getPricePerFullShare()` which WETH also responds to |
| cyToken | `isCToken()` + `token()` both return underlying | Yearn vault tokens that are also Compound tokens |
| aETH | Detected by `rate()` method returning ETH ratio | Lido-style staking wrappers |
| rETH | Detected by specific oracle method check | Rocket Pool token |
| Plain Token | None of the above checks succeed | No rate conversion needed |

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

## Error Types

| Exception | When Raised |
|-----------|-------------|
| `CurveError` | Base class for all Curve-specific errors |
| `MissingCurveData` | Required on-chain data unavailable via fetchers (e.g., D_fetcher is None for a crypto pool) |
| `BrokenPool` | Pool has < 2 tokens or returns invalid data |
| `InvalidSwapInputAmount` | Swap amount exceeds available liquidity |
| `NoLiquidity` | Pool has zero reserves for the requested direction |

## Debugging Swap Mismatches

When `get_dy()` disagrees with the on-chain contract call for a specific pool, the mismatch is almost always a wrong strategy enum — not a floating-point or arithmetic error.

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

The address→strategy mapping in `_pool_strategies.py` was derived from old class-level frozensets, not verified against contract source. Two common errors:

1. **Wrong `LendingRateStyle`.** A pool was grouped into a cToken/yToken frozenset because the old Python code routed it through `_stored_rates_from_ctokens()`, but the contract has `USE_LENDING = [False, ...]`. The old code worked because the `_stored_rates_from_*()` method returns `PRECISION * LENDING_PRECISION` when `use_lending` is all-False — same as `rate_multipliers`. The new code creates a fetcher that makes spurious on-chain calls, potentially returning different rates.

2. **Missing address.** A pool not in the mapping falls through to `PoolStrategies()` defaults (`SwapStyle.STANDARD`, `LendingRateStyle.NONE`). This is correct for plain 2-token stablecoin pools but wrong for unlisted lending, metapool, or crypto pools.

### Step 4: Verify with on-chain call

```bash
cast call <pool_address> "get_dy(int128,int128,uint256)" <i> <j> <dx> --block <block>
```

Compare against the Python pool's `get_dy()` result. If they match for several block heights and token pairs, the strategy is correct.

### Common pitfalls

- **sUSD pool pattern.** Pool `0xA5407eAE` holds DAI, USDC, USDT, sUSD (no cTokens) but the old code had it in the `CTOKEN_ADDRESSES` frozenset. The contract's `USE_LENDING = [False, False, False, False]` means `_stored_rates()` just returns `PRECISION_MUL * LENDING_PRECISION`.
- **Y pool sub-variants.** Some Y pools use `(xp[j] - y - 1)` in the dy formula (`RATE_ADJUSTED`) while others use `(xp[j] - y)` without the `-1` (`RATE_ADJUSTED_NO_ONE`). The Vyper source is the only way to distinguish.
- **A_PRECISION in get_y.** The contract stores `A` as the raw value (not scaled by `A_PRECISION`). But degenbot stores `self.a_coefficient` and scales it in `_a()`. When `YVariant.VARIANT_0` divides `amp` by `A_PRECISION` before passing to `_get_d()`, the intermediate values change — this is correct, matching the contract's direct use of `self.A`. But the D calculation must also use a matching `d_variant` that omits `A_PRECISION` from the formula.
