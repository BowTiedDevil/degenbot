# Balancer V2 unmodeled families: invariants, contracts, modeling cost

**Date:** 2026-11-18. **Status:** research complete; no implementation.
**Pin:** mainnet block **26204126**, timestamp **1792142039**.
**Node:** archive reth; every on-chain claim is pinned here except the explicitly
labelled historical vectors used to exercise above-target and pre-maturity
branches.

This is the canonical resumption guide for the unmodeled-family epic. Three
companion evidence files retain the full command/output transcripts and model
notes:

- [`2026-11-18-balancer-linear-evidence.md`](2026-11-18-balancer-linear-evidence.md)
- [`2026-11-18-balancer-routing-yieldspace-evidence.md`](2026-11-18-balancer-routing-yieldspace-evidence.md)
- [`2026-11-18-balancer-cross-cutting-evidence.md`](2026-11-18-balancer-cross-cutting-evidence.md)

## 0. Executive decision

**Linear pools are a conditional GO.** Their deployed math is compact and now
has a 0-wei oracle: a checked-uint256 transcription of `LinearMath.sol` and
`ScalingHelpers.sol` matched **432/432** quote cases across 48 deployed pools
(155 value-equal, 277 revert-class-equal). Build them for coverage if the
project accepts the graph-admission and state-sync prerequisites. Measured
near-term arb value is weak: zero Linear-pool swaps in the recent 100k-block
window and only 25/48 pools quotable in both directions at the pin.

**Boosted-underlying routing is a NO-GO at this pin.** The exact three-leg
route is proven, but the referenced bb-s-DAI pool holds only 1.8177 DAI and
1000 USDC returns 1.7951 DAI. It is a correctness reference, not a live venue.
Reopen if liquidity changes and Linear pools have landed.

**Element, Sense, adapters, FX/custom, VPT, rate-anchored/TwapLive, mocks and
drained no-name pools are NO-GO.** Their current flow does not justify bespoke
invariants. Element and Sense are also past maturity and drained.

Two cross-cutting facts outrank every builder:

1. **Balancer currently has no graph admission.** `PoolKind` is V2/V3/V4 only,
   so another Balancer builder does not make a pool discoverable by
   `find_paths` or the backrun DFS. Decide and test graph admission explicitly.
2. **Balancer currently has no Vault log ingestion.** Refresh is pull-based,
   and `refresh_balancer` stamps `block.unwrap_or(0)`, making an update without
   an explicit block a silent no-op. Fix that clock before relying on any new
   family's live state.

## 1. Corpus and arb relevance

The corpus is `tests/balancer/data/balancer_v2_pool_registrations_26204126.json`:
1,954 Vault registrations, of which 173 are positively identified Balancer
pools whose weighted/stable family probes do not answer.

The measured family breakdown is:

| Group | Pools | Evidence at the pin | Verdict |
|---|---:|---|---|
| Linear (`bb-*`: Aave, ERC4626, Euler, Yearn, Morpho, Idle, etc.) | 48 | 0 recent swaps; 8 older swaps + 4 joins/exits; exact model complete | **Conditional GO** for coverage, after graph/clock decisions |
| Element principal-token pools | 37 | 0 swaps in 500k blocks; 138 joins/exits; matured | **NO-GO** |
| Element adapters | 23 | 0 swaps and 0 joins/exits in 500k blocks; two-token adapters, not Linear | **NO-GO** |
| Sense Space | 25 | 1 older swap + 1 join/exit; matured/drained | **NO-GO** |
| FX/custom (Xave EURS/XSGD/fxPHP/GBPT, other bespoke curves) | 14 | 3 recent swaps; bespoke invariants | **NO-GO** pending material venue evidence |
| Mock Linear (`DO NOT USE`, symbol `TEST`) | 13 | Test deployments | **NO-GO permanently** |
| RateAnchored / TwapLive | 7 | 9 recent swaps + 14 joins/exits, but 1-wei/test-like balances | **NO-GO for now**; re-evaluate after real volume |
| Verified Pool Token | 3 | Old, no measured activity | **NO-GO** |
| No-name | 3 | Zero balances | **NO-GO** |

