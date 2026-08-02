# Exploration: The `no-profit` Backrun-Bot Crash (path 13308)

Status: **RESOLVED + FIXED** — root cause confirmed and both a defensive
diagnostic gate and a production decoder fix implemented. Last updated 2026-08-02.

## Symptom

The backrun bot (V3-V4-V3 arbitrage) crashes out of its drain loop with a
"no-profit" failure when run with `DEGENBOT_SIM_EXIT_ON_FAIL=1`:

```
no-profit -> the block stops / pump exits
```

The solver predicts a **positive** profit for a path, the simulator/executor is
asked to run it against on-chain state, and the result is a **loss** (or
sub-threshold net), so the optimistic path is rejected and the pump bails.

The canonical failing candidate is **path_id 13308** at **block 25664704**:
a V3-V4-V3 path. The solver recorded:

- `optimal_input = 1982369771046931` wei WETH (≈ `1.982e15` ≈ `1.98e-3` WETH)
- `hop_outputs = [3720117117094320378, 3719677, 1982489173871955]`
  (≈ `3.72e18` DAI, `3.72e6` USDC, `1.98e15` WETH)
- recorded solver profit ≈ **+1.19e11 wei WETH ≈ +1.19e-7 WETH** (≈ $0.0004)

Executing the same plan on-chain nets **−2.58e12 wei WETH** (gross) — a sign
flip on a sub-penny amount. Both are far below any gas cost, so the path is, in
practice, genuinely unprofitable. The mystery is *why the solver manufactured a
`+` that does not reproduce.*

## Environment

- Ethereum mainnet, local archive node `http://host.containers.internal:8545`
- Bot launched via `run_bot.sh` (`DEGENBOT_SIM_EXIT_ON_FAIL=1`,
  solver-state gate always on)
- DB at `~/.config/degenbot/degenbot.db` (snapshots of pool state)
- Process on TMUX pane; crash captured from pane scrollback

## The path (block 25664704)

| hop | kind | pool | tokens | fee | zfo | solver-log `[solver-st]` |
|-----|------|------|--------|-----|-----|--------------------------|
| 0 | V3 | `0x60594a405d53811d3bc4766596efd80fd545a270` | DAI/WETH | 500 (uniswap_v3) | false | `sq=1828452401381250048945819760 liq=13801775473645881022537 fee=5` |
| 1 | V4 | manager `0x000000000004444c5dc75cb358380d2e3de08a90`, pool `0xd967702f…34be` | DAI/USDC | 100, proto 102425 | true | `sq=79230384235196890462207 liq=75348420803185805 fee=2` |
| 2 | V3 | `0x1ac1a8feaaea1900c4166deeed0c11cc10669d36` | USDC/WETH | 500 (pancakeswap_v3) | true | `sq=1829540213027809537560229722251387 liq=19254855557107419 fee=5` |

The `[solver-st]` line is the solver's actual per-hop input state
(`[solver-st] path_id=13308 hops=[...]`), captured from the tmux scrollback.

## Verified facts (already established)

### Mutable state (sqrt/tick/liquidity) — verified at block 25664704
Read via on-chain oracles:
- V3: `slot0()` + `liquidity()` on the pool contract.
- V4: StateView `getSlot0(bytes32)` + `getLiquidity(bytes32)`.

Results vs the solver's logged input: **hop0 and hop1 match byte-for-byte**
(both sqrt AND liquidity). **hop2 (v3_2) sqrt differs by ~0.07%**:
my on-chain read `1828292228524991058860736046727168` vs solver-log
`1829540213027809537560229722251387` (liquidity matches). This is the ONLY
scalar that does not match the solver's own recorded input.

### Liquidity tick mapping — verified at block 25664704
`scripts/verify_path13308_tickmap.py` checks per-tick + completeness:
- v3_0: 154/154 ticks byte-identical (gross+net), 22 bitmap words, **0 missing,
  0 extra**.
- v3_2: 351/351 identical, 14 words, **0 missing, 0 extra**.
- v4: 12/12 identical; active word (-1080) 5 populated, **0 missing**.

Mapping is byte-for-byte on-chain reality. (Had a false "extra bits" alarm from
wrong bitmap indexing: word is `(tick/tickSpacing) >> 8`, division truncates
toward zero — NOT `tick >> 8`.)

