## ECWSJM — climb-stop neighbor gating: measured NEUTRAL (reverted, zero diff)

Unified the forward-neighbor coarse-grid gate (33-point + 0.1% grace) across both stop directions; profit invariant preserved by construction (same gate the falls already run; RED contract demanded exact recorded optimum + budget cut).

A/B via cl_solve_replay (701 paths × 9): gated refine total == legacy refine total == 108,873 probes. Per-path split identical too (legacy deterministic identical counts). The corpus never has a non-competitive neighbor at a stop — every climb opens the gate, so the unconditional-climb tail was not spending redundant probes on the observed path space.

Verdict: no win to ship ("only if it beats legacy on the bench"). Reverted to zero-diff.

Lesson for the live 74k ternary budget: narrowing probes are nearly all in the CURRENT-piece refine windows that the gate cannot skip; neighbor tail is a side-stream. The remaining spend lives inside walk_refine_window itself where finding must stay exact at the 1e6 grid.