Activity was measured from Vault `Swap` and `PoolBalanceChanged` logs over two
windows: recent 100k blocks and the preceding 400k. Across all 173 pools there
were **21 swaps and 164 joins/exits over 500k blocks**. Eighteen pools had any
recent event, twenty had any event over the whole interval, and seventeen had
zero balances. Registrations are not evidence of flow; the newest
RateAnchored rows look like test deployments rather than markets.

The original census's 62 Element principal-token rows separate into 37 Element
pools plus 25 Sense pools. The two addresses originally called "Compound
adapters" (`0x000b87c8…` and `0x06C55Dad…`) are two-token `cUSDC-adapter`
pools holding cUSDC and a Sense principal token. They are not Linear pools and
must not become Linear fixtures.

### Graph ceiling

`degenbot-pathfinding/src/graph.rs::PoolKind` and
`degenbot-db/src/pathfinding.rs::fetch_path_graph_edges` admit only V2/V3/V4.
Unknown `pools.kind` is refused. The backrun lane's family projection is
similarly narrow. Weighted and Stable pools are already outside this graph;
Linear would be outside it too. Direct `Bot.build_pool` and hand-built paths
can use a new family, but automatic discovery requires a separate product
decision and red tests for `pools.kind`, graph-edge conversion, and backrun
family tagging.

## 2. Linear pools: exact swap math

### 2.1 Deployed sources and population

Primary sources read: Balancer V2 monorepo `LinearPool.sol`, `LinearMath.sol`,
`ScalingHelpers.sol`, `BalancerQueries.sol`, and deployed verified sources for
bb-s-DAI, bb-a-DAI, bb-e-DAI, bb-i-DAI, the Vault, and BalancerQueries.
Deployed `LinearMath.sol` matches master except import order in the 2021
wrapper. The wrapper revisions differ in indices, target packing, and virtual
supply.

There are **48 ERC-20 Linear pools**, all `GENERAL` specialization with three
tradable tokens: main, wrapped, and their own BPT. None is `TWO_TOKEN`. BPT
index varies: 41 at 0, 4 at 2, 3 at 1. Main index varies too (18 at 2).
Never infer an index from the family name.

The Vault vector includes BPT because BPT↔main and BPT↔wrapped are the
join/exit swap kernels. `totalSupply` is usually dominated by `2^112-1`
pre-minted BPT, but 24 pools are below that value because BPT was burned.
`totalSupply - BPT-in-Vault` is the 2023 real supply; the 2021 revision uses
`(2^112-1) - BPT-in-Vault`.

### 2.2 Fixed point, scaling, and fee placement

All arithmetic is checked uint256: underflow reverts (`BAL#001`), never wraps.
Scaling uses `mulDown`; output rounds down for GIVEN_IN and up for GIVEN_OUT.
The BPT kernels use plain `Math.divDown/divUp`, not FixedPoint's 1e18-scaled
division.

```text
invariant = toNominal(mainBalance) + wrappedBalance

toNominal(real):
  real <  lower: real - mulDown(lower - real, fee)
  real <= upper: real
  else:          real - mulDown(real - upper, fee)
```

The fee is a **real-to-nominal rebalancing wedge**, not an output haircut:

| Main balance | Main → wrapped marginal price | Wrapped → main marginal price |
|---|---|---|
| Inside targets | 1 | 1 |
| Below lower | `1 + fee` | `1 / (1 + fee)` |
| Above upper | `1 - fee` | `1 / (1 - fee)` |

A band-crossing swap can pay or capture the entire wedge. This is why applying
`fee` to final output is structurally wrong.

### 2.3 The twelve kernels

| Pair | GIVEN_IN | GIVEN_OUT |
|---|---|---|
| main→wrapped | `toNominal(m+in) - toNominal(m)` | `toNominal(m) - toNominal(m-out)` |
| wrapped→main | `m - fromNominal(toNominal(m)-in)` | `fromNominal(toNominal(m)+out) - m` |
| main→BPT | `divDown(supply*Δnominal, invariant)` | main-in required for requested BPT |
| wrapped→BPT | `divDown(supply*newInv, prevInv)-supply` | wrapped-in required for requested BPT |
| BPT→main | `m - fromNominal(toNominal(m)-divDown(inv*bptIn,supply))` | `divUp(supply*Δnominal,invariant)` |
| BPT→wrapped | wrapped remaining after BPT burn | BPT required for requested wrapped |

