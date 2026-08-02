# Exploration: The `no-profit` Backrun-Bot Crash (path 13308)

Status: **ACTIVE INVESTIGATION** — last updated 2026-08-02

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
0x19b47279 and 7 data words. ABI brute-force (int128/int256, 6/7 fields) did not
resolve the exact signature; the trailing two data words (0, ~1.2e13) suggest an
extra event param beyond Uniswap's Swap. Follow-on task: reverse + decode this
ABI so PancakeSwap-V3-family pools become live.

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

Unit tests added (fresh_pool_is_not_stale, stale_pool_is_flagged_after_threshold;
6 pass). This surfaces the desync loudly at runtime. It does NOT keep the bot
running in production (gate is off by default; still optionally panics). The
PRODUCTION fix is the PancakeSwap-V3 swap decoder (root) and/or refreshing stale
CL pool scalars from on-chain immediately before solving.

## Key numbers (for quick reference)
- recorded `optimal_input` = 1982369771046931
- recorded `hop_outputs` = [3720117117094320378, 3719677, 1982489173871955]
- predicted profit ≈ +1.19e11 wei WETH; executed gross ≈ −2.58e12 wei WETH
- solver-log v3_2 sqrt = 1829540213027809537560229722251387
- on-chain v3_2 sqrt @25664704 = 1828292228524991058860736046727168
