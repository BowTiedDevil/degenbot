# Balancer V2 Linear pools — exact-math, rate-source, and builder-input evidence

**Scope:** Track A (Linear pools) research only. No source, test, or build changes; the only
repository write is this document.
**Node:** archive reth at `http://host.containers.internal:8545` (`TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`),
`eth_chainId` = `0x1`, head at probe time = `0x1937159` (26440025).
**Pin:** `26204126` for every state/quote claim, except one clearly-labelled archive-block vector
(see §6.4) because at the pin no corpus Linear pool sits above its upper target.
**Corpus:** `tests/balancer/data/balancer_v2_pool_registrations_26204126.json`
(`pin_block: 26204126`, Vault `0xBA12222222228d8Ba445958a75a0704d566BF2C8`).
**Quoter:** `BalancerQueries` `0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5` (verified source, §3).

---

## 0. Bottom line

**GO for a Linear-pool arm, with a hard scope: 12 swap kernels, one rate getter per wrapped-token
revision, checked-uint256 revert parity, and two quote guards (Vault cash, nominal underflow).**
A transcription of `LinearMath.sol` plus `ScalingHelpers.sol`, run as a checked-uint256 model,
reproduced **432/432** on-chain `querySwap` results at the pin **exactly (0 wei, no rounding drift)**
across 48 deployed pools, including the 277 cases that must revert and the exact revert class.
The model is small (five pure helpers plus twelve kernels) and fully determined by chain reads; no
constant in it is a guess.

Costs and risks that dominate the build (details in §7, §8, §10):

* Linear pools are **3-token GENERAL pools** whose third token is the BPT itself: BPT↔main and
  BPT↔wrapped swaps are join/exit paths that a swap-level model must also price or refuse.
* The **BPT index is not fixed** (0, 1, and 2 all occur in deployed pools); main/wrapped indices are
  not fixed either (main at index 2 exists).
* **Checked arithmetic is observable.** 6/48 pools at the pin sit in a state where
  `LinearMath._toNominal` underflows and *every* swap reverts `BAL#001`; 16/48 have `getRate()`
  reverting. A model that silently wraps (or refuses to model reverts) will quote where the chain
  reverts.
* **The rate source is read live on every quote** (there is no cached rate and no duration in any
  deployed LinearPool revision, despite `PriceRateCache` being imported as dead code). 9/48 pools are
  unquotable at the pin because the wrapped-token rate call itself reverts.
* **The Vault's payout guard is part of the quote.** `onSwap` math alone is not enough: the Vault
  reverts `BAL#001` if the payout exceeds the pool's *cash* balance for that token (managed balances
  cannot fund a swap). 8/48 pools are one-directional at the pin purely for this reason.

---

## 1. Method

All on-chain reads used `cast call --rpc-url $RPC --block 26204126` (or the equivalent
`eth_call` with an explicit block), and the quoter path used a hand-encoded
`BalancerQueries.querySwap` call (selector `0xe969f6b3`). No test suite, build, or repo file was run
or modified. Scratch probes live under `/tmp` only.

Quote template (this is the exact call shape used for every vector):

```bash
SIG='querySwap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool))'
cast sig "$SIG"        # 0xe969f6b3
cast call --rpc-url http://host.containers.internal:8545 --block 26204126 \
  0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5 \
  "$SIG" \
  "($POOL_ID,0,$ASSET_IN,$ASSET_OUT,$AMOUNT,0x)" \
  "(0x0000000000000000000000000000000000000000,false,0x0000000000000000000000000000000000000000,false)"
```

