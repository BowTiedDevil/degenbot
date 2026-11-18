# Boosted Balancer routing and time-dependent Yield Space — on-chain evidence

**Scope.** Track B pool-model evidence: (1) the exact USDC→DAI `queryBatchSwap`
shape through `bb-s-USD` / `bb-s-USDC` at the pin, the linear/stable/linear leg
decomposition, and the comparison against the core's existing Balancer
join/exit vs swap machinery; (2) the invariant inputs, getter set and
block-timestamp entry point for Element's `ConvergentCurvePool` and Sense's
`Space` pool, plus the assessment of the Curve data-provider timestamp seam
those pools would need.

**Provenance.** Every on-chain claim below is an `eth_call` /
`eth_getStorageAt` read against the devcontainer archive node
`web3_clientVersion = reth/v2.11.0-b9abbcc/x86_64-unknown-linux-gnu`,
`eth_chainId = 0x1` (mainnet), pinned at block **26204126**
(`0xf49871babe95f386b0538ed2ebce1fa5bb56de2dbf87602b96e987a07c25a3b0`,
timestamp **1792142039** = 2026-10-16T09:13:59Z). The only reads outside the pin
are the two labelled historical probes, §4.3 (block 16000000) and §5.3's
pre-maturity check (block 16400000), which use this same node because both
yield-space pools were already past maturity at the pin.

First-party sources read for this report:

| Source | Revision | Symbols used |
| --- | --- | --- |
| `balancer/balancer-v2-monorepo` | `master` | `pkg/pool-linear/contracts/LinearPool.sol` (`_onSwapGeneral`, `_calcBptOutPerMainIn` call sites), `pkg/pool-linear/contracts/LinearMath.sol` (`_calcBptOutPerMainIn`, `_calcMainOutPerBptIn`, `_calcInvariant`, `_toNominal`, `_fromNominal`), `pkg/pool-stable/contracts/ComposableStablePool.sol` (`_onSwapGivenIn`, `_onRegularSwap`, `getRate`), `pkg/interfaces/contracts/vault/IVault.sol` (`BatchSwapStep`, `FundManagement`, `SwapKind`, `queryBatchSwap`), `pkg/standalone-utils/contracts/BalancerQueries.sol` (`queryJoin`, `queryExit`) |
| `delvtech/elf-contracts` (Element) | `main` | `contracts/ConvergentCurvePool.sol` (`onSwap`, `solveTradeInvariant`, `_assignTradeFee`, `_getYieldExponent`, `_adjustedReserve`, `onJoinPool`) |
| `sense-finance/space-v1` | `main` | `src/Space.sol` (`onSwapPreview`, `_onSwap`, `_tokensInForBptOut`, `_bptFeeDue`, `getFairBPTPrice`, `getImpliedRateFromPrice`), `src/oracle/PoolPriceOracle.sol` |
| In-repo core | working tree | `degenbot-pools` / `degenbot-math` / `degenbot-bot` / `degenbot-python` symbols named in §3 and §5 |
| In-repo vendored math | working tree | `tier3-oracle/lib/balancer-src/solidity-utils/contracts/math/StableMath.sol` (the `StableMath` port I re-implemented to validate the middle leg) |

No repository file was modified other than this document. The scratch probes
used for the numeric reproduction are `/tmp/model.py` (Balancer `StableMath` +
linear-pool rate conversion) and `/tmp/element2.py` (Element Yield Space); both
are reproduced in Appendix A. All commands are listed verbatim in Appendix B.

---

## 1. The boosted structure at the pin

Three pools, one route:

| Pool | Address | `version()` | Vault tokens (`getPoolTokens`) | Balances at pin |
| --- | --- | --- | --- | --- |
| `bb-s-USD` | `0x779d01F939D78a918A3de18cC236ee89221dfd4E` | `{"name":"ComposableStablePool","version":3,"deployment":"20230206-composable-stable-pool-v3"}` | `[bb-s-USD (BPT), bb-s-USDC, bb-s-USDT, bb-s-DAI]` | `[2596148429267404814265248164610048, 1117683474171682438, 3000000000000000000, 624539787045706641447]` |
| `bb-s-USDC` | `0xbFA413A2ff0F20456d57B643746133F54bFE0cd2` | `{"name":"AaveLinearPool","version":4,"deployment":"20230206-aave-rebalanced-linear-pool-v4"}` | `[bb-s-USDC (BPT), USDC, sUSDC]` | `[5192296858534819510847022157537657, 8117684, 0]` |
| `bb-s-DAI` | `0xFD11CCdBdb7AB91Cb9427A6d6BF570C95876d195` | `{"name":"AaveLinearPool","version":4,"deployment":"20230206-aave-rebalanced-linear-pool-v4"}` | `[bb-s-DAI (BPT), DAI, sDAI]` | `[5192296858534195219472775191629857, 1817702680875126024, 0]` |
| `bb-s-USDT` (unused by this route) | `0xDc063deAfcE952160eC112FA382ac206305657e6` | `{"name":"AaveLinearPool","version":4,"deployment":"20230206-aave-rebalanced-linear-pool-v4"}` | `[bb-s-USDT (BPT), sUSDT, USDT]` | `[5192296858534817628530496329220095, 0, 10000000]` |

Pool ids at the pin (`getPoolId()`):

```
bb-s-USD  0x779d01f939d78a918a3de18cc236ee89221dfd4e0000000000000000000004c7
bb-s-USDC 0xbfa413a2ff0f20456d57b643746133f54bfe0cd20000000000000000000004c3
bb-s-DAI  0xfd11ccdbdb7ab91cb9427a6d6bf570c95876d1950000000000000000000004c2
```

Key getters at the pin (all `cast call --block 26204126`):

