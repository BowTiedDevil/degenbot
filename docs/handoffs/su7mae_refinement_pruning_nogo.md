# Spike result: NO-GO on refinement coarsening (for now)

## Measurements
- Pre-gate production: refine_sims = 43% of ALL sims (1.51M of 3.54M/block).
- Post-gate residual (profitable paths only, from capture data): refine_sims ≈ 59% of remaining sims (path 105: 168/287; path 2: 362/616).
- Projected absolute saving from eliminating ALL refinement probes: ~0.15-0.17s of a ~0.3s projected post-gate median solve phase.

## Go/No-Go criterion (from task)
Projected wall-clock saving >= 2x ... AND golden profits within PROFIT_SLACK_PCT.

## Verdict: NO-GO
- Absolute saving post-gate (~0.15s) no longer material: the gate already removes ~92% of walk volume, projecting solve p95 from 2.95s to well under block cadence.
- Refinement pruning touches `walk_refine_window`, the exactness-critical machinery guarded by F1/EHSWSX corner proofs — risk disproportionate to sub-0.2s saving.
- Re-open trigger: if live post-gate telemetry shows solve-phase p95 still approaching block cadence, OR paths grow >5x (path-cap raise), revisit with PROFIT_SLACK_PCT license.
