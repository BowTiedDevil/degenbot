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