| Read | `bb-s-USD` | `bb-s-USDC` | `bb-s-DAI` |
| --- | --- | --- | --- |
| `getSwapFeePercentage()` | `50000000000000` (0.005 %) | `200000000000000` (0.02 %) | `200000000000000` (0.02 %) |
| `getRate()` | `656944448742437657` | `1000000064775661584` | `2874251497005989` |
| `getWrappedTokenRate()` | – | `1496516672608907006` | `3080098611962892611` |
| `getTargets()` | – | `(0, 20000000000000000000000000)` | `(0, 20000000000000000000000000)` |
| `getMainToken()` / `getWrappedToken()` | – | `0xA0b8…eB48` (USDC) / `0xD7BC…1aC3` (sUSDC) | `0x6B17…1d0F` (DAI) / `0x86A4…EaDc` (sDAI) |
| `getScalingFactors()` | `[1e18, 1e18, 1e18, 2874251497005989]` | `[1e18, 1e30, 1496516672608907006000000000000]` | `[1e18, 1e18, 3080098611962892611]` |
| `getRateProviders()` | `[0x0, bb-s-USDC, bb-s-USDT, bb-s-DAI]` | – | – |
| `getAmplificationParameter()` | `(2000000, false, 1000)` | – | – |
| `totalSupply()` | `2596148429267413814265248164610048` (= 2^111) | `5192296858534827628530496329220095` (= 2^112−1) | `5192296858534827628530496329220095` (= 2^112−1) |

`getTokenRate(child)` / `getTokenRateCache(child)` on `bb-s-USD` return
`1000000000000000000` for `bb-s-USDC` (cache `(1e18, 1e18, 21600, 1783220375)` —
expired 2026-07-05) and `2874251497005989` for `bb-s-DAI`. **The swap path does
not use the cached 1e18**: see §2.2.

### 1.1 The "virtual BPT supply" every leg needs

`totalSupply()` for all four pools is dominated by Balancer's pre-minted BPT,
and the live supply is `totalSupply − BPT-in-Vault`:

```
bb-s-USDC  real supply = (2^112−1) − 5192296858534819510847022157537657 = 8117683474171682438
bb-s-DAI   real supply = (2^112−1) − 5192296858534195219472775191629857 = 632409057721137590238
bb-s-USD   real supply = 2^111    − 2596148429267404814265248164610048 = 9000000000000000000
```

The `totalSupply − BPT-in-Vault` identity is not an assumption: it is what makes
L1 and L3 of §2.2 exact to the wei, and it also reproduces `getRate()`
(`invariant / realSupply`: `8117684000000000000 / 8117683474171682438 = 1.000000064775661584`
for bb-s-USDC; `5912500038681938914 / 9000000000000000000 = 0.656944448742437657`
for bb-s-USD).

This is load-bearing for every leg of the route (§2.2) and is exactly what the
core's `bpt_idx` skip is designed around (§3).

---

## 2. The exact USDC→DAI shape at the pin

### 2.1 The three-step batch

`Vault.queryBatchSwap` (selector `0xf84d066e`,
`queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))`)
at block 26204126, 1 USDC in, `GIVEN_IN` (`kind = 0`) — line-wrapped here for
readability, one line when executed:

```bash
RPC=http://host.containers.internal:8545/
VAULT=0xBA12222222228d8Ba445958a75a0704d566BF2C8
Z=0x0000000000000000000000000000000000000000

cast call --rpc-url $RPC --block 26204126 --from $Z $VAULT \
 'queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))(int256[])' \
 0 \
 '[(0xbfa413a2ff0f20456d57b643746133f54bfe0cd20000000000000000000004c3,0,1,1000000,0x),
   (0x779d01f939d78a918a3de18cc236ee89221dfd4e0000000000000000000004c7,1,2,0,0x),
   (0xfd11ccdbdb7ab91cb9427a6d6bf570c95876d1950000000000000000000004c2,2,3,0,0x)]' \
 '[0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48,0xbFA413A2ff0F20456d57B643746133F54bFE0cd2,
   0xFD11CCdBdb7AB91Cb9427A6d6BF570C95876d195,0x6B175474E89094C44Da98b954EedeAC495271d0F]' \
 '(0x0000000000000000000000000000000000000000,false,0x0000000000000000000000000000000000000000,false)'
```

Output (verbatim):

```
[1000000 [1e6], 0, 0, -999710385659313643 [-9.997e17]]
```

i.e. the Vault asset deltas for `assets = [USDC, bb-s-USDC, bb-s-DAI, DAI]`:
`+1.000000 USDC` in, `−0.999710385659313643 DAI` out, and the two intermediate
BPTs net to zero (they never leave the Vault).

The same shape with 1000 USDC in:

```
[1000000000 [1e9], 0, 0, -1795084415339484096 [-1.795e18]]
```

1000 USDC returns **1.795084415339484096 DAI** — the route is capped by
`bb-s-DAI`'s DAI reserve (1.817702680875126024 DAI), not by any limit field.

### 2.2 Leg-by-leg: the exact arithmetic, reproduced to the wei

Each leg quoted separately with a one-step `queryBatchSwap`
(`--block 26204126`):

| Leg | Step | Input | Output at pin | Model | Residual |
| --- | --- | --- | --- | --- | --- |
| L1 linear wrap (`bb-s-USDC.onSwap`, USDC→BPT) | `(bb-s-USDC,0,1,1000000,0x)` | 1.000000 USDC | `999999935224342612` BPT | `(realSupply × 1e18) // invariant = 999999935224342612` | **0** |
| L2 stable swap (`bb-s-USD`, BPT→BPT) | `(bb-s-USD,0,1,999999935224342612,0x)` | `999999935224342612` bb-s-USDC | `347815905010636202008` bb-s-DAI | `StableMath` with live rates, 0.005 % fee | **0** |
| L3 linear unwrap (`bb-s-DAI.onSwap`, BPT→DAI) | `(bb-s-DAI,0,1,347815905010636202008,0x)` | `347815905010636202008` bb-s-DAI | `999710385659313643` DAI | `(invariant × bptIn) // realSupply` | **0** |