The full expanded formulas and rounding directions are in the Linear evidence
file §5. The implementation should port the deployed functions directly rather
than re-deriving them from this summary.

### 2.4 Parity result and reference vectors

The model used only chain reads: pool id, indices, targets, fee, live rate,
cash/managed balances, totalSupply, BPT balance, and token decimals.

```text
48 pools × 9 cases = 432 cases
432 matched / 0 mismatched
155 exact returned integers; 277 exact revert classes
```

Representative 0-wei vectors:

| Pool / state | Case | On-chain result |
|---|---|---:|
| bb-s-DAI `0x2B218683…5176`, ERC4626, inside | main→wrapped, 1 wei | 0 |
| same | wrapped→main, 9 wei | 10 |
| same | main→BPT, `1e16` | `7406999999999999840` |
| same | BPT→main, `1e16` | `13500742540839` |
| bb-a-DAI `0x804CdB91…bF8f`, 2021 Aave, below lower | main→wrapped, `1e18` | `839134750951962919` |
| same | wrapped→main, `1e18` | `1191703714886723798` |
| bb-e-DAI `0xeB486AF8…bc0`, Euler, below lower | main→wrapped, `1e18` | `1000200000000000000` |
| same | wrapped→main, `1e18` | `999800039992001600` |
| bb-i-DAI `0xDbA274B4…8037`, above upper, **block 17418847** | main→wrapped, `1e18` | `973416028191958334` |
| same, band-crossing | wrapped→main, `25000e18` | `25682539495794522189098` |

The last value equals the bare conversion plus
`4,926,557,340,864,239,098` wei—the entire captured fee wedge. No corpus
Linear pool is above its upper target at pin 26204126, so the above-target
branch is deliberately evidenced at the separate archive block, not attributed
to the pin.

Two quote guards are part of chain truth:

- **Vault cash guard.** Payout cannot exceed the token's Vault cash. Managed
  balances cannot fund it. Eight pools are one-directional for this reason;
  bb-s-DAI holds only 9 wei sDAI and reverts `BAL#001` on a `1e15` DAI output.
- **Nominal underflow.** Six pools revert `BAL#001` in `_toNominal` on every
  swap. A wrapping model would incorrectly quote them.

At the pin 28 pools are inside targets, 11 below, 9 unmeasurable due to rate
reverts, and none above. Twenty-five quote both directions, eight quote one,
and fifteen quote neither.

## 3. Linear builder inputs and rate revisions

### 3.1 Required state

A complete builder needs:

- address, Vault, pool id;
- `getMainToken`, `getWrappedToken`, and explicit main/wrapped/BPT indices;
- Vault `getPoolTokens`: token vector, balances, `lastChangeBlock`;
- cash and managed balances, because only cash funds payouts;
- `getTargets`, `getSwapFeePercentage`;
- token decimals and scaling factors;
- wrapped-token rate from the revision-specific source below;
- `totalSupply` and Vault BPT balance for real/virtual supply;
- `update_block` and the registration journal's genesis anchor.

`getBptIndex()` existence and the token vector must agree. BPT supply is
max-minted phantom supply in shape, but burns are real and revision-specific;
use the wrapper's formula rather than a constant.

### 3.2 Rate source by deployed revision

| Wrapper / wrapped token | Rate read |
|---|---|
| ERC4626 | `convertToAssets(10^(18 + wrappedDecimals - mainDecimals))` |
| 2021 AaveLinearPool | Aave v2 `getReserveNormalizedIncome(mainToken) / 1e9` |
| Euler | `convertBalanceToUnderlying(10^(36 - mainDecimals))` |
| Yearn | `pricePerShare()` |
| Aave-v3-as-ERC4626 deployments | ERC4626 getter may revert `Not implemented`; refuse rather than substitute |

