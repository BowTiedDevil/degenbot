# Context — Curve StableSwap Pools

## Pool Types

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Stableswap Pool** | A Curve V1-style pool using the StableSwap invariant (x·y·D formula with amplification coefficient A) | Stable pool, Curve pool (use specific version) |
| **Metapool** | A Stableswap pool where one coin is another pool's LP token, enabling nested liquidity | Meta pool, nested pool |
| **Base Pool** | The underlying pool whose LP token is used as a coin in a metapool | Underlying pool, parent pool |
| **Lending Pool** | A pool containing interest-bearing tokens requiring rate conversion to underlying | cToken pool, Compound pool |
| **Crypto Pool** | A Curve pool using the CryptoSwap invariant with dynamic fees and Newton's method | Volatile pool, crypto-stable pool |
| **Plain Pool** | A simple Stableswap pool with 2–8 plain ERC-20 tokens (no lending rates, no base pool) | Direct pool, standard pool |

## Pool State Parameters

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **A Coefficient** | The amplification parameter controlling price slippage in the StableSwap invariant | Amplification factor |
| **D** | The invariant value representing total pool liquidity when all tokens have equal value | Invariant |
| **Virtual Price** | The price of a pool LP token relative to underlying | LP price |
| **Stored Rates** | The exchange rates for lending tokens to their underlying assets | Lending rates |
| **Precision Multipliers** | Scaling factors normalizing token decimals to 18 | Decimals adjustment |
| **Admin Balances** | Accumulated fees held by pool admin before distribution | Fee balances |

## Data Provider

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **CurveDataProvider** | A `@runtime_checkable` protocol with 13 methods (`D()`, `gamma()`, `virtual_price()`, `base_virtual_price()`, `price_scale()`, `admin_balances()`, `lending_rate()`, `redemption_price()`, `block_timestamp()`, `block_number()`, `token_balance()`, `token_total_supply()`, `is_crypto()`) that serves as the single I/O seam for Curve pools | Data provider, provider |
| **CurveDataProviderImpl** | The production implementation of `CurveDataProvider` in `data_provider_impl.py` — a structured class with real methods and shared I/O helpers (`_call`, `_call_single`, `_call_raw_single`, `_wrap_revert`); constructed by the builder with a `ProviderAdapter` directly | Impl |
| **FakeCurveDataProvider** | A test double implementing `CurveDataProvider` with fixed return values | Fake provider |
| **Per-block caches** | Private `_cache_*` fields on `CurveStableswapPool` that consolidate all per-block `BoundedCache` instances for on-chain data. Each has a corresponding `_get_cached_*` accessor method implementing the try-cache → call-provider → store → return pattern. Formerly `CurveOnChainCache` (absorbed into the pool by Plan 068). Pickled as direct pool attributes via `PoolPickleMixin`. | On-chain cache, cache fields |
| **Calculation-time I/O** | The property that a pool may call `CurveDataProvider` methods during `get_dy()` and related calculation methods, as opposed to construction-time I/O (which is absent for all pools). Exposed by the `requires_io_at_calculation_time` property. The method `_resolve_calculation_inputs_via_io` signals that I/O may occur during calculation input resolution. | I/O at calc time, runtime I/O |

## Variant Enums

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **DVariant** | An enum identifying which D-calculation formula pair a pool uses | D variant, D group |
| **YVariant** | An enum identifying which Y-calculation formula a pool uses | Y variant, Y group |
| **YDVariant** | An enum identifying which Y_D-calculation formula a pool uses | Y_D variant, Y_D group |

## Strategy Enums

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **SwapStyle** | An enum identifying which computation path `get_dy()` uses for dy calculation. Has a `make_calculator()` factory method returning the matching `DyCalculator` instance. | Fee style, swap type |
| **MetapoolRateStyle** | An enum identifying how a metapool constructs its rate tuple. Has a `make_calculator()` factory method returning the matching metapool `DyCalculator` instance. | Metapool rate |
| **MetapoolUnderlyingStyle** | An enum identifying how a metapool computes `get_dy_underlying()`. Has a `make_calculator()` factory method returning the matching metapool underlying `DyCalculator` instance. | Underlying style |
| **LendingRateStyle** | An enum identifying which rate source provides lending rates | Rate source, rate style |
| **PoolStrategies** | A frozen dataclass combining all strategy enums and DyCalculator instances into a single value object. Auto-constructs calculators from enum values in `__post_init__` via each enum's `make_calculator()` method. Explicitly-passed calculator arguments are preserved. | Strategies, pool config |