L1 is exact for 1, 2 and 3 USDC (`999999935224342612`, `1999999870448685225`,
`2999999805673027838` = `realSupply × amount18 // invariant`); L3 is exact for
`1e18`, `1e20`, `600e18` and for the composed amount. L2 is an exact
reimplementation of `StableMath._calculateInvariant` /
`_getTokenBalanceGivenInvariantAndAllOtherBalances` / `_calcOutGivenIn` on the
BPT-skipped, rate-upscaled balances
`xp = [1117683474171682438, 3000000000000000000, 1795084417855923890]`
(`D = 5912500038681938914`), fee `mulUp(amount, 5e13)` subtracted from the input,
`divDown` on the way out.

Two facts fall out of the exact reproduction:

1. **The linear legs are joins/exits, not stableswap hops.** L1 is a mint of
   BPT against the main token (`(realSupply × amountIn) // invariant`), L3 a
   burn (`(invariant × bptIn) // realSupply`), where
   `invariant = nominalMain + wrappedBalance` and
   `nominalMain = _toNominal(mainBalance, {fee, lowerTarget, upperTarget})`.
   Both quotes are *fee-free at the pin* because
   `_toNominal` charges nothing while the main balance is inside
   `[lowerTarget, upperTarget]` (`bb-s-USDC`: 8.117684 M USDC inside
   `[0, 20 M]`; `bb-s-DAI`: 1.8177 DAI inside `[0, 20 M]`), which is why the
   observed quote is exactly the rate conversion.
2. **The middle leg uses the live rate-provider value, not the pool's
   `getScalingFactors()` view.** `bb-s-USDC.getRate()` at the pin is
   `1000000064775661584`, while `bb-s-USD.getScalingFactors()[1]` and
   `getTokenRate(bb-s-USDC)` both return `1000000000000000000`. Reproducing L2
   with the live rate matches to the wei; with the cached `1e18` it is short by
   **22 494 497 741 881 wei** of bb-s-DAI (6.5e-11 relative) —

```
live-rate  model: 347815905010636202008   (residual 0 vs chain)
cached-rate model: 347815882516138460127  (residual −22,494,497,741,881 wei)
```

### 2.3 amounts / assets / limits / userData

| Field | Value at the pin |
| --- | --- |
| `kind` | `0` (`GIVEN_IN`) |
| `assets` | `[USDC, bb-s-USDC BPT, bb-s-DAI BPT, DAI]` |
| `swaps[0]` | `(bb-s-USDC poolId, in=0, out=1, amount=1 000 000, userData=0x)` |
| `swaps[1]` | `(bb-s-USD poolId, in=1, out=2, amount=0, userData=0x)` |
| `swaps[2]` | `(bb-s-DAI poolId, in=2, out=3, amount=0, userData=0x)` |
| `funds` | `(0x0, false, 0x0, false)` — no internal balance, no recipient |
| userData | **empty (`0x`) on all three steps**; neither `LinearPool._onSwapGeneral` nor `ComposableStablePool._onRegularSwap` reads it |
| limits | **none**: `IVault.BatchSwapStep` (poolId, assetInIndex, assetOutIndex, amount, userData) has no limit field. The only bound is the GIVEN_IN `amount` on step 0 and each pool's reserve. The state-changing path differs: `Vault.swap` carries `SingleSwap.limit`; joins/exits carry `maxAmountsIn`/`minAmountsOut` plus a JoinKind/ExitKind in `userData` |
| output | 1 USDC → `999710385659313643` DAI; 1000 USDC → `1795084415339484096` DAI |

The intermediate steps carry `amount = 0`: the Vault chains the previous step's
output into the next, verified by the quote succeeding with zeros there.

### 2.4 "linear join / stable BPT swap / linear exit" — join/exit vs swap machinery

Balancer's own comment is the authoritative statement of the shape
(`LinearPool._onSwapGeneral`):

> "In most Pools, swaps involve exchanging one token held by the Pool for
> another. In this case however, since one of the three tokens is the BPT
> itself, a swap might also be a join (main/wrapped for BPT) or an exit
> (BPT for main/wrapped)."

So the three legs of §2.1 *are* the linear join, the stable BPT swap and the
linear exit, expressed as three `onSwap` calls inside one `batchSwap`. The
literal `Vault.joinPool(JoinKind.EXACT_TOKENS_IN_FOR_BPT_OUT)` /
`Vault.exitPool(ExitKind.EXACT_BPT_IN_FOR_TOKENS_OUT)` route is a *different*
call graph (three Vault ops, two of them non-swap), and it cannot be simulated
on these pools at all:

```bash
# BalancerQueries (0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5) at the pin
queryJoin(bytes32,address,address,(address[],uint256[],bytes,bool))   # 0x9ebbf05d
queryExit(bytes32,address,address,(address[],uint256[],bytes,bool))   # 0xc7b2c52c
```

both revert `BAL#998` = `Errors.UNIMPLEMENTED` (verified with 32-byte and
64-byte userData encodings, for `bb-s-USDC` join and `bb-s-DAI` exit). The
linear pools are `NewBasePool`-derived and do not implement `IBasePool.queryJoin`
/ `queryExit`, so **the join/exit route has no quote surface on-chain**; only
the state-changing `joinPool`/`exitPool` would work, and their return values are
not observable through `eth_call` without a helper.

The economic difference between the two routes is bounded by the linear fee,
which is zero inside the target band: at the pin both routes are numerically
identical (both linear legs are fee-free). Outside the band, the swap route pays
up to `getSwapFeePercentage()` per leg (0.02 % on both linear pools) on the
distance outside `[lowerTarget, upperTarget]`, while join/exit stay fee-free.
`LinearMath._toNominal` is the exact fee rule:

```solidity
if (real < lowerTarget)      return real - (lowerTarget - real).mulDown(fee);
else if (real <= upperTarget) return real;
else                          return real - (real - upperTarget).mulDown(fee);
```