Across 39 distinct wrapped tokens, 14 use `convertToAssets`, 12 expose Aave
static-token `rate()`, 3 use Yearn `pricePerShare`, and 10 need another
revision-specific getter. No corpus Linear pool wraps cDAI or cUSDC.

**Rates are live on every quote.** The `PriceRateCache` import is dead in these
deployed LinearPools; there is no cache slot or duration. Read the rate at the
same block as the balances and math on every calculation, like the composable
stable arm's rate-provider values. Never cache it across blocks, and never
translate a reverting rate into zero.

`getRate()` is different from the wrapped rate: it is the BPT price
`divUp(invariant, virtualSupply)`. Sixteen pools have `getRate()` reverting at
the pin and nine have `getWrappedTokenRate()` reverting.

### 3.3 Wrapper revisions

| Property | 2021 | 2023 |
|---|---|---|
| BPT position | Any sorted index (0/1/2) | Composable registration, index 0 |
| Targets | Two `uint96` in BasePool miscData | Two `uint32` × `1e18` in pool state |
| Swap virtual supply | `(2^112-1) - bptBalance` | `totalSupply - bptBalance` |
| `getRate()` denominator | Approximate supply above | Exact supply above |

One 2021 pool has a 680.81× difference between those supply formulas. It is
unquotable today, but using the wrong revision would still be a correctness
bug.

## 4. Boosted-pool underlying routing

### 4.1 Verified batch

The live route is **one `Vault.queryBatchSwap`** with three steps:

1. USDC → bb-s-USDC BPT through the Linear wrap/join;
2. bb-s-USDC BPT → bb-s-DAI BPT through the ComposableStable swap;
3. bb-s-DAI BPT → DAI through the Linear unwrap/exit.

```text
assets = [USDC, bb-s-USDC BPT, bb-s-DAI BPT, DAI]

swaps[0] = (bb-s-USDC poolId, 0, 1, 1_000_000, 0x)
swaps[1] = (bb-s-USD poolId,  1, 2, 0,         0x)
swaps[2] = (bb-s-DAI poolId,  2, 3, 0,         0x)
funds = (0x0, false, 0x0, false)
```

`BatchSwapStep` has no limit field. Intermediate amounts are zero because the
Vault chains the prior output. There is no recipient/internal-balance
requirement for a quote.

Verified at the pin:

```text
1 USDC    -> deltas [1000000, 0, 0, -999710385659313643]
1000 USDC -> deltas [1000000000, 0, 0, -1795084415339484096]
```

The main session independently reran the 1-USDC batch and obtained the exact
same deltas.

### 4.2 Leg arithmetic and existing machinery

- L1: `bptOut = realSupply * amountIn // invariant`.
- L2: existing StableMath over BPT-skipped, rate-upscaled balances and live
  rate-provider values.
- L3: `out = invariant * bptIn // realSupply`.

All three legs reproduce to 0 wei. `realSupply = totalSupply - BPT-in-Vault`.
The middle leg is exactly the current `BalancerStablePoolIdentity`,
`BalancerRateProvider`, `capture_hop`, and `read_swap` machinery. The outer
legs require LinearMath and real supply.

The composable's cached `getTokenRate()`/`getScalingFactors()` is not the live
quote rate. Using the live Linear pool `getRate()` matches; substituting the
cached `1e18` changes L2 output by 22,494,497,741,881 wei. All legs must use
one block-consistent rate view.

`queryJoin` and `queryExit` revert `BAL#998 UNIMPLEMENTED` on these pools.
The quotable representation is therefore a **multi-hop swap route**, not a
native join/exit simulation. A Vault batch encoder is needed only for atomic
execution, not for quoting. Collapsing the three legs to one two-token hop is
38 bp wrong on the 1-USDC reference.

### 4.3 Venue verdict

bb-s-DAI holds `1.817702680875126024` DAI. A 1000-USDC input returns only
`1.795084415339484096` DAI. **Do not start boosted-underlying implementation
for current flow.** Preserve this route as the future 0-wei test vector and
reopen when liquidity and graph admission justify it.

## 5. Time-dependent Yield Space

### 5.1 Element ConvergentCurvePool

Required inputs:

```text
underlyingReserve, bondReserve, totalSupply(), expiration,
unitSeconds, percentFee, block.timestamp
```

The exponent is `a = 1 - (expiration - block.timestamp) / unitSeconds`.
`onSwap` inflates the bond reserve by LP `totalSupply`, then evaluates the
power invariant `x^a + y^a = k`. The fee is output-token denominated.

At historical block 16000000, the contract and model reproduce the Vault
quotes exactly when the contract's own `solveTradeInvariant` output is used;
the pure `LogExpMath` model differs only by its documented floor (a few
thousand wei at `1e18` scale). At that point the timestamp changes quotes by
1.05–1.30% on identical reserves. At the pin the pool is past maturity
(2023-02-24), `a = 1`, reserves are drained, and quotes revert `BAL#001`.

### 5.2 Sense Space

Required inputs:

```text
reserves[pti], reserves[1-pti], _initScale, totalSupply(),
ts, maturity, g1, g2, block.timestamp, adapter.scaleStored()
```

The pool computes `t = ts * max(maturity - now, 0)` and
`a = 1 - (pTIn ? g2 : g1) * t`, then evaluates a power invariant. It uses
**two scales in one swap**: private `_initScale` for reserves and the live
adapter scale for traded amounts. `_initScale` has no getter (slot 8 is
`1.004941` at the researched blocks); the view-safe live read is
`adapter.scaleStored()`, not state-changing `scale()`.

At the pin the pool is past maturity (2023-01-01), `a = 1`, totalSupply is
1e6 wei of dust, and it was never traded. There is no venue to justify this
port.

### 5.3 Timestamp seam

The existing Curve seam is sufficient:
`CurveDataProvider::block_timestamp` → `resolve_dy_inputs` →
`DyCalculationInputs.block_timestamp`. It is already unconditional on the
`get_dy` path and also governs metapool cache freshness.

A Yield Space family should reuse that trait and carry the timestamp in its
hop inputs. The only necessary policy change is to refuse a missing timestamp
with a typed error instead of `unwrap_or(0)`: zero silently selects the
matured constant-sum branch.

**Element and Sense remain NO-GO as venues.** Their formulas and timestamp
entry points are now documented so a future live deployment does not require
rediscovery.

## 6. Registration and state synchronization

### 6.1 Current weighted/stable shape

Weighted registration reads `getPoolId`, Vault `getPoolTokens`,
`getSwapFeePercentage`, `getNormalizedWeights`, token decimals/scaling, and a
deployed-code `pow_version`. Stable reads the common fields plus
`getAmplificationParameter`, optional `bpt_idx`, invariant version, and
rate-provider-derived scaling factors. Registration rechecks vector arity and
seeds a genesis balance journal.

Current stable construction stores `rate_provider: None` even though the
trait exists. Linear and boosted work should not repeat that gap: live rates
are calculation inputs, not optional metadata.

### 6.2 There is no Vault event-ingestion path

`PoolBalanceChanged` exists only as a balance-vector event vocabulary. There
is no Balancer decoder or subscription in the live ingestion/updater path.
The current state update is `refresh_pool → refresh_balancer` → Vault
`getPoolTokens` → `apply_balances`, with reorg history from balance deltas.

`refresh_balancer` currently:

- fetches `lastChangeBlock` and discards it;
- stamps `block.unwrap_or(0)`; and
- consequently makes `Bot.update(balancer_pool)` without an explicit block a
  silent no-op after registration.

The cheap prerequisite is to resolve the fetch block and stamp it (using
`lastChangeBlock` consistently with the other refresh arms). First red test:
`refresh_balancer_stamps_the_fetched_block`.

### 6.3 What each family adds

| Family | Registration/state addition |
|---|---|
| Linear | Linear params, explicit indices, targets, real/virtual supply, revision-specific live rate reader, cash-aware quote behavior |
| Boosted underlying | No new pool family; a three-leg route over Linear + existing Stable, with block-consistent rates and optional Vault batch execution encoding |
| Element | Yield Space state, totalSupply, expiration/unitSeconds, timestamp input and pow math |
| Sense | Element's inputs plus `pti`, `g1/g2`, `_initScale`, and live `adapter.scaleStored()` |
| Custom/FX/VPT/rate-anchored | A separately proved invariant and registration/sync model per family; no shared shortcut |

