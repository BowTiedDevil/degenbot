# Investigation — no-profit crash, path 142603 (V4-V4-V3), block 25723658

Status: **ROOT-CAUSED (verified by replay)** · opened 2026-08-10 · artifacts:
`scripts/capture_path142603_v4v4v3_fixture.py` +
`rust/crates/degenbot/examples/path142603_v4v4v3_solver_fixture.rs` +
`tests/fixtures/path142603_v4v4v3_block25723658.json`.

## Symptom
Live backrun bot (`examples/eth_backrun_v2_v3_v4_rust.py`, `DEGENBOT_SIM_EXIT_ON_FAIL=1`)
aborted its drain loop at block **25,723,658** on the `no-profit` sim bucket:

```
[sim-fail]    path=142603 type=V4-V4-V3 bucket=no-profit hop_outputs=[676293,676607,351475872056229]
[sim-bals]    path=142603 weth 10e18->9.999999999826849175 (d=-173150825)  → net LOSS
[sim-diag]    {"path_id":142603,...,"revert_info":"no-profit","optimal_input":351476045207054,
              "hop_outputs":[676293,676607,351475872056229]}
[sim-trap]    exiting on first sim failure at block=25723658 (DEGENBOT_SIM_EXIT_ON_FAIL=1)
```

The solver **selected** this path (it reported a positive profit) but the revm
sim executed it to a net **−173,150,825 wei WETH** (`gross_profit == 0` →
`no-profit` bucket). This is the intended conservative tripwire firing — it
surfaced a genuine solver/execution profit divergence.

## Path identity (from the live `[sim-fixture]`)
| hop | family | pool | tokens | fee | zfo | solver `[solver-st]` @09:20:28.46 |
|-----|--------|------|--------|-----|-----|-----------------------------------|
| 0 | V4 | `0x4f88f7c99022…b224a7` | USDC/WETH | 500 | false | `sq=1805608580658710341842839157274073 liq=5280893848216399` |
| 1 | V4 | `0x3ad280c97568…f30b5` | USDC/USDT | 500 | true | `sq=79271439270369815651912831461 liq=1018741430873` |
| 2 | V3 | `0xc7bBeC68d12a…b0e9b` | WETH/USDT | 100 | false | `sq=3475987689862807899316370 liq=239043320771726009` |

Recorded solve: `optimal_input = 351476045207054`,
`hop_outputs = [676293, 676607, 351475872056229]`.

## Verified on-chain ground truth (archive RPC @25723658)
Captured by the state scraper; confirmed against `cast`/StateView:

- **Pool A (V4 USDC/WETH):** `sq=1805608580658710341842839157274073`,
  `liq=5280893848216399` → **matches solver byte-for-byte** (price **and** liquidity).
- **Pool C (V3 WETH/USDT):** `sq=3475987689862807899316370`,
  `liq=239043320771726009` → **matches solver byte-for-byte**.
- **Pool B (V4 USDC/USDT):** `sq=79271439270369815651912831461`, `tick=10` →
  **matches solver**, but `getLiquidity = 718_152_690_765` whereas the solver
  held `1_018_741_430_873`. Pool B's liquidity dropped ~3.05e11 at
  **block ~25720300** (a ModifyLiquidity removal); the solver's liquidity clock
  is **frozen ~3,300 blocks behind** (its `tick_data_block = 25722568`,
  `pump_count=0` — the same "dropped-from-live-pump" marker from the earlier
  post-backfill investigation).

So two distinct observations: (1) a **staged-clock liquidity desync on pool B**
(price honest, liquidity stale), and (2) the solve reporting **positive profit
on a path that nets a WETH loss**. The replay below shows (1) is a real but
**separate** latent defect; **(2) is the cause of this no-profit**.

## Replay (the discriminating experiment)
`path142603_v4v4v3_solver_fixture.rs` reconstructs the three pools from the
fixture (DB tick_data + on-chain scalars at 25723658) and runs the production
Möbius solver, with `FIXTURE_V4_B_LIQ` to swap pool B's liquidity.

**Run 1 — on-chain-correct pool B liq (718152690765, the fixture default):**
```
optimal_input (recomputed): 351476045207054   == recorded  ✓
hop_outputs  (recomputed):  [676293,676607,351475872056229]  == recorded  ✓
profit       (recomputed):  346369630          (POSITIVE, on a round-trip LOSS)
per-hop oracle: hop0 true, hop1 true, hop2 true  (solver == twin everywhere)
```
**Run 2 — stale pool B liq (1018741430873, what the solver held):**
```
optimal_input: 254233491245024  hop_outputs:[489184,489411,254233590038312]  profit: 618313762
=> DIFFERS from the recorded solve
```

**Interpretation.** Run 1 reproduces the recorded solve byte-for-byte on the
on-chain-honest state; Run 2 (stale liq) gives a *different* solve. Therefore
**pool B's stale liquidity is NOT the cause of this no-profit** — it is a
real, independent defect, but the recorded phantom solve is a faithful function
of the correct liquidity. The discriminating variable is elsewhere.