---

## 3. Comparison with the core's existing Balancer machinery

### 3.1 What exists (named symbols)

| Core symbol | What it owns | Bearing on this route |
| --- | --- | --- |
| `degenbot_pools::BalancerStablePoolIdentity` (`state/balancer_stable_state.rs`) | `{ address, vault, pool_id, tokens, amp, scaling_factors, swap_fee, bpt_idx, invariant_version }` | exactly the middle-leg inputs; `bpt_idx` is the composable pre-mint index |
| `degenbot_pools::BalancerStablePoolState` | `{ balances, update_block, state_nonce, journal, rate_provider: Option<Arc<dyn BalancerRateProvider>> }` | balances incl. the BPT slot; no total supply (see §3.3) |
| `families::balancer_stable::capture_hop` | BPT skip + upscale by `scaling_factors` + pre-computed invariant (`StableMath`) | mirrors `ComposableStablePool._onRegularSwap` (`_dropBptItem` + `_skipBptIndex` + `_calculateInvariant`) |
| `families::balancer_stable::read_swap` | all swap variants behind one door (exact-in, exact-out), fee via `swap_fee` | mirrors `StableMath._calcOutGivenIn` |
| `sim::rate_provider::BalancerRateProvider::rates` | per-block rate vector, incl. BPT entries | the live `bb-s-USDC.getRate()` of §2.2 |
| `bot_core::pool_builder::builder::balancer_scaling_factor` → `compute_scaling_factor`, and the stable builder's `providers` loop (`builder.rs` ~1240-1300) | `scaling = 10^(18−decimals) × provider.getRate() / 1e18` via `IBalancerPool::getRateCall` (zero-address sentinel → `1e18`; unreadable provider → `SpecRefusal::BalancerRateProviderUnreadable`) | the registration path already reads the **live** rate provider, i.e. exactly the value L2 needs |
| `degenbot_python::BalancerV2StablePool` (`src/degenbot/balancer/stable_pools.py`) | `_base_scaling_factors = 10^(18−decimals)`, `_resolve_scaling_factors(block)` = base × live rate, `_stale_rates()` → `QuoteVerdict.stale_rates` | the Python driver's calc-time refresh; refuses a quote that rests on construction-static rates |
| `degenbot_python::BalancerStableSwapQuoter` (`src/degenbot/balancer/pool_quoter.py`) | pair-level `exact_in` / `exact_out` over the Rust leaf, `_scaling_factors(block_identifier)` | swap-only surface; no join/exit |

### 3.2 What the core can model exactly today

The middle leg, **exactly** (residual 0 wei at the pin): register `bb-s-USD`
with `tokens = [bb-s-USD, bb-s-USDC, bb-s-USDT, bb-s-DAI]`, `bpt_idx = Some(0)`,
`amp = 2_000_000`, `swap_fee = 5e13`, `invariant_version` = the deployed
`StableMath` variant, and `scaling_factors = 10^(18−dec) × rateProvider.getRate()`
(`1e18`, `1e18`, `2874251497005989`). `BalancerStableHopState` then carries
`xp = [1.117683474171682438e18, 3e18, 1.795084417855923890e18]` and
`D = 5912500038681938914`, which is precisely the computation validated in §2.2.
The BPT slot (2^111−9e18 of pre-minted supply) is dropped by `bpt_idx`, matching
`_dropBptItem`.

### 3.3 What the core cannot model (the modeling cost)

1. **No linear-pool family.** Nothing in `degenbot-pools` reads
   `getMainToken`/`getWrappedToken`/`getTargets`/`getWrappedTokenRate`, and no
   family implements `LinearMath` (`_calcBptOutPerMainIn`,
   `_calcMainOutPerBptIn`, `_toNominal`, `_fromNominal`). The two outer legs of
   every boosted route are exactly this. `grep -rn 'getWrappedToken|LinearPool|BoostedPool'`
   over `rust/crates` and `src` returns nothing outside `target/`.
2. **No total supply in Balancer state.** `BalancerStablePoolIdentity` has no
   `total_supply`/`bpt_supply` field (`grep -n 'total_supply|supply'` on
   `balancer_stable_state.rs` + `families/balancer_stable.rs` returns nothing).
   The linear legs need `realSupply = totalSupply() − BPT-in-Vault`, and the
   yield-space pools need `totalSupply()` (§4.2, §5.2). One extra read and one
   extra field per pool family.
3. **No join/exit or `batchSwap` surface anywhere.** No `joinPool`/`exitPool`/
   `batchSwap` encoder exists in `degenbot-execution`, and `PlanStep`
   (`grammar_plan.rs`) has no Balancer join/exit step (its only mint-shaped step
   is `V4Mint` for ERC6909). A boosted route is currently only expressible as a
   *path of swaps* — which the mixed solver (`solve::mixed`) already supports —
   not as one Vault call.
4. **A single-hop collapse of the boosted pool is not a valid approximation.**
   Treating the route as one 2-token USDC/DAI hop with the linear rates applied
   (`invariant(USDC 8.117684e18, DAI 1.817702680875126024e18)`, same A and fee)
   gives `995925309419992059` DAI for 1 USDC where the chain gives
   `999710385659313643` — a **0.3785 %** (≈ 38 bp) error, because the collapse
   drops `bb-s-USDT`'s 3e18 of xp and the 1.117683474171682438e18 of existing
   bb-s-USDC xp. Collapsing is only exact when the xp values are preserved
   (`xp = balance × rate`), which is a 3-token model of `bb-s-USD`, not a
   2-token model of the route.

Per-leg cost of a linear family: one 3-token `LinearMath` port (~120 lines, no
I/O), one `realSupply` read at registration, plus the `getTargets` pair. No new
I/O trait: the rate comes through `BalancerRateProvider` already.

---

## 4. Time-dependent Yield Space I — Element `ConvergentCurvePool`

