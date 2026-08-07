# 7E5D7W Checkpoint — CL-hop clamp margin policy

**Task:** Margin policy for the CL-hop clamp (epic PJLIAE)

**Decision:** Clamp margin = **1 wei**, confirmed and documented with a measured
basis. The committed CL-hop input is `input_consumed − 1` (the tier-3-proven
`v4_simulate_swap`/`v3_simulate_swap` pool twin's `input_consumed` minus 1 wei).

## Why 1 wei is the defensible margin

The margin's only job is to guarantee the clamp never lands *exactly* on an
**over-predicted** tight value — i.e. it must be strictly larger than the worst
solver-vs-engine over-prediction magnitude, so the exact-in loop terminates on
`amountRemaining == 0` at the last funded tick instead of marching empty bitmap
words to the price limit (the path-5000 20.7M-gas EMPTY-HALT class, AGENTS.md
UO3JM4).

### Measured worst over-prediction = 0 wei

The solver-vs-twin parity corpus already asserts **byte-exact equality** across
every covered topology:

- `v4_crossing_solver_vs_sim_parity` — fee-3000/ts-60 multi-tick, liquidity
  1e13–1e21, both swap directions, amount sweeps incl. boundary-adjacent dust.
- `v4_fee1_solver_path_matches_v4_simulate_swap` — the UO3JM4 fee-1/ts=1 real
  pool (`0x76f75965…`, fee=50, protocol_fee 53261), both directions, tiny
  outputs.
- `v4_word_boundary_solver_divergence` — sparse topologies spanning uninitialized
  word boundaries.
- `v3_crossing_solver_vs_sim_parity` — the V3 twin.

The historical live `+1..+3` wei residuals (fee-1, ts=1, paths 10338/57150) were
**localized to crossing-math rounding and fixed at the source** (the zfo step-0
current-tick flooring in `compute_tick_ranges`; the `+3` single-step collapse),
*not* absorbed by margin. Post-fix the crossing math is exact, so the true worst
observed over-prediction across the covered corpus is **0 wei**.

### Margin = 1 > 0, maximum extraction

- **Strictly greater than worst observed (0):** 1 wei is the smallest positive
  integer > 0, satisfying the acceptance criterion "margin > worst observed
  solver-vs-engine over-prediction".
- **Maximum extraction / zero output loss:** the path-5000 fixture records that
  `input_consumed − 1` yields a clamped output byte-identical to the solver's
  output (unlike the earlier 21,000-wei demo which cost ~630 wei). A bps-fraction
  margin was rejected: it is unnecessary (worst is 0) and would forfeit real
  output on large inputs.
- **Low-water mark:** the path-5000 probe's recorded leftover (20,139 wei past
  `input_consumed`) is ≫ margin, so `input_consumed − 1` still clamps cleanly
  below capacity.

## Self-checking guard

A new sweep, `cl_hop_clamp_margin_exceeds_worst_solver_over_prediction` in
`degenbot-solvers/tests/v4_crossing_solver_vs_sim_parity.rs`, measures the strict
over-prediction direction (solver output − twin output when positive) across the
fee-1/ts-1 and fee-3000/ts-60 corpus and asserts `margin (1) > worst observed
(=0)`. If a future topology re-introduces a non-zero over-prediction ≥ 1, this
test trips — guarding the VAASFM maximum-extraction choice against regression.

## Verification

- `cargo test -p degenbot-solvers` — parity + divergence + margin sweep green.
- Path-5000 / fee-1 fixtures still pass through their full profitable range with
  the 1-wei margin (byte-identical outputs, per the VAASFM checkpoint).