### DB liquidity snapshot currency
- No V3 Mint/Burn events in the backfill window.
- V4 pool's last `ModifyLiquidity` is exactly the DB `liquidity_update_block`
  `25610799`, none follow.
- So all three DB tick maps were already current at the solver block.

## The core discrepancy

`rust/crates/degenbot/examples/path13308_solver_fixture.rs` reconstructs the
three pools from the fixture JSON and runs the production solver via
`ArbitrageEngine::register_and_solve_path`. It returns **`None`
(no profitable input)** — it does NOT reproduce the recorded
`optimal_input=1.982e15`.

This is true with:
- the fixture v3_2 sqrt (`182829…`), and
- the solver-log v3_2 sqrt (`182954…`).

(NOTE: the initial v3_2-sqrt override test had a bug — the env override was
applied to BOTH V3 registers, corrupting hop0. Must be fixed and re-run.)

## Hypotheses (ranked)

1. **Stale third-leg state manufactured a phantom micro-profit.**
   The bot's `V3PoolState` for `0x1ac1a8fe` held sqrt `182954…` at solve time,
   which does NOT match on-chain at block 25664704 (`182829…`). The solver is
   exquisitely sensitive to this ~0.07% on a 0.05% pool; with the *held*
   (stale) state it finds +1.19e-7 WETH; with the *on-chain* state it finds
   nothing/negative. Executing against real on-chain state nets −2.58e-6 WETH
   → no-profit. **Prime suspect.** Need to re-run with a FIXED override that
   applies ONLY to v3_2, and confirm it reproduces +1.19e-7 (validating this
   causal chain), or conclusively rule it out.
2. **Path-construction / token-routing divergence.** My standalone
   `register_and_solve_path` may build a different edge/routing than the bot's
   resolved path (input token, edge token-out, direction flags).
3. **Fee-unit mismatch.** `[solver-st]` logs `fee=5` (V3) / `fee=2` (V4);
   my params pass raw `fees` (500/100). If the solver expects a different unit
   (e.g. /100), both my runs used a 100× fee → could kill the arb. The
   recorded result came from the correct fee unit. MUST audit the solver's fee
   semantics.
4. **Rounding artifact in `register_and_solve_path` insertion.** It only inserts
   a result when `profit != 0`; the reconstructed solve may compute profit that
   rounds to exactly 0 in my state → `None`, while the bot's (stale v3_2) state
   computed +1.19e-7. Same root as #1.
5. **Solver-state gate / simulator divergence.** The sign flip (+1.19e-7
   predicted vs −2.58e-6 executed) may be a simulator-vs-solver fee/rounding
   mismatch rather than a stale state. Needs simulator-side tracing.

## Reproduction artifacts

- `scripts/capture_path_13308_fixture.py` — rebuild the fixture from DB + node.
- `tests/fixtures/path13308_v3v4v3_block25664704.json` — the captured pool state.
- `scripts/verify_path13308_tickmap.py` — tick-mapping verification.
- `rust/crates/degenbot/examples/path13308_solver_fixture.rs` — solver runner.

## Next steps (autonomous)

1. Fix the v3_2-sqrt override bug (apply only to hop2); re-run the solver with
   the solver-log v3_2 sqrt; check whether it reproduces the recorded
   `optimal_input`/`hop_outputs` exactly.
2. Audit the solver's fee-unit semantics (log `fee=5`/`fee=2` vs param raw
   `500`/`100`).
3. Add tracing inside `solve_path` (degenbot-solvers/mixed) to dump, per hop:
   sqrt, tick, liquidity, fee, amount_in/out, and whether profit is
   zero/negative — even when the result is not inserted.
4. Dump the resolved path edges (input token, route) for the reconstructed path
   vs what the bot resolved.
5. Simulator-side tracing: reconcile the recorded hop_outputs with a
   state-simulation from the verified fixture to find where the +1.19e-7
   predicted becomes −2.58e-6 executed.

## RESOLUTION (root cause confirmed)

Re-running the solver with the solver-log (stale) v3_2 state replicates the
recorded solve EXACTLY:
- optimal_input = 1982369771046931 (match)
- hop_outputs = [3720117117094320378, 3719677, 1982489173871955] (match)
- profit = 119402825024 (~+1.19e11)

With the VERIFIED on-chain v3_2 state (sqrt 182829…, tick 200941) the solver
returns None (no arbitrage).