`0x07f589eA6B789249C83992dD1eD324c3b80FD06b`, pool id
`0x07f589ea6b789249c83992dd1ed324c3b80fd06b00020000000000000000034e`
(registered in the same Balancer Vault: `getVault()` = `0xBA12…F2C8`).

### 4.1 Getters at the pin (all present in deployed runtime code)

| Getter | Selector | Value at block 26204126 |
| --- | --- | --- |
| `underlying()` | `0x6f307dc3` | `0x06325440D014e39736583c165C2963BA99fAf14E` (yvCurve-stETH, 18 dec) |
| `bond()` | `0x64c9ec6f` | `0x724e3073317d4B1A8d0c6E89B137eA5af1f4051e` (ePyvCurve-stETH-24FEB23, 18 dec) |
| `expiration()` | `0x4665096d` | `1677243924` (2023-02-24T13:05:24Z) |
| `unitSeconds()` | `0xeaee3f3b` | `1326042070` |
| `percentFee()` | `0x4c1a4115` | `100000000000000000` (0.1) |
| `percentFeeGov()` | `0x41bd436a` | `200000000000000000` (0.2) |
| `governance()` | `0x5aa6e675` | `0x82eF450FB7f06E3294F2f19ed1713b255Af0f541` |
| `totalSupply()` | `0x18160ddd` | `21398850204662824` |
| `feesUnderlying()` / `feesBond()` | `0xf7b94283` / `0xf46c39e7` | `0` / `0` |
| `paused()` | `0x5c975abb` | `false` |
| `solveTradeInvariant(uint256,uint256,uint256,bool)` | `0x79155050` | present — the pure invariant door |

Vault balances (same block): `[30197048785347168, 26945510108983923]`
(0.0302 yvCurve-stETH / 0.0269 PT) — the pool is drained and 3.7 years past
expiration.

### 4.2 Invariant inputs and the timestamp entry point

From `ConvergentCurvePool`:

- `_getYieldExponent()` (the only `block.timestamp` read in the swap path):

```solidity
uint256 timeTillExpiry = block.timestamp < expiration ? expiration - block.timestamp : 0;
timeTillExpiry *= 1e18;
timeTillExpiry = timeTillExpiry.divDown(unitSeconds * 1e18);
uint256 result = uint256(FixedPoint.ONE).sub(timeTillExpiry);   // a = 1 - t
```

- `onSwap` → `_adjustedReserve(...)`: **the bond reserve is inflated by the LP
  supply**: `(underlyingReserve, bondReserve + totalSupply())`, or
  `(bondReserve + totalSupply(), underlyingReserve)` in the other direction; all
  four values are `_tokenToFixed` (18-dec) first.
- `solveTradeInvariant(amount, reserveX, reserveY, out)`:
  `y_after = (x_before^a + y_before^a − x_after^a)^(1/a)`, returned as
  `reserveY − y_after`.