## The key arithmetic (the actual root cause)
The recorded solve returns `hop_outputs[2] = 351475872056229` WETH on input
`351476045207054` WETH → the round trip nets **−173,150,825 wei WETH** (a loss,
matching the executed `[sim-bals]`). Yet the solver reports
`profit = +346,369,630`. The gap is:

```
+346,369,630  (solver-reported profit)
− (−173,150,825) (true round-trip WETH delta)
= 519,520,455   ← EXACTLY the live [clamp-cl-hop] delta on hop2
```

The live log recorded, for this path at block 25723658:
```
[clamp-cl-hop] path_id=142603 hop=2 family=V3 hop_outputs=351476391576684
               twin_out=351475872056229 delta=519520455
```

So the solver's **V3 hop2 (USDT→WETH) raw output over-predicted the byte-exact
twin by 519,520,455 wei**. The solver computes
`profit = final_output − input` (`mobius_v3_int.rs`), i.e.
`351476391576684 − 351476045207054 = +346,369,630`, on the **over-predicted**
`final_output`. The post-solve CL clamp (`clamp_cl_hop_capacity`,
`solver_dispatch.rs`) realigns `hop_outputs`/`consumed_inputs` to the twin and
logs `[clamp-cl-hop]`, **but does not recompute `profit`**. The poisoned
positive profit remains the filter/threshold signal, so a path that is
genuinely unprofitable after the clamp is still selected and dispatched → the
sim/executor correctly realizes the honest (twin) output → net loss → `no-profit`
→ `DEGENBOT_SIM_EXIT_ON_FAIL` abort.

This is the same family as path-73385 (solver CL output over-prediction
corrected by the clamp), but at a **~5.2e8 wei** magnitude (still sub-basis-point
on a 0.01% pool) and with the selection-profit not realigned by the clamp.

## Hypotheses (ranked, verdicts)
1. **H1 — pool B stale liquidity manufactured the phantom profit.** *NOT
   established as the cause; the replay cannot cleanly discriminate the
   liquidity dimension.* Run 1 (correct liq) reproduces the recorded optinal,
   run 2 (stale liq) diverges — BUT both reconstructions use the DB's sparse
   8-tick V4 map for pool B rather than the live solver's map, so the
   run-1-vs-run-2 difference may be tick-map fidelity, not purely the liquidity
   scalar. What IS established: the live solver's `[solver-st]` for path 142603
   consistently shows pool B `liq=1018741430873` (stale) across 82 near-crash
   lines while on-chain is `718152690765`, and the solve-time gate's
   `skip_in_progress_hop` skips that hop without checking its liquidity clock.
   So pool-B stale liquidity is a real observed defect (staged-clock, gate-
   masked) whose causal role here is uncertain — independent of the Bug-B
   root cause, which stands regardless (run 2 also finds a phantom profit,
   `618313762`, that would equally be clamped to a loss).
2. **H2 — V3 hop2 output over-prediction poisons the selection profit.**
   *VALIDATED*: `profit = +346,369,630 = raw_hop2 − input` where raw_hop2
   over-predicts the twin by 519,520,455; executes to the twin → net loss.
   This is the root cause.
3. **H3 — sub-penny magnitude makes the path "genuinely unprofitable".** True
   (the round trip is a real loss) but *incomplete* — the bot *should* never
   have selected it; the selection signal was stale-by-clamp, not just tiny.
4. **H4 — executor/exact-out divergence.** *REFUTED*: the solver's own model
   already loses (raw final < input); execution faithfully realizes the twin
   loss. Not executor-caused.

## Secondary latent defect (verified, independent, do-not-conflate)
**Pool B (V4 USDC/USDT) staged-clock liquidity desync.** Price clock
(`update_block`) is on-chain-honest at the solve block, but the liquidity clock
(`tick_data_block=25722568`) is ~3,300 blocks stale — a ModifyLiquidity removal
at ~25720300 was not applied. The solve-time gate's `skip_in_progress_hop`
(`update_block >= block` → skip) short-circuits verification for exactly this
hop (its price clock reached the solve block via swap incorporation), so the
stale liquidity is never diffed. Would produce a separate phantom-profit class
on a pool whose price is quiet but whose float is stale. Not the cause of THIS
crash, but a real gate gap.

**ROOT MECHANISM FOUND + FIXED (Bug A).** CL pools have an *in-range*
`liquidity()` scalar (read on-chain as `liquidity()`) that a Mint / Burn /
`ModifyLiquidity` overlapping the current range must adjust **in place** — and
that adjustment is NOT emitted by the event (the event carries only the net
delta). The one shared, in-range-aware path is `apply_liquidity_update`
(`in_range_active_liquidity` + the `block_number > initial_state_block`
historical-replay guard). Audit of every update path:
- live V3/V4 apply (`apply_{v3,v4}_liquidity_update_by_pool_id`) → shared ✓
- buffered-drain apply (`apply_buffered_{v3,v4}_event`) → shared ✓
- Python FFI → shared ✓
- `merge_tick_word` → tick-bitmap merge (scalar-prefetched elsewhere) ✓
- `sync_{v3,v4}_pool_state` → full-state reset (caller supplies `liquidity`) ✓
- **backfill→Live branch (`buffer_backfill_{v3,v4}_liquidity_update`) — FIXED:
  a parallel inline `apply_liquidity_to_tick_range` + manual `tick_data_block`
  advance that NEVER adjusted the in-range `liquidity()` scalar (and never
  advanced the price clock / journaled).** A post-seed in-range event arriving
  via a late backfill chunk on a Live pool updated the tick map but left the
  active-liquidity scalar stale — exactly the observed pool-B signature
  (`tick_data_block` advanced, in-range `liquidity` frozen).