ABI encoding note (used by the model harness, verified against `cast decode-calldata`): the head is
five words — `SingleSwap` offset `0xa0`, then the four static `FundManagement` words — and the swap
tuple is `[poolId, kind, assetIn, assetOut, amount, 0xc0, userDataLen, userData…]`. `funds.sender`
and `funds.recipient` are never read by the query path: `Vault.queryBatchSwap` performs no token
transfers at all (`Swaps.sol`: *"Does not cause any transfer of tokens - instead it returns the net
Vault token deltas"*), and `BalancerQueries.querySwap` is a thin single-step wrapper over it.

Reference vector, full calldata (bb-s-DAI, 1 wei DAI → sDAI, 778 hex chars):

```text
0xe969f6b300000000000000000000000000000000000000000000000000000000000000a0
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
2b218683178d029bab6c9789b1073aa6c96e517600000000000000000000058c
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000006b175474e89094c44da98b954eedeac495271d0f
00000000000000000000000083f20f44975d03b1b09e64809b757c47f942beea
0000000000000000000000000000000000000000000000000000000000000001
00000000000000000000000000000000000000000000000000000000000000c0
0000000000000000000000000000000000000000000000000000000000000000
```

Returned word: `0x0000000000000000000000000000000000000000000000000000000000000000`.

---

## 2. Three brief premises that the chain contradicts

### 2.1 "Compound adapters `0x000b87…` / `0x06C55D…`" are not Linear pools

```bash
cast call --block 26204126 0x000b87c8A4c6CBCEf7a2577e8aa0Dc134C67c3D8 'name()(string)'
# "Compound USD Coin Adapter"
cast call --block 26204126 0x000b87c8A4c6CBCEf7a2577e8aa0Dc134C67c3D8 'symbol()(string)'
# "cUSDC-adapter"
cast call --block 26204126 0x000b87c8A4c6CBCEf7a2577e8aa0Dc134C67c3D8 'getMainToken()(address)'
# Error: execution reverted, data: "0x"
cast call --block 26204126 0x000b87c8A4c6CBCEf7a2577e8aa0Dc134C67c3D8 'getPoolId()(bytes32)'
# 0x000b87c8a4c6cbcef7a2577e8aa0dc134c67c3d8000200000000000000000178
```

Measured facts:

| address | symbol | poolId specialization | Vault tokens (`getPoolTokens`) | balances |
| --- | --- | --- | --- | --- |
| `0x000b87c8A4c6CBCEf7a2577e8aa0Dc134C67c3D8` | `cUSDC-adapter` | `…0002…` = TWO_TOKEN | `[0x003d32a8…, 0x39AA39c0… cUSDC]` | `[1181490, 308845270292]` |
| `0x06C55Dad9a50aDbC824Ea50eF7acF54a290E5493` | `cUSDC-adapter` | `…0002…` = TWO_TOKEN | `[0x39AA39c0… cUSDC, 0xE54B2AEA…]` | `[79935968258, 79162223]` |

`0x003d32a8C728Ed4d452fD06C07491d87a723a9C9` is `"1st July 2022 cUSDC Sense Principal Token, A3"`
(`sP-cUSDC:01-07-2022:3`, 8 decimals) and `0xE54B2AEAa78CfeD31fE4779244F36D7d322b0aC1` is the A4
sibling. The two `cUSDC-adapter` addresses are two-token Vault pools that custody one of those Sense
principal tokens plus cUSDC; the PT names identify the Sense fixed-rate family (the adapter contract
itself is unverified on both explorers, so the vendor label is inference, the custody composition is
measured). They answer `getPoolId`/`getVault`/ERC-20 metadata but have **no LinearPool ABI at all** (`getMainToken`, `getWrappedToken`, `getTargets`, `getSwapFeePercentage`,
`getRate` all revert with empty data). They are out of scope for Linear math and are the only
`TWO_TOKEN` entries the brief's "Compound" naming points at.

No corpus address exposes a LinearPool over a Compound wrapped token: the wrapped tokens of the 48
ERC-20 Linear pools are Aave v2/v3 static tokens, sDAI, ERC-4626 vaults (Euler, Yearn, Maple, Idle,
Fuji, Instadapp, Gearbox), agEUR and stkAPE. There is no cDAI/cUSDC-wrapped Linear pool to verify.

### 2.2 Linear pools are `GENERAL` (3 tokens, BPT included), never `TWO_TOKEN`

Corpus rows for the pools in this report: `specialization: 0` (= `IVault.PoolSpecialization.GENERAL`).
Across the 51 corpus addresses that answer `getMainToken`, specialization is `0` for 48 and `1`
(`MINIMAL_SWAP_INFO`) for 3 non-pool addresses (§4.1); **zero** are `2` (`TWO_TOKEN`).

The consequence matters more than the label: `getPoolTokens` returns **three** tokens and the BPT is
itself tradable through the Vault (`_swapGivenBptIn`/`_swapGivenBptOut` in `LinearPool.sol`). BPT at
index 0 is the composable-registration case; index 1 and 2 exist in the older revision (§7.2).

### 2.3 There is no cached rate and no duration in any deployed Linear pool

Every LinearPool source revision imports `@balancer-labs/v2-pool-utils/contracts/rates/PriceRateCache.sol`
and declares `using PriceRateCache for bytes32;`, but declares **no cache storage slot and calls none
of the library's functions**. Measured: `getWrappedTokenRate()` on all 4 pools below changes on every
block where the underlying rate moves, and each `onSwap` performs the rate `STATICCALL` inline (visible
in `debug_traceCall`). The "plain cache + duration" shape from the brief does not exist in this pool
family; the rate is live, per quote.

---

## 3. Primary sources read

Monorepo (network): `balancer/balancer-v2-monorepo@master`

* `pkg/pool-linear/contracts/LinearMath.sol` (336 lines) — the 12 kernels, `_toNominal`, `_fromNominal`, `_calcInvariant`.
* `pkg/pool-linear/contracts/LinearPool.sol` (771 lines) — `_onSwapGeneral`, the six `_swapGiven*` dispatchers, `getScalingFactors`, `getRate`, `getTargets`, `getSwapFeePercentage`, `getVirtualSupply`, `_getVirtualSupply`, `_INITIAL_BPT_SUPPLY`, `_TARGET_SCALING`, `_TARGET_BITS`, `_MAX_UPPER_TARGET`, `setTargets`, `setSwapFeePercentage`.
* `pkg/standalone-utils/contracts/BalancerQueries.sol` — `querySwap` = one-step `queryBatchSwap`.

Deployed exact-match sources (Sourcify v2 `exact_match` / Blockscout verified), i.e. the bytecode
actually running at the pin:

| contract | address | verified name | files |
| --- | --- | --- | --- |
| bb-s-DAI | `0x2B218683178d029BAB6c9789b1073aA6c96E5176` | `ERC4626LinearPool` | 48 |
| bb-a-DAI (2021 rev) | `0x804CdB9116a10bB78768D3252355a1b18067bF8f` | `AaveLinearPool` | 39 |
| bb-e-DAI | `0xeB486AF868AeB3b6e53066abc9623b1041b42bc0` | `EulerLinearPool` | 48 |
| bb-i-DAI (ERC-4626/Idle) | `0xDbA274B4D04097b90A72b62467d828cEFD708037` | `ERC4626LinearPool` | 48 |
| Vault | `0xBA12222222228d8Ba445958a75a0704d566BF2C8` | `Vault` | 45 |
| BalancerQueries | `0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5` | `BalancerQueries` | 81 |

Revision check (the point of reading deployed sources rather than master):

```text
diff master LinearMath.sol  deployed LinearMath.sol (bb-s-DAI)  -> 0 lines (byte-identical)
diff master LinearMath.sol  deployed LinearMath.sol (0x804CdB)   -> 2 lines (import order only)
diff master LinearPool.sol  deployed (bb-s-DAI)                  -> comments + one function visibility
diff master LinearPool.sol  deployed (0x804CdB, 2021)            -> structural (BasePool, 96-bit targets;
                                                                    see §7.2)
```

So: **the math is one implementation**, and the modeling risk lives in the pool wrapper revision
(indices, targets packing, virtual supply) and in the rate getter — not in `LinearMath`.

Symbols cited throughout: `LinearMath._calcWrappedOutPerMainIn`, `_calcMainOutPerWrappedIn`,
`_calcWrappedInPerMainOut`, `_calcMainInPerWrappedOut`, `_calcBptOutPerMainIn`, `_calcBptInPerMainOut`,
`_calcMainInPerBptOut`, `_calcMainOutPerBptIn`, `_calcBptOutPerWrappedIn`, `_calcBptInPerWrappedOut`,
`_calcWrappedInPerBptOut`, `_calcWrappedOutPerBptIn`, `_calcInvariant`, `_toNominal`, `_fromNominal`;
`ScalingHelpers._upscale`, `_downscaleDown`, `_downscaleUp`, `_computeScalingFactor`;
`FixedPoint.mulDown/mulUp/divDown/divUp/add/sub`; `Math.divDown/divUp/mulUp`;
`LinearPool.getScalingFactors`, `getRate`, `getVirtualSupply`, `_getVirtualSupply`, `_getWrappedTokenRate`,
`_onSwapGeneral`, `_swapGivenBptIn/_swapGivenBptOut`.

---

## 4. What the corpus actually contains

### 4.1 Census at the pin

`family: "unmodeled"` rows: **173**. Probing `getMainToken()` on each (batched `eth_call` at the pin):
**64 answer**. Of those, 13 have symbol `TEST` (test deployments) and 3 are non-pools:

| address | corpus spec | behaviour |
| --- | --- | --- |
| `0x30004CDab9Aab2060Bc230455c874981064CCD1b` | `1` | answers `getMainToken`/`getWrappedToken`/`getPoolId`; `name`, `symbol`, `getVault` revert; `getBptIndex` = `type(uint256).max` |
| `0x8B16914F0F9C5Ba44377787cc120D3713B0959E1` | `1` | same, `getBptIndex` = 2 |
| `0xD098bA344Ab1F1068360d07358a7921b591a46E7` | `1` | same, `getBptIndex` = 2 |

The remaining **48** are ERC-20 Linear pools and are the population behind every number in this
document. Their distributions at the pin:

| property | distribution |
| --- | --- |
| pool specialization | 48 × `GENERAL` (0); 3 × `MINIMAL_SWAP_INFO` only among the non-pools above |
| BPT index | 41 × `0`, 4 × `2`, 3 × `1` |
| main/wrapped index pair | 5 distinct orders: `(1,2)` × 23, `(2,1)` × 18, `(1,0)` × 3, `(0,2)` × 3, `(0,1)` × 1 |
| target position of main balance | 28 inside, 11 below lower, 9 unmeasurable (rate reverts) |
| `getWrappedTokenRate()` reverts | 9 |
| `getRate()` reverts | 16 |
| `_toNominal(main)` underflows (checked `sub`) | 6 |
| any `managed` balance > 0 | **0** (all asset managers are empty at the pin; `cash == total` everywhere) |
| `totalSupply` below `2^112-1` (BPT burned) | 24 |
| quotable both directions (1-wei probe) | 25 |
| quotable one direction only (cash-limited) | 8 |
| both directions revert | 15 |

### 4.2 Rate-interface census (39 distinct wrapped tokens)

| wrapped-token rate interface | tokens |
| --- | --- |
| `convertToAssets(uint256)` (ERC-4626) | 14 |
| `rate()` returning a 1e27 ray (Aave static aToken) | 12 |
| `pricePerShare()` (Yearn v2 vault) | 3 |
| none of the above (Euler, Gearbox, Instadapp, …) | 10 |

---

## 5. The exact math

### 5.1 Fixed point (as deployed, `FixedPoint.sol` / `Math.sol`)

```text
mulDown(a,b) = a*b / 1e18
mulUp(a,b)   = 0 if a*b == 0 else (a*b - 1)/1e18 + 1
divDown(a,b) = a*1e18 / b
divUp(a,b)   = 0 if a == 0 else (a*1e18 - 1)/b + 1
Math.divDown(a,b) = a / b                    # plain, used by the BPT kernels
Math.divUp(a,b)   = 0 if a == 0 else 1 + (a-1)/b
add(a,b), sub(a,b) are CHECKED: BAL#000 / BAL#001
```

`LinearMath` reaches `FixedPoint.add/sub` through `using FixedPoint for uint256`, and both are
`_require`-guarded in both deployed revisions (2021 and 2023 sources diff only in `mulUp/divUp`
inline assembly). **Underflow reverts; it never wraps.** Measured: `bb-e-USDT`
`0x3C640f0d…` `querySwap(main→wrapped, 1)` → `execution reverted: BAL#001` inside the pool's `onSwap`.

### 5.2 Scaling (18-decimal normalisation)

```text
sf_main    = 1e18 * 10^(18 - mainDecimals)
sf_wrapped = mulDown(1e18 * 10^(18 - wrappedDecimals), wrappedTokenRate)
sf_bpt     = 1e18
upscale(x, sf)      = mulDown(x, sf)      # rounds DOWN, even for the amount given in
downscaleDown(x, sf) = divDown(x, sf)
downscaleUp(x, sf)   = divUp(x, sf)
```

`_upscale` being `mulDown` is not cosmetic: for a 6-decimal main token, `upscale(1 wei, 1e30) = 1e12`
exactly, and for a 6-decimal *wrapped* token the given amount can lose 12 decimals of precision before
the math even starts. Any model that multiplies instead of `mulDown` diverges on dust-sized inputs.

### 5.3 Targets and the nominal balance

```text
to_nominal(real):                        from_nominal(nominal):
  real <  lower: real - mulDown(lower-real, fee)   nominal <  lower: divDown(nominal + mulDown(fee, lower), 1e18+fee)
  real <= upper: real                              nominal <= upper: nominal
  else:          real - mulDown(real-upper, fee)   else:               divDown(nominal - mulDown(fee, upper), 1e18-fee)
invariant(nominalMain, wrapped) = nominalMain + wrapped
```

`to_nominal` is **monotone increasing with a slope that depends on the side of the band**:
`1+fee` below the lower target, `1` inside, `1-fee` above the upper target. `from_nominal` is its
inverse with slopes `1/(1+fee)`, `1`, `1/(1-fee)`. Fees are *not* stored and *not* deducted from the
output; they exist only as the difference `real - nominal`.

### 5.4 Fee placement (the one thing a swap model must get right)

`fee = getSwapFeePercentage()`, `swapFeePercentage` is the *rebalancing* wedge, not a haircut. Marginal
prices in 18-decimal main units:

| main balance | main → wrapped (`out/in`) | wrapped → main (`out/in`) |
| --- | --- | --- |
| inside `[lower, upper]` | `1` | `1` |
| below `lower` (pool wants main) | `1+fee` (trader is paid) | `1/(1+fee)` (trader pays) |
| above `upper` (pool has excess main) | `1-fee` (trader pays) | `1/(1-fee)` (trader is paid) |

Measured, one DAI in each direction (pin; `rate` factors removed by choosing a `rate = 1e18` pool):

```text
bb-e-DAI (below lower, fee 2e14):  main→wrapped 1e18 -> 1000200000000000000  (1.0002, = 1+fee)
                                   wrapped→main 1e18 ->  999800039992001600  (0.99980004, = 1/(1+fee))
bb-a-DAI (below lower, fee 2e14):  main→wrapped 1e18 ->  839134750951962919  (= 1.0002 / 1.191942055629701143)
                                   wrapped→main 1e18 -> 1191703714886723798  (= 1.191942055629701143 / 1.0002)
bb-i-DAI (above upper, fee 2e14):  main→wrapped 1e18 ->  973416028191958334  (= 0.9998 * 1.027104517538146318)
                                   wrapped→main 1e18 -> 1027309979534053128  (= 1.027104517538146318 / 0.9998)
bb-s-DAI (inside, fee 1e12):       main→wrapped 1 wei -> 0                 (inside ⇒ out_up = 1, then divDown(1, rate) = 0)
                                   wrapped→main 9 wei -> 10                (upscale(9) = mulDown(9, rate) = 10, inside ⇒ out = 10)
```

The most informative single vector is a swap that **crosses back into the band**, because it shows the
fee wedge being paid out in full rather than proportionally. `bb-i-DAI` at block `17418847`, main
balance `54632786704321195494283`, `upper = 30000000000000000000000`, `fee = 2e14`:

```text
wrapped→main, amount = 25 000e18:
  chain = 25682539495794522189098
  = 25 000e18 * 1.027104517538146318  (1:1 conversion at the live rate)
  + 4 926 557 340 864 239 098          (the entire pending fee wedge, real - nominal)
```

After the swap the balance is `28 950 247 208 526 673 305 185` — inside the band — so the wedge goes
to zero and the trader captures all of it. A model that applies `fee` as an output haircut cannot
reproduce this number.

### 5.5 The twelve kernels (as deployed)

Balances are **upscaled** before the kernel; amounts are upscaled by the *given* token's scaling
factor and downscaled by the *calculated* token's factor (`downscaleDown` for GIVEN_IN,
`downscaleUp` for GIVEN_OUT).

