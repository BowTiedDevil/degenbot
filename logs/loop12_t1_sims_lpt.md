## KUKHMX — done: measured-sims LPT cost (loop-12)

LPT bin cost = max(structural word-boundary proxy, previous-block MEASURED walk sims) — the measured count predicts repeat-shape combinatorics (path 27817 spends 26k sims/block) better than structure alone; the proxy floors fresh dirty pools.

- RED: `sims_aware_cost_prefers_measured_last_block_walk` (E0425 before) → green.
- 7/7 lpt tests green; full degenbot-bot suite 6/6 result files green.
- Pure scheduling: solutions byte-identical by construction; solve_fn records sims into a single parking_lot-guarded map (lock held for one insert per path). Snapshot clone once per bin construction (≈2.8k entries).
- Live verification deferred to next restart: watch `[solve-phase]` slowest.paths tail + phase_us/solve.cpu at constant paths; the makespan proxy is monotone-better so no regression gate is needed, but the line is observed anyway.
