## D4DEBJ/4BZNEU — golden-section refine: measured NEGATIVE (reverted, zero diff)

Golden-section narrowing (mode 1 via DEGENBOT_REFINE_MODE) cut refine probes — guard deep 178→117 (−34%), replay path 37751 171→93 (−46%), path 42375 94 vs 173 — and matched profit on guard fixtures and most of the heavy corpus.

BUT the heavy replay F2 gate regressed: golden match 687/701 vs ternary 692/701 — and the new mismatches are REAL LOST PROFIT at the gate edge, not flat-top wiggles:

- path 44361 [192,2,180]: under_shoot 10,126,404 wei (>> PROFIT_EPS 100k)
- path 44366 [192,2,280]: under_shoot 45,562,616 wei
- path 44367 [192,2,724]: under_shoot 137,610,392 wei

Cause: with staircase-perturbed plateaus P(x) is only unimodal up to bracket-scale artifacts; the golden final bracket can exclude a window-edge bump that nearby plateaus mask, while ternary's asymmetric +1/−1 mid placements keep it. The unit-level strict-dominance test (deep fixture) passed but the replay corpus is the correct oracle.

Verdict: classic ternary stays. Reverted mobius_v3_int.rs to zero-diff; suites green (11 ok-result files, deterministic replay unchanged).
