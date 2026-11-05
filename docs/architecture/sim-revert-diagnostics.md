# Simulation-revert diagnostics pipeline

End-to-end flow for attributing a reverted simulation candidate: the on-chain
revert, the FFI failure record it leaves behind, the `[sim-diag]` log line
rendered from that record, and the `SolverCalc` basis that line's expected-vs-
actual gap carries. Covers the recompute caveats (V2 vs V3/V4).

## Pipeline

```
simulation revert (the Rust sim)
        │
        ▼
FFI failure record      (rust/crates/shells/degenbot-python/src/simulation/render.rs)
        │  reverting_frame, captured_swaps, hop_outputs, call_trace
        ▼
`_render_one_failure`  (src/degenbot/runner/_render.py)
        │
        ├─ `[sim-fail] path=… type=… bucket=… revert@depth=… hops=…`   (INFO)
        │
        └─ `format_sim_diag_line(…)` → one `[sim-diag] <json>` line    (DEBUG)
                   │  payload: path_id, path_type, solve_block, block, age,
                   │           revert_info, optimal_input, hop_outputs,
                   │           captured_swaps
                   ▼
        the expected-vs-actual gap — `hop_outputs[i]` vs the i-th captured
        swap's output amount — is the `SolverCalc` basis
```

The four-way TSV analyzer that once closed this flow (`logs/permutation_analyzer.py`'s
`classify_candidate`, driven by `logs/test_all_permutations.sh`) retired with
`logs/` itself — `git show cbd76d848`. Nothing in the tree grades a logged
`[sim-diag]` line any more: the live `SolverCalc` classifier is
`degenbot_arbitrage::pool_divergence::is_solver_calc_failure`, deciding the same
gap in-process from the failure record. Rendering is capped at
`_SIM_FAIL_RENDER_CAP` records per batch. The analyzer's other input, the Rust
per-hop recompute (`fetch_onchain`, `HopRecompute`'s `expected_out_*` /
`matches_solver`), retired too — `rust/crates/engine/degenbot-bot/src/arb_engine/diagnostic.rs`
opens with that retirement, and the captured swap is the basis in its place.

### Where each piece lives

| Stage | File |
|---|---|
| Capture (frames, swaps, slots) | `rust/crates/engine/degenbot-simulation/src/sim/capture/` — see `inspector-diagnostics.md` |
| Failure record (`SimFailure`) | `rust/crates/engine/degenbot-arbitrage/src/simulator.rs` |
| FFI record dicts | `rust/crates/shells/degenbot-python/src/simulation/render.rs` |
| `[sim-fail]` / `[sim-diag]` renderers | `src/degenbot/runner/_render.py` (`_render_one_failure`, `format_sim_diag_line`, `SimDiagWindow`) |
| `SolverCalc` classifier | `rust/crates/engine/degenbot-arbitrage/src/pool_divergence.rs` (`is_solver_calc_failure`) |
| On-demand path snapshot (`diagnostic_inspect_path`) | `rust/crates/engine/degenbot-bot/src/arb_engine/diagnostic.rs`, `rust/crates/shells/degenbot-python/src/bot/engine/solve.rs` |

## Four-way classification

The retired analyzer's rules, recorded because they say what a `[sim-diag]`
payload can and cannot support — nothing in the tree computes them. Applied per
reverted candidate (from its `[sim-diag]` payload), in order:

1. **Drift** — any hop has `drift == true` (engine_state ≠ onchain_state). The
   map basis is verified at startup (snapshot + backfill phases — see the
   `WFDTUR` task), so a drift flag does **not** indict the snapshot; it means
   the per-block event pump desynced post-backfill, or the sim block tag
   differs from the solve block.
2. **SolverCalc** — no drift AND any hop `recompute.matches_solver == false`
   (an independent recompute of the on-chain output disagrees with the
   solver's reported `hop_outputs`). Meaningful for **V2** hops (the only
   family with a genuine recompute).
3. **Encoding** — no drift AND every hop `matches_solver == true`, yet the sim
   reverted (the amounts were right, so the encoded command stream must be
   wrong).
4. **Unknown** — bare/empty revert (`0x execution reverted`) OR recompute
   unavailable for the reverting hop family. Classified conservatively —
   **never** as "stale".

### Fallback

When `[sim-diag]` lines are absent (older logs predating LAV44W), every revert
falls into the `Unknown` column and the TSV carries a `# basis:` note:
`fallback-unknown (no [sim-diag] lines — older logs)`. The `NoProfit` column is
still derived from the bot's authoritative `by reason: no-profit=N` summary
(the detailed `[sim-fail]` line count undercounts).

### Verification-basis header note

The `# basis:` line also records the verification basis detected in the run,
because it qualifies what a `Drift` verdict means:

- `[verify] … OK` present → drift is attributable to per-block pump desync (or a
  sim-block ≠ solve-block mismatch); the snapshot is not implicated.
- `verify … SKIPPED` present → drift could equally indicate a bad snapshot
  (no verifier check ran), so the verdict is less certain.

## Recompute caveats (V2 vs V3/V4)

Per the `V3/V4 recompute feasibility spike` (spike write-up file removed in the stale-docs cleanup `71ec78b2`; recoverable from git history):

- **V2** — `recompute_v2_amount_out` reuses the solver's `IntHopState::swap`
  (the canonical Uniswap V2 `getAmountOut`) against the engine reserves and,
  post-`fetch_onchain`, the on-chain reserves. A divergence
  (`matches_solver == false`) is a genuine solver-calc-error / drift signal.
  This is the only family that populates `expected_out_engine` +
  `expected_out_onchain` + `matches_solver`.

- **V3/V4** — engine-state recompute is **identity**: only `degenbot-concentrated-liquidity-math`
  exists, which is what the solver ran, so re-simulating the engine state
  reproduces `solver_out` (an internal-consistency check, not an independent
  calc check). Furthermore, a genuine on-chain recompute needs the **full tick
  map** — a heavy RPC fetch the diagnostic path does not perform (it fetches
  only the scalar `slot0`/`liquidity`). So V3/V4 hops get a **partial**
  `HopRecompute` (the solver's reported `amount_in`/`solver_out` only;
  `expected_out_*` / `matches_solver` stay `None`) and never trigger the
  `SolverCalc` verdict. A V3/V4 no-drift revert resolves to `Unknown` (honest)
  unless a V2 hop's `matches_solver` drives `Encoding`.

  The scalar `drift` flag (PCG2M3 — `slot0`/`tick`/`liquidity` mismatch) and the
  decoded revert reason are the V3/V4 classification basis instead. A genuine
  on-chain tick-map recompute is a future enhancement if the scalar drift +
  revert reason prove insufficient.

## Why "stale" is retired

The prior `Stale_Reverts`/`Bug_Reverts` dichotomy dismissed `IIA`,
`UniswapV2: K`, `ERC20: transfer amount exceeds balance`, `Dai/insufficient-
balance`, etc. as blameless state divergence. But at `age=0` against fixed-state
simulation, those reverts are **not** blameless — they witness drift, a solver
calc error, or an encoding bug. Discarding them as "stale" masked the
possibility of real bugs. The four-way classifier attributes every reverted
candidate instead; the stale-reason patterns now appear only as descriptive
sub-info in the decoded `revert_info` if surfaced at all.