| pair | GIVEN_IN | GIVEN_OUT |
| --- | --- | --- |
| main→wrapped | `toNominal(m+in) - toNominal(m)` | `toNominal(m) - toNominal(m-out)` |
| wrapped→main | `m - fromNominal(toNominal(m) - in)` | `fromNominal(toNominal(m) + out) - m` |
| main→BPT | `divDown(supply * Δnominal, invariant)` | `fromNominal(toNominal(m) + divUp(inv*bptOut, supply)) - m` |
| wrapped→BPT | `divDown(supply*newInv, prevInv) - supply` | `divUp((supply+bptOut)*prevInv, supply) - nominal - wrapped` |
| BPT→main | `m - fromNominal(toNominal(m) - divDown(inv*bptIn, supply))` | `divUp(supply*Δnominal, invariant)` |
| BPT→wrapped | `wrapped - (divUp((supply-bptIn)*prevInv, supply) - nominal)` | `supply - divDown(supply*newInv, prevInv)` |

Rounding: `mulDown`/`divDown` for amount-out paths, `mulUp`/`divUp` (and `Math.divUp`) for amount-in
paths; the fee term is always `mulDown`. BPT kernels use **plain** `Math.divDown/divUp`, *not*
`FixedPoint`'s 1e18-scaled versions — a transcription that uses `FixedPoint.divDown` everywhere is
wrong by a factor of 1e18 on the BPT paths (bug found and fixed in the reference model during this
work; the corrected model then matched all BPT vectors).

