# Simulation-revert diagnostics pipeline

End-to-end flow for attributing a reverted simulation candidate, from the
on-chain revert through the structured `[sim-diag]` log line to the analyzer's
four-way classification. Covers the spike's recompute caveats (V2 vs V3/V4).

## Pipeline

```
simulation revert (eth_call in simulate_one)
        │
        ├─[ unchanged ]─ `[sim-fail]` + `[sim-revert-data]` lines (human/debug)
        ├─[ unchanged ]─ deduped `[v-state]` verbose block (sampled per block+type)
        │
        └─[ LAV44W, always-on ]─ `_emit_sim_diag(path_id, …)`
                   │  calls diagnostic_inspect_path(path_id, rpc_url)
                   │    → builds the DiagnosticPathState snapshot
                   │      (engine_state per hop; fetch_onchain → onchain_state +
                   │       drift/field_drift; thread_solver_result_and_recompute
                   │       → optimal_input/hop_outputs + per-hop recompute)
                   ▼
            `_format_sim_diag_line(snapshot, …)`  (examples/eth_backrun_helpers.py)
                   ▼
            one `[sim-diag] <json>` line per reverted candidate  (no dedup, no env gate)
                   ▼
            logs/permutation_analyzer.py  `classify_candidate(sim_diag)`
                   ▼
            four-way TSV columns: Drift / SolverCalc / Encoding / Unknown
```

### Where each piece lives

| Stage | File |
|---|---|
| Snapshot build + onchain fetch + recompute | `rust/crates/degenbot-bot/src/solvers/uniswap_engine/diagnostic.rs` |
| PyO3 entry (`diagnostic_inspect_path`) | `rust/crates/degenbot-python/src/bot/engine/solve.rs` |
| Always-on emit + JSON-line builder | `examples/eth_backrun_v2_v3_v4_rust.py`, `examples/eth_backrun_helpers.py` |
| Classifier + TSV driver | `logs/permutation_analyzer.py`, `logs/test_all_permutations.sh` |

## Four-way classification

Applied per reverted candidate (from its `[sim-diag]` payload), in order:

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

Per the [V3/V4 recompute feasibility spike](../spikes/v3-v4-recompute-feasibility.md):

- **V2** — `recompute_v2_amount_out` reuses the solver's `IntHopState::swap`
  (the canonical Uniswap V2 `getAmountOut`) against the engine reserves and,
  post-`fetch_onchain`, the on-chain reserves. A divergence
  (`matches_solver == false`) is a genuine solver-calc-error / drift signal.
  This is the only family that populates `expected_out_engine` +
  `expected_out_onchain` + `matches_solver`.

- **V3/V4** — engine-state recompute is **identity**: only `degenbot-cl-math`
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