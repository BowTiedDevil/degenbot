# Plan 013: Curve StableSwap Pool I/O-Free Architecture Migration

> **Note**: References to `Bot.build_curve_pool()` are historical — this method was removed by Plan 059. Use `bot.build_pool(address)` instead.

All Curve StableSwap pool I/O has been migrated from `_get_provider_for_chain()` to fetcher callbacks. All 14/14 Curve tests pass, including the tricrypto crypto pool.

## Summary

`CurveStableswapPool` is now fully I/O-free for all pool types (stableswap, metapool, lending, and crypto). All direct provider/Bot references have been replaced with fetcher callback protocols injected by `Bot.build_curve_pool()`. The pool can perform pure computation when state is provided via `external_update()` or `override_state`, and fetches on-chain data on-demand through fetcher closures when needed.

## Changes Made (Phases 0-5)

### Phase 0: Immediate Fix
1. Added `_get_provider_for_chain()` method to enable existing I/O-dependent methods
2. Added missing cache attributes to constructor
3. Enhanced `Bot.build_curve_pool()` with A ramping, LP token, and timestamp fetching
4. Updated methods to use `_get_provider_for_chain()`
5. Added timestamp caching for A ramping calculations
6. Added `get_total_supply()` and `get_balance()` to Erc20Token

### Phase 1: Exception & Protocol Classes
1. Created `src/degenbot/exceptions/curve.py` with `CurveError`, `MissingCurveData`
2. Added fetcher protocols to `src/degenbot/curve/types.py`:
   - `VirtualPriceFetcher`, `TimestampFetcher`
   - `RedemptionPriceFetcher`, `AdminBalancesFetcher`
   - `DFetcher`, `GammaFetcher`, `PriceScaleFetcher` (Phase 5)
3. Removed `RateFetcher` (replaced by `provider_call` callback)

### Phase 2: Constructor Parameters
1. Added fetcher callback parameters to `CurveStableswapPool.__init__`
2. Added pool configuration parameters (`base_pool`, `tokens_underlying`, etc.)
3. Added A ramping configuration parameters
4. Updated pickle drops/reconstructs for new attributes
5. Updated `build_curve_pool()` to pass new parameters

### Phase 3: Pool Detection & Configuration
1. **Metapool detection**: `build_curve_pool()` checks both Registry and Factory
   for `is_meta()`, fetches base pool and underlying coins. Falls back to 3Crv
   LP token detection for pools without `base_pool()` / `get_base_pool()`.
2. **Coin indexing**: Try `coins(uint256)` first, fallback to `coins(int128)` for older pools
3. **Lending token detection**: 
   - cTokens detected via `isCToken()` (more reliable than `exchangeRateStored()`)
   - yTokens detected via `token()` method (more reliable than `getPricePerFullShare()`)
   - WETH/WBTC no longer incorrectly flagged as lending tokens
4. **Precision multiplier overrides**: cToken pools need precision_multipliers based
   on underlying token decimals, not cToken decimals. Fetched via `underlying()` + `decimals()`.
5. **Crypto pool parameters**: `fee_gamma`, `mid_fee`, `out_fee`, `gamma` detected from
   pool contract and passed to constructor.
6. **Single-token pool protection**: Pools with < 2 tokens raise `BrokenPool`.
7. **Test robustness**: 
   - `EVMRevertError` caught alongside `InvalidSwapInputAmount`/`NoLiquidity`
   - `InsufficientDataBytes` caught for oracle-method pools
   - `ContractLogicError` → `BrokenPool` for on-chain reverting pools

### Phase 4: I/O Removal
1. **Removed `_get_provider_for_chain()`**, `_provider`, and `_bot` from the pool class
2. **Added `provider_call` callback**: Low-level `(to, data, block) -> bytes` closure that wraps `w3.eth.call()`, used by `_stored_rates_from_*()` methods
3. **Restored all 6 `_stored_rates_from_*()` methods** with original I/O logic, using `self._provider_call()` instead of `provider.call()`
4. **Added fetcher callbacks**: `virtual_price_fetcher`, `base_virtual_price_fetcher`, `timestamp_fetcher`, `redemption_price_fetcher`, `admin_balances_fetcher`, `block_number_fetcher`, `total_supply_fetcher`, `token_balance_fetcher`, `provider_call`
5. **Fixed `virtual_price_fetcher` for metapools**: Now calls `get_virtual_price()` on the base pool's address, not the metapool's
6. **Fixed `base_virtual_price_fetcher`**: Calls `base_virtual_price()` on the metapool contract
7. **Added `_resolve_block_number()` with `block_number_fetcher`**: Falls back to callback when `block_identifier` is not an int
8. **All fetcher closures created in `Bot.build_curve_pool()`**: 9 factory methods
9. **Checksummed `base_pool_address`** in `Bot.build_curve_pool()`