**Root cause: phantom micro-profit from a STALE third-leg pool state.** The bot's
V3PoolState for the PancakeSwap V3 pool 0x1ac1a8fe (USDC/WETH) held sqrt/tick
that diverged from on-chain at solve time by ~0.07% (~9 ticks, tick 200941 vs
200950). On-chain v3_2 price was actively drifting:
- block 25664500: tick 200952
- block 25664600: tick 200943
- block 25664700-25664704: tick 200941
- solver held: tick 200950 (≈ state at ~25664550)

So the pool state lagged on-chain by ~100 blocks (~20 min). Under the stale
state the solver manufactures a +1.19e-7 WETH arb; under true on-chain state
none exists; executing the phantom plan nets -2.58e-6 WETH -> no-profit crash.

### Why wasn't it caught?
The existing solver-state accuracy gate (AV42C7) verifies V4 pool states via the
StateView contract. V3 pools have no StateView analog; the gate does not appear
to double-check V3 sqrt/tick against a fresh on-chain slot0() read. So a stale
V3 hop state slides through, and the solver happily solves against a phantom
price. (TO CONFIRM in solve_dispatch / solver-state gate code.)

**UPDATED FINDING (depth): the anchor diff blesses honest-but-stale states.** The
gate (`verify_solver_hop_states`) DOES verify V3 (`fetch_v3_slot0_liquidity`), but
it diffs each hop at the hop's OWN `update_block` ANCHOR — deliberately
tolerating 1-2 blocks of normal latency. A pool that is honest AT ITS OWN
(old) anchor passes, even if on-chain moved past it by 100 blocks. So a frozen
snapshot (missed swaps) sails through the exact-anchor check. This is the gap.

## ROOT-CAUSE MECHANISM (confirmed)

The PancakeSwap V3 pool 0x1ac1a8fe emits Swap events with a NON-canonical
topic0: `0x19b47279256b2a23a1665c810c8d55a1758940ee09377d4f8d26497a3577dc83`
(canonical Uniswap V3 Swap is `0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67`).
degenbot's V3 swap decoder matches ONLY `0xc42079f9…` (V3_SWAP_TOPIC), and there
IS NO PancakeSwap-family decoder (rg over decoders/pools/bot: zero hits). So the
pump never decodes these pools' swaps -> their sqrt/tick freeze -> on-chain drifts
away. The solver then solves against the frozen price -> phantom +1.19e-7 WETH
arb -> executing against true on-chain nets -2.58e-6 WETH -> no-profit crash.

Evidence: v3_2 on-chain tick slid 201057 (seed 25609605) -> 200941 (25664704)
while the solver held 200950 (~state at ~25664520). The swap log at the drift
carries amount0/amount1/sqrt/liq/tick (tick `0x310fb`=200955) with topic0
0x19b47279 and 7 data words. The ABI is CONFIRMED against the verified
`PancakeV3Pool.sol` source (Etherscan): the trailing two words are
`uint128 protocolFeesToken0` (recorded 0) / `uint128 protocolFeesToken1`
(recorded ~1.2e13..1.6e13) — PancakeSwap replaced Uniswap's `uint24 fee` with
these two protocol-fee accumulators, changing the signature to
`Swap(address,address,int256,int256,uint160,uint128,int24,uint128,uint128)`
= `0x19b47279…`. The decoder's five state words remain byte-identical to V3.

## FIX IMPLEMENTED (defensive; diagnostic-gate)

`solver_state_verifier.rs` now closes the anchor-gap: for a CL hop whose
`update_block` trails the solve block by more than `MAX_CL_STALENESS_BLOCKS`
(=3), it takes a FRESH solve-block read and, if it still diverges, reports a
stale/desynced hop. Combined signals avoid false positives:
- quiet-but-correct pool (chain unchanged): solve-block state == solver state,
  not flagged even if update_block is old;
- 1-2 block in-progress fastcase: below the 3-block threshold, not flagged;
- missed-swap stale pool (PancakeSwap case): old update_block AND solve-block
  divergence -> flagged -> the AV42C7 assert-gate panics with a precise
  "STALE at solve block" message instead of the opaque no-profit.

## PRODUCTION FIX IMPLEMENTED (root — PancakeSwap swaps now decoded)