`virtualSupply` in the BPT kernels is `totalSupply - bptBalance(cash+managed)` in the 2023 revision and
`(2^112 - 1) - bptBalance` in the 2021 revision (§7.2).

---

## 6. Verified pools and 0-wei reference vectors

All values below are `eth_call` results at the stated block. "model" is the checked-uint256
transcription of §5 driven by chain reads only (poolId, indices, targets, fee, rate, cash+managed
balances, totalSupply, BPT balance, token decimals).

### 6.1 `bb-s-DAI` `0x2B218683178d029BAB6c9789b1073aA6c96E5176` — `ERC4626LinearPool`, inside targets

```text
poolId     0x2b218683178d029bab6c9789b1073aa6c96e517600000000000000000000058c
tokens     [BPT, DAI 0x6B17…1d0F, sDAI 0x83F2…BEeA]   indices main=1 wrapped=2 bpt=0
balances    [5192296856329202016082404958510929, 37316871073202615, 9]   (cash, managed = 0)
targets    lower 0 , upper 1000000000000000000000000
fee        1000000000000  (1e12)
rate       1181798682856607449   sf = [1e18, 1e18, 1181798682856607449]
supply     totalSupply 5192296856329229656688808879694672 ; BPT 5192296856329202016082404958510929 ; vsup 27640606403921183743
state      main18 = 37316871073202615 ; nominal = 37316871073202615 ; position = inside
getRate    1350074254083975  (== divUp(nominal+wrapped, vsup), model-verified)
paused     false ; recoveryMode = true (recovery mode does not disable swaps)
```

| # | kind | in→out | amount | returned word | value | model |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | GIVEN_IN | main→wrapped | 1 | `0x…0000` | 0 | 0 |
| 2 | GIVEN_IN | wrapped→main | 9 | `0x…000a` | 10 | 10 |
| 3 | GIVEN_OUT | wrapped→main | 1e18 | revert `BAL#001` | — | same |
| 4 | GIVEN_IN | main→BPT | 1e16 | `0x…66caf3418b517f60` | 7406999999999999840 | same |
| 5 | GIVEN_IN | BPT→main | 1e16 | `0x…00000c4762f69e27` | 13500742540839 | same |
| 6 | GIVEN_OUT | BPT→main | 1e15 | `0x…0a477eb9c121bff1` | 740699999999999985 | same |
| 7 | GIVEN_OUT | main→BPT | 1e15 | `0x…0000013a56b24304` | 1350074254084 | same |
| 8 | GIVEN_IN | main→wrapped | 1e15 | revert `BAL#001` | — | same |

Rows 3 and 8 are not math failures: the pool holds **9 wei of sDAI cash**, and the Vault's
`decreaseCash` guard reverts `BAL#001` whenever the payout exceeds cash. That is the whole reason this
pool's usable vectors are dust-sized at the pin; it is otherwise a textbook inside-target pool
(`out_up == in_up`, then one `divDown` by the rate).

### 6.2 `bb-a-DAI` `0x804CdB9116a10bB78768D3252355a1b18067bF8f` — `AaveLinearPool` (2021 revision), below lower target

