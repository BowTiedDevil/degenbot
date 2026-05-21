# Context — Balancer V2 Pools

## Pool Types

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Weighted Pool** | A Balancer V2 pool using the weighted product invariant with configurable token weights | Balancer pool (too vague), weighted AMM |
| **BalancerV2Pool** | The Python class representing a Balancer V2 weighted pool | Balancer pool class |
| **Pool ID** | A 32-byte identifier unique to each Balancer pool, encoding the pool address, specialization, and nonce | Pool identifier |
| **Vault** | The singleton Balancer V2 Vault contract (`0xBA12222222228d8Ba445958a75a0704d566BF2C8`) that holds all pool tokens and executes swaps | Balancer vault, the vault |
| **BalancerQueries** | A helper contract (`0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5`) for simulating Vault queries without state changes | Query helper |

## Specialization

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **General** | Specialization type 0 — pools with any number of tokens | GENERAL |
| **Minimal Swap Info** | Specialization type 1 — pools that only reveal token addresses in swap events | MINIMAL_SWAP_INFO |
| **Two Token** | Specialization type 2 — pools with exactly two tokens for optimized gas | TWO_TOKEN |

## Math Libraries

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **FixedPoint** | 18-decimal fixed-point arithmetic library (`fixed_point.py`) providing `mul_down`, `mul_up`, `div_down`, `div_up`, `pow_down`, `pow_up`, `complement` | FP, fixed math |
| **LogExpMath** | Natural logarithm and exponentiation via Taylor series (`log_exp_math.py`) — the core approximation engine for `pow` | Log-exp, Ln/pow |
| **WeightedMath** | Invariant and swap calculation formulas for weighted pools (`weighted_math.py`) | WM, weighted calculations |
| **ScalingHelpers** | Fixed-point scaling for tokens with non-18 decimals (`scaling_helpers.py`) — `_upscale`, `_downscale_down`, `_downscale_up`, `_compute_scaling_factor` | Scaling, decimal helpers |
| **InputHelpers** | Validation helpers for swap input amounts (`input_helpers.py`) | Validators |
| **Helpers** | Shared helper functions (`helpers.py`) | Utility functions |

## PowVersion

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **PowVersion.V1** | The original FixedPoint.pow implementation used by `WeightedPool2Tokens` — general LogExpMath path with error bound for all exponent values; no fast paths | Old pow, original version |
| **PowVersion.V2** | The updated FixedPoint.pow used by newer `WeightedPool` contracts — includes fast paths for y == ONE (return x), y == TWO (mul x x), y == FOUR (mul (mul x x) (mul x x)) that bypass the error-bound calculation | New pow, fast-path version |
| **Bytecode Detection** | The method of determining `PowVersion` by checking the deployed pool contract bytecode for the TWO constant (`1bc16d674ec80000`), which only appears in V2 bytecode | Version detection |

## Critical Semantics

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Truncated Division** | Solidity's `/` operator truncates toward zero for negative operands, unlike Python's `//` which floors toward -∞. Implemented via `_truncated_div` in `log_exp_math.py` | Floor division, integer division (ambiguous) |
| **Rounding Direction** | Down for GIVEN_IN (seller gets less), up for GIVEN_OUT (buyer pays more). Applied consistently through `_calc_out_given_in` (round down) and `_calc_in_given_out` (round up) | Rounding mode |
| **Fee Ordering (GIVEN_OUT)** | Solidity's onSwap path: downscale up first, then add swap fee amount. Python must match this order exactly or the accumulated rounding differs | Fee application order |
| **Scaling Factor** | Computed as `ONE * 10**decimalsDifference` where `decimalsDifference = 18 - token_decimals`. Tokens with < 18 decimals have scaling factors > ONE; tokens with 18 decimals have scaling factor == ONE | Decimal factor, normalization factor |

