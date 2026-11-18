# Balancer V2 cross-cutting evidence: registration params, Vault state-sync, and the 173 unmodeled rows

**Track:** C (cross-cutting) · **Pin:** block `26204126` · **Node:** the devcontainer archive reth
(`web3_clientVersion` → `reth/v2.11.0-b9abbcc/x86_64-unknown-linux-gnu`, `eth_chainId` → `0x1`)
**Pin facts:** `eth_getBlockByNumber(0x18fd81e)` → hash
`0x13207898e1259f2c66b6a13bd8639d9df0266235ee2ec1f0d3704dfbdc3cbc2e`, timestamp `1792142039`
(2026-10-16T09:13:59Z); the sweep pin constant is `tests/sweep_pin.py::SWEEP_FORK_BLOCK`.

Every on-chain number below was read at that pin from that node; the `cast` invocation that produced it
is quoted next to it. Scratch probes live under `/tmp` (`probe_unmodeled.py`, `probe2.py`,
`ana3.py`…`ana7.py`); nothing in the repository was modified except this file.

**Scope.** (1) The current weighted/stable registration parameters and the one Vault state-sync path;
(2) what the 173 `unmodeled` corpus rows actually are, whether any is an arbitrage venue, and what each
shape would cost to model; (3) go/no-go per family. Layout of the weighted/stable *hop* internals is
covered only as far as a new family has to imitate it.

---

## 1. Method and the corpus this report measures

The pool universe is the checked-in registration corpus
`tests/balancer/data/balancer_v2_pool_registrations_26204126.json` (recorded by
`scripts/record_balancer_pool_registrations.py`, replayed by
`tests/balancer/test_balancer_registry_sweep.py`). Its header is a scan of the Vault's
`PoolRegistered(bytes32,address,uint8)` log (`topic0 0x3c13bc30b8e878c53fd2a36b679409c073afd75950be43d8858768e956fbc20e`)
from the Vault's deployment block `12272146` to the pin:

| census bucket | rows | meaning (per the corpus header) |
|---|---|---|
| `weighted` | 1569 | `getNormalizedWeights()` answered at the pin |
| `stable` | 193 | `getAmplificationParameter()` answered at the pin |
| `unmodeled` | 173 | `getPoolId()` answered, neither family probe did |
| `unknown` | 19 | no decodable `getPoolId()` at all (EIP-7702 designators, junk registrations) |

`duplicate_registrations: 63` (the Vault permits re-registering a pool contract; the corpus keeps the
earliest registration per address).

For the 173 `unmodeled` rows I ran four independent probes at the pin:

1. **selector census** — `eth_call` of 29 identity selectors per pool at `0x18fd81e`
   (`getPoolId 0x38fff2d0`, `getNormalizedWeights 0xf89f27ed`, `getAmplificationParameter 0x6daccffa`,
   `getSwapFeePercentage 0x55c67628`, `getMainToken 0x4de046d5`, `getWrappedToken 0xf174e241`,
   `getWrappedTokenRate 0xf5431aa8`, `getBptIndex 0x82687a56`, `getTargets 0x63fe3b56`,
   `getVirtualSupply 0xde82cd34`, `getMainIndex 0x4d64cd74`, `getWrappedIndex 0x16b8d6ff`,
   `getRate 0x679aefce`, `getRateProviders 0x238a2d59`, `getScalingFactors 0x1dd746ea`,
   `name 0x06fdde03`, `symbol 0x95d89b41`, `totalSupply 0x18160ddd`, …);
2. **Vault truth** — `getPoolTokens(bytes32)` and `getPool(bytes32)` on
   `0xBA12222222228d8Ba445958a75a0704d566BF2C8` (`0xf94d4668`, `0xf6c00927`), giving the authoritative
   token vector, balances and `lastChangeBlock`;
3. **deployed-interface census** — `cast selectors` over `eth_getCode` at the pin for every one of the
   173 contracts (the accurate method; a naive `PUSH4` substring scan is *not* reliable across solc
   dispatch styles — see §4.1);
4. **flow** — `eth_getLogs` on the Vault for `Swap(bytes32,address,address,uint256,uint256)`
   (`0x2170c741…`) and `PoolBalanceChanged(bytes32,address,address[],int256[],uint256[])`
   (`0xe5ce2490…`), `topics[1]` filtered to the 173 pool ids, over two disjoint windows:
   `[pin−100000, pin]` (≈14 days) and `[pin−500000, pin−100001]` (the 400k blocks before that, ≈56 days).

Plus a nested-BPT test: `getPoolId()` on each of the 234 distinct Vault tokens of these pools (65 answer
→ 65 Balancer-pool tokens; all 48 linear pools hold their *own* BPT, exactly one row holds a *foreign*
one).

---

## 2. Current weighted/stable registration parameters (the shape a new family must imitate)

### 2.1 Weighted

`rust/crates/engine/degenbot-pools/src/state/balancer_weighted_state.rs::RegisterBalancerWeightedPoolParams`
carries, and `bot_core/pool_builder/builder.rs::build_balancer_weighted` fills:

| field | source (exact read) | symbol |
|---|---|---|
| `address`, `vault` | request | — |
| `pool_id: [u8;32]` | `getPoolId()` | `BalancerPoolId::read` |
| `tokens`, `balances` | Vault `getPoolTokens(poolId)` → `(tokens, balances, lastChangeBlock)` | `IBalancerVault::getPoolTokensCall` |
| `swap_fee: u128` | `getSwapFeePercentage()` | `IBalancerPool::getSwapFeePercentageCall` |
| `weights: Vec<U256>` | `getNormalizedWeights()` | `IBalancerPool::getNormalizedWeightsCall` |
| `pow_version: u8` | deployed bytecode: presence of the 8-byte `2e18` constant (`0x1BC16D674EC80000`) | `detect_pow_version` |
| `scaling_factors: Vec<U256>` | `ONE * 10^(18−decimals)` per token | `balancer_scaling_factor` / `compute_scaling_factor` |
| `balances`, `update_block` | registration state | — |

Refusals before any state exists: `BROKEN_BALANCER_V2_POOLS` → `SpecRefusal::BalancerBrokenPool`;
own BPT among the Vault tokens (`tokens.contains(&address)`) → `SpecRefusal::BalancerComposableWeighted`;
`decimals()` not decodable → `SpecRefusal::NoCodeCoin`; `decimals() > 18` → `PoolBuilderError::Decoding`.
The write door `degenbot-substrate/src/balance_vector_orchestration.rs::register_balancer_weighted_pool`
re-checks the arity (`tokens == balances == weights == scaling_factors`) and seeds
`BalancerWeightedPoolState::from_params` with a **genesis journal anchor**
(`journal.push_delta(BalancesBlockDelta { block: update_block, before == after == balances })`).

Hop projection: `degenbot-pools/src/families/balancer_weighted.rs::capture_hop` is **pairwise on indices
0/1 only** — `balances[0]*sf[0]` vs `balances[1]*sf[1]`, weights 0/1, and it returns
`solve::BalancerWeightedHopState`. `read_swap` answers `ExactIn` (0/1), `PairExactIn` and
`PairExactOut` with an explicit index pair; any other request shape is `ReadOutcome::NotComputable`.

### 2.2 Stable

`RegisterBalancerStablePoolParams` + `build_balancer_stable`:

| field | source | symbol |
|---|---|---|
| `pool_id`, `tokens`, `balances` | as weighted | — |
| `swap_fee` | `getSwapFeePercentage()` | — |
| `amp: u128` | `getAmplificationParameter()` → first word (`.value`) | `IBalancerPool::getAmplificationParameterCall` |
| `bpt_idx: Option<usize>` | `tokens.iter().position(|t| *t == address)` | — |
| `invariant_version: u8` | `derive_invariant_version(bpt_idx.is_some())` — composable → V1, else V2; caller override wins | `derive_invariant_version` |
| `scaling_factors` | `ONE*10^(18−decimals)` **times** each rate-provider rate / `ONE` | `getRateProvidersCall` + `getRateCall` |
| `rate_provider: Option<Arc<dyn BalancerRateProvider>>` | *never set by the builder* (`rate_provider: None`); the trait lives in `degenbot-pools/src/sim/rate_provider.rs` | `BalancerRateProvider` |

Degradation rules: `getRateProviders()` reverting is *absence* (a static-rate pool), not a failure; a
rate provider that reverts or returns unparseable data is a **pool fact** →
`SpecRefusal::BalancerRateProviderUnreadable` (this is the refusal the sweep measured on nested-pool rate
providers). Arity/`bpt_idx < tokens.len()` are re-checked in `register_balancer_stable_pool`.

Hop projection: `families/balancer_stable.rs::capture_hop` drops the BPT index, upscales by the
rate-multiplied scaling factors and pre-computes the invariant once, under the core lock; pairwise 0/1
after the BPT skip, same as weighted.

### 2.3 The two families are the *only* Balancer shapes with a state slice, and none of the seven
registered families reaches the arbitrage graph

`degenbot-pools/src/sim/registry.rs::PoolEntry` = `{V2, V3, V4, Curve, BalancerWeighted, BalancerStable,
AerodromeV2}`; `RegisteredPoolFamily::ALL` is the same seven. That is the engine's modelling surface.

The pathfinding graph is narrower still: `degenbot-pathfinding/src/graph.rs::PoolKind` is `{V2, V3, V4}`
with `KNOWN_KINDS` listing only V2/V3/V4 `pools.kind` strings and `DECLARED_UNSUPPORTED_KINDS = ["lfj_binned"]`.
`degenbot-db/src/pathfinding.rs::fetch_path_graph_edges` **refuses** a `pools` row whose `kind` is outside
that vocabulary (`DbError::UnknownPoolKind`) instead of skipping it, and the backrun lane's
`degenbot-strategy/src/backrun_engine.rs::LaneFamilyTag` is likewise `{V2, V3, V4}`
(`from_identity` deliberately projects the balance-vector families to `None`).

**Consequence (cross-cutting, load-bearing):** no Balancer pool — weighted, stable, or any family
proposed here — is discoverable by `find_paths`/`find_paths_async` or by the anchored backrun DFS today.
A Balancer registration can only be reached by an explicit `Bot.build_pool(address)` plus a hand-built
`ResolvedMixedPath`; the solver's Balancer arms (`solve_balancer_weighted_path_int`,
`BalancerStableHopState` …) are exercised by tests/harness corpora and by direct quoter calls. That is
the ceiling on *any* Balancer family's arb value, and it is unchanged by adding a builder.

---

## 3. Vault `PoolBalanceChanged`: journal and state-sync, traced

### 3.1 There is no log-driven sync for Balancer at all

`PoolBalanceChanged` appears in this tree in exactly three places, none of them a decoder or a
subscription:

