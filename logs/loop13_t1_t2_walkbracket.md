## YHR3ZH + ELBFQ6 — done: walk sim atomization → edge-bracket tolerance measured NEGATIVE

T1 (adopted instrumentation): per-piece sim split counters. Live-corpus replay (path 3671 [318,88,291]): total 18.8k sims = right-edge bisection 15,888 (84%), anchor 1,060, refine 167, left-edge 0 (warm-seeded reuse), residual ~1.7k. Path 27320 [6,220,291]: 25.5k sims / ~69-per-piece bisection to the 4-wei bracket.

T2 (measured negative, reverted to zero diff): DEGENBOT_WALK_EDGE_BRACKET_WEI sweep.
- T=1024..2^20: live-corpus replay kept 104/104 golden + deterministic, heaviest 40.4ms → 5.0ms — but 5 adversarial lib tests fail:
  - corner-profit test: corner collapses to 1.17M (coarse window edge corrupts the refine bracket)
  - fine-grid oracle deep family: solver=1.79M vs oracle=140.09M — coarse advance edges mis-attribute the climb and STOP the walk before the late deep-liquidity peak.
- Stratified fix (coarse advance + tight stop re-bisect) and hi-anchored straddles both keep the oracle defeat — the decision basin loss is intrinsic to a coarse right edge, not to probe placement. T=8 passes corner but saves ~0; no tolerance wins.
- Verdict: per-piece ≤4-wei bisection is load-bearing for the walk climb correctness; the bisection budget (84% of walk sims) is irreducible without changing the climb oracle.

## T3 precursor note
Live gate-burst family (10760-class: gate 24–153ms) misses the min_sims=8000 capture filter — T3 re-captures with MIN_US to land those.