The fix routes both backfill→Live branches through the shared
`apply_liquidity_update` (single path — no shotgun surgery), which carries the
historical-replay guard, the in-range scalar adjust, the two-stamp clocks, and
reorg journaling in one place. Behavior is unchanged for the common pre-seed
backfill case; the post-seed case is now correct. Regression tests:
`backfill_live_in_range_post_seed_adjusts_in_range_liquidity` +
`backfill_live_in_range_pre_seed_does_not_adjust_scalar` (bot_core/mod.rs).
Note: this fixes the in-place scalar during backfill; the solve-time gate still
`skip_in_progress_hop`s untouched hops, so D3 (two-stamp check on the skip path)
was added below as a defensive follow-on guard.

## Diagnostics to validate / invalidate (proposed)
**D1 (validates H2, the fix) — IMPLEMENTED:** `clamp_cl_hop_capacity` now
recomputes `profit` from the twin-aligned `hop_outputs`/`consumed_inputs` via
the pure helper `recompute_clamped_profit` (`profit = final_output −
consumed_inputs[0]`, saturating; a post-clamp loss → `0` → dropped by the
`profit > min_profit` delivery gate). Units tests (`profit_clamp_recompute_tests`
in `solver_dispatch.rs`) cover the path-142603 case, genuine-profit preservation,
the `consumed_inputs[0]`-not-`optimal_input` semantics, and the degenerate
`None`. E2E: the replay harness now reports `profit = 0` for path 142603 (was
+346,369,630) → not selected → no `no-profit` trap.

**D2 (observability, H2):** env-gated per-selected-path trace logging
`(profit_before_clamp, profit_after_clamp, Σ clamp deltas)` — turns the silent
stale-profit into a grep-able `[profit-clamp]` line and exposes any residual
`profit_after > 0` on a path that clamps down. Also asserts the invariant
`profit_after ≤ Σ(hop i twin) − input` (no path positive after clamp).

**D3 (secondary pool-B class, H1 reasoning) — IMPLEMENTED.** The solve-time
verifier now runs a staged-clock probe on the `skip_in_progress_hop` path: for a
skipped CL hop (`update_block >= block`) with a completed, non-zero map anchor
(`tick_data_block < block`) and a pronounced price-ahead-of-map gap
(`update_block − tick_data_block > 3`), it reads on-chain `liquidity()` at the
**tick-data anchor** (`tick_data_block`) and WARNs `[staged-clock]` if the
solver's in-range `liquidity` scalar diverges — surfacing a fresh-price +
stale-in-range-liquidity desync instead of silently skipping it.
`DEGENBOT_TRACE_STAGED_CLOCK=1` opt-in, non-aborting (observational; the hard
abort is untouched). The tick-data anchor is immune to the mid-block-capture
ambiguity that motivates the skip (a completed block is reproducible).
Read-the-anchor is sound against benign swap-only pools: a healthy pool advances
both clocks together (`apply_swap` advances both), so its scalar matches on-chain
at `tick_data_block`. +3 unit tests (`staged_clock_candidate_*`, env gate);
440 degenbot-bot tests pass; clippy + fmt clean.

**D4 (reproduction):** run the delivered replay harness on any recurrence —
`FIXTURE_TARGET`, `FIXTURE_*_PID`, `FIXTURE_*_LIQ` env overrides + re-capture
via `scripts/capture_path142603_v4v4v3_fixture.py` — and check `profit_after`
versus the recorded solve.

## Artifacts
- `scripts/capture_path142603_v4v4v3_fixture.py` — state scraper (DB tick_data +
  on-chain scalars at TARGET).
- `tests/fixtures/path142603_v4v4v3_block25723658.json` — captured pool states
  + recorded solve.
- `rust/crates/degenbot/examples/path142603_v4v4v3_solver_fixture.rs` — replay
  harness (reproduces the recorded solve; `FIXTURE_V4_B_LIQ` probes the
  liquidity variable).

## Key numbers
- solve block 25723658 · path V4-V4-V3 (USDC/WETH → USDC/USDT → WETH/USDT)
- recorded `optimal_input` 351476045207054 · `hop_outputs` [676293, 676607, 351475872056229]
- solver profit **+346,369,630** · executed net **−173,150,825** · gap **519,520,455**
- live `[clamp-cl-hop]` hop2 raw 351476391576684 → twin 351475872056229 (delta 519520455)
- pool B on-chain liq 718_152_690_765 vs solver 1_018_741_430_873 (stale ~3300 blocks)
