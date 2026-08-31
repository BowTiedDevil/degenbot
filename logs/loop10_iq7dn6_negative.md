## IQ7DN6 — done (measured negative; reverted, zero diff)

Exact-hull wavefront over approx-sorted composed pairs (f64 log-sum order, exact hull math on fed reduced lines; misorders only LOOSEN the bound, never under-cut). Envelope suite passes in both modes.

gate_bench A/B:
- pid 3239 heavy: legacy 1083-1113us → wavefront 1263-1279us (fed-line reductions make product 1094us vs legacy 157us).
- pid 3692: 353 → 531us; pid 7036: 288 → 326us; pid 209: 228 → 319us.

Every family loses 10-50%: reducing + pushing all ~1536 composed lines in streamed form dominates the endpoint-eval sweep it replaces. The stage-1 endpoint sweep remains the optimal prune front for these distributions.

Loop-10 verdict recorded: both remaining stage-1 designs (cascade GUKDGA, wavefront IQ7DN6) measured negative; the legacy exact sweep is the validated floor with live finance event watching it (gate.prune_stage1_us).