- `v3_pancakeswap_swap_decoder.rs` (degenbot-decoders): decodes the PancakeSwap
  V3 Swap (topic0 `0x19b47279…`; state fields byte-identical to Uniswap V3, two
  extra trailing fee-accounting words). Verified against a REAL on-chain log at
  block 25655667 (sqrt/liq/tick match `slot0()`/`liquidity()` exactly); 3 unit
  tests incl. the real-log decode.
- `block_pump.rs`: added `V3_PANCAKESWAP_SWAP_TOPIC` to `RELEVANT_TOPICS`
  (6 -> 7), so the WS + backfill pre-filter no longer drops PancakeSwap swaps.
- `bot_core/mod.rs` `process_backfill_logs`: added the PancakeSwap topic0 arm,
  feeding the same `apply_v3_swap` as canonical V3.
- `log_dispatcher.rs`: registered a `V3PancakeSwapDecoder` beside `V3SwapDecoder`.

So PancakeSwap-V3-family pools stay LIVE (their swaps are applied), eliminating
the stale-state phantom-arb class at the source instead of only surfacing it.

## Unit tests added
- fresh_pool_is_not_stale, stale_pool_is_flagged_after_threshold (verifier).
- decode_real_pancake_swap / wrong_topic / truncated_data (decoder).
Validate: degenbot-bot 314 tests pass; decoders 109 pass; clippy + fmt clean;
full workspace (lib+tests+examples) builds.

## FOLLOW-ON: reproducible `empty`-bucket V3-V4-V3 sim-Halt (UNI/MATIC V4)

### Symptom (deterministic)
A DIFFERENT, non-PancakeSwap crash surfaced via the `DEGENBOT_SIM_EXIT_ON_FAIL=1`
trap (W2UWZO / DEGENBOT-459) and reproduced on a second independent run at a
different live block (25668480 then 25669640):

```
[sim-fail] path=22398 type=V3-V4-V3 bucket=empty
  revert@depth=8 target=0x0000…4A90 (PoolManager) sel=0x00000000
  label=empty kind=halt gas=~4328k swaps_before=0 revert=0x
[sim-trap] exiting on first sim failure … (DEGENBOT_SIM_EXIT_ON_FAIL=1)
```

- path = `V3(MATIC/WETH 0x290A, 0.3%) → V4(UNI/MATIC 0x929b9b09…c2d40) →
  V3(UNI/WETH 0x360b, 1%)`, route WETH→MATIC→UNI→WETH.
- Reverting frame = the V4 `PoolManager`, call depth 8, EMPTY calldata,
  `kind=halt` (an EVM Halt: INVALID/OOG — not a clean `Error(string)`), 0
  captured swaps. Bucket `empty` is NOT in the trap ignore-set (which only
  holds `CurrencyNotSettled`), so it traps by design.

