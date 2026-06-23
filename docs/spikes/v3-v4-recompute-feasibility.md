# Spike: V3/V4 hop-output recompute feasibility

Outcome of `ergo` task `6AEEDJ` (epic `MHQZLL`). Decides how the
drift/solver-calc/encoding classifier recomputes a hop's expected output.

## TL;DR

- **V2 — full recompute, both states.** Trivial (`getAmountOut` from reserves); no
  tick map needed. Implement engine-state AND onchain-state recompute in task `NL2YY3`.
- **V3/V4 — engine-state recompute is IDENTITY (skip it).** The engine retains the
  tick map per pool, and the **only** swap-step math is `degenbot-cl-math`
  (`compute_swap_step_v3`/`v4` + `sqrt_price_math`), which is *the same math the
  solver ran*. Recomputing a hop from the engine state + the solver's `amount_in`
  reproduces `hop_outputs[i]` — it confirms internal consistency, nothing more. It
  cannot detect a solver-internal calc bug (the shared math would reproduce it).
- **V3/V4 — onchain-state recompute IS feasible but only detects drift, not
  solver-calc.** The onchain tick map is fetchable (the liquidity verifier
  `verify_v3_liquidity_map`/`verify_v4_liquidity_map` already does it via `tickBitmap`+
  `ticks` / `StateView.getTickLiquidity`), but the diagnostic snapshot retains only
  scalar state (`slot0`/`liquidity`). To recompute against onchain state the
  recompute must reuse the verifier's onchain tick-map fetch.
- **No independent V3/V4 swap math exists** anywhere (Python `degenbot` has no
  second implementation; `cl-math` is the single one, intended to match the contracts).
  So a recompute can never be a *mathematically independent* calc check — its value is
  running the same math against *different state* (drift detection), not against the
  solver's own state.

## Recommended approach (feeds tasks `4BKMKX` and `PCG2M3`)

1. **V2 (`NL2YY3`): ** `getAmountOut(amount_in, reserve_in, reserve_out, fee)` reusing
   the existing V2 library math (see `_v2_get_amount_out` in `cmd_executor.vy` for the
   reference formula; the engine has the equivalent). Populate
   `HopRecompute.expected_out_engine` (engine reserves), `expected_out_onchain`
   (onchain reserves), `matches_solver` (onchain recompute vs `solver_out`). This is
   the meaningful `SolverCalc` detector.

2. **V3/V4 (`4BKMKX`): ** Implement **onchain-state recompute only** — reuse the
   verifier's onchain tick-map fetch (factor it out of `liquidity_verifier.rs` into a
   shared helper) + onchain `slot0`/`liquidity`, feed the solver's `amount_in` + direction
   into the engine's per-hop simulator (`int_simulate_path` / `compute_swap_step`), compare
   to `solver_out`. Do NOT implement engine-state recompute (identity — skip).
   `matches_solver` reflects onchain-recompute vs solver_output. Document on
   `HopRecompute` that this shares the solver's math by necessity (no independent impl),
   so it catches **drift** (solver's state ≠ onchain) and a divergence between
   `cl-math` and the deployed contract math, but NOT a solver-internal calc error.

3. **Classifier consequence (feeds `CPCZZV`): ** Because V3/V4 recompute shares the
   solver's math, the classifier resolves to:
   - `drift == true` → **Drift**.
   - `drift == false` → every revert is **Encoding** or **Unknown** (a solver calc bug
     against correct state can't be distinguished from encoding by recompute; and, since
     `cl-math` is intended to match the contract math, a no-drift sim should *succeed* — so
     a no-drift revert is structurally an encoding/sequencing fault, e.g.
     `CurrencyNotSettled`/`SwapAmountCannotBeZero`/bare-`execution reverted`). The
     `SolverCalc` column stays meaningful for V2 only; for V3/V4 it's effectively
     unpopulated, which is the honest outcome.
   - **Fallback if onchain V3/V4 recompute proves too costly** (re-fetching the full tick
     map per reverted candidate is heavy): drop it, rely on the structured `drift` flag
     (`PCG2M3`) + decoded revert reason, and classify V3/V4 no-drift reverts as
     `Encoding`/`Unknown` (never `SolverCalc`, never `Stale`).

## The three failure modes, restated (resolution ground truth = the sim)

At `age=0` against fixed-state `eth_simulateV1` with a per-block debounce solve, a
revert is exactly one of: drift, solver calc error, encoding bug. The startup map
verification (`WFDTUR`) makes the snapshot/backfill basis trustworthy, so a `drift` flag
specifically indicts the per-block event pump (post-backfill desync) or a sim-block ≠
solve-block mismatch — not snapshot staleness.

## Facts established (file:line references)

- Engine retains per-pool `tick_data`: `rust/crates/degenbot-bot/src/bot_core/mod.rs`
  (`V3PoolState.tick_data` / `V4PoolState`), exposed via `v3_pools_snapshot()` /
  `v4_pools_snapshot()` (~mod.rs:1283, 2757).
- Sole V3/V4 swap-step math: `rust/crates/degenbot-cl-math/src/cl_lib/swap_math.rs`
  (`compute_swap_step_v3` ~:…, `compute_swap_step_v4` :231) + `sqrt_price_math.rs`
  (`get_amount0_delta`/`get_amount1_delta`). The solver itself composes these
  (`mobius_v3_int.rs`: "matches `computeSwapStep` in the Uniswap V3/V4 contracts").
- Per-hop simulation the solver uses: `int_simulate_path(x, hops: &[IntHopState])` in
  `mobius_int.rs:258` (returns `SimulationResult { output, consumed, … }`).
- Onchain tick-map fetch capability (reusable): `liquidity_verifier.rs`
  `verify_v3_liquidity_map` (`tickBitmap(int16)` + `ticks(int24)`'s
  `(liquidityGross, liquidityNet)`) and `verify_v4_liquidity_map`
  (`StateView.getTickLiquidity(bytes32,int24)`). StateView address
  `0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227` (already wired, `WFDTUR`).
- Diagnostic onchain fetch retains SCALAR ONLY — `fetch_v3_onchain`
  (`diagnostic.rs:421`, `slot0`+`liquidity`) / `fetch_v4_onchain` (:534,
  `StateView.getSlot0`+`getLiquidity`). `DiagnosticPoolState::V3`/`::V4` carry no
  `tick_data`. → onchain recompute must borrow the verifier's tick-map fetch.