- `degenbot-pools/src/families/balance_vector.rs::BalanceVectorEvent { balances, block_number }` — the
  family-blind event *vocabulary* (“One buffered balance-vector event (a Curve `Exchange` or a Balancer
  Vault `PoolBalanceChanged`)”), consumed by `FamilyApply::apply_event`;
- `state/state_history.rs` and `state/balancer_{weighted,stable}_state.rs` doc comments describing what
  the apply door mutates;
- the Vault ABI copy in `tests/balancer/test_pools.py`.

`degenbot-decoders` and `degenbot-eventhub` contain no Balancer log decoder; `degenbot-pool-updater`'s
`ExchangeSpec`/`load_active_exchange_specs` discovers only V2/V3/V4 `PoolCreated`/`Initialize` topics
(and refuses an unknown `exchanges.name`). The only producer of a Balancer balance event is the Python
companion: `src/degenbot/balancer/pools.py::BalancerV2Pool.external_update` →
`Pool.apply_balances` → `BotState::apply_balance_update_by_pool_id`. Nothing in the driver decodes
`PoolBalanceChanged` off the wire, so in the shipped system the *caller* is the event source.

### 3.2 The one live path is pull-based, and its clock is broken

`bot_core/refresh.rs::refresh_pool` dispatches on `RegisteredPoolFamily`; the Balancer arm is
`refresh_balancer(state, pool_id, io, block, stable)`, which reads
`IBalancerVault::getPoolTokensCall{ poolId }` **from the Vault** and applies the `balances` half through
`apply_balances` → `BotState::apply_balance_update_by_pool_id` →
`BalanceVectorPoolState::apply_balance_update` (journal `BalancesBlockDelta { before, after }`, advance
`update_block`, bump `state_nonce`). Reorg rollback is `ReorgPoolState::restore_before_block` over that
journal, anchored by the registration-block genesis delta.

Two clock facts matter for any new family:

1. Every other arm resolves the block first (`resolve_block(io, block)` → explicit block else
   `get_block_number()`). `refresh_balancer` does **not**:
   ```rust
   // The historical Balancer update stamped `block_number or 0` ...
   let apply_block = block.unwrap_or(0);
   ```
   The Python seam `Pool.refresh_via` leaves `None` as `None`
   (`py_bot_io.rs::resolve_block_against` only maps *tags* to numbers), so
   `Bot.update(balancer_pool)` with no explicit block fetches at the head and then stamps `0`.
   `apply_balances` refuses it (`if block < update_block || current == balances → Unchanged`, and
   `0 < registration_block` always), so the update is a **silent no-op**: the pool's state is frozen at
   its registration block unless the caller passes a block number. The docstring of `refresh_via`
   (“defaults to the current chain head”) is therefore false for these two families. Nothing in the
   shipped runner/updater calls `Bot.update` today (the refresh entry is operator/test driven), so this
   is a latent API defect rather than a live pipeline break — but it is exactly the entry a future
   Balancer sync path (or a new family copying `refresh_balancer`) would build on.
2. The read already fetches the right clock and throws it away: `getPoolTokens` returns
   `lastChangeBlock`, and `refresh_balancer` keeps only `.balances`. That third word is the natural
   stamp (it is what `getPoolTokens` reports for the last join/exit/swap on the pool).

The scale of the staleness is measurable on the corpus (App. 5.5): of the 159 rows that ever changed
balances, **106 have a Vault `lastChangeBlock` more than 1,000,000 blocks after their registration
block** (median gap 4,160,131 blocks), so a registration-time snapshot is stale by millions of blocks
before any arb decision is made.

**Cross-cutting recommendation (cheap, do it first regardless of family work):** make
`refresh_balancer` use `resolve_block` for the fetch and stamp `max(block, lastChangeBlock)`, with a red
test `refresh_balancer_stamps_the_fetched_block` (a `FakeRpc`-style IO serving
`getPoolTokens(...).lastChangeBlock = B`; `refresh_pool(..., None)` must leave `update_block == B`, not the
registration block and not `0`).

---

## 4. The 173 unmodeled rows, measured

### 4.1 All 173 are genuine Vault pools; the corpus's “unmodeled” label is exact

Every row answers `getPoolId()` **and** implements the Balancer pool callback surface measured from its
deployed code at the pin (`cast selectors` over `eth_getCode`):