```text
poolId     0x804cdb9116a10bb78768d3252355a1b18067bf8f0000000000000000000000fb
tokens     [aDAI 0x02d60b84…8Ef6, DAI 0x6B17…1d0F, BPT]  indices main=1 wrapped=0 bpt=2   <-- BPT at 2
balances    [195840046155206615983, 661875172214192963403, 5192296858534621088734102470572582]
targets    lower 2900000000000000000000000 , upper 10000000000000000000000000
fee        200000000000000  (2e14)
rate       1191942055629701143   sf = [1191942055629701143, 1e18, 1e18]
supply     totalSupply == 2^112-1 ; BPT 5192296858534621088734102470572582 ; vsup 206539796393858647513
state      main18 = 661875172214192963403 ; wrapped18 = 233429987188852523713 ; nominal = 82007547248635801996 ; below lower
getRate    1527248210489993954  (== divUp(nominal+wrapped, vsup), model-verified; this revision's
                                 approximation for vsup coincides with totalSupply-based vsup here, §7.2)
paused     false ; no inRecoveryMode() in this revision
```

| # | kind | in→out | amount | returned word | value | model |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | GIVEN_IN | main→wrapped | 1e18 | `0x…0ba5349c2d2d7927` | 839134750951962919 | same |
| 2 | GIVEN_IN | wrapped→main | 1e18 | `0x…1089c836fb5628d6` | 1191703714886723798 | same |
| 3 | GIVEN_OUT | main→wrapped | 1e18 | `0x…1089c836fb5628d6` | 1191703714886723798 | same |
| 4 | GIVEN_OUT | wrapped→main | 1e18 | `0x…0ba5349c2d2d7928` | 839134750951962920 | same |
| 5 | GIVEN_IN | BPT→main | 1e18 | `0x…1530ca680d355d7f` | 1526942821925608831 | same |
| 6 | GIVEN_IN | main→BPT | 1e18 | `0x…0916af252fe19e44` | 654903370080951876 | same |
| 7 | GIVEN_IN | main→wrapped | 1e6 | `0x…00000000000ccdde` | 839134 | same |
| 8 | GIVEN_OUT | main→wrapped | 1e6 | `0x…0000000000122f17` | 1191703 | same |

This is the pool that proves the **fee is a wedge and not a haircut**: below the lower target the
main→wrapped quote is *better* than the bare rate (`1+fee` then divide by rate), while the reverse
direction is worse by `1/(1+fee)`.

### 6.3 `bb-e-DAI` `0xeB486AF868AeB3b6e53066abc9623b1041b42bc0` — `EulerLinearPool`, below lower target

```text
poolId     0xeb486af868aeb3b6e53066abc9623b1041b42bc000000000000000000000046c
tokens     [BPT, DAI 0x6B17…1d0F, eDAI 0xe025E3ca…D9DC]  indices main=1 wrapped=2 bpt=0
balances    [5192296850329319016486130259664433, 514465552577786742882, 394163142520294167179245]
targets    lower 2000000000000000000000000 , upper 10000000000000000000000000
fee        200000000000000 ; rate 1000000000000000000 ; sf = [1e18, 1e18, 1e18]
supply     totalSupply 5192296850729226063091213468094486 ; BPT 5192296850329319016486130259664433 ; vsup 399907046605083208430053
state      main18 = 514465552577786742882 ; nominal = 114568445688302300231 ; below lower
getRate    985923389730464468  (== divUp(nominal+wrapped, vsup), model-verified)
```

| # | kind | in→out | amount | returned word | value |
| --- | --- | --- | --- | --- | --- |
| 1 | GIVEN_IN | main→wrapped | 1e18 | `0x…0de16c99c8588000` | 1000200000000000000 |
| 2 | GIVEN_IN | wrapped→main | 1e18 | `0x…0de000d6d6250440` | 999800039992001600 |
| 3 | GIVEN_OUT | main→wrapped | 1e18 | `0x…0de000d6d6250440` | 999800039992001600 |
| 4 | GIVEN_OUT | wrapped→main | 1e18 | `0x…0de16c99c8588000` | 1000200000000000000 |
| 5 | GIVEN_IN | BPT→main | 1e18 | `0x…0dae00cbf81a8d99` | 985726244481568153 |
| 6 | GIVEN_IN | main→BPT | 1e18 | `0x…0e1428972f3663ef` | 1014480445862470639 |
| 7 | GIVEN_IN | main→wrapped | 1e6 | `0x…00000000000f4308` | 1000200 |
| 8 | GIVEN_OUT | main→wrapped | 1e6 | `0x…00000000000f4178` | 999800 |

Every row above was model-verified at 0 wei (and so were the `bb-e-DAI` BPT/GIVEN_OUT rows in the
aggregate sweep of §6.5).

Note the historical detail this pool pins down: its `totalSupply` is **below** `2^112-1` by
`7805601565439282861125609` (BPT burned by recovery-mode exits), which is exactly the case where the
2021 revision's `_INITIAL_BPT_SUPPLY`-based virtual supply and the 2023 revision's `totalSupply`-based
one disagree (see §7.2 for a live 680× example that is currently unquotable).

### 6.4 Archive block `17418847`: `bb-i-DAI` `0xDbA274B4D04097b90A72b62467d828cEFD708037` — above upper target

At the pin, **no** corpus Linear pool is above its upper target (28 inside, 11 below, 9 unmeasurable),
so the `1-fee` branch cannot be measured at the pin. It is measured on the same archive node at block
`17418847`, where this ERC-4626 (Idle) pool held `54632.79` DAI against `upper = 30000`:

```text
poolId     0xdba274b4d04097b90a72b62467d828cefd708037000000000000000000000486
tokens     [BPT, idleDAIJunior4626 0x0C80f31b…514CA, DAI 0x6B17…1d0F]  indices main=2 wrapped=1 bpt=0
balances    [5192296858343810674569367866164285, 133202251235352622304747, 54632786704321195494283]
targets    lower 0 , upper 30000000000000000000000 ; fee 2e14
rate       1027104517538146318  (== idleDAIJunior4626.convertToAssets(1e18) at that block)
supply     vsup 191016953961128463055810 ; main18 = 54632786704321195494283 ; nominal = 54627860146980331255185 ; above upper
getRate    1002217291016062728  (model-verified)
paused     false ; recoveryMode = false
```

