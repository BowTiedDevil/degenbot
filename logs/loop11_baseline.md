## Loop-11 FWB3SH baseline (legacy ternary refine)

Q6DMHV (T1) captured BEFORE solve-code changes.

### Guard fixtures (cargo test -p degenbot-solvers active_set_walk_piece_and_simulation_counts_are_bounded --release)

- deep late-liquidity (Σ ranges = 13): pieces=11 sims=832 refine_sims=178 (ternary=112 grid=66) word_steps=1125
- 3-hop moderate: pieces=1 sims=251 refine_sims=259 (ternary=160 grid=99) word_steps=1885

### heavy_cl_solve_captures.jsonl (cl_solve_replay --release, 9x)

- 701 paths; golden profit-gate 692/701 exact, 9 optimal-input deltas (profit-equal flat tops), deterministic 701/701.
- Median line highlights: path 400 = 31922us sims=43410 refine=110 pieces=828; path 304 = 2849us refine=168 pieces=53; path 2 = 1217us refine=185.
- Ternary share of refine: guard fixtures 63%/62%.

### RED run (golden mode stub = mode 0)

```
[loop-11 RED] ternary refine_sims=178 profit=173876084 golden refine_sims=178 profit=173876084
test result: FAILED ... golden_refine_mode_never_loses_profit_and_spends_fewer_probes
```
Expected: equal counts while mode 1 is a stub → strict-fewer assertion fails (the RED signal).