## DyCalculator Protocol

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **DyCalculationInputs** | A frozen dataclass constructed by `CurveStableswapPool.get_dy()` carrying all pre-resolved data for a single dy calculation (balances, rates, xp, block data, invariant solver closures, and optional I/O results for crypto/live-admin/metapool). All I/O and cache lookups happen before this object is created — calculators read only from it. | Calculation inputs, inputs object |
| **DyCalculator** | A runtime-checkable protocol defining `calculate(i, j, dx, *, inputs: DyCalculationInputs, override_state) -> int` | Dy strategy, dy solver |
| **StandardDyCalculator** | Computes dy for STANDARD swap style (plain pool, no rate adjustment) | Basic calculator |
| **RateAdjustedDyCalculator** | Computes dy for RATE_ADJUSTED swap style using `rate_multipliers` | Lending calculator |
| **RateAdjustedNoOneDyCalculator** | Computes dy for RATE_ADJUSTED_NO_ONE (no `-1` minimum) | No-one rate calculator |
| **RawBalanceDyCalculator** | Computes dy from raw balances without rate adjustment | Balance calculator |
| **CryptoDyCalculator** | Computes dy using CryptoSwap invariant (Newton's method, dynamic fee, price_scale) | Crypto calculator |
| **LiveAdminDyCalculator** | Computes dy with admin balance subtraction (live A amplification) | Admin calculator |
| **LiveAdminDynamicDyCalculator** | Computes dy with admin balances and dynamic fee | Dynamic admin calculator |
| **LiveAdminDynamicPrecisionDyCalculator** | Computes dy with admin balances, dynamic fee, and precision adjustment | Precision admin calculator |
| **LiveAdminOracleDyCalculator** | Computes dy with admin balances and oracle price | Oracle admin calculator |
| **NoOneFeeRateDyCalculator** | Computes dy without the `-1` slip on fee calculation | No-one fee calculator |
| **CytokenDyCalculator** | Computes dy for cyToken pools with cyToken rate conversion | Cytoken calculator |
| **Metapool*DyCalculator** | A family of calculators (PrecisionVp, RedemptionVp, Standard) for metapool `get_dy` | Metapool rate calculator |
| **MetapoolUnderlying*DyCalculator** | A family of calculators (Redemption, PrecisionVp, Standard) for `get_dy_underlying` | Metapool underlying calculator |

## Pool Tracker

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **CurveStableswapPoolTracker** | A Pool Tracker that tracks Curve StableSwap pools and delegates construction to Bot | Curve manager |

## Relationships

- A **Metapool** has exactly one **Base Pool**
- A **Pool** has 2–8 coins
- A **Lending Pool** has **Stored Rates** for each lending token
- A **Crypto Pool** uses **CurveDataProvider** methods `D()`, `gamma()`, and `price_scale()`
- **A Coefficient** ramping uses **CurveDataProvider** `block_timestamp()`
- A **CurveStableswapPoolTracker** tracks Curve pools and delegates construction to **Bot**
- A **Pool** holds a single **CurveDataProvider** (injected by builder), replacing the former 13 individual fetcher callbacks
- A **Pool** holds per-block **cache fields** (`_cache_*`) with accessor methods (`_get_cached_*`) that implement try-cache→call-provider→store→return; on cache miss they delegate to **CurveDataProvider**
- **DyCalculator** objects are held by **PoolStrategies** and replace dispatch branches in `get_dy()` / `_get_dy_underlying()`
- **DyCalculationInputs** is constructed by `get_dy()` before calling the **DyCalculator**; all I/O, rate resolution, and cache lookups happen in `get_dy()`, so the calculator receives only pre-resolved data with no private member access
- Pure math functions in `calculations/stableswap.py` raise `ValueError`; pool wrappers catch and re-raise as `EVMRevertError`

## Resolved Ambiguities

### 1. Coin vs Token vs Underlying

**Ruling:** Use **coin** for Curve pool terminology; use **token** for ERC-20 contracts; use **underlying** for the base asset of a lending token.

- ✅ "The pool has 3 **coins**: DAI, USDC, USDT"
- ✅ "The **token** at address 0xA0b8… is cDAI"
- ✅ "The **underlying** for cDAI is DAI"
- ❌ "The pool has 3 tokens" (use **coins**)
- ❌ "The underlying token" (use **underlying** or **underlying coin**)

### 2. Rate Fetcher Unit

**Ruling:** Rates are always returned as `PRECISION` (10^18) scaled integers. Non-lending tokens return exactly `PRECISION`.

- ✅ "The rate for cDAI is 1.02e18"
- ❌ "The rate for cDAI is 1.02" (missing PRECISION scaling)

### 3. Base Pool Detection Priority

**Ruling:** Registry → Factory → 3Crv LP token check → contract methods.

### 4. Virtual Price Source for Metapools

**Ruling:** Use **base pool** virtual price when valuing the base pool LP token coin, not the metapool's own virtual price.

### 5. Lending Token Detection Methods

**Ruling:** Prefer `isCToken()` and `token()` over `exchangeRateStored()` and `getPricePerFullShare()` — the latter produce false positives with WETH.

### 6. A Coefficient Timing

**Ruling:** A ramping uses **block timestamps**, not block numbers.

### 7. CurveDataProvider methods vs provider_call

**Ruling:** Use `self._data_provider.lending_rate()` for all lending rate-fetching. The old `provider_call` backdoor has been removed. All I/O flows through the single `CurveDataProvider` seam.

### 8. Crypto pool vs Stableswap pool

**Ruling:** **Crypto pool** = CryptoSwap invariant (dynamic fees, Newton's method). **Stableswap pool** = standard x·y·D invariant. Don't call crypto pools "volatile pools" (that's the Aerodrome term for constant-product pools).

## Example dialogue

> **Dev:** "The Curve **pool** has 3 **tokens**: DAI, USDC, USDT."
> **Domain expert:** "Use **coins** for Curve terminology. The **pool** has 3 **coins**. **Token** is for the ERC-20 contract itself — a coin is what the pool holds."
>
> **Dev:** "And this is a **Crypto pool** because it holds volatile assets?"
> **Domain expert:** "No — **Crypto pool** specifically means a pool using the CryptoSwap invariant with dynamic fees and Newton's method. A StableSwap pool holding volatile assets would still be a **Stableswap pool**. Don't call Crypto pools 'volatile pools' either — that's the Aerodrome term for constant-product pools."
>
> **Dev:** "The metapool's virtual price comes from the metapool itself, right?"
> **Domain expert:** "No — use the **Base Pool's** virtual price when valuing the base pool LP token coin. The metapool's own virtual price is for the metapool's LP token, which is a different thing."
>
> **Dev:** "I need to fetch lending rates. Should I use **provider_call**?"
> **Domain expert:** "No — `provider_call` has been removed. The pool calls `self._data_provider.lending_rate()`, which is part of the **CurveDataProvider** seam. The builder creates a **CurveDataProviderImpl** that takes a `ProviderAdapter` directly; the pool never accesses connections directly. For tests, use a **FakeCurveDataProvider** with fixed return values."
>
> **Dev:** "What are all the **variant enums** for?"
> **Domain expert:** "Mainnet Curve pools use different calculation formulas depending on the contract version. **DVariant**, **YVariant**, and **YDVariant** identify which formula a pool uses for D, y, and y_D calculations respectively. They replace the old class-level address frozensets that coupled configuration data to pool behavior."
>
> **Dev:** "How do the **DyCalculators** get their data? Do they access the pool directly?"
> **Domain expert:** "No — that's the **DyCalculationInputs** pattern. `get_dy()` on the pool class does all the I/O and cache lookups first, then constructs a **DyCalculationInputs** frozen dataclass with pre-resolved rates, XP, block data, and invariant solver closures. The calculator receives that object instead of the pool, so there's zero private member access. The calculator is pure math."