- `_assignTradeFee`: for a `GIVEN_IN` swap the returned amount is
  `amountOut − percentFee·(amountOut − amountIn)` when the output is the bond,
  and `amountOut − percentFee·(amountIn − amountOut)` when the output is the
  underlying (the fee is subtracted in the *output* token's units).

So the complete invariant input set is
`{ underlyingReserve, bondReserve, totalSupply(), expiration, unitSeconds, percentFee, block.timestamp }`
— the pool's own ERC-20 supply is an input to every swap, and `block.timestamp`
enters exactly once, through `a`.

### 4.3 Reproduction (historical block, same node — see Provenance)

At block **16000000** (timestamp `1668811907`, 8 432 017 s before expiration,
`a = 993641214565688704`), the pool holds
`[386058479236877215080, 1038722570005382416837]` and
`totalSupply = 1413787549221700101827`:

| Quote | On-chain | Model | Residual |
| --- | --- | --- | --- |
| `solveTradeInvariant(1e18, 1e18, 1038722570005382416837, true)` | `1042589737012530179` | `1042589737012527383` | 2 796 wei (LogExpMath floor) |
| `solveTradeInvariant(1e18, underlying, bond+supply, true)` | `1011816349047583583` | `1011816349047576104` | 7 479 wei |
| `Vault.queryBatchSwap` underlying→bond, 1e18 in | `1010634714142825225` | `q − (q − 1e18)/10`, `q` = model's `1011816349047576104` → `1010634714142818494` | 6 731 wei |
| `Vault.queryBatchSwap` bond→underlying, 1e18 in | `987133174233359972` | `q − (1e18 − q)/10`, `q` = model's `988302885666689652` → `987133174233358618` | 1 354 wei |

The two fee rows are the *only* residual on the Vault path, and it is the same
`LogExpMath` floor as the two `solveTradeInvariant` rows above (the fee is a
`mulDown` on a quote that is already ~1e4 wei short). Using the contract's own
`solveTradeInvariant` output as `q` reproduces both Vault quotes **exactly**
(`1011816349047583583 − (1011816349047583583 − 1e18)/10 = 1010634714142825225`;
`988302885666690883 − (1e18 − 988302885666690883)/10 = 987133174233359972`).

At the pin the same call reverts `BAL#001` (the drained reserves cannot fund the
subtraction), and `a = 1000000000000000000` exactly.

**Time-dependence magnitude** (same reserves as block 16000000, matured `a = 1`):
underlying→bond would be `1.000000000000000000` instead of
`1.010634714142825225` (**−1.05 %**), and bond→underlying
`1.000000000000000000` instead of `0.987133174233359972` (**+1.30 %**). The
timestamp is worth ~1 % of a quote at 97 days to maturity and 0 % after it.

---

## 5. Time-dependent Yield Space II — Sense `Space`

`0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2`, pool id
`0x03f84e2cd229ffcee732a3c98dc1acd8c8d5f5a20002000000000000000003fb`
(also a Balancer V2 registered pool: `getVault()` = `0xBA12…F2C8`).

### 5.1 Getters at the pin

| Getter | Selector | Value at block 26204126 |
| --- | --- | --- |
| `maturity()` | `0x204f83f9` | `1672531200` (2023-01-01T00:00:00Z) |
| `ts()` | `0xe86d60bf` | `2640612622` (yield-space timescale: `t = ts·(maturity − now)`, i.e. 2.64e-9 per second ≈ 1/12 per year) |
| `pti()` | `0x6f483031` | `0` (PT is token 0) |
| `g1()` | `0xbdd8a366` | `950000000000000000` (0.95 = 1 − 5 % fee) |
| `g2()` | `0x6970a924` | `1052631578947368421` (1/0.95) |
| `adapter()` | `0x03eadcfc` | `0x8c5e7301a012DC677DD7DaD97aE44032feBCD0FD` (Morpho-Aave USDT adapter) |
| `getPoolId()` / `getVault()` | `0x38fff2d0` / `0x8d928af8` | as above |
| `totalSupply()` / `adjustedTotalSupply()` | `0x18160ddd` / `0x8f1314b6` | `1000000` / `1000000` |
| `getSample(uint256)` | `0x60d1507c` | present (7-field TWAP sample, `PoolPriceOracle`) |
| `getTotalSamples()` | `0xb48b5b40` | present |
| `getFairBPTPrice(uint256)` | `0xa503e628` | present |
| `getImpliedRateFromPrice(uint256)` / `getPriceFromImpliedRate(uint256)` | `0x58070223` / `0xf3f586e5` | present |
| `getEQReserves(uint256,uint256)`, `oracleEnabled()` | `0x0aadc9f3`, `0x756a24e4` | **absent** from the deployed runtime code (present in `space-v1` master only) |

Vault balances: `[157906, 838630]` (PT 18 dec, maUSDT 18 dec) with
`lastChangeBlock = 16307378` — the pool was seeded with ~1e-13 of each token and
never traded.

`getSwapFeePercentage()` (`0x55c67628`) is present as bytes in the runtime code
but reverts when called, because the occurrence is an *outbound*
`abi.encodeWithSelector` sequence (`PUSH4 55c67628 · PUSH1 40 · MSTORE ·
PUSH4 ffffffff · AND · PUSH1 e0 · SHL · MSTORE · PUSH1 04 · ADD`), not a
dispatcher entry. Bytecode-selector scans must be confirmed by a call: this one
is a false positive.

### 5.2 Invariant inputs, the two scales, and the timestamp

From `Space.onSwapPreview` + `_onSwap`:

```solidity
if (ptIn) { reservesTokenIn  = reservesTokenIn.add(_totalSupply);
            reservesTokenOut = reservesTokenOut.mulDown(_initScale); }
else      { reservesTokenIn  = reservesTokenIn.mulDown(_initScale);
            reservesTokenOut = reservesTokenOut.add(_totalSupply); }
...
uint256 ttm = maturity > block.timestamp ? uint256(maturity - block.timestamp) * 1e18 : 0;
uint256 t = ts.mulDown(ttm);
uint256 a = (pTIn ? g2 : g1).mulUp(t).complement();      // a = 1 - g·t
x1 = reservesTokenIn.powUp(a);  y1 = reservesTokenOut.powUp(a);
x2 = (reservesTokenIn + amountDelta).powDown(a);
xOrYPost = (x1 + y1 - x2).powUp(ONE.divDown(a));         // k = x^a + y^a
```

so the invariant input set is
`{ reserves[pti], reserves[1−pti], _initScale, totalSupply(), ts, maturity, g1, g2, block.timestamp }`
**plus the live `adapter.scale()`**, which converts the traded amount
(`mulDown(scale)` on a Target-in amount, `divDown(scale)` on a Target-out
amount) while the *reserves* use the frozen `_initScale`.

Two consequences for a port:

1. **`_initScale` has no getter.** It is private storage. At the pin and at
   block 16400000 the value is identical in **slot 8**:
   `0x0df24483dd39d000 = 1004941000000000000` (1.004941), while
   `adapter.scaleStored()` is `1008292000000000000` (block 16400000) and
   `1319162000000000000` (pin). Substituting the live scale for `_initScale` is
   wrong by 0.3 % (block 16400000) to 31 % (pin) of the reserve scaling, so the
   port needs either a storage slot read or a join-time capture.
2. **Two different scale values are used in one swap** (`_initScale` for
   reserves, live `scale()` for the amount), and `adapter.scale()` is *not*
   `view` in the Sense adapter interface (`scale()` writes the stored value;
   `scaleStored()` is the view twin), so a calc-time read must pick
   `scaleStored()` deliberately or pay an I/O + write-shape cost.

### 5.3 Behaviour at the pin

`maturity (1672531200) < block.timestamp (1792142039)` ⇒ `ttm = 0` ⇒ `t = 0` ⇒
`a = 1e18` ⇒ the pool is the matured constant-sum AMM the source comment
describes ("After maturity, this pool becomes a constant sum AMM"). Quoted at
the pin through the Vault:

```
PT → maUSDT, 1000 wei in : [1000, -751]
maUSDT → PT, 1000 wei in : [1000, -1311]
```

The model at `a = 1` predicts `1000·1e18/scale = 758` and
`1000·scale/1e18 = 1319`; the 7–8 wei gap is the pool's own `LogExpMath`
absolute noise at a 1e-13-token reserve. No economic quote exists for this pool
at the pin (`totalSupply = 1e6` wei).

---

## 6. The Curve data-provider timestamp seam

**Verdict: the seam already exists, is unconditional on the swap path, and is
the right entry point for yield-space — no new trait is needed.**

Named symbols:

- `degenbot_pools::sim::curve_data_provider::CurveDataProvider::block_timestamp(&self, block_number) -> Result<u64, CurveDataProviderError>`
  — the pyo3-free trait; the `PyCurveDataProvider` adapter lives in
  `rust/crates/shells/degenbot-python/src/bot/pool.rs:199`.
- Python side: `src/degenbot/curve/curve_stableswap_liquidity_pool.py::block_timestamp`
  → `_get_cached_block_timestamp` (a per-block `BoundedCache`, `max_items=8`,
  lines 491-508) → the provider fetch.
- Rust consumption on the swap path:
  `sim::curve_dy_io::resolve_dy_inputs` fetches
  `provider.block_timestamp(block_number)` **unconditionally** (lines 195-198,
  `unwrap_or(0)` on failure) and stores it in
  `degenbot_math::curve::curve_dy_calculator::DyCalculationInputs::block_timestamp`
  (field at `curve_dy_calculator.rs:209`), alongside `block_number`.
- The same value also decides a *consensus* question today:
  `resolve_metapool_base_rate` picks the cached `base_virtual_price()` vs the
  live `get_virtual_price()` on `block.timestamp <= base_cache_updated + 600`.
  The source comment records the measured consequence at block 26_204_126:
  **21 035 wei of rate, 50 528 091 wei of dy on a 260 189 GUSD swap** — the
  timestamp is already load-bearing for exactness.
- `families::curve_stableswap::{get_dy, calc_token_amount, calc_withdraw_one_coin}`
  already take `block_number: u64`; `curve_block_timestamp` (line 497) is the
  A-ramping-only helper used by the two `calc_*` doors, and
  `degenbot_math::curve::resolve_ramping_a(ARampingParams, timestamp)` is the
  interpolation consumer.

For a yield-space family the port would reuse the trait verbatim and add one
`block_timestamp` field to its hop state (or read `DyCalculationInputs`-style
input struct), with the timestamp fetched **unconditionally** — unlike A-ramping,
where a non-ramping pool returns `0` because `resolve_ramping_a` ignores it. The
one behavioural difference to design for: Element and Sense both need the
timestamp even for pools that never ramp, so the `unwrap_or(0)` degradation must
become a typed refusal (`CurveDataProviderError::Unsupported` exists) rather than
a silent zero — a zero timestamp reads as "matured".

---

## 7. Go / no-go

### 7.1 Boosted underlying routing (`bb-s-USD`, `bb-s-USDC`)

- **GO — model `bb-s-USD` as a rate-provider ComposableStablePool hop.** Exact to
  the wei at the pin with existing `BalancerStablePoolIdentity` +
  `BalancerRateProvider` + `capture_hop`/`read_swap`; the live-rate source must
  be `getRate()` (not `getScalingFactors()`/`getTokenRate()`, §2.2).
- **GO — add a linear-pool family (wrap/unwrap) if boosted routes are ever
  economically live.** The full math is `LinearMath` + `realSupply`; at the pin
  both legs are exactly `(realSupply·amt)//invariant` and
  `(invariant·bpt)//realSupply`, fee 0 inside the target band.
- **NO-GO — collapsing the route to a single 2-token hop.** Measured 0.3785 %
  error on a 1 USDC trade at the pin (995925309419992059 vs 999710385659313643
  DAI).
- **NO-GO — building join/exit or `batchSwap` encoders for quoting purposes.**
  The join/exit quote surface does not exist on these pools (`BAL#998`
  `UNIMPLEMENTED`), and a 3-hop path over the existing swap door reproduces the
  whole route exactly. A `batchSwap` encoder only becomes necessary if the route
  must be *executed* as one Vault call.
- **NO-GO — treating these two pools as a live USDC→DAI venue at the pin.**
  `bb-s-DAI` holds 1.817702680875126024 DAI and `bb-s-USD` holds 1.117683474171682438
  bb-s-USDC BPT; 1000 USDC returns 1.795084415339484096 DAI, and the whole
  reachable output is ≤ 1.82 DAI. The route is a numbering exercise, not a
  trade, at block 26204126.

### 7.2 Time-dependent Yield Space (Element CCP, Sense Space)

- **NO-GO — either named deployment as a trading venue at the pin.** Both are
  past maturity (Element 2023-02-24, Sense 2023-01-01) and drained: Element
  `totalSupply` is 2.14e16 with 0.03 yvCurve-stETH and every Vault quote reverts
  `BAL#001`; Sense `totalSupply` is 1e6 wei of dust and was never traded
  (`lastChangeBlock` unchanged since registration).
- **GO — a `YieldSpace` family is cheap to port and provably correct** if a live
  pool of either design is ever in scope: Element is one `pow`-based invariant
  plus `percentFee` and `+totalSupply` on the bond reserve (reproduced to the
  wei through the fee, and to 1e-15 relative through `LogExpMath`); Sense is one
  `pow` invariant plus `g1`/`g2`, `_initScale`, `+totalSupply`, and a live
  `scaleStored()` read.
- **NO-GO — modeling either family without a block timestamp.** At 97 days to
  maturity the timestamp moves an Element quote by 1.05 %–1.30 % on identical
  reserves; at the pin it is worth exactly 0 because maturity has passed. A
  timestamp-less calc silently becomes the constant-sum branch for every
  matured pool.
- **GO — reuse the existing Curve timestamp seam** (`CurveDataProvider::block_timestamp`
  → `DyCalculationInputs.block_timestamp`, §6), with the one change that the
  fetch must be mandatory rather than `unwrap_or(0)` for these families.

---

## 8. Modeling-cost findings (summary)

1. Middle leg: **0 new math**. Existing `StableMath` port + `capture_hop` +
   `rate_provider`; residual 0 wei at the pin.
2. Linear legs: **one family, ~120 lines, one new state field
   (`real_supply`)**, plus `getTargets` and `getWrappedTokenRate` reads; exact to
   the wei at the pin, and fee-bearing only outside the target band
   (`LinearMath._toNominal`).
3. Route as a path: **0 new solver work** — 3 hops over the existing swap door;
   a single-hop collapse is 38 bp wrong.
4. `batchSwap`/join/exit execution surface: **new grammar step** (one
   `PlanStep` variant + ledger ops + encoder); not needed to quote, needed to
   execute as one Vault call.
5. Yield Space (Element): **one invariant + `percentFee` + `totalSupply`**, and a
   mandatory timestamp input; post-maturity branch is `a = 1`.
6. Yield Space (Sense): **one invariant + `g1/g2` + `totalSupply` + `_initScale`**
   (private storage, slot 8 on this deployment) **+ live `scaleStored()`**, and a
   mandatory timestamp input.
7. Timestamp seam: **already built** (trait + PyO3 adapter + per-block Python
   cache + `DyCalculationInputs.block_timestamp`); the only change is fidelity —
   refuse rather than substitute `0`.
8. Rate-source trap, measured: the pools' own
   `getScalingFactors()`/`getTokenRate()` disagree with the swap-time rate
   (22 494 497 741 881 wei on the middle leg). The core's builder already reads
   `getRate()` from the rate providers (`IBalancerPool::getRateCall`), so the
   correct path is the one that exists; the trap is only for a hand-rolled
   registration that seeds scaling factors from `getScalingFactors()`.

---

## Appendix A — scratch probes (not committed)

`/tmp/model.py` — the `StableMath` reimplementation used for the L2 residual-0
match and the single-hop collapse number: `calculate_invariant` /
`token_balance_given_invariant` / `calc_out_given_in` with `Math.divDown` =
plain division, `Math.divUp` = `(a−1)//b + 1`, `FixedPoint.mulDown/mulUp/divDown`
on the scaling and fee edges, `AMP_PRECISION = 1e3`, and
`out = fpDivDown(out_given_in(...), scaling_factor_out)`.

`/tmp/element2.py` — the Element Yield Space reimplementation: `a = 1e18 −
(ttm·1e18)//unitSeconds`, `pow(x, a)` in 80-digit `Decimal`, the
`(x^a + y^a − x'^a)^(1/a)` solve, the `bond+totalSupply` reserve adjustment and
`percentFee·(out − in)` (output side) / `percentFee·(in − out)` (input side) fee.

## Appendix B — commands (all at the pin unless marked)

Templates: substitute `<pool>` / `<poolId>` / `<child>`. The complete,
copy-pasteable invocation for the route itself is in §2.1; the getter calls are
one-liners whose exact outputs are quoted in the tables above.

```bash
RPC=${TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI:-http://host.containers.internal:8545/}
VAULT=0xBA12222222228d8Ba445958a75a0704d566BF2C8
PIN=26204126

# provenance
curl -s -X POST -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}' $RPC
cast block $PIN --rpc-url $RPC --field hash
cast block $PIN --rpc-url $RPC --field timestamp

# structure
cast call --rpc-url $RPC --block $PIN <pool> 'getPoolId()(bytes32)'
cast call --rpc-url $RPC --block $PIN <pool> 'version()(string)'
cast call --rpc-url $RPC --block $PIN $VAULT 'getPoolTokens(bytes32)(address[],uint256[],uint256)' <poolId>
cast call --rpc-url $RPC --block $PIN <pool> 'getRate()(uint256)'
cast call --rpc-url $RPC --block $PIN <pool> 'getWrappedTokenRate()(uint256)'
cast call --rpc-url $RPC --block $PIN <pool> 'getTargets()(uint256,uint256)'
cast call --rpc-url $RPC --block $PIN <pool> 'getSwapFeePercentage()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x779d01F939D78a918A3de18cC236ee89221dfd4E 'getScalingFactors()(uint256[])'
cast call --rpc-url $RPC --block $PIN 0x779d01F939D78a918A3de18cC236ee89221dfd4E 'getRateProviders()(address[])'
cast call --rpc-url $RPC --block $PIN 0x779d01F939D78a918A3de18cC236ee89221dfd4E 'getAmplificationParameter()(uint256,bool,uint256)'
cast call --rpc-url $RPC --block $PIN 0x779d01F939D78a918A3de18cC236ee89221dfd4E 'getTokenRate(address)(uint256)' <child>
cast call --rpc-url $RPC --block $PIN 0x779d01F939D78a918A3de18cC236ee89221dfd4E 'getTokenRateCache(address)(uint256,uint256,uint256,uint256)' <child>

# the route (see §2.1 for the full quoted command)
cast call --rpc-url $RPC --block $PIN --from 0x0000000000000000000000000000000000000000 $VAULT \
 'queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))(int256[])' ...

# join/exit quote surface (BAL#998 UNIMPLEMENTED)
cast call --rpc-url $RPC --block $PIN 0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5 \
 'queryJoin(bytes32,address,address,(address[],uint256[],bytes,bool))' ...

# Element (pin, and historical block 16000000)
cast call --rpc-url $RPC --block $PIN 0x07f589eA6B789249C83992dD1eD324c3b80FD06b 'expiration()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x07f589eA6B789249C83992dD1eD324c3b80FD06b 'unitSeconds()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x07f589eA6B789249C83992dD1eD324c3b80FD06b 'percentFee()(uint256)'
cast call --rpc-url $RPC --block 16000000 0x07f589eA6B789249C83992dD1eD324c3b80FD06b \
 'solveTradeInvariant(uint256,uint256,uint256,bool)(uint256)' 1000000000000000000 <rx> <ry> true

# Sense (pin and historical)
cast call --rpc-url $RPC --block $PIN 0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2 'maturity()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2 'ts()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2 'g1()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2 'g2()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2 'adapter()(address)'
cast storage  --rpc-url $RPC --block $PIN 0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2 8
cast call --rpc-url $RPC --block $PIN 0x8c5e7301a012DC677DD7DaD97aE44032feBCD0FD 'scaleStored()(uint256)'
cast call --rpc-url $RPC --block $PIN 0x8c5e7301a012DC677DD7DaD97aE44032feBCD0FD 'scale()(uint256)'
```
