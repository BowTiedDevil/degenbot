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

## Fetcher Protocols

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **VirtualPriceFetcher** | A fetcher callback returning the base pool's virtual price | VP fetcher |
| **TimestampFetcher** | A fetcher callback returning the block timestamp for A-ramping calculations | Time fetcher |
| **RedemptionPriceFetcher** | A fetcher callback returning the LSD redemption price | Redemption fetcher |
| **AdminBalancesFetcher** | A fetcher callback returning admin fee balances | Admin fetcher |
| **DFetcher** | A fetcher callback returning the on-chain invariant D for crypto pools | D fetcher |
| **GammaFetcher** | A fetcher callback returning the on-chain gamma parameter for crypto pools | Gamma fetcher |
| **PriceScaleFetcher** | A fetcher callback returning on-chain price_scale values for crypto pools | Price scale fetcher |
| **LendingRateFetcher** | A fetcher callback returning per-token lending rates for a block | Rate fetcher |

## Variant Enums

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **DVariant** | An enum identifying which D-calculation formula pair a pool uses | D variant, D group |
| **YVariant** | An enum identifying which Y-calculation formula a pool uses | Y variant, Y group |
| **YDVariant** | An enum identifying which Y_D-calculation formula a pool uses | Y_D variant, Y_D group |

## Strategy Enums

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **SwapStyle** | An enum identifying which computation path `get_dy()` uses for dy calculation | Fee style, swap type |
| **MetapoolRateStyle** | An enum identifying how a metapool constructs its rate tuple | Metapool rate |
| **MetapoolUnderlyingStyle** | An enum identifying how a metapool computes `get_dy_underlying()` | Underlying style |
| **LendingRateStyle** | An enum identifying which rate source provides lending rates | Rate source, rate style |
| **PoolStrategies** | A frozen dataclass combining all strategy enums into a single value object | Strategies, pool config |

## Pool Tracker

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **CurveStableswapPoolManager** | A Pool Tracker that tracks Curve StableSwap pools and delegates construction to Bot | Curve manager |

## Relationships

- A **Metapool** has exactly one **Base Pool**
- A **Pool** has 2–8 coins
- A **Lending Pool** has **Stored Rates** for each lending token
- A **Crypto Pool** uses **DFetcher**, **GammaFetcher**, and **PriceScaleFetcher**
- **A Coefficient** ramping uses **TimestampFetcher**
- A **CurveStableswapPoolManager** tracks Curve pools and delegates construction to **Bot**

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

### 7. LendingRateFetcher vs provider_call

**Ruling:** Use **LendingRateFetcher** for all lending rate-fetching. The old `provider_call` backdoor has been removed.

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
> **Domain expert:** "No — `provider_call` has been removed. Use a **LendingRateFetcher** — it's a typed fetcher callback injected at construction. Each lending variant (cToken, yToken, cyToken, aETH, rETH, oracle) has its own fetcher closure. The pool calls it on-demand; it never accesses connections directly."
>
> **Dev:** "What are all the **variant enums** for?"
> **Domain expert:** "Mainnet Curve pools use different calculation formulas depending on the contract version. **DVariant**, **YVariant**, and **YDVariant** identify which formula a pool uses for D, y, and y_D calculations respectively. They replace the old class-level address frozensets that coupled configuration data to pool behavior."
