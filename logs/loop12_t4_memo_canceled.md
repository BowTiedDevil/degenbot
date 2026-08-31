## IBFXZP — CANCELED: no measurement surface (loop-12)

Per-pool walk-sim memo across blocks needs a corpus where the walk actually spends the live 26k sims; the loop rule for perf work is measure-then-adopt, and a memo of the production walk adds cross-block state to a pure solver — unjustified without measured value, rejected without measured parity.

Fixture probes (W family, replicated live shape families):
- W v1: [1,390,3] hops => pieces=7 sims=341 (walk stops near the prefix despite deep-late liquidity)
- W v2: hop3 widened to 300x 3e12 bars => pieces=7 sims=341 unchanged — capacity is not the limiter; the ±64 straddle climb simply terminates early on synthetic geometry.

None of the synthesizable shapes reproduces the live 27817 stickiness (323 pieces / 26.4k sims). Without that, any memo A/B would show ~0 savings and unproven parity.

If live capture becomes available (DEGENBOT_SOLVER_CAPTURE on a single block), add path 27817's real crossing tables to the corpus and reopen.
