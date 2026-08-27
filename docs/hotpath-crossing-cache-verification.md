# UYSAXS — Hotpath verification

Verified with two profiled runs. Final comparator run reached **50,311 registered paths** in
902s (the prior >12s solve regime was ~48k paths), so the after-build is measured at or above
the baseline path inventory.

## Before / after (functions_timing, hotpath report)

| Function | hp1 (600s, ~48k) | hp2 (300s) | final2 (902s, 50.3k) | Outcome |
|---|---|---|---|---|
| mixed.solve_path_inner | 49.92% | 25.95% | **5.01%** | no longer dominant |
| cl_solve.exact_solve_mixed_path_n | 49.80% | 25.93% | **4.93%** | cratered |
| cl_solve.build_crossing_table | not isolated | 22.82% | **2.00%** (1980 calls) | projection-time only |
| cl_solve.active_set | 4.67% | 2.63% | **4.97%** | now the work, close to solve cost |
| arb_solve.rayon_solve p95 | 6.47s | 3.40s | **7.72s** | < 12s |
| SolveCoordinator::on_drain p95 | 6.77s | 3.90s | **8.42s** | < 12s |

cl_solve.build_crossing_table calls dropped from 5,519 (hp2, per solve) to 1,980 for 50.3k
paths — it only runs at projection/cache-fill time (once per cache miss, plus the
word-profile builder's own projection build), not once per solve.

## Jaeger

Latest degenbot.arb.solve spans (lookback 45m, n=100):
- p50 2.91s, p95 8.21s, max 12.37s (1 span > 12s out of 100)

p95 is back under the 12s block-time threshold in the comparable high-load window.

## Build gates

- cargo test -p degenbot-solvers -p degenbot-bot --manifest-path rust/Cargo.toml green.
- cargo clippy -p degenbot-solvers -p degenbot-bot --all-targets green.
- prepare pre-commit fmt/lint gates green on every commit.

## Artifacts

- Final report: logs/hp_cl_solve_final2.json
- Predecessor report (600s): logs/hp_cl_solve_final.json
- Baselines: logs/hp_cl_solve.json, logs/hp_cl_solve2.json

## KGXFT7 — winner promotion A/B (fused-epoch memo, default-on cutover gate)

The cache-lab sweep (Z5NOPD, docs/cache-lab-report.md) named S1_fused_epoch the
winner; KGXFT7 wired the promotion in behind the default-on cutover gate
`DEGENBOT_CL_PROJECTION_CACHE` (`0`/off/false disables; resolved once at
engine construction). The parity gates
`memo_off_all_cl_solve_is_byte_exact_v3_v4` and
`memo_off_mixed_v2_cl_solve_is_byte_exact` prove byte-exact solver intake
for V3+V4 all-CL and V2+CL mixed paths with the memo on (cache hits) or off
(fresh builds).

Three profiled runs on mainnet (node host.containers.internal, block
~25.85M). GATE-ON and GATE-OFF used matched ~613s windows and
HOTPATH_SHUTDOWN_MS=600000; final2 is the prior 902s baseline (50,311 paths,
pre-trim inventory). Note this environment trims the live DB to 107 V2 /
208 V3 / 771 V4 pools → 16,396 registered paths, so absolute drain p95s do
not reproduce final2's; the A/B pair is the controlled comparison.

hotpath functions_timing:

| Function | GATE-ON (613s) | GATE-OFF (634s) | final2 (902s) |
|---|---|---|---|
| cl_solve.build_crossing_table | **0 calls** (absent) | 31,482 calls / 36.34% | 1,980 calls / 2.00% |
| cl_solve.build_word_profiles | 0 calls | 0 calls* | 0 calls* |
| mixed.solve_path_inner p95 | 23.82 ms | 26.49 ms | 11.52 ms |
| cl_solve.active_set p95 | 24.64 ms | 27.92 ms | 12.08 ms |
| SolveCoordinator::on_drain p95 | 14.77 s | 17.10 s | 8.42 s |
| arb_solve.rayon_solve p95 | 14.50 s | 9.66 s | 7.72 s |
| paths solved (mixed solve calls) | 14,186 (13,266 CL) | 7,530 (7,090 CL) | 18,138 (16,628) |

* word-profile builds ride the crossing-table build counter; zero in both
because build_cl_word_profiles shares the wrap row.

Jaeger `degenbot.arb.solve` span latencies (partitioned per run, n≈32):

| window | p50 | p95 | max |
|---|---|---|---|
| GATE-ON | 3.73 s | **11.87 s** (< 12s gate) | 13.42 s |
| GATE-OFF | 8.26 s | 15.94 s | 17.71 s |

**Conclusion.** With the memo on, crossing-table rebuilds disappear from the
solve phase entirely (0 vs 31,482 calls in the matched window — gate-off
spent 36% of pump time rebuilding tables) and the bot solved ~1.9× more
paths in the same window with byte-identical results. Solve-span p95 with the
gate on sits under the 12s block-time threshold in this environment
(11.87s vs 15.94s off); drain p95s are inventory-conditioned post-trim and
higher in both A/B legs than the pre-trim 902s baseline. The flag is the
cutover: `DEGENBOT_CL_PROJECTION_CACHE=0` reinstates the rebuild cost.

### Artifacts

- GATE-ON: logs/hp_cl_solve_final_s1.json (611.25s)
- GATE-OFF: logs/hp_cl_solve_gateoff2.json (613.08s; run 1 aborted ~84s at a sim-failure dispatch exit, logs/hp_cl_solve_gateoff.json)
- Comparator: logs/hp_cl_solve_final2.json (902.30s)