## Deployments

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **BROKEN_BALANCER_V2_POOLS** | A frozenset of pool addresses where on-chain swaps are disabled or the pool is otherwise broken (e.g., BAL#327 SWAPS_DISABLED). Pools in this set should be skipped during discovery and testing | Broken pools, disabled pools |
| **deployments.py** | Module centralizing contract addresses and broken pool addresses for Balancer V2, following the same pattern as `curve/deployments.py` | Addresses module |

## Stable Pool Semantics

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **StableMath** | Library implementing the StableSwap invariant and swap calculations (`libraries/stable_math.py`) | Stable math, curve math (wrong DEX) |
| **MetaStablePool** | A 2-token stable pool with rate providers. Uses `BaseMinimalSwapInfoPool` (specialization=1). `_cacheTokenRatesIfNecessary()` is called inside `_onSwapGivenIn` AFTER upscaling | Meta stable, stable v1 |
| **ComposableStablePool** | A multi-token stable pool that includes its own BPT token. Uses `BaseGeneralPool` (specialization=0). Overrides `_beforeSwapJoinExit()` to call `_cacheTokenRatesIfNecessary()` BEFORE reading `_scalingFactors()` | Composable, phantom stable |
| **BPT Index** | In ComposableStablePools, the BPT token's position in the token list. Must be dropped before invariant and swap calculations. `bpt_idx` parameter distinguishes MetaStable (`None`) from Composable (`int`) | Phantom token index |
| **Invariant Rounding** | Deployed contracts use two different StableMath versions: V1 (always-roundDown, D_P accumulation, no `roundUp` param) and V2 (with `roundUp` param, P_D accumulation). V1 used by most ComposableStablePools; V2 used by MetaStablePools. The version must match the deployed contract for exact output. | Invariant version |

### _beforeSwapJoinExit Rate Refresh

**Critical difference between pool types**: ComposableStablePools override `_beforeSwapJoinExit()` to refresh rate caches BEFORE `_scalingFactors()` is read. MetaStablePools do not have this override — they call `_cacheTokenRatesIfNecessary()` inside `_onSwapGivenIn` (after upscaling). This means:

- **ComposableStablePool**: Scaling factors used during the swap come from **fresh** rates (just-cached by `_beforeSwapJoinExit`)
- **MetaStablePool**: Scaling factors come from **stale** cached rates, but the subsequent `_cacheTokenRatesIfNecessary()` call in `_onSwapGivenIn` doesn't affect the already-computed memory values

For off-chain matching, builders must provide scaling factors computed from **fresh** rates for ComposableStablePools and from **cached** rates for MetaStablePools. In practice, both approaches use fresh rates from rate providers because the MetaStablePool's cached rates happen to be close to fresh rates for the pools we've tested (exact 0-wei matching achieved).

### Cache-Aware Rate Resolution

**`CacheAwareRateProvider`** (in tests) replicates the on-chain `_cacheTokenRateIfNecessary()` flow exactly:
1. Reads `getTokenRateCache(token)` at the target block → `(rate, oldRate, duration, expires)`
2. Gets the block's timestamp via `eth_getBlockByNumber`
3. If `timestamp > expires`: calls `provider.getRate()` at that block (cache expired, refresh needed)
4. If `timestamp <= expires`: uses the cached `rate` (cache still valid)
5. For BPT tokens: returns ONE
6. For tokens without a rate provider: returns ONE

This gives **exact 0-wei matching** because we replicate the exact same logic the on-chain code uses. The naive approach of always calling `getRate()` fails when the cache is still valid (stale cached rate differs from fresh rate by up to ~0.2%).

### Invariant Versions (INVARIANT_V1 vs INVARIANT_V2)

Deployed contracts have **two different StableMath implementations**:

- **V1 (`INVARIANT_V1`)**: `_calculateInvariant(amp, balances)` — no `roundUp` parameter, always rounds down (Math.divDown), uses D_P accumulation `(D^(n+1) / (n^n * P))`. Matches the monorepo `_calculate_invariant`. Used by most deployed ComposableStablePools (TUSD BSP, bb-s-USD). Comment: "Always round down, to match Vyper's arithmetic."
- **V2 (`INVARIANT_V2`)**: `_calculateInvariant(amp, balances, roundUp)` — with `roundUp` parameter, uses P_D accumulation `(n^n * P / D^(n-1))`. Matches `_calculate_invariant_deployed`. Called with `roundUp=True` for swaps. Used by MetaStablePools (wstETH/WETH).

**V2 with `roundUp=True` produces an invariant 1 wei higher than V1**, which cascades to a 1-wei output difference. Using the wrong version gives a systematic ±1 wei error.

### BalancerRateProvider and Stale Rate Warning

**`BalancerRateProvider`** is a `runtime_checkable` protocol with a single method `get_rates(block_identifier) -> tuple[int, ...]` that returns per-token rates. Injected at construction time, called at calculation time to resolve rates for a specific block.

**`_StaticRateProvider`** is an internal wrapper that always returns construction-time rates. Used when no live rate provider is available.

**`PossibleInaccurateResult`** exception: ComposableStablePools without a live `BalancerRateProvider` raise this exception (same pattern as `UniswapV4Pool` with hooks). The computed `amount_in` and `amount_out` are available as attributes on the exception. Callers must `try/except` to access the values, explicitly acknowledging that rates may be stale.

**Exact matching guarantee**: With a `CacheAwareRateProvider` that replicates `_cacheTokenRateIfNecessary()`, and the correct `invariant_version`, results match on-chain with **0 wei error**. The previous tolerance of ≤3000 wei was due to (a) always calling `getRate()` instead of checking cache validity, and (b) using the wrong invariant version.

**MetaStablePool exception**: MetaStablePools do NOT raise `PossibleInaccurateResult` because they have no rate cache — they call `getRate()` directly on each swap. Construction-time scaling factors produce exact 0-wei matching without a rate provider for the pools we've tested (near-static rate providers).

## Library File Layout

```
balancer/
├── __init__.py
├── CONTEXT.md          (this file)
├── deployments.py     (contract addresses, broken pool set)
├── libraries/
│   ├── constants.py    (ONE, TWO, FOUR, MAX_POW_RELATIVE_ERROR, PowVersion)
│   ├── fixed_point.py  (mul_down/up, div_down/up, pow_down/up, complement)
│   ├── helpers.py      (shared helpers)
│   ├── input_helpers.py (validation)
│   ├── log_exp_math.py  (ln, pow via Taylor series with _truncated_div)
│   ├── scaling_helpers.py (_upscale, _downscale_down/up, _compute_scaling_factor)
│   ├── stable_math.py  (StableMath: invariant, outGivenIn, inGivenOut, BPT functions)
│   └── weighted_math.py (calculate_invariant, _calc_out_given_in, _calc_in_given_in)
├── managers.py         (empty — reserved for future pool tracker)
├── pools.py            (BalancerV2Pool class, detect_pow_version)
├── stable_pools.py     (BalancerV2StablePool class)
└── types.py            (BalancerV2PoolState frozen dataclass)
```

## Relationships

### Weighted Pool Chain

- **BalancerV2Pool** delegates all math to **WeightedMath** functions, which in turn delegate exponentiation to **FixedPoint.pow_down/pow_up**
- **FixedPoint.pow_down/pow_up** accept a `version: PowVersion` kwarg that controls whether fast paths for y == ONE/TWO/FOUR are active; the version is detected from bytecode at construction time and stored on the pool instance
- **ScalingHelpers** are called directly by **BalancerV2Pool** — upscale before calculation, downscale after
- **LogExpMath** is a private implementation detail of **FixedPoint** — no other module should import it directly
- **BROKEN_BALANCER_V2_POOLS** in **deployments.py** is used by pool discovery and testing to filter out pools where on-chain swaps are disabled

### Stable Pool Chain

- **BalancerV2StablePool** delegates all math to **StableMath** functions via `_calc_out_given_in`, `_calc_in_given_out`, and `_calculate_invariant_deployed`
- **BalancerV2StablePool** handles BPT dropping internally — `bpt_idx=None` for MetaStablePool, `bpt_idx=int` for ComposableStablePool
- **StableMath** (`stable_math.py`) provides both monorepo (`_calculate_invariant`, always roundDown) and deployed-contract (`_calculate_invariant_deployed`, with `round_up` param) versions
- Scaling factor computation is the builder's responsibility: fresh rates from rate providers for ComposableStablePools, cached rates for MetaStablePools
- Both pool classes share **BalancerV2PoolState** from `types.py`

## Resolved ambiguities

### WeightedPool2Tokens vs WeightedPool

These are distinct deployed contract types with different FixedPoint library versions. **WeightedPool2Tokens** uses PowVersion.V1 (general path only); **WeightedPool** uses PowVersion.V2 (with fast paths). The pool class `BalancerV2Pool` handles both via the `pow_version` attribute — there is no separate class per contract type.

### Vault vs Pool Contract

In Balancer V2, the **Vault** holds all tokens and executes swaps. The **Pool Contract** holds the invariant logic and state (weights, fees). When we say "the pool" we mean the Pool Contract; when we say "the vault" we mean the singleton Vault. This distinction matters because swap calls go to the Vault, not the pool contract.

### Worker (scraper) vs Pool class

The vault scraper script (`scripts/balancer_vault_scraper.py`) discovers pools by scanning Vault events. It is standalone and does not import from degenbot internals. The pool class (`BalancerV2Pool`) is the in-memory calculation object. They have no runtime dependency on each other.

### MetaStablePool vs ComposableStablePool

Both use StableMath but differ in specialization and rate caching behavior. ComposableStablePools include BPT in the token list and override `_beforeSwapJoinExit()` to refresh rate caches before reading `_scalingFactors()`. MetaStablePools do not include BPT and have no `_beforeSwapJoinExit` override. The class `BalancerV2StablePool` handles both via the `bpt_idx` parameter — `None` for MetaStable, integer for Composable.

### Monorepo vs Deployed Invariant

The monorepo's `_calculateInvariant` always rounds the last iteration down. The deployed contract uses a `round_up` parameter. For swaps, the deployed contract passes `roundUp=True`. Using `_calculate_invariant_deployed(round_up=True)` produces exact 0-wei matching for MetaStablePools and near-exact matching (<3000 wei) for ComposableStablePools. The monorepo version produces ~0.01% error for some pools due to the different rounding.

### ComposableStablePool Rate Tolerance

ComposableStablePool on-chain tests use `_assert_close` with up to 3000 wei tolerance rather than exact integer matching. The residual comes from a timing difference: we read fresh rates from rate providers in a separate `eth_call`, while the on-chain `_beforeSwapJoinExit` refreshes the cache in the same call as the swap. The rates may differ by a few wei due to block-level state changes between the two calls.