| interface flag (selector) | count / 173 |
|---|---|
| `getPoolId` `0x38fff2d0` | 173 |
| `onSwap` 4-arg shape `0x01ec954a` (`cast selectors` renders `(uint256,address,address,uint256,uint256,uint256,address,address,bytes),uint256[],uint256,uint256`) | 64 (48 linear + 13 mocks + 3 VPT) |
| `onSwap` 2-balance shape `0x9d2c110c` | 157 (35 of which are also in the 64) |
| `onJoinPool(bytes32,address,address,uint256[],uint256,uint256,bytes)` `0xd5c096c4` | 173 |
| `onExitPool(...)` `0x74f3b009` | 173 |
| `getSwapFeePercentage` `0x55c67628` | 67 (48 linear + 13 mocks + 3 VPT + 3 no-name) |
| `getNormalizedWeights` / `getAmplificationParameter` | 0 / 0 (this *is* the corpus's bucket) |

A warning for anyone re-running this: a substring scan for `PUSH4 <selector>` inside the runtime code
reports “no onSwap” for a known-good `WeightedPool` too, because the deployed contracts do not dispatch
that selector through a `PUSH4`-compare the way a naive scan assumes. Use `cast selectors`, or call the
Vault. Confirmed by direct call: `cast call <bb-a-USDC> "getNormalizedWeights()(uint256[])"` and
`"getAmplificationParameter()(uint256,bool,uint256)"` both revert, while the same pool answers
`getMainToken()`, `getWrappedToken()`, `getBptIndex()`, `getTargets()`, `getVirtualSupply()`,
`getWrappedTokenRate()` and `getSwapFeePercentage()` (§4.3).

### 4.2 Venue census (name-derived, then corroborated by interface + Vault truth)

`name()`/`symbol()` were read on every row (170/173 answer; the 3 `no-name` rows have no ERC-20 metadata).

| group | n | registered blocks | zero-balance | recent 100k: swap / PBC | older 400k: swap / PBC | newest `lastChangeBlock` |
|---|---|---|---|---|---|---|
| `balancer-linear(bb-*)` — Aave/Yearn/ERC4626/Morpho/Euler/Gearbox/Idle/Sturdy/Fuse/Tessera linear pools, branded “Boosted Pool” | 48 | 13,767,310–17,677,991 | 0 | 0 / 7 | 8 / 4 | 26,187,441 |
| `element` — “LP Element Principal Token …” | 37 | 12,705,875–15,918,648 | 0 | 0 / 49 | 0 / 89 | 26,203,423 |
| `sense` — “Sense Space … Sense Principal Token …” | 25 | 14,805,267–17,790,480 | 1 | 0 / 0 | 1 / 1 | 25,962,083 |
| `element-adapter` — “… Adapter” (wstETH-adapter, cUSDC-adapter, …) | 23 | 14,336,341–14,695,309 | 6 | 0 / 0 | 0 / 0 | 25,576,548 |
| `fx-custom` — `LP-EURS-USDC`, `BPT-XSGD-USDC`, `LP-fxPHP-USDC`, `LP-GBPT-USDC`, `BPT-DAI-USDC`, `LP Unreal Ownership Token…` | 14 | 15,274,944–18,639,793 | 5 | 3 / 0 | 0 / 0 | 26,183,535 |
| `balancer-mock-linear` — name literally `DO NOT USE - Mock Linear Pool`, symbol `TEST` | 13 | 16,035,544–17,173,186 | 0 | 0 / 0 | 0 / 0 | 17,173,186 |
| `rate-anchored` — `RateAnchored USDC/USDT`, `RA-LIVE-USDC-USDT`, `TwapLive USDC-USDT` | 7 | 26,109,352–26,203,132 | 2 | 9 / 14 | 0 / 0 | 26,203,473 |
| `no-name` — no metadata, `mainToken=USDC`, `wrappedToken=aUSDC` | 3 | 25,531,667–25,553,518 | 3 | 0 / 0 | 0 / 0 | 25,553,518 |
| `vpt` — `Verified Pool Token` | 3 | 20,701,273–21,106,133 | 0 | 0 / 0 | 0 / 0 | 21,233,899 |

Totals: **21 swaps and 164 `PoolBalanceChanged` events** across all 173 pools in a 500k-block window
(`[pin−500000, pin]`); **18 pools** saw any event in the recent 100k blocks, **20** in the whole 500k.
Bandwidth limit made two disjoint windows, so the “recent” column is the last ~14 days and the “older”
column the ~56 days before that.

Structural facts:

- 17 pools hold **exactly zero** Vault balances; 36 have a largest balance ≤ 1e6 raw units (dust by any
  token's decimals). All 3 `no-name` rows, 6/23 adapters, 5/14 fx, 2/7 rate-anchored.
- The 48 linear pools all hold **their own BPT** among the Vault tokens, and `(bpt_idx, main_idx,
  wrapped_idx)` is a permutation of `{0,1,2}` in every one of them — but the permutation varies:
  `getBptIndex()` is 0 in 41/48 (2 in 4, 1 in 3) and `getMainIndex()` is 1 in 26/48 (2 in 18, 0 in 4).
  Never assume an order; read the three getters. This is the same shape the stable arm already models via
  `bpt_idx`/`BalancerComposableWeighted`.
- Exactly **one** row holds a *foreign* BPT: the Element pool `0x28b0379D98fb80Da460C190c95f97C74302214B1`
  (`yvBalancer-BoostedAaveUSD-04MAY23`) holds `0xa13a9247ea42d743238089903570127dda72fe44` (bb-a-USD) as
  a token. This is the only nested-pool-underlying case in the corpus.
- Registrations do not imply volume, and the corpus says so: the newest rows (0–13 days before the pin)
  are all `rate-anchored`, with balances of **1 wei per token** (or zero) and 9 swaps — test traffic, not
  a market. The `element` group has 49 join/exit events in 14 days and **zero** swaps in 500k blocks.

### 4.3 Measured shapes, one representative each (exact commands and outputs)

**Linear pool** — `0x82698aeCc9E28e9Bb27608Bd52cF57f704BD1B83` (“Balancer Aave Boosted Pool (USDC)”, symbol
`bb-a-USDC`, specialization 0 = general, 3 Vault tokens):

```
$ cast call 0x82698aeCc9E28e9Bb27608Bd52cF57f704BD1B83 "getMainToken()(address)"  --block 26204126 --rpc-url $RPC
0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48              # USDC
$ cast call … "getWrappedToken()(address)"     → 0xd093fA4Fb80D09bB30817FDcd442d4d02eD3E5de   # aUSDC
$ cast call … "getBptIndex()(uint256)"         → 0
$ cast call … "getTargets()(uint256,uint256)"  → 6000000000000000000000000 [6e24], 20000000000000000000000000 [2e25]
$ cast call … "getVirtualSupply()(uint256)"    → 4724936388201208551909 [4.724e21]
$ cast call … "getWrappedTokenRate()(uint256)" → 1210368834176350178 [1.21e18]
$ cast call … "getSwapFeePercentage()(uint256)"→ 10000000000000 [1e13]
$ cast call … "getNormalizedWeights()(uint256[])"          → execution reverted
$ cast call … "getAmplificationParameter()(uint256,bool,uint256)" → execution reverted
$ cast call 0xBA12222222228d8Ba445958a75a0704d566BF2C8 \
    "getPoolTokens(bytes32)(address[],uint256[],uint256)" 0x82698aec…0336 --block 26204126 --rpc-url $RPC
[0x82698aeCc9E28e9Bb27608Bd52cF57f704BD1B83, 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48, 0xd093fA4Fb80D09bB30817FDcd442d4d02eD3E5de]
[5192296855611588749544131500303307, 25966763, 0]
24550034
```

`getWrappedTokenRate() = 1.210368834176350178e18` is the entire price of this pool. The balance vector
`[5192296855611588749544131500303307, 25966763, 0]` says the wrapped side is **empty**, and the Vault
agrees when asked to trade:

```
$ cast call 0xBA12222222228d8Ba445958a75a0704d566BF2C8 \
    "queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))" \
    0 "[(0x82698aecc9e28e9bb27608bd52cf57f704bd1b83000000000000000000000336,0,1,1000000,0x)]" \
    "[0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48,0xd093fA4Fb80D09bB30817FDcd442d4d02eD3E5de]" \
    "(0x0000000000000000000000000000000000000000,false,0x0000000000000000000000000000000000000000,false)" \
    --block 26204126 --rpc-url $RPC
Error: execution reverted: BAL#001            # SUB_OVERFLOW: no wrapped balance to pay out
```

**Boosted-underlying (same family, different wrapper)** — `0x159cB00338fB63f263fd6f621Df619cEf71DA954`
(bb-ma2-USDC, Morpho Aave v2): Vault tokens `[BPT, USDC, 0xA5269A8e31B93Ff27B887B56720A25F844db0529]`,
balances `[5.19e33, 16,882,909, 4]`, `lastChangeBlock 25,392,370`. `0x126e7643235ec0ab9c103c507642dC3F4cA23C66`
(bb-t-stkAPE, Tessera): `[BPT, APE 0x4d224452…, sAPE 0x7966C5BA…]`, balances `[5.19e33, 35.097e18,
26.058e18]`, `lastChangeBlock 26,035,637`; `sAPE` answers `symbol()` but reverts `getPoolId()`/`getRate()`
— a plain staking wrapper, not a nested pool. The wrapper is the *yield source*: its price lives in
`getWrappedTokenRate()` (or, for an ERC4626 wrapper, `convertToAssets`), never in a curve.

**Element (custom two-token)** — `0x10a2F8bd81Ee2898D7eD18fb8f114034a549FA59`
(“LP Element Principal Token yvUSDC-28JAN22”, specialization 2 = two-token):
Vault tokens `[0x8a2228705ec979961F0e16df311dEbcf097A2766, USDC]`, balances `[16,081,002,061,
20,762,223,719]`, `lastChangeBlock 26,202,338` (hours before the pin). Code selectors: ERC-20 surface +
`getPoolId`, `onSwap` 2-balance `0x9d2c110c`, `onJoinPool 0xd5c096c4`, `onExitPool 0x74f3b009`, **no**
`getSwapFeePercentage`, **no** `getNormalizedWeights`/`getAmplificationParameter`. The swap path is live
and prices at the tranche's redemption rate — this is the *whole* reason its 49 recent events are joins and
exits rather than swaps:

```
$ cast call 0xBA12222222228d8Ba445958a75a0704d566BF2C8 \
    "queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))" \
    0 "[(0x10a2f8bd81ee2898d7ed18fb8f114034a549fa59000200000000000000000090,0,1,1000000,0x)]" \
    "[0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48,0x8a2228705ec979961F0e16df311dEbcf097A2766]" \
    "(0x0000000000000000000000000000000000000000,false,0x0000000000000000000000000000000000000000,false)" \
    --block 26204126 --rpc-url $RPC
0x0000…0020 0x0000…0002 0x0000…000f4240 0xffff…fff0bdc0   # +1,000,000 USDC in, −1,000,000 PT out: exact 1:1
```

**Custom / recent** — `0xc2cf7Bf63f93DAa12641840ABb1E20Adea99a3Cd` (“RateAnchored USDC/USDT”, registered
26,201,958 = ≈7 hours before the pin, `lastChangeBlock` 26,202,525): Vault tokens `[USDC, USDT]`,
balances `[1, 1]`; code has `token0() 0x0dfe1681` / `token1() 0xd21220a7` (Uniswap-V2 spellings), a
`0x7be219a2` selector, and a private error ABI — the swap reverts with a bare custom error
`0x9d8bfcbd` for a 1000-unit query:

```
$ cast call 0xBA12222222228d8Ba445958a75a0704d566BF2C8 \
    "queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))" \
    0 "[(0xc2cf7bf63f93daa12641840abb1e20adea99a3cd0002000000000000000007dc,0,1,1000,0x)]" \
    "[0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48,0xdAC17F958D2ee523a2206206994597C13D831ec7]" \
    "(0x0000000000000000000000000000000000000000,false,0x0000000000000000000000000000000000000000,false)" \
    --block 26204126 --rpc-url $RPC
Error: server returned an error response: error code 3: execution reverted, data: "0x9d8bfcbd"
```

**VPT** — `0x48f9df5C181f6aaF40Fef79c776FffbC3b5f31c8`: `[BPT 0x48f9…, 0x917991d5…, USDC]`, balances
`[5.19e33, 5.0e25, 1e6]`, `lastChangeBlock 21,106,202` (≈1.9 years before the pin), no events in either
window.

**Mocks** — 13 pools named `DO NOT USE - Mock Linear Pool` / symbol `TEST`, registered 16.0M–17.2M, no
events, `lastChangeBlock` == registration block. They implement the full LinearPool interface; they must
never be modelled as venues, and any future family probe must keep refusing them under a name (they are
not “unknown”: `getPoolId` answers).

**`no-name`** — 3 pools (specialization 1, 2 tokens) with **no** `name()`/`symbol()`/`totalSupply()`,
`mainToken = USDC`, `wrappedToken = aUSDC`, fee 0, zero balances, registered 91–94 days before the pin.
A LinearPool deployment that was never initialized (the getters answer, the state does not exist).

### 4.4 What the 173 rows are worth, arbitrage-wise

The definition that matters for this bot: *a venue is arb-relevant if a swap moves its price and a
competing venue quotes a different price for the same token pair.* Applied to the measured data:

- **Linear pools (48)** — the price is `getWrappedTokenRate()`, i.e. the wrapper's redemption rate. There
  is no curve to push: a swap converts at the rate, so the “arb” is a *depeg of a yield wrapper*, and
  the only counter-venue for aUSDC/ma2USDC/sAPE is the wrapper's own issuer (Aave/Morpho/Tessera), which
  is 1:1. Measured: 0 swaps in the recent 100k blocks across all 48; 8 in the 400k before. 21 of the 48
  have at least one side at exactly zero (13 wrapped-zero, 10 main-zero, 2 both), which is why the Vault
  refuses a trade with `BAL#001` (SUB_OVERFLOW) rather than quoting. **Verdict: no arb edge today.**
- **Element (37) + adapters (23)** — the pool price is a time-decaying redemption curve; the measured
  live swap is literally 1:1 on a matured tranche, and 100% of recent activity is join/exit. There is no
  price to arbitrage against a market (the PT's only redemption path is the tranche). **No.**
- **Sense (25)** — 1 swap in 500k blocks across all 25, 1 join/exit. **No.**
- **fx-custom (14)** — 3 swaps in the recent 100k, all in `0x5F8B11995D7F95faA59cA6fD5fFA1C0dbBe0EC7b`
  (`LP-EURS-USDC`, whole book: 202.446059 USDC + 535.86 EURS), none in the 400k before; 5/14 hold zero
  balances. A genuine but microscopic FX venue. **Not worth a family.**
- **rate-anchored (7)** — brand new (registered 0–13 days before the pin), 9 swaps + 14 PBC, balances of
  1 wei/token: a live *test* deployment of a third-party custom AMM on the shared Vault. **Watch, do not
  model.**
- **VPT (3), no-name (3), mocks (13)** — dead or never initialized. **No.**

Sanity check on the whole bucket: the 173 unmodeled pools produced **21 swaps in ~70 days**, while the
two families the engine already models (`weighted` 1,569 + `stable` 193) cover 1,762 of the 1,954
registered pools. The unmodeled set is 8.9% of the corpus by count and a rounding error of it by flow.


## 5. What each family would require

Primary sources are the Balancer V2 monorepo (not vendored in this repository):
`pkg/pool-linear/contracts/{LinearPool.sol,LinearMath.sol}`,
`pkg/pool-utils/contracts/{BasePool.sol,BaseGeneralPool.sol}`,
`pkg/vault/contracts/{Vault.sol,balances/Balances.sol}`,
`pkg/pool-weighted/contracts/WeightedPool.sol`, `pkg/pool-stable/contracts/ComposableStablePool.sol`,
`pkg/solidity-utils/contracts/math/FixedPoint.sol`. Each interface claim below is *also* backed by the
on-chain census in §4.1/§4.3, which is the only evidence that survives a redeploy.

### 5.1 Linear (Aave/Yearn/ERC4626/Morpho/Euler/Gearbox/Idle/Sturdy/Fuse/Tessera)

**Identity/probe.** `getMainToken()` + `getWrappedToken()` + `getBptIndex()` answer, while
`getNormalizedWeights()`/`getAmplificationParameter()` revert — the exact signature that today falls into
`probe_balancer_type`'s “unmodeled” arm. 48/48 measured.

**Registration parameters a builder must assemble**

| field | read | note |
|---|---|---|
| `pool_id`, `vault` | `getPoolId()` / request | — |
| `tokens[3]`, `balances[3]` | Vault `getPoolTokens` | order varies per pool: (bpt, main, wrapped) is a permutation of `{0,1,2}` (measured: main index 0/1/2 in 4/26/18 of 48) |
| `bpt_idx`, `main_idx`, `wrapped_idx` | `getBptIndex()` / `getMainIndex()` / `getWrappedIndex()` | all three answer (measured: 64/64 linear rows) |
| `swap_fee` | `getSwapFeePercentage()` | 48/48 answer (e.g. 1e13 = 0.001%) |
| `lower_target`, `upper_target` | `getTargets()` | 6e24/2e25 measured; the bands LinearMath clamps to |
| `virtual_supply` | `getVirtualSupply()` | 4.724e21 measured (NOT `totalSupply()`, which returns 2^112−1 on these) |
| `wrapped_token_rate` | `getWrappedTokenRate()` | 1.210368834176350178e18 measured; **mutable** (yield accrual) |
| `scaling_factors[3]` | `ONE*10^(18−decimals)` | standard |

**Math.** A port of `LinearMath` (`_calcOutGivenIn`/`_calcInGivenOut`, `_toNominal`/`_fromNominal`,
`_MAX_UPPER_TARGET`) over the *nominal* (rate-scaled) main/wrapped balances, with the two band
constraints. `degenbot-math/src/balancer/` today has `fixed_point.rs`, `log_exp_math.rs`,
`stable_math.rs`, `weighted_math.rs` only — there is no linear leaf.

**State slice.** A new `degenbot-pools` module is required (the weighted one cannot host it: 3 tokens,
one of which — the BPT — is *not* tradeable, two mutable scalars, and a pair that is not `tokens[0..2]`).
It must also carry the two mutable quantities the weighted slice does not have (`wrapped_token_rate`,
`virtual_supply`), because a linear pool's price changes with no balance changing at all.

**Resolver/hop.** This is the expensive part. `capture_hop` in both existing families is pairwise on
`balances[0]/balances[1]`, and the graph is pair-keyed by the DB row's `token0_id`/`token1_id`. A linear
pool's tradeable pair is `(main, wrapped)` = Vault indices `(1, 2)` for the measured layout, so the
family needs either (a) explicit index fields on the hop state (like the stable arm's `bpt_idx` skip, done
once at capture), or (b) a registered "pair view" that fixes the two indices at registration. (b) is what
the weighted module's header calls a Python-side `BalancerPairView` — a symbol that does not exist in the
tree; whatever ships must be Rust-owned or the solver cannot see it.

**Sweep.** `tests/balancer/test_balancer_registry_sweep.py::_FAMILY_CLASS` gains `"linear"`,
`_CORPUS_FAMILIES` grows, and the corpus must be **re-recorded** because the census moves
(48 linear + 13 mocks out of `unmodeled`).

### 5.2 Boosted-underlying (a linear pool whose wrapped token is itself yield-bearing / a pool token)

Measured variants: aUSDC (Aave), ma2USDC (Morpho Aave v2), sAPE (Tessera staking), sDAI, cUSDC, yv* tokens.
The rate source is per-variant:

- ERC4626-style wrapper → `convertToAssets(1e18)` on the wrapper (yearn/ERC4626 linear pools);
- Aave/Morpho aToken → `getWrappedTokenRate()` on the linear pool itself (the pool derives it from the
  wrapper's `scaledBalanceOf`/index);
- **nested Balancer pool** → the wrapper's BPT price, i.e. the nested pool's virtual price / `getRate()`.
  Exactly one corpus row is this case (the Element pool holding bb-a-USD), but the *stable* family's
  existing `getRateProviders()`+`getRate()` seam (`build_balancer_stable`, `BalancerRateProvider`,
  `SpecRefusal::BalancerRateProviderUnreadable`) is already the right shape for it.

Requirement: **do not make this a family.** It is a rate-source variant of §5.1 (and for stable pools it
already exists). What it *does* require is a decision about rate staleness: a linear pool's price is
`rate × (bands)`, so a cached rate is silently wrong; the weighted/stable slices deliberately freeze rates
at registration (`rate_provider: None` from the builder). Any linear implementation must either take
`rate_provider: Some(...)` from day one or refuse (`StaleRateResult` is the Python-side precedent in
`BalancerV2StablePool`).

### 5.3 Element / Sense (vendor custom two-token pools)

Measured: specialization 2 (two-token), 2-balance `onSwap 0x9d2c110c`, `onJoinPool`/`onExitPool`, and a
vendor-specific getter surface with **no** Balancer config getters. Element: no `getSwapFeePercentage` at
all (37/37), so the fee is inside `onSwap` and nothing else can be read; one owner-param selector
(`0xf46c39e7`) answers with a `uint256` and others revert. Sense: 15/25 expose `getSwapFeePercentage`,
the rest revert; its series metadata lives behind selectors that revert on the pools I probed
(`0x590aa985`, `0x1dccd830`) — i.e. read paths are series-state-dependent.

Requirement to model either: a bespoke invariant (Element's tranche curve; Sense's yield-space curve), a
bespoke parameter reader per vendor, and a per-pool state slice. Neither has a published library in the
Balancer monorepo. That is a vendor integration, not a Balancer family.

### 5.4 Custom (fx-stable pools, VPT, rate-anchored, no-name)

- fx-custom: 2-token, 2-balance `onSwap`, no Balancer config getters, `symbol()` `LP-EURS-USDC` etc.
  Vendor not determinable from on-chain metadata (no `name()` convention beyond the pool symbol).
- VPT: general 3-token pool with a BPT at index 0 (`[BPT, 0x917991d5…, USDC]`) and a fee getter.
- rate-anchored: 2-token, Uniswap-V2-style `token0()/token1()`, private error ABI, `0x7be219a2` selector,
  1-wei balances.
- no-name: LinearPool interface, never initialized.

Each would need its own builder + math + state. None of them is Balancer-source-derivable.


---

## 5.5 Evidence appendix — all 173 `unmodeled` rows at the pin

`ntok` = Vault token count; `lastChange` = the third word of Vault `getPoolTokens`
(`lastChangeBlock`, the last join/exit/swap that moved balances); `sw`/`pbc` = Vault
`Swap` / `PoolBalanceChanged` log counts in `[pin−100000, pin]`. Names are the pool's own
`name()`, truncated. Group name is the venue bucket of §4.2.

| address | group | ntok | lastChange | sw | pbc | name |
|---|---|---|---|---|---|---|
| `0x000b87c8A4c6CBCEf7a2577e8aa0Dc134C67c3D8` | element-adapter | 2 | 24217455 | 0 | 0 | Compound USD Coin Adapter |
| `0x03f84e2Cd229FFceE732a3c98DC1ACD8C8d5f5a2` | sense | 2 | 16307378 | 0 | 0 | Sense Space 1st Jan 2023 maUSDT Sense Principal T… |
| `0x057d17716A79899F161E4b0cDC0f686F806a4981` | rate-anchored | 2 | 26203263 | 1 | 2 | RateAnchored Live USDC/USDT |
| `0x06C55Dad9a50aDbC824Ea50eF7acF54a290E5493` | element-adapter | 2 | 23656783 | 0 | 0 | Compound USD Coin Adapter |
| `0x07f589eA6B789249C83992dD1eD324c3b80FD06b` | element | 2 | 21077785 | 0 | 0 | LP Element Principal Token yvCurve-stETH-24FEB23 |
| `0x09b1b33BaD0e87454ff05696b1151BFbD208a43F` | element | 2 | 24113135 | 0 | 0 | LP Element Principal Token yvCurve-MIM-11FEB22 |
| `0x0A0FB4ff697dE5ac5B6770CD8eE1B72AF80b57CF` | balancer-mock-linear | 3 | 16641632 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x0AFBd58bEcA09545e4fb67772faf3858e610bcD0` | balancer-linear(bb-*) | 3 | 16742504 | 0 | 0 | Balancer Idle Junior PYT Boosted agEUR |
| `0x0DA893E882277af482E442B2e73EB22e731F0EbC` | fx-custom | 2 | 15370626 | 0 | 0 | LP-fxPHP-USDC |
| `0x0d05Aac44aC7Dd3c7ba5d50Be93EB884A057d234` | balancer-mock-linear | 3 | 17052587 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x10a2F8bd81Ee2898D7eD18fb8f114034a549FA59` | element | 2 | 26202338 | 0 | 3 | LP Element Principal Token yvUSDC-28JAN22 |
| `0x126e7643235ec0ab9c103c507642dC3F4cA23C66` | balancer-linear(bb-*) | 3 | 26035637 | 0 | 0 | Balancer Tessera Boosted APE Pool |
| `0x13cF9E8115F35828a26062b6C05A56c72f54e0C6` | element | 2 | 15660699 | 0 | 0 | LP Element Principal Token yvCurve-alUSD-16SEP22 |
| `0x14792d3F6FcF2661795d1E08ef818bf612708BbF` | element | 2 | 19214256 | 0 | 0 | LP Element Principal Token yvCurve-MIM-29APR22 |
| `0x159cB00338fB63f263fd6f621Df619cEf71DA954` | balancer-linear(bb-*) | 3 | 25392370 | 0 | 0 | Balancer Morpho Aave v2 Boosted Pool (USDC) |
| `0x1AfeC1430d5615090067f5a3373f838aADDe9904` | element-adapter | 2 | 0 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x1B724E53D52A79763cf77Dd86EB2A42f1e108e48` | fx-custom | 2 | 15275762 | 0 | 0 | LP-fxPHP-USDC |
| `0x1ECbB397ac6AC884930c184c35037157DF0fB513` | rate-anchored | 2 | 0 | 0 | 0 | RateAnchored USDC/USDT |
| `0x1F99E92f4b550E2A8c0CFea252552f35f90BB5Df` | sense | 2 | 17594626 | 0 | 0 | Sense Space 1st July 2023 maUSDC Sense Principal … |
| `0x223029189aBA95C191cae748b1c4C3E0cFbC25A5` | sense | 2 | 16311473 | 0 | 0 | Sense Space 1st Jan 2023 wstETH Sense Principal T… |
| `0x22939E40Cf467DE8F5Db4F05A4027e5D4c1C658c` | element | 2 | 12735157 | 0 | 0 | LP Element Principal Token yvCurveLUSD-30JUN21 |
| `0x246ffB4d928E394a02E45761FecdbA6C2e79B8eB` | balancer-linear(bb-*) | 3 | 18245128 | 0 | 0 | Balancer Instadapp Lite Boosted Pool (WETH) |
| `0x24a82db9e1D0E96D9cAfa84a58825Fbe9701ced2` | element-adapter | 2 | 23680931 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x28b0379D98fb80Da460C190c95f97C74302214B1` | element | 2 | 18686703 | 0 | 0 | LP Element Principal Token yvBalancer-BoostedAave… |
| `0x2B218683178d029BAB6c9789b1073aA6c96E5176` | balancer-linear(bb-*) | 3 | 25457006 | 0 | 0 | Balancer Savings DAI Boosted Pool |
| `0x2BBf681cC4eb09218BEe85EA2a5d3D13Fa40fC0C` | balancer-linear(bb-*) | 3 | 25927663 | 0 | 0 | Balancer Aave Boosted Pool (USDT) |
| `0x2F4eb100552ef93840d5aDC30560E5513DFfFACb` | balancer-linear(bb-*) | 3 | 19526233 | 0 | 0 | Balancer Aave Boosted Pool (USDT) |
| `0x2FF1A9Dbdacd55297452cFD8a4d94724Bc22a5F7` | balancer-linear(bb-*) | 3 | 25137822 | 0 | 0 | Balancer Idle Boosted Pool (USDT) |
| `0x30004CDab9Aab2060Bc230455c874981064CCD1b` | no-name | 2 | 25553518 | 0 | 0 | — |
| `0x331d50e0b00fc1C32742F151E56B9B616227E23E` | balancer-mock-linear | 3 | 16600029 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x335d1709D4dA9aca59d16328db5Cd4eA66BFe06b` | balancer-linear(bb-*) | 3 | 25392370 | 0 | 0 | Balancer Morpho Aave v2 Boosted Pool (DAI) |
| `0x3420231Dc5Ed1330a54bCb0bdc62C0CeE0e7C51C` | element-adapter | 2 | 0 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x34c20B0B123a411E01B8958ad57984132ad03e63` | element-adapter | 2 | 0 | 0 | 0 | Compound USD Coin Adapter |
| `0x34d179259336812A1C7d320A0972e949dA5fa26d` | element-adapter | 2 | 23742439 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x36288792A4A5ADC7C19aF25341e093197381BbA6` | sense | 2 | 17594627 | 0 | 0 | Sense Space 1st July 2023 maUSDT Sense Principal … |
| `0x395d8A1D9aD82B5ABE558F8AbbFe183B27138af4` | balancer-mock-linear | 3 | 16886361 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x3A693EB97b500008d4Bb6258906f7Bbca1D09Cc5` | element | 2 | 15151041 | 0 | 0 | LP Element Principal Token yvCrvTriCrypto-15AUG21 |
| `0x3BB22Fc9033b802F2AC47c18885F63476F158aFC` | balancer-linear(bb-*) | 3 | 25214384 | 0 | 0 | Balancer Aave Boosted Pool (TUSD) |
| `0x3C640f0d3036Ad85Afa2D5A9E32bE651657B874F` | balancer-linear(bb-*) | 3 | 20266127 | 0 | 0 | Balancer Euler Boosted Pool (USDT) |
| `0x3CdAE4f12A67ba563499e102f309c73213cB241c` | balancer-linear(bb-*) | 3 | 15367135 | 0 | 0 | Balancer Aave Boosted Pool (USDC) |
| `0x3E1f8fC2fbAb8695B5749aFB1F390d30Bb33C880` | sense | 2 | 20151167 | 0 | 0 | Sense Space 1st June 2027 wstETH Sense Principal … |
| `0x3f9FEe026fCebb40719A69416C72B714d89a17d9` | element-adapter | 2 | 18076822 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x3fCb7085B8F2F473F80bF6D879cAe99eA4DE9344` | balancer-linear(bb-*) | 3 | 25587356 | 0 | 0 | Balancer Morpho Aave v3 Boosted Pool (WETH) |
| `0x454Ed96955d04D2f5cDD05E0Fd1C77975bFE5307` | balancer-linear(bb-*) | 3 | 24827588 | 0 | 0 | Balancer Aave Boosted Pool (TUSD) |
| `0x489eeDB33F82574AfEABb3F4e156FBf662308AdA` | element | 2 | 15660697 | 0 | 0 | LP Element Principal Token yvCurve-LUSD-16SEP22 |
| `0x48aD3b70CB684Df5151bE44ee317DAe5681Cc944` | sense | 2 | 17594628 | 0 | 0 | Sense Space 1st July 2023 w-aurawstETH-rETH-sfrxE… |
| `0x48f9df5C181f6aaF40Fef79c776FffbC3b5f31c8` | vpt | 3 | 21106202 | 0 | 0 | Verified Pool Token |
| `0x4A82b580365Cff9B146281Ab72500957a849ABDC` | balancer-linear(bb-*) | 3 | 18241721 | 0 | 0 | Balancer Gearbox Boosted Pool (USDC) |
| `0x4Db9024fc9F477134e00Da0DA3c77DE98d9836aC` | element | 2 | 24800069 | 0 | 0 | LP Element Principal Token yvWBTC-26NOV21 |
| `0x4bd6D86dEBdB9F5413e631Ad386c4427DC9D01B2` | element | 2 | 25933146 | 0 | 0 | LP Element Principal Token yvWBTC-29APR22 |
| `0x4cE277Df0FeB5B4d07a0ca2ADCf5326E4005239d` | balancer-mock-linear | 3 | 17045360 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x4ee4DdFb958416Ca2AFcA3a894C3C3ED9a3CDDa1` | sense | 2 | 25576548 | 0 | 0 | Sense Space 1st Aug 2022 wstETH Sense Principal T… |
| `0x533A7d8D1d35DeC92269220F927E9Ad12F59d4cb` | element-adapter | 2 | 0 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x544c823194218f0640daE8291c1f59752d25faE3` | element | 2 | 24368139 | 0 | 0 | LP Element Principal Token yvcrvSTETH-28JAN22 |
| `0x55bEC22f8f6C69137cEAf284d9B441Db1B9bFedc` | fx-custom | 2 | 0 | 0 | 0 | BPT-XSGD-USDC |
| `0x56F30398d13F111401d6e7ffE758254a0946687d` | element | 2 | 25672750 | 0 | 0 | LP Element Principal Token yvCurveLUSD-29APR22 |
| `0x56df5ef1A0A86c2A5Dd9cC001Aa8152545BDbdeC` | element | 2 | 24556243 | 0 | 0 | LP Element Principal Token yvUSDC-16SEP22 |
| `0x5746aFD392B13946AaCBdA40317751dB27D8B918` | element | 2 | 18687651 | 0 | 0 | LP Element Principal Token yvUSDC-24FEB23 |
| `0x5BFe65bcA476BdA0d0D393372394aE352EC0627b` | element-adapter | 2 | 14932157 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x5F8B11995D7F95faA59cA6fD5fFA1C0dbBe0EC7b` | fx-custom | 2 | 26183535 | 3 | 0 | LP-EURS-USDC |
| `0x5ad5f864b346445a16fC95C59174269Ed288e640` | sense | 2 | 25587318 | 0 | 0 | Sense Space 1st Apr 2023 wstETH Sense Principal T… |
| `0x60Ce0aC20A0CD8fCDC9720f62696E9136d03A1b3` | fx-custom | 2 | 0 | 0 | 0 | LP-xSGD-USDC |
| `0x60D604890feaa0b5460B28A424407c24fe89374a` | balancer-linear(bb-*) | 3 | 26176775 | 0 | 3 | Balancer Aave v3 Boosted Pool (WETH) |
| `0x62DCcE87E77C67325fD984B8fE234f333bCC0D29` | sense | 2 | 16949717 | 0 | 0 | Sense Space 1st Apr 2023 maUSDC Sense Principal T… |
| `0x63E9B50DD3eB63BfBF93B26F57b9EFB574e59576` | element | 2 | 17498527 | 0 | 0 | LP Element Principal Token yvCurve-alUSD-29APR22 |
| `0x64db005d040EE62E8fa202291C73E8a6151A0399` | element-adapter | 2 | 24217443 | 0 | 0 | Compound USD Coin Adapter |
| `0x652D486B80C461C397B0d95612A404da936f3db3` | balancer-linear(bb-*) | 3 | 24820024 | 0 | 0 | Balancer Boosted Aave USDC |
| `0x6667c6fa9f2b3Fc1Cc8D85320b62703d938E4385` | balancer-linear(bb-*) | 3 | 26187441 | 0 | 2 | Balancer Aave v3 Boosted Pool (DAI) |
| `0x66bB9D104c55861feb3Ec3559433F01f6373c966` | fx-custom | 2 | 18019016 | 0 | 0 | BPT-DAI-USDC |
| `0x67F8db40638D8e06Ac78E1D04a805F59d11aDf9b` | sense | 2 | 21406085 | 0 | 0 | Sense Space 1st July 2023 maDAI Sense Principal T… |
| `0x6AC02eCD0c2A23B11f9AFb3b3Aaf237169475cac` | element | 2 | 17874985 | 0 | 0 | LP Element Principal Token yvCurve-EURS-11FEB22 |
| `0x6Dd0F7c8F4793ed2531c0df4fEA8633a21fDcFf4` | element | 2 | 24110005 | 0 | 0 | LP Element Principal Token yvcrv3crypto-29APR22 |
| `0x6De69beB66317557E65168BD7D3fff22a89dBb11` | element | 2 | 12733860 | 0 | 0 | LP Element Principal Token yvDAI-27JUN21 |
| `0x6a1eB2e9B45e772F55bd9a34659A04B6F75Da687` | balancer-linear(bb-*) | 3 | 17658820 | 0 | 0 | Balancer Aave Boosted Pool (FRAX) |
| `0x6a556A2C2511E605a6c464Ab5dCcFdC0B19822E7` | element-adapter | 2 | 18076843 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x71628c66C502F988Fbb9e17081F2bD14e361FAF4` | element | 2 | 26183306 | 0 | 3 | LP Element Principal Token yvDAI-16OCT21 |
| `0x7337224D59CB16C2Dc6938CD45A7b2c60C865D6A` | balancer-linear(bb-*) | 3 | 25392360 | 0 | 0 | Balancer Morpho Aave v2 Boosted Pool (USDT) |
| `0x73f8E7A9A19E284a9Ac85704Af58454cFe75f059` | fx-custom | 2 | 22227545 | 0 | 0 | LP-GBPT-USDC |
| `0x74CBfAF94A3577c539a9dCEE9870A6349a33b34f` | balancer-mock-linear | 3 | 17173186 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x76E5F9860Bd5b8666DE322496f114B8a89183A2E` | element-adapter | 2 | 18190013 | 0 | 0 | Compound USD Coin Adapter |
| `0x787546Bf2c05e3e19e2b6BDE57A203da7f682efF` | element | 2 | 26109082 | 0 | 1 | LP Element Principal Token yvUSDC-29OCT21 |
| `0x7968c9764F7e0da5642Ec17B5446D7b6A5d9c2e9` | element-adapter | 2 | 0 | 0 | 0 | Compound USD Coin Adapter |
| `0x7D3fF89007E7847128b4f5c1546549afe0BA67Ed` | element-adapter | 2 | 0 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x7Edde0CB05ED19e03A9a47CD5E53fC57FDe1c80c` | element | 2 | 26184360 | 0 | 22 | LP Element Principal Token yvUSDC-29APR22 |
| `0x7F4A33deE068C4fA012d64677C61519a578dfA35` | element | 2 | 24978177 | 0 | 0 | LP Element Principal Token yvDAI-24FEB23 |
| `0x804CdB9116a10bB78768D3252355a1b18067bF8f` | balancer-linear(bb-*) | 3 | 25927663 | 0 | 0 | Balancer Aave Boosted Pool (DAI) |
| `0x813E3fE1761f714c502d1d2d3a7CCEB33f37F59D` | balancer-mock-linear | 3 | 16169944 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x82698aeCc9E28e9Bb27608Bd52cF57f704BD1B83` | balancer-linear(bb-*) | 3 | 24550034 | 0 | 0 | Balancer Aave Boosted Pool (USDC) |
| `0x84864E5AF8ab5aAc31b7a7aF389183cbc4119F64` | fx-custom | 2 | 0 | 0 | 0 | LP-fxPHP-USDC |
| `0x85DCA8667D020e694FDfF06E7eE85e0c5C7C61a4` | element | 2 | 12866465 | 0 | 0 | LP Element Principal Token yvDAI-08DEC23 |
| `0x893B30574BF183d69413717f30b17062eC9DFD8b` | element | 2 | 26138252 | 0 | 1 | LP Element Principal Token yvCurveLUSD-27DEC21 |
| `0x8B16914F0F9C5Ba44377787cc120D3713B0959E1` | no-name | 2 | 25531667 | 0 | 0 | — |
| `0x8E59d8bb3D742AC2045F028BCFC39c7dc933B57d` | rate-anchored | 2 | 26203473 | 1 | 2 | RateAnchored Live USDC/USDT v2 |
| `0x8E6eC57A822c2F527F2Df7C7D7D361dF3E7530a1` | balancer-linear(bb-*) | 3 | 20193526 | 0 | 0 | Balancer Yearn Boosted Pool (DAI) |
| `0x8f4063446F5011bC1C9F79A819EFE87776F23704` | balancer-linear(bb-*) | 3 | 22416434 | 0 | 0 | Balancer Fuse-8 Boosted DAI |
| `0x8fFD1dc7C3eF65f833CF84dbCd15b6Ad7f9C54EC` | element | 2 | 24098472 | 0 | 0 | LP Element Principal Token yvDAI-16SEP22 |
| `0x90CA5cEf5B29342b229Fb8AE2DB5d8f4F894D652` | element | 2 | 26111717 | 0 | 1 | LP Element Principal Token yvUSDC-17DEC21 |
| `0x910FD04A174E705B30C98E04ba4689AB95DBb1fc` | rate-anchored | 2 | 26110147 | 3 | 4 | TwapLive USDC-USDT v2 |
| `0x9210F1204b5a24742Eba12f710636D76240dF3d0` | balancer-linear(bb-*) | 3 | 25965812 | 0 | 0 | Balancer Aave Boosted Pool (USDC) |
| `0x9224eF7cFcCFBD8AE1Ee6111FC1246C68a32869D` | fx-custom | 2 | 0 | 0 | 0 | LP-fxPHP-USDC |
| `0x94C476E4675FAaC1Bf60211c5E39486bCF720cBD` | element | 2 | 14114040 | 0 | 0 | LP Element Principal Token yvCurve-MIM-11FEB22 |
| `0x9516a2d25958EdB8da246a320f2c7d94A0DBe25d` | balancer-mock-linear | 3 | 17045882 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0x9796Ea4490f99330150fC400Cbc9Ac1292DfbC79` | fx-custom | 2 | 17881882 | 0 | 0 | LP Unreal Ownership Token:YEARN:yvDAI:1:2023-5-21 |
| `0x9A6dCC67FddaEedE76Cc978D4D23BB0A7f9b4282` | element-adapter | 2 | 14371844 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0x9B1c8407a360443A9E5ECA004713E4088FAB8AC0` | balancer-linear(bb-*) | 3 | 25108138 | 0 | 0 | Balancer Yearn Boosted Pool (USDC) |
| `0x9f41a3d1EC37090e52a9102b24Bc6921A813925F` | sense | 2 | 19909954 | 0 | 0 | Sense Space 1st June 2027 wstETH Sense Principal … |
| `0xA1697F9Af0875B63DdC472d6EeBADa8C1fAB8568` | balancer-linear(bb-*) | 3 | 26136469 | 0 | 1 | Balancer Aave v3 Boosted Pool (USDT) |
| `0xA3823e50f20982656557A4A6a9C06Ba5467aE908` | balancer-linear(bb-*) | 3 | 24820024 | 0 | 0 | Balancer Boosted Aave DAI |
| `0xA47D1251CF21AD42685Cc6B8B3a186a73Dbd06cf` | element | 2 | 26055910 | 0 | 0 | LP Element Principal Token yvDAI-28JAN22 |
| `0xA68CcD2c3805c175c9F9CAf30Ae497E173dAB30e` | sense | 2 | 25612676 | 0 | 0 | Sense Space 1st July 2023 cUSDC Sense Principal T… |
| `0xA8D4433BAdAa1A35506804B43657B0694deA928d` | element | 2 | 26203423 | 0 | 17 | LP Element Principal Token yvCurveLUSD-28SEP21 |
| `0xABb93e3787b984cb62dCD962af8732f52Ff57586` | element | 2 | 16689116 | 0 | 0 | LP Element Principal Token yvCurve-stETH-16SEP22 |
| `0xABc0191377f1513B7785e08aF880d77eC5618B75` | sense | 2 | 0 | 0 | 0 | Sense Space 1st Jan 2023 wstETH Sense Principal T… |
| `0xAC5B4EF7eDe2F2843a704E96dcAA637F4BA3Dc3f` | balancer-mock-linear | 3 | 17052604 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0xAd0e5e0778cAC28f1ff459602b31351871B5754a` | fx-custom | 2 | 18965386 | 0 | 0 | BPT-EURS-USDC |
| `0xC51D3287A8c20dc865dF7b5Da6f5b2d596eE694B` | sense | 2 | 17161399 | 0 | 0 | Sense Space 1st May 2023 ag-wrapper-sanFRAX_EUR S… |
| `0xC58150838855a5605C9030398c6D22afd5Bb09EE` | fx-custom | 2 | 18669382 | 0 | 0 | LP-GBPT-USDC |
| `0xC80Fef22fCcc277aC0fFEA84D111888A767f717e` | element | 2 | 15660607 | 0 | 0 | LP Element Principal Token yvCurve-3Crypto-16SEP22 |
| `0xC9AD279994980F8DF348b526901006972509677F` | element | 2 | 19214344 | 0 | 0 | LP Element Principal Token yvCurve-alUSD-28JAN22 |
| `0xCdBE59755723d6D34c3F7A071796AAB362f00362` | element-adapter | 2 | 23656765 | 0 | 0 | Compound USD Coin Adapter |
| `0xCe621Fc54D2547294A46b4D5c142bdEB2d0F0Aeb` | element-adapter | 2 | 23651847 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0xD098bA344Ab1F1068360d07358a7921b591a46E7` | no-name | 2 | 25531667 | 0 | 0 | — |
| `0xD40954A9Ff856f9A2C6eFa88aD45623157A7dfF0` | element-adapter | 2 | 25575639 | 0 | 0 | Compound USD Coin Adapter |
| `0xD4e7C1F3DA1144c9E2CfD1b015eDA7652b4a4399` | balancer-linear(bb-*) | 3 | 24707532 | 0 | 0 | Balancer Euler Boosted Pool (USDC) |
| `0xD6E355036f41dC261B3f1ed3Bbc6003E87AAdB4f` | balancer-linear(bb-*) | 3 | 24548672 | 0 | 0 | Balancer Yearn Boosted Pool (USDT) |
| `0xDEC02e6642e2c999aF429F5cE944653CAd15e093` | balancer-mock-linear | 3 | 16588079 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0xDbA274B4D04097b90A72b62467d828cEFD708037` | balancer-linear(bb-*) | 3 | 24842974 | 0 | 0 | Balancer Idle Boosted Pool (DAI) |
| `0xDc063deAfcE952160eC112FA382ac206305657e6` | balancer-linear(bb-*) | 3 | 16784075 | 0 | 0 | Balancer Sturdy Boosted Pool (USDT) |
| `0xEF779137CF7d0f55e2c486ed680787D4d2D93625` | sense | 2 | 17855834 | 0 | 0 | Sense Space 1st Apr 2023 wstETH Sense Principal T… |
| `0xEcf2c7A432E5f7D8BA0A85B12f2aE3e4874ec690` | element-adapter | 2 | 18076846 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0xEdf085f65b4F6c155e13155502Ef925c9a756003` | element | 2 | 26182330 | 0 | 1 | LP Element Principal Token yvDAI-29APR22 |
| `0xF201E99F5ea2D846Bd7Dc3229A2e602ab1d2e308` | sense | 2 | 17594624 | 0 | 0 | Sense Space 1st July 2023 idleUSDCJunior4626 Sens… |
| `0xF22ff21e17157340575158aD7394E068048dd98B` | balancer-linear(bb-*) | 3 | 16741495 | 0 | 0 | Balancer Idle Boosted Pool |
| `0xF6dc4640D2783654BeF88E0dF3fb0F051f0DfC1A` | element | 2 | 24229722 | 0 | 0 | LP Element Principal Token yvcrv3crypto-12NOV21 |
| `0xF71d0774B214c4cf51E33Eb3d30Ef98132e4DBaA` | balancer-linear(bb-*) | 3 | 16592592 | 0 | 0 | Balancer Euler Boosted Pool (FRAX) |
| `0xFD11CCdBdb7AB91Cb9427A6d6BF570C95876d195` | balancer-linear(bb-*) | 3 | 25559262 | 0 | 0 | Balancer Sturdy Boosted Pool (DAI) |
| `0xa00486f88C98e2131E043e6B28fA53D46db7e6C7` | sense | 2 | 25962083 | 0 | 0 | Sense Space 1st Dec 2022 wstETH Sense Principal T… |
| `0xa16109e6ad6f7a3C4e97fb104d2A0031b2D16640` | fx-custom | 2 | 15305726 | 0 | 0 | LP-xSGD-USDC |
| `0xa4fCd765171E50702093e020Fa2FfE22432224aa` | fx-custom | 2 | 0 | 0 | 0 | LP-xSGD-USDC |
| `0xa8b103A10A94f4f2D7Ed2FdCd5545E8075573307` | balancer-mock-linear | 3 | 16637932 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0xa9E5001F4AF1983D5Dc9986b1dF7E5769f56F81f` | rate-anchored | 2 | 0 | 0 | 0 | TwapLive USDC-USDT |
| `0xae37D54Ae477268B9997d4161B96b8200755935c` | balancer-linear(bb-*) | 3 | 25151023 | 0 | 0 | Balancer Aave Boosted Pool (DAI) |
| `0xb03C6B351A283bc1Cd26b9cf6d7B0c4556013bDb` | element | 2 | 25958643 | 0 | 0 | LP Element Principal Token yvcrvSTETH-15APR22 |
| `0xb0F75E97A114A4EB4a425eDc48990e6760726709` | balancer-linear(bb-*) | 3 | 22416434 | 0 | 0 | Balancer Fuse-8 Boosted LUSD |
| `0xb6d249437270f710e0362d4566cE1A2f1ba56bc2` | sense | 2 | 16949716 | 0 | 0 | Sense Space 1st Apr 2023 maUSDT Sense Principal T… |
| `0xb7Dff8b71E62BfF9f850795bcea61Ffbca7c87A3` | sense | 2 | 25577250 | 0 | 0 | Sense Space 1st Dec 2022 wstETH Sense Principal T… |
| `0xbA5f10B7fa4974E2909ec211eB95b9424E3C3Ee2` | vpt | 3 | 21080192 | 0 | 0 | Verified Pool Token |
| `0xbB6881874825E60e1160416D6C426eae65f2459E` | balancer-linear(bb-*) | 3 | 26090948 | 0 | 0 | Balancer Aave v3 Boosted Pool (WETH) |
| `0xbB6C7b5E0804d07aE31A43E6E83Ea66fb128a3BB` | element-adapter | 2 | 25576548 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0xbFA413A2ff0F20456d57B643746133F54bFE0cd2` | balancer-linear(bb-*) | 3 | 25461702 | 0 | 0 | Balancer Sturdy Boosted Pool (USDC) |
| `0xbc0F2372008005471874e426e86CCFae7B4De79d` | balancer-linear(bb-*) | 3 | 24908482 | 0 | 0 | Balancer Idle Boosted Pool (USDC) |
| `0xc0B5FC5fc888c18356318CbbCE6E6fE0bC949402` | sense | 2 | 17594625 | 0 | 0 | Sense Space 1st July 2023 wstETH Sense Principal … |
| `0xc2cf7Bf63f93DAa12641840ABb1E20Adea99a3Cd` | rate-anchored | 2 | 26202525 | 3 | 4 | RateAnchored USDC/USDT |
| `0xc50d4347209F285247BDa8A09Fc1C12CE42031c3` | balancer-linear(bb-*) | 3 | 19441838 | 0 | 0 | Balancer Aave v3 Boosted Pool (USDC) |
| `0xc8C79fCD0e859e7eC81118e91cE8E4379A481ee6` | balancer-linear(bb-*) | 3 | 22416434 | 0 | 0 | Balancer Fuse-8 Boosted FEI |
| `0xcbFA4532D8B2ade2C261D3DD5ef2A2284f792692` | balancer-linear(bb-*) | 3 | 26136474 | 0 | 1 | Balancer Aave v3 Boosted Pool (USDC) |
| `0xce16E7ed7654a3453E8FaF748f2c82E57069278f` | element | 2 | 24098480 | 0 | 0 | LP Element Principal Token yvcrvSTETH-15OCT21 |
| `0xcfAE6E251369467F465f13836Ac8135bd42f8A56` | balancer-linear(bb-*) | 3 | 19441832 | 0 | 0 | Balancer Aave v3 Boosted Pool (USDT) |
| `0xcfD44644926D66247685a104515f5dc1C0d7dBe2` | sense | 2 | 25771387 | 0 | 0 | Sense Space 1st Mar 2023 BB_wstETH4626 Sense Prin… |
| `0xd0f001dbC3B5C3cFd5d2584f8C2cBD998c4D61e1` | sense | 2 | 18251069 | 0 | 0 | Sense Space 1st Oct 2023 wstETH Sense Principal T… |
| `0xd8dc4Cc15945A16AA5201BCaBFA1D32D2Cb39Fd6` | sense | 2 | 25612571 | 0 | 0 | Sense Space 1st June 2023 wstETH Sense Principal … |
| `0xdBBFb4ccd10a0828b95Ce6FbE8a7502A9e6a76F5` | sense | 2 | 24217449 | 0 | 0 | Sense Space 1st July 2023 cDAI Sense Principal To… |
| `0xe03aF00faBe8401560c1FF7d242d622a5b601573` | balancer-linear(bb-*) | 3 | 18241696 | 0 | 0 | Balancer Gearbox Boosted Pool (DAI) |
| `0xe0c50a16D6fA0D85DaA64C785F6fd47F13f82CA0` | vpt | 3 | 21233899 | 0 | 0 | Verified Pool Token |
| `0xe23D111cb71414830B93F078977B7f53A5e996eE` | balancer-mock-linear | 3 | 16035544 | 0 | 0 | DO NOT USE - Mock Linear Pool |
| `0xe6bCC79f328eec93D4Ec8F7ED35534d9AB549faA` | balancer-linear(bb-*) | 3 | 24820024 | 0 | 0 | Balancer Boosted Aave USDT |
| `0xe7D03B47FA2b3E8f7E6fa0DCf17ff0b54815C92b` | sense | 2 | 16307373 | 0 | 0 | Sense Space 1st Jan 2023 maUSDC Sense Principal T… |
| `0xeB486AF868AeB3b6e53066abc9623b1041b42bc0` | balancer-linear(bb-*) | 3 | 24762204 | 0 | 0 | Balancer Euler Boosted Pool (DAI) |
| `0xea34B59a4E7075203A93F97178b2581d5f3f9919` | element-adapter | 2 | 15646202 | 0 | 0 | Compound USD Coin Adapter |
| `0xefEab9874F6c9CEf9675Ed46916F55464295ea14` | element-adapter | 2 | 18076826 | 0 | 0 | Wrapped liquid staked Ether 2.0 Adapter |
| `0xf06bBBdec03bf7A5222308de75f4e7cD7cff9759` | rate-anchored | 2 | 26202619 | 1 | 2 | RateAnchored USDC/USDT |
| `0xfa24A90A3F2bBE5FEEA92B95cD0d14Ce709649f9` | balancer-linear(bb-*) | 3 | 19441906 | 0 | 0 | Balancer Aave v3 Boosted Pool (DAI) |
| `0xfeF969638C52899f91781f1Be594aF6f40B99BAd` | balancer-mock-linear | 3 | 16599925 | 0 | 0 | DO NOT USE - Mock Linear Pool |


---

## 6. Recommended changes, per family, and the first red test for each

The pattern to mirror is exactly the one that shipped for weighted:
`build_balancer_weighted` (builder) → `construct_balancer_weighted` (route arm) →
`register_balancer_weighted_pool` (write door + arity) → `capture_hop`/`read_swap` (family read module) →
`_FAMILY_CLASS` (sweep). Red/green: the first test must fail on the current tree for a reason that
names the missing symbol, not for a missing file.

### 6.0 Cross-cutting, in order (these are prerequisites, not cleanup)

1. **Fix the Balancer refresh clock** — `bot_core/refresh.rs::refresh_balancer`. Use `resolve_block` for
   the fetch block and stamp the `lastChangeBlock` the `getPoolTokens` read already returns instead of
   `block.unwrap_or(0)`.
   *First red test:* `refresh_balancer_stamps_the_fetched_block` in `refresh.rs`'s test module — a stub
   `RpcConstruction` whose `call` decodes the `getPoolTokens` selector and returns
   `(tokens, balances, 26_200_000)`, with a registered `BalancerWeighted` pool at `update_block = 100`;
   `refresh_pool(&state, id, &io, None)` must return `Changed` and leave `update_block == 26_200_000`
   (today: `Unchanged`, `update_block` still 100).
2. **Decide Balancer's graph admission** — `degenbot-pathfinding/src/graph.rs::PoolKind` /
   `KNOWN_KINDS`, `degenbot-db/src/pathfinding.rs::fetch_path_graph_edges`. Today a `pools` row with
   `kind = "balancer_weighted"` makes the whole chain's graph read return `DbError::UnknownPoolKind`;
   `DECLARED_UNSUPPORTED_KINDS` is the existing seam for "declared but not admitted".
   *First red test:* `fetch_path_graph_edges_refuses_no_row_kind` in `degenbot-db` — one `pools` row of
   each kind present in the DB, asserting the exact intended behavior (admit an edge, or refuse with a
   message naming `balancer_weighted` and the skip-list remedy). Whichever answer is chosen, the current
   silent whole-graph failure must become a decision.
3. **Split the sweep's unmodeled bucket** — `test_balancer_registry_sweep.py::_CORPUS_FAMILIES` /
   `_UNMODELED_REFUSAL` plus `scripts/record_balancer_pool_registrations.py::_FamilyProbe`. Give the
   measured groups their own corpus families (`linear`, `element`, `sense`, `custom`, `mock`) while all
   keep refusing as `SpecRefusal::BalancerUnmodeledFamily` (except `linear`, once §6.1 lands). This is a
   *data + vocabulary* change: re-record at the pin, no engine risk.
   *First red test:* `test_corpus_is_the_pinned_vault_registration_scan` (offline) — it asserts
   `dict(census) == corpus["census"]` and the closed vocabulary, so a re-recorded corpus with the new
   buckets fails until the sweep's maps are updated. That is the correct red.

### 6.1 Linear — GO, *after* 6.0(2); conditional on a live rate provider

New/changed symbols, in dependency order:

| layer | change |
|---|---|
| `degenbot-math/src/balancer/linear_math.rs` (new) | `calc_out_given_in` / `calc_in_given_out`, `to_nominal`/`from_nominal`, band clamps; property tests against the deployed contract on a fork |
| `degenbot-pools/src/state/balancer_linear_state.rs` (new) | `RegisterBalancerLinearPoolParams { address, vault, pool_id, tokens, bpt_idx, main_idx, wrapped_idx, lower_target, upper_target, swap_fee, wrapped_token_rate, virtual_supply, balances, update_block, rate_provider }`, `BalancerLinearPoolIdentity`, `BalancerLinearPoolState`, `from_params` (genesis anchor), `ReorgPoolState`, `BalanceVectorPoolState` |
| `degenbot-pools/src/families/balancer_linear.rs` (new) | `capture_hop` (index-mapped pair, rate-multiplied nominal balances), `read_swap`, `FamilyApply`, `observe`, a `solve::BalancerLinearHopState` |
| `degenbot-pools/src/sim/registry.rs` | `RegisteredPoolFamily::BalancerLinear`, `PoolEntry::BalancerLinear` (the exhaustive matches in `refresh_pool`, `families`, `apply_*` become compile errors — that is the design) |
| `degenbot-substrate/src/balance_vector_orchestration.rs` | `register_balancer_linear_pool` (arity: tokens == balances == scaling_factors == 3; `bpt_idx/main_idx/wrapped_idx` distinct and < 3) |
| `bot_core/pool_builder/builder.rs` | `build_balancer_linear(vault, address, io, block)` — reads of §5.1; refuses `BROKEN_BALANCER_V2_POOLS`, a token vector that is not `{bpt, main, wrapped}`, an unreadable `getWrappedTokenRate` (`SpecRefusal::BalancerRateProviderUnreadable`), an unreadable `decimals` (`SpecRefusal::NoCodeCoin`) |
| `bot_core/pool_builder/route.rs` + `construction_door.rs` | `construct_balancer_linear`, `PoolConstruction::BalancerLinear` |
| `bot_core/pool_builder/pool_identity.rs` | `BalancerFamily::Linear` + a `getMainToken`+`getWrappedToken`+`getBptIndex` arm in `probe_balancer_type`; `identity_for_kind("balancer_linear")`; `PoolFamily::from_identity` projection |
| Python | `builders/construction.py::request_for` arm, `types/pool_type.py` descriptor, `balancer/linear_pools.py::BalancerV2LinearPool.from_handle`, `pool_class_for_descriptor` entry |
| sweep | `_FAMILY_CLASS["linear"] = BalancerV2LinearPool`, `_CORPUS_FAMILIES`, corpus re-record |

*First red test* (mirror of `build_balancer_weighted_assembles_params`, same file
`bot_core/pool_builder/tests.rs`): `build_balancer_linear_assembles_params`, a `FakeRpc` keyed by
4-byte selector serving `getPoolId`, `getPoolTokens(bytes32)` = `([TO, T0, T1], [5_192_296_858_534_827_628_530_496_329_220_096,
26_000_000, 0], 26_200_000)`, `getSwapFeePercentage` = 1e13, `getMainToken` = `T0`, `getWrappedToken` = `T1`,
`getBptIndex` = 0, `getMainIndex` = 1, `getWrappedIndex` = 2, `getTargets` = (6e24, 2e25),
`getVirtualSupply` = 4.724e21, `getWrappedTokenRate` = 1.21e18, `decimals` = 6. Assertions:
`bpt_idx == Some(0)`, `main_idx == 1`, `wrapped_idx == 2`, `tokens == [TO, T0, T1]`,
`scaling_factors == [1e18, 1e30, 1e30]` (BPT 18 dp, USDC/aUSDC 6 dp, per
`compute_scaling_factor`'s `ONE * 10^(18−decimals)`), `wrapped_token_rate == 1_210_368_834_176_350_178`,
`lower_target/upper_target == 6e24/2e25`.

*Second red test* (mirror of `build_balancer_weighted_refuses_a_composable_weighted_pool`):
`build_balancer_linear_refuses_a_non_linear_vault_shape` — a 2-token or 4-token Vault reply, or a
`getMainToken()` that is not in the token vector, must refuse with the new named reason rather than panic
or produce a 3-arity state.

*Third* (fork tier, mirrors the corpus leg): extend `test_balancer_registry_sweep.py` so the 48 `linear`
rows build as `BalancerV2LinearPool` and the 13 mocks stay refused-by-name (they are *not* venues; name
them in the corpus as `mock` so the sweep asserts the refusal).

### 6.2 Boosted-underlying — GO as a rate-source sub-case, NO-GO as a family

- Reuse `RegisterBalancerStablePoolParams::rate_provider`'s trait for linear: a `rate_provider: Option<Arc<dyn BalancerRateProvider>>`
  field on the linear params, populated by the builder when the wrapped token is not a plain aToken.
- Nested-BPT wrapper: the provider reads the nested pool's virtual price (`getRate()`/
  `getVirtualSupply`+balances), which is exactly what the stable arm's `getRate()` path already does.
- *First red test:* `build_balancer_linear_refuses_an_unreadable_wrapped_rate` — `getWrappedTokenRate()`
  reverts ⇒ `PoolBuilderError::Spec(SpecRefusal::BalancerRateProviderUnreadable)`, never a
  silently-frozen rate (mirror of the stable rate-provider refusal already in `builder.rs`).

### 6.3 Element / Sense / adapters — NO-GO; sweep vocabulary only

No builder, no math, no state. The only change worth making is §6.0(3): give them their own corpus
buckets so the refusal message stops being the whole story. Cost: one re-record, two dicts.

### 6.4 Custom (fx, VPT, rate-anchored, no-name) — NO-GO

Same as 6.3. Keep `SpecRefusal::BalancerUnmodeledFamily` as the refusal; track the `rate-anchored`
vendor (7 pools registered 0–13 days before the pin) as a watch item only.

---

## 7. Reproduction of every measurement

Node and pin:

```bash
cast chain-id --rpc-url $RPC                                        # 1
cast client --rpc-url $RPC                                          # reth/v2.11.0-b9abbcc/…
cast block 26204126 --rpc-url $RPC | head -3                        # hash 0x13207898…, ts 1792142039
```

Corpus + census (offline):

```bash
python3 - <<'PY'
import json, collections
d=json.load(open("tests/balancer/data/balancer_v2_pool_registrations_26204126.json"))
print(d["pin_block"], d["census"], d["duplicate_registrations"], len(d["pools"]))
print(collections.Counter(r["family"] for r in d["pools"]))
PY
```

Per-row selector/interface/log census: `/tmp/probe_unmodeled.py` (29 selectors × 173 pools + Vault reads),
`/tmp/probe2.py` (code selector census, 234 token metadata reads, two `eth_getLogs` windows, block
timestamps), `/tmp/ana3.py`…`/tmp/ana7.py` (aggregation), `/tmp/gen_appendix.py` (Appendix A).
Every `eth_call` used `"params": [{"to": …, "data": …}, "0x18fd81e"]`; every log query used
`{"address": "0xBA12222222228d8Ba445958a75a0704d566BF2C8", "topics": [[<swap>,<pbc>], [<173 pool ids>]],
"fromBlock": …, "toBlock": …}`.

The two Solidity facts that a future reader will want to re-check are the *Vault* answers, not the pool's:
`getPoolTokens` (token order/balances/`lastChangeBlock`) and `queryBatchSwap` (whether the Vault can
actually route a swap to the pool). Both are one `cast call` each and both are decisive.

---

## 8. Go / No-Go and modeling cost

### 8.1 Verdict

| item | verdict | why (measured) |
|---|---|---|
| **Balancer refresh clock** (`refresh_balancer`) | **GO — do first** | `Bot.update(pool)` with no block is a silent no-op for every weighted/stable pool (§3.2); small fix, one red test |
| **Graph admission decision** (`PoolKind`) | **GO — decide** | no Balancer family reaches `find_paths`/backrun DFS; a Balancer `pools.kind` row makes the whole graph read fail (§2.3) |
| **Sweep vocabulary split** | **GO — cheap** | 173 rows are 9 real groups; the closed vocabulary hides it (§4.2) |
| **Linear family** | **CONDITIONAL GO** | 48 pools, real Balancer library exists, 26M USDC in one pool — but 0 swaps in 14 days, half the book one-sided, price = rate, and no graph edge to trade on |
| **Boosted-underlying** | **GO as rate variant** / **NO-GO as family** | the wrapper is a rate, not a curve; nested-BPT is 1 row |
| **Element** (+ adapters) | **NO-GO** | 0 swaps in 500k blocks; recent activity is 49 joins/exits; live swap is exactly 1:1 on a matured tranche |
| **Sense** | **NO-GO** | 1 swap and 1 join/exit in 500k blocks across 25 pools |
| **fx-custom** | **NO-GO** | 3 swaps in 14 days, all in one pool; 5/14 zero-balance; vendor unidentifiable from chain metadata |
| **VPT** | **NO-GO** | dead since block 21.1M (~1.4 years); no events |
| **rate-anchored** | **NO-GO (watch)** | brand-new (0–13 days) but 1-wei balances and a private error ABI |
| **mocks / no-name** | **NO-GO, must stay refused** | `DO NOT USE` mocks and never-initialized linear pools; they implement the full interface, so a family probe must keep naming them |

Bottom line for the track: **the unmodeled bucket is not where the missing alpha is.** 21 swaps in ~70
days across 173 pools, 17 of them with zero balances, and no graph edge for any Balancer pool. The
highest-value Balancer work in this scope is the state-sync clock and the graph decision, not another
family.

### 8.2 Modeling cost (estimates, not measurements)

| family | new Rust | builder reads | math | resolver/hop | sweep/Python | total estimate |
|---|---|---|---|---|---|---|
| Linear | state slice ≈250 LoC, family module ≈300, registry/route/door ≈120 | 9 getters + Vault read | `linear_math.rs` ≈400 LoC + property tests | index-mapped pair (new concept: tradeable pair ≠ `tokens[0..2]`) | `BalancerV2LinearPool` companion ≈250 LoC, corpus re-record | **≈5–7 engineer-days** + 2 fork pins; +2 days if rates must be live per block |
| Boosted-underlying | 0 (field + trait reuse) | 1 optional read + provider | 0 | 0 | rate-provider test | **≈0.5 day** |
| Element | full vendor integration | vendor getters only | bespoke curve (unpublished) | bespoke pair | companion + pins | **≈5–8 days for zero measured volume** |
| Sense | as Element | series-state-dependent getters | bespoke yield-space curve | bespoke | companion + pins | **≈4–6 days, 1 swap/70 days** |
| Custom (each vendor) | as Element | vendor getters | bespoke | bespoke | companion | **≥3 days each, no evidence of flow** |
| Cross-cutting 6.0(1) | ~10 LoC + test | — | — | — | — | **≈2 hours** |
| Cross-cutting 6.0(2) | `PoolKind` variant + 3 maps + DB read | — | — | one `HopType` per Balancer family | — | **≈1–2 days** (design decision dominates) |
| Cross-cutting 6.0(3) | 0 | — | — | — | 2 dicts + re-record (~10 min at the node) | **≈2 hours** |

### 8.3 What would change a NO-GO

- A linear pool that actually trades its wrapped side (a non-zero wrapped balance **and** ≥100 swaps per
  14 days). Today: 0 swaps in 14 days across 48 pools, 8 in the 56 days before, several one-sided.
- A graph admission (`PoolKind`) for Balancer, which is the real gate on any Balancer arb value.
- For the `rate-anchored` vendor: any pool with more than dust balances and sustained flow across
  releases (currently 1 wei/token and a 2-week history).

---



**Track C verdict.** GO on the two cheap cross-cutting fixes (the Balancer refresh clock, the sweep
vocabulary split) and on making Balancer's graph admission an explicit decision; CONDITIONAL GO on the
linear family, only after that admission and only with a live rate provider; NO-GO on Element, Sense, the
adapters, the FX/VPT/rate-anchored custom pools, and on treating boosted-underlying as a family. The
unmodeled bucket holds 8.9% of the registered corpus and 21 swaps in ~70 days — it is not where the
missing alpha is.