| # | kind | in→out | amount | returned word | value |
| --- | --- | --- | --- | --- | --- |
| 1 | GIVEN_IN | main→wrapped | 1e18 | `0x…0d8244b8630da53e` | 973416028191958334 |
| 2 | GIVEN_IN | wrapped→main | 1e18 | `0x…0e41bcfbc4574f08` | 1027309979534053128 |
| 3 | GIVEN_OUT | main→wrapped | 1e18 | `0x…0e41bcfbc4574f09` | 1027309979534053129 |
| 4 | GIVEN_OUT | wrapped→main | 1e18 | `0x…0d8244b8630da53f` | 973416028191958335 |
| 5 | GIVEN_IN | BPT→main | 1e18 | `0x…0de94da7a9d7369a` | 1002417774570976922 |
| 6 | GIVEN_IN | main→BPT | 1e18 | `0x…0dd8250d677c3ed3` | 997588056963563219 |
| 7 | GIVEN_IN | wrapped→main | 25000e18 | `0x…057040d55f7fc457ed2a` | 25682539495794522189098 (band-crossing, §5.4) |
| 8 | GIVEN_IN | main→wrapped | 30000e18 | `0x…062f11d527d7cf149843` | 29202480845758750038083 |

Row 7 is the wedge-capture vector: `25 000 · rate + wedge`, exact to the wei.

### 6.5 Aggregate parity sweep (the actual strength of the claim)