Every family needs explicit reorg/journal behavior and a pinned-block refresh
path. No family should infer state freshness from registration alone.

## 7. Implementation plan and first red tests

### 7.1 Cross-cutting first

1. Decide Balancer graph admission: `pools.kind` vocabulary, graph-edge
   conversion, `PoolKind`, and backrun family projection. First red test: a
   Balancer row reaches `find_paths` rather than `UnknownPoolKind`.
2. Fix refresh clocks. First red test: `refresh_balancer_stamps_the_fetched_block`.
3. Split sweep vocabulary by measured family rather than keeping all 173 under
   one expectation: Linear must-build, named no-go families, mocks, and broken
   rows with exact refusal reasons. First red test: corpus rows resolve to the
   new family vocabulary instead of `BalancerUnmodeledFamily`.

### 7.2 Linear, if the conditional GO is accepted

1. Add `BalancerFamily::Linear` resolution using the full
   `getMainToken`/`getWrappedToken`/`getTargets` shape, with non-pool mocks
   excluded by `getVault`/ERC-20 and corpus assertions.
2. Add `build_balancer_linear` and typed params. Read indices rather than
   assuming order; preserve 2021/2023 supply and target revisions.
3. Port `LinearMath` with checked uint256 and the deployed `Math`/`FixedPoint`
   rounding split. Include the Vault cash guard and named rate/refusal paths.
4. Add live revision-specific rate adapters and reorg-safe refresh state.
5. Add the Python `BalancerV2LinearPool` wrapper and descriptor mapping, then
   regenerate stubs through the justfile.

First red tests:

- `build_balancer_linear_assembles_params` on bb-s-DAI;
- the bb-e-DAI below-target vector above, then bb-a-DAI's 2021 BPT-at-2 case;
- BPT join/exit and GIVEN_OUT vectors, including the plain-`Math` division
  distinction;
- Vault cash exhaustion and `_toNominal` underflow match chain revert classes;
- pinned corpus: buildable Linear rows go green; rate-broken, cash-guarded,
  mock, and custom rows keep named refusals.

The above-target bb-i-DAI historical vector belongs in the math suite even
though it is outside the sweep pin.

### 7.3 Boosted, only after a new value case

Use a multi-hop Linear → Stable → Linear representation. Reproduce the exact
1-USDC batch to 0 wei and then add USDT/DAI and reverse-direction vectors.
Keep rates block-consistent. Do not build a separate join/exit simulator.

### 7.4 Deferred families

Do not open implementation for Element, Sense, adapters, FX/custom, VPT, or
RateAnchored/TwapLive until a named venue demonstrates material flow. The
first test then is the family's historical live quote, with timestamp,
totalSupply, and scale inputs made explicit. Mocks and zero-balance no-name
rows require no implementation.

## 8. Final go/no-go table

| Work item | Decision now | Resume condition |
|---|---|---|
| Refresh clock and sweep vocabulary | **GO** | None; cheap correctness work |
| Balancer graph admission | **Decision required before arb-value claims** | Product choice plus graph tests |
| Linear pools | **Conditional GO** | Accept graph/sync prerequisites; exact oracle is ready |
| Boosted underlying routing | **NO-GO at pin** | Real liquidity after Linear lands |
| Element CCP | **NO-GO** | Live pre-maturity venue with flow |
| Sense Space | **NO-GO** | Live pre-maturity venue with flow and scale evidence |
| Element adapters | **NO-GO** | Named venue with flow; not a Linear subset |
| FX/custom, VPT, rate-anchored/TwapLive | **NO-GO** | Material volume justifies each invariant |
| Mock and zero-balance no-name pools | **NO-GO** | Never for mocks; new legitimate deployment for no-name |

The central research result is that Linear modeling is no longer a math risk:
it is a checked-arithmetic, revision, live-rate, and integration problem. The
remaining go decision is whether those integration costs buy something the
pathfinding and venue economics can use.