### What was verified (the conservative checks pass — NOT stale state)
- `[debug-v4-solve]` for `0x929b9b09` (engine's own dump): `protocol_fee=102425`,
  `coverage=Tracked`, `n_ranges=1`, `drain=0`, `zero_for_one=false`, and
  `sqrt_price_x96`/`tick`/`liquidity` match on-chain EXACTLY.
- Raw `slot0` read straight from PoolManager storage at `S_state`:
  `sqrtPriceX96=457034773347195373970576742286`, `tick=35050`,
  `lpFee=100`, `protocolFee=0x19019` → 25 pips ​/direction.
- On-chain effective swap fee = `calculateSwapFee(25, 100)` = **125 pips**
  (0.0125%). The solver models the SAME (its `pool_key.fee=100`, threaded
  through `calculate_swap_fee` incl. the protocol fee: 25+100−0 = 125).
  **The fee is consistent — there is NO fee-model bug.**

### Correction: the `fee_bps=2` `[solver-st]` display is a ROUNDING artifact
An earlier read concluded the solver over-charged (200 vs 125 pips). That was
WRONG: `fee_bps = 10000 − (1_000_000 − swap_fee) / 100` (integer division)
yields 2 for EVERY `swap_fee` in [101, 200], so `fee_bps=2` is fully
consistent with the true 125 pips. Pinned by a new unit test
(`calculate_swap_fee_uni_matic_929b9b09_pool_fixture_125_pips` in
`degenbot-cl-math`): assert `calculate_swap_fee(25, 100) == 125` + the
`fee_bps=2` rounding. The RZKFKR protocol-fee threading fix IS present and
correct here.

### What remains OPEN (the real lead)
With state exact AND fee consistent, the `Halt` (empty-calldata call into the
PoolManager at depth 8, `swaps_before=0`) is a STRUCTURAL/executor-execution
divergence, not a solver-fee or stale-state one. This is the known W2UWZO /
tier-3-oracle open item (root `2LTKVO`): reproduce the executor's exact
V3→V4→V3 call through real PoolManager bytecode and attribute the reverting
frame. Until root-caused, the `empty` bucket should NOT be added to the trap
ignore-set (it signals a genuine divergence, not a thin-margin false positive).

#### Executor source located + structural hypotheses ruled OUT (HRT357)
The canonical executor source IS available: `/workspaces/executor`, written in
**Vyper 0.5.0a3** — `contracts/cmd_executor.vy` (2077 lines, command-stream
VM), with `contracts/recompile.py` producing the shipped runtime bytecode by
injecting the 5 mainnet immutables (owner, WETH, POOL_MANAGER, WETH/NATIVE
delta slots) after the CBOR metadata. This is the same contract the simulator
injects (`INJECT_EXECUTOR_CODE`).

The V3→V4→V3 command stream (`three_hop_v3_v4_v3` in
`degenbot-executor/src/composers.rs`) nests the V4 hop deep in the callback
chain: top `V3_SWAP` on `v3c` (UNI/WETH) → callback runs `V3_SWAP` on `v3a`
(MATIC/WETH) → its callback runs `ERC20_TRANSFER(weth→v3a)` + `V4_UNLOCK` →
`unlockCallback` runs `[V4_SETTLE, V4_SWAP_DYNAMIC(UNI/MATIC), V4_TAKE_COMPACT,
V4_SETTLE_ALL]` → `poolManager.swap`. `V4_SWAP_DYNAMIC` reads the input
delta via `exttload` and calls `swap` with `amountSpecified = −delta` and
`sqrtPriceLimitX96 = MIN/MAX_SQRT_PRICE` — the extreme bounds.

Cross-checking the executor against the **real mainnet** verified source
(`PoolManager v0.8.26`, fetched via Etherscan) RULED OUT every structural
mismatch hypothesis:

1. **Callback name** (`unlockCallback` vs `lockAcquired`) — the deployed
   mainnet interface is `IUnlockCallback.unlockCallback`; the executor's
   `unlockCallback(bytes) → bytes` MATCHES. The project's *fake* PM also calls
   `unlockCallback` (line 623 of `fake_uniswap_v4_pool_manager.vy`), so this
   was never a divergence.
2. **`exttload`** (the delta reader `_read_pm_delta` uses) — present on
   mainnet (`src/Exttload.sol`).
3. **`IPoolManager` ABI** — the executor's `interfaces/UniswapV4/IPoolManager.vyi`
   (`swap(PoolKey,SwapParams,bytes)`, `settle() payable`, `sync`, `take`,
   `PoolKey{currency0,currency1,fee:uint24,tick_spacing:int24,hooks}`,
   `SwapParams{zero_for_one,amount_specified,sqrt_price_limit_x96}`) matches
the v0.8.26 ABI exactly — no wrong-selector/fallback path.
4. **Fee model** — consistent (125 pips; see correction above).
5. **Pool-key encoding** — `c0=UNI, c1=MATIC, fee=100, ts=1, hooks=0` routes
to the real `0x929b9b09` pool.

So the executor is **structurally compatible** with the deployed mainnet PM;
the bug is a subtle runtime flow issue (delta/sync-settle ordering, or a
native/WETH custody detail in the real PM's stricter accounting). The
remaining step — and the actual `2LTKVO` slice — is the **revm replay with a
Vyper source-map**: compile `cmd_executor.vy` with `bytecode_runtime` +
`source_map`, seed the 3 pools + canonical PM in revm, drive the live command
stream, and map the depth-8 empty-calldata `Halt` PC back to a Vyper line.

## Key numbers (for quick reference)
- recorded `optimal_input` = 1982369771046931
- recorded `hop_outputs` = [3720117117094320378, 3719677, 1982489173871955]
- predicted profit ≈ +1.19e11 wei WETH; executed gross ≈ −2.58e12 wei WETH
- solver-log v3_2 sqrt = 1829540213027809537560229722251387
- on-chain v3_2 sqrt @25664704 = 1828292228524991058860736046727168