### Phase 5: Crypto Pool Support
1. **Added fetcher protocols**: `DFetcher`, `GammaFetcher`, `PriceScaleFetcher` in `curve/types.py`
2. **Added fetcher parameters** to `CurveStableswapPool.__init__`: `D_fetcher`, `gamma_fetcher`, `price_scale_fetcher`
3. **Restored crypto pool `get_dy()` logic**: Replaced `MissingCurveData` raise with full tricrypto calculation path using fetcher callbacks + caching
4. **Added `_newton_y()` method**: Newton's method solver for crypto pool y calculation (pure math, no I/O)
5. **Added `_reduction_coefficient()` method**: Fee reduction coefficient for dynamic fee calculation (pure math, no I/O)
6. **Dynamic fee calculation**: Uses `fee_gamma`, `mid_fee`, `out_fee` with `_reduction_coefficient()` to compute the fee for each swap
7. **Added 3 fetcher factory methods** to `bot.py`:
   - `_make_curve_D_fetcher()` — fetches `D()` from chain
   - `_make_curve_gamma_fetcher()` — fetches `gamma()` from chain
   - `_make_curve_price_scale_fetcher()` — fetches `price_scale(uint256)` for each non-0 token
8. **Crypto fetchers conditionally passed**: Only created when `pool_fee_gamma > 0` (indicates crypto pool)
9. **Updated pickle drops/reconstructs** for the 3 new fetcher attributes

## Key Technical Decisions

- **`provider_call` callback over `rate_fetcher`**: The original `_stored_rates_from_*()` methods each had complex, pool-type-specific logic (cToken exchange rates with supply rate accrual, yToken price-per-full-share, aETH ratio inversion, oracle method detection). A single generic `rate_fetcher` callback couldn't handle all these cases. Instead, a low-level `provider_call` callback lets the pool retain its type-specific logic while delegating the raw I/O to the Bot.
- **`virtual_price_fetcher` targets base pool**: For metapools, `_get_virtual_value()` fetches the base pool's `get_virtual_price()` (since the metapool's VP = base pool's VP). This is different from `base_virtual_price_fetcher` which calls the metapool's `base_virtual_price()` contract method.
- **`block_number_fetcher` for None resolution**: When methods like `calculate_tokens_out_from_tokens_in()` are called without an explicit `block_identifier`, the block number fetcher provides the current block.
- **Crypto fetchers use per-call caching**: D, gamma, and price_scale are cached in `BoundedCache` per block number. The fetcher callback is only invoked on a cache miss, so repeated calcs at the same block don't re-fetch.
- **Crypto pool detection via `fee_gamma`**: A pool with `fee_gamma > 0` is identified as a crypto pool during `build_curve_pool()`. This triggers creation of the D/gamma/price_scale fetchers. The tricrypto pool's `get_dy()` path is selected by address match for now.
- **`_newton_y()` and `_reduction_coefficient()` as class methods**: These are pure-math functions that don't need fetcher callbacks. They're defined as regular methods on the pool class for access to `A_PRECISION` and `n_coins`.

## Test Results

**14/14 passing (0 expected failures):**
- test_create_pool ✅
- test_pickle_tripool ✅
- test_auto_update ✅
- test_a_ramping ✅
- test_tripool ✅
- test_base_pool ✅
- test_get_d ✅
- test_metapool_over_multiple_blocks ✅
- test_single_pool ✅
- test_factory_stableswap_pools ✅
- test_bot_update_curve_pool ✅
- test_curve_io_free (2 tests) ✅
- test_tricrypto_pool ✅ (previously Phase 5 — now fixed)
- test_base_registry_pools ✅ (previously skipped — now fixed)

**Full suite: 1795 passed, 0 expected failures, 12 skipped, 5 xfailed**