For each of the 48 ERC-20 Linear pools, nine cases were quoted at the pin and compared against the
model (checksum of the sweep lives in §10's harness note):

```text
cases per pool: main→wrapped (cash/1000), wrapped→main (cash/1000),
                GIVEN_OUT both directions, main→BPT, BPT→main,
                GIVEN_OUT main→BPT, GIVEN_OUT BPT→main,
                main→wrapped with 10x the main cash (forces the Vault cash guard)

result: pools=48  cases=432  match=432  mismatch=0
        value-equal 155 ; revert-class-equal 277
```

"match" means either the same 256-bit integer or the same revert class (`BAL#000`, `BAL#001`,
`BAL#004`, or bare). A model that wraps instead of reverting, or that omits the cash guard, fails
roughly 60% of the cases.

---

## 7. Rate sources and revision semantics

### 7.1 The three rate shapes (plus two more that exist in the corpus)

| pool type | `_getWrappedTokenRate()` | identity verified at the pin |
| --- | --- | --- |
| `ERC4626LinearPool` | `IERC4626(wrapped).convertToAssets(10**(18 + wrappedDecimals - mainDecimals))`, non-malicious reverts bubbled via `ExternalCallLib.bubbleUpNonMaliciousRevert` | `bb-s-DAI`: rate `1181798682856607449` == `sDAI.convertToAssets(1e18)`; `bb-i-DAI` @17418847: `1027104517538146318` == `idleDAIJunior4626.convertToAssets(1e18)` |
| `AaveLinearPool` (2021 rev) | `ILendingPool(lendingPool).getReserveNormalizedIncome(mainToken) / 1e9`; constructor asserts `mainToken == IStaticAToken(wrapped).ASSET()` | `bb-a-DAI`: pool rate `1191942055629701143` == `aDAI.rate() / 1e9` == `AaveV2LendingPool(0x7d2768dE32b0b80b7a3454c06BdAc94A69DDc7A9).getReserveNormalizedIncome(DAI) / 1e9`, where the raw ray answer is `1191942055629701143438023756` (floor of `/1e9` = `1191942055629701143`) |
| `EulerLinearPool` | `IEulerTokenMinimal(wrapped).convertBalanceToUnderlying(10**(36 - mainDecimals))` | `bb-e-DAI`: trace shows the `STATICCALL` to eDAI returning `1e18`; rate `1000000000000000000` |
| `YearnLinearPool` | `_sharesToAmount(wrapped, 1e18)` (i.e. `pricePerShare(1e18)`) | `bb-yv-DAI` rate `1233506545293171739` == `yvDAI.pricePerShare()` |
| Aave v3 static-token pools deployed as `ERC4626LinearPool` | the ERC-4626 getter above | **reverts**: `bb-a-WETH 0xbB688187…` `getWrappedTokenRate()` → `execution reverted: Not implemented` (the wrapped `0x0392…2264` does not implement `convertToAssets`) |

The `_rateScaleFactor` in the ERC-4626/Euler constructors is exactly
`10**(18 + wrappedDecimals - mainDecimals)` (comment: *"One wrapped token is pre-scaled by
1e(18 - mainTokenDecimals) to achieve the most precise rate"*), so for 18/18 pools it is `1e18` and the
rate is used verbatim; for a 6-decimal main token it is `1e30`/`1e42`.

`getRate()` (the pool's BPT price) is **not** the wrapped rate: it is
`divUp(invariant, virtualSupply)` where `invariant = toNominal(mainBalance) + wrappedBalance`. Verified
on all four pools in §6 (model == chain, e.g. `1527248210489993954`, `985923389730464468`,
`1002217291016062728`, `1350074254083975`).

### 7.2 Two deployed wrapper revisions (and the traps in each)

| property | 2021 revision (`bb-a-DAI 0x804CdB…`, `bb-f-*`, `bb-a-USDT 0xe6bCC…`) | 2023 revision (`bb-s-DAI`, `bb-e-*`, `bb-i-*`, …) |
| --- | --- | --- |
| registration | `BasePool(vault, PoolSpecialization.GENERAL, _sortTokens(main, wrapped, this), …)` | `PoolRegistrationLib.registerComposablePool(vault, GENERAL, sort(main, wrapped), managers)` |
| BPT position | wherever the address sorts: **0, 1 or 2** | forced to **0** |
| `getBptIndex()` | immutable | `pure returns (_BPT_INDEX = 0)` |
| targets storage | `BasePool` miscData, two raw `uint96` (18-dec units), `_MAX_UPPER_TARGET = 2^96-1` | `_poolState`, two `uint32` fields multiplied by `_TARGET_SCALING = 1e18`; `_MAX_UPPER_TARGET = (2^32-1)*1e18 = 4.294967295e27`; fractional targets rejected (`FRACTIONAL_TARGET`) |
| virtual supply in swap math | `_getApproximateVirtualSupply = (2^112 - 1) - bptBalance` | `_getVirtualSupply = totalSupply - bptBalance` |
| `getRate()` denominator | the approximate supply | the exact supply |
| reentrancy guard | none | `whenNotInVaultContext` via `VaultReentrancyLib` on `setTargets`/`setSwapFeePercentage` |
| fee bounds | `BasePool` `1e12 … 1e17` | `LinearPool` `_MIN_SWAP_FEE_PERCENTAGE = 1e12`, `_MAX_SWAP_FEE_PERCENTAGE = 1e17` |
| recovery mode | present but not exposed per-pool (`inRecoveryMode()` reverts) | exposed, `inRecoveryMode()` answers |

The approximate-supply trap is real and currently latent: `bb-a-DAI 0xae37D54A…` (2021 revision)
has `totalSupply = 5192296857915371696099530132309038`, i.e. `619 455.93` BPT below the premint.
`bptBalance = 5192296857914460478875678662508798`, so:

```text
2^112-1 - bptBalance   = 620367149654817666711297   (what its swap math and getRate() use)
totalSupply - bptBalance =     911217223851469800240   (what a naive model would use)
ratio = 680.81x
```

The pool is currently unquotable (`getRate()` reverts `BAL#001`, and the sweep matches that revert), so
the 680× error is not observable through `querySwap` today — but the formula must still follow the
revision, because the divergence is not a rounding difference.

Measured BPT-index distribution among the 48 (a builder must never assume 0):
`41 × 0`, `4 × 2`, `3 × 1`.

---

## 8. Deployed refresh behaviour and quote hazards at the pin

**No rate caching.** Every quote performs the wrapped-token rate call inline; there is no stored rate,
no timestamp, and no duration. Consequences for a bot: (a) a rate cached across blocks is wrong; (b) a
rate read that reverts must become "no quote", not "rate = 0"; (c) a re-org-safe quote pins both the
rate read and the swap math to the same block.

**Rate-source reverts (9/48 pools) are a quote blocker, not a math edge case.** Example:
`bb-a-WETH 0xbB6881874825E60e1160416D6C426eae65f2459E` (an `ERC4626LinearPool`) has
`getWrappedTokenRate()` → `Not implemented`, so `getScalingFactors()`, `getRate()` and every swap
revert. The same code shape is healthy on `bb-s-DAI`; the difference is the wrapped token's
implementation, not the pool.

**Nominal underflow bricks 6/48 pools.** Because `FixedPoint.sub` is checked, a main balance `real`
below `lower` with `mulDown(lower - real, fee) > real` makes `_toNominal` revert `BAL#001` on every
path. At the pin:

```text
bb-a-USDT  0x2F4eb100…  main 33.363410  lower 6 000 000  fee 1e14
bb-e-USDT  0x3C640f0d…  main 51.943827  lower 1 000 000  fee 2e14
bb-g-USDC  0x4A82b580…  main  6.892638  lower   200 000  fee 2e14
bb-a-USDC  0x82698aeC…  main 25.966763  lower 6 000 000  fee 1e14
bb-a-DAI   0xae37D54A…  main 48.927542  lower 6 000 000  fee 1e14
bb-g-DAI   0xe03aF00f…  main  1.827001  lower   200 000  fee 2e14
```

All six revert on `main↔wrapped` in both directions (measured at 1 wei), and all six have `getRate()`
reverting. They are the reason a checked-arithmetic model is the correct model: silently wrapping
would produce a *huge* nominal invariant and a `divDown`-by-huge BPT price that looks plausible.

**The Vault's cash guard is a real, frequently-binding constraint.** `onSwap` may compute a payout the
pool cannot make: `Swaps.sol` calls `tokenOutBalance.decreaseCash(amountOut)`, and `decreaseCash` does a
checked `cash.sub(amount)` — **managed balances are not available to fund a swap**. Measured on
bb-s-DAI (9 wei of sDAI cash): `main→wrapped` with 1e15 DAI reverts `BAL#001` even though the math is
fine. 8/48 pools are one-directional at the pin solely because one side's cash is 0 (e.g.
`bb-e-USDC 0xD4e7C1F3…`: 436 794 eUSDC cash, 424.46 USDC cash → `wrapped→main` works, `main→wrapped`
only up to 424 USDC). At the pin `managed == 0` for every corpus Linear pool, but the guard must be
modeled from `cash` (not `cash+managed`) because `cash+managed` is what `onSwap` sees.

**Asset managers are configured but empty in this corpus; the read path still needs them.** `onSwap`
receives `balance.total()` = cash + managed (`BalanceAllocation.total`), so a correct builder must read
`getPoolTokenInfo` and add. At the pin **0 of 144** token rows across the 48 pools have `managed > 0`
(78 of those rows do have a nonzero asset-manager *address*), so `getPoolTokens` happens to be
numerically equal today — using it would pass this sweep and break the moment a manager holds
anything. `bb-s-DAI`'s sDAI row is the canonical example: `cash 9, managed 0, manager 0xF650…45a5`.

**Recovery mode does not gate quotes.** `bb-s-DAI` and `bb-e-DAI` are in recovery mode at the pin and
still quote (recovery mode only affects joins/exits). `getPausedState()` is `false` for all four pools
in §6; a paused pool reverts (`whenNotPaused`).

---

## 9. Builder inputs (every field the model needs, and where it comes from)

| input | call | exact semantics / trap |
| --- | --- | --- |
| pool id | `getPoolId()(bytes32)` | required for every Vault read; corpus `pool_id` matches it for all 48 |
| main / wrapped token | `getMainToken()(address)`, `getWrappedToken()(address)` | token *addresses*, not indices |
| indices | `getMainIndex()`, `getWrappedIndex()`, `getBptIndex()` | never assume 0/1/2; observed main at 2, wrapped at 0, BPT at 0/1/2 |
| token order | `Vault.getPoolTokens(poolId)` | index order for the balance array; BPT is a member |
| targets | `getTargets()(uint256,uint256)` | 18-decimal units in both revisions (2023 decodes `uint32 × 1e18`) |
| fee | `getSwapFeePercentage()(uint256)` | the rebalancing wedge, not an output haircut; range `1e12…1e17` |
| wrapped rate | `getWrappedTokenRate()(uint256)` | live external call; 18-decimal fixed point; reverts are quote-blocking |
| scaling factors | `getScalingFactors()(uint256[])` (public in both revisions) | `[bpt=1e18, main, wrapped]` in index order; matches `_computeScalingFactor` × rate |
| decimals | `decimals()` on both tokens | needed if `getScalingFactors` is unavailable; `upscale` is `mulDown` |
| balances | `Vault.getPoolTokenInfo(poolId, token)` → `(cash, managed, lastChangeBlock, manager)` | onSwap sees `cash + managed`; the payout guard uses `cash` only; `manager` may be a nonzero address while `managed == 0` (bb-s-DAI's sDAI row), so never infer one from the other |
| BPT supply | `totalSupply()` on the pool | 2023: `vsup = totalSupply - bptBalance`; 2021: `vsup = 2^112-1 - bptBalance` |
| BPT balance | `getPoolTokenInfo(poolId, poolAddress)` | part of `vsup`; do not use `getPoolTokens()[bptIndex]` if a manager ever holds BPT |
| pause / recovery | `getPausedState()`, `inRecoveryMode()` | pause blocks swaps; recovery does not; 2021 revision has no `inRecoveryMode()` |
| pool BPT price (optional) | `getRate()(uint256)` | `divUp(invariant, vsup)` — a *check*, not an input to swap math |

---

## 10. Modeling cost, go/no-go

**Go.** Scope the arm as: GIVEN_IN and GIVEN_OUT for the three token pairs (main↔wrapped, main↔BPT,
wrapped↔BPT), 12 kernels, one rate getter per wrapped-token revision, checked-uint256 semantics, and
the two quote guards. Everything else is plumbing.

Cost findings, in descending order of risk:

1. **Revert parity is the expensive part, not the formulas.** The arithmetic is ~150 lines; matching
   the chain on *when it refuses* is what takes the work: `BAL#001` from `_toNominal` (6 pools at the
   pin), `BAL#001` from `decreaseCash` (cash-only guard, 8 pools one-directional), `BAL#004` from
   `virtualSupply == 0`, and bare reverts from a wrapped-token rate call (9 pools). A model with
   wrapping arithmetic or a `checked=False` bignum path will pass t1-style happy-path tests and fail
   half the real population.
2. **Two wrapper revisions must both be implemented** (index discovery + target packing + virtual
   supply formula). The 2021 revision's `_INITIAL_BPT_SUPPLY` supply is a live 680× divergence on one
   deployed pool; treating both revisions identically is a silent correctness bug, not a stylistic one.
3. **BPT swaps double the surface.** Linear pools are composable joins/exits priced by the same
   invariant; if the bot only models main↔wrapped it will misprice (or fail to find) routes that use
   BPT as an intermediate, and it cannot represent the pool's real token set (3 tokens, BPT included).
4. **Rate plumbing is per-wrapped-token, not per-pool.** Five shapes are already deployed
   (`convertToAssets`, Aave v2 `getReserveNormalizedIncome/1e9`, Euler `convertBalanceToUnderlying`,
   Yearn `pricePerShare`, plus the reverting Aave-v3-as-ERC-4626 shape). A single "rate provider"
   abstraction with a per-pool selector plus a failure mode covers them; a hardcoded `convertToAssets`
   covers 14 of 39 wrapped tokens.
5. **No caching is required (or safe).** Each quote needs one rate `eth_call` at the pinned block. That
   is the same I/O the pool itself performs, so a simulation that batch-reads rates per block is
   sufficient; a persistent cache introduces staleness with no upside.
6. **Population economics are small.** At the pin the 48 pools hold dust-to-~$1M each and 15 are
   unquotable outright; the largest usable states are a few hundred thousand dollars. A Linear arm is
   worth building for *completeness of the Balancer surface* (and because `querySwap` parity is a cheap,
   exact oracle), not because the pools are currently deep. Route-finding value today is concentrated
   in the ~25 fully-quotable pools.

Verification recipe for the implementation (cheap and exact): the harness in §1 reproduces any vector
from five chain reads plus `getScalingFactors`; the acceptance bar used here — **432/432 across 48
pools, 0 wei and identical revert classes** — is reproducible in one sweep, so the arm can carry a
standing on-chain parity gate rather than a corpus of golden numbers.

**No-go conditions** (would invalidate the above): a Linear pool revision in the corpus whose
`LinearMath` differs from the master transcription (none found: 2021 and 2023 deployed
`LinearMath.sol` are byte-identical modulo import order), or a wrapped-token rate that is not
deterministic at a pinned block (none found: all read paths are `view`).

---

## Appendix A — representative raw outputs

```text
$ cast call --block 26204126 0x2B218683178d029BAB6c9789b1073aA6c96E5176 'getTargets()(uint256,uint256)'
0
1000000000000000000000000

$ cast call --block 26204126 0x2B218683178d029BAB6c9789b1073aA6c96E5176 'getScalingFactors()(uint256[])'
[1000000000000000000 [1e18], 1000000000000000000 [1e18], 1181798682856607449 [1.181e18]]

$ cast call --block 26204126 0x2B218683178d029BAB6c9789b1073aA6c96E5176 'getRate()(uint256)'
1350074254083975

$ cast call --block 26204126 0xBA12222222228d8Ba445958a75a0704d566BF2C8 \
    'getPoolTokens(bytes32)(address[],uint256[],uint256)' \
    0x2b218683178d029bab6c9789b1073aa6c96e517600000000000000000000058c
[0x2B218683178d029BAB6c9789b1073aA6c96E5176, 0x6B175474E89094C44Da98b954EedeAC495271d0F, 0x83F20F44975D03b1b09e64809B757c47f942BEeA]
[5192296856329202016082404958510929, 37316871073202615, 9]
25457006

$ cast call --block 26204126 0xBA12222222228d8Ba445958a75a0704d566BF2C8 \
    'getPoolTokenInfo(bytes32,address)(uint256,uint256,uint256,address)' \
    0x2b218683178d029bab6c9789b1073aa6c96e517600000000000000000000058c 0x83F20F44975D03b1b09e64809B757c47f942BEeA
9
0
25457006 [2.545e7]
0xF6502226DFe510646b358F90e77320bfA8Ae45a5
# cash 9, managed 0, lastChangeBlock 25457006, asset manager 0xF650…45a5 (set, but holding nothing)
```

## Appendix B — the sweep harness (scratch, `/tmp` only)

Not committed; reproduced here so the numbers can be re-derived:

* `rpc.py` — JSON-RPC batch + minimal ABI decode (`cast sig`/`cast calldata` for encoding).
* `lin.py` — checked-uint256 transcription of §5 (five fixed-point helpers, `to_nominal`/`from_nominal`,
  twelve kernels, `Pool` state loader from §9's input list).
* `vec.py` — model quote vs chain quote, including revert-class normalisation and the cash guard.
* `sweep4.py` — 48 pools × 9 cases; prints `cases=432 match=432 mismatch=0` and the per-case diff list.

The model is deliberately driven only by chain reads: no golden constants from this document are used
as inputs, so the 0-wei agreement is a property of the transcription, not of curated fixtures.
