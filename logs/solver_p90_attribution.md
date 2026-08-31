## Solver latency attribution — overnight run (blocks 25873262–25876093)

Sources: live Prometheus scrape (2,839 blocks, 2,575 dirty solves), Jaeger rank (solve spans 3.7–4.5s wall), and the per-block [solve-phase] + [solver-st] telemetry.

### 1. Solve wall = path count × path CPU, byte-for-byte

Per-block (typical; e.g. 25876092):
- paths.solved = 2,826, solve.cpu = 2,916 ms → mean path CPU ≈ 1.03 ms
- Prometheus confirms: mean solve.duration/cycle = 2.94 s, mean solve_path = 1.09 ms, mean gate = 437 µs; 2,575 cycles × 2.67k paths = 6.87M path-solves — 2,826 × 1.03 ms = 2.9 s, no hidden overhead.

### 2. Gate machinery = 71% of solve CPU (new leader)

Same block: gate sums 2,079 ms of 2,916 ms CPU:
- compose 989 ms (34%), prune stage1 454 ms (16%), product 229 ms, hull 205 ms, derive 126 ms, search 76 ms.
- histograms across a worse block (25873270): gate 7.58 s of 10.52 s CPU = 72%.
The remaining ~29% is the walk (932k sims / 2.40M word steps), refine (213k probes; ternary 167.6k + grid 45.4k).

### 3. The p90 tail is three populations

Per-path latency is trimodal:
- Bulk ~0.5–1.5 ms: gate-dominated normal shapes.
- Walk-heavy repeats (same pools every block): path 27817 V3·0x99ac8ca7→V3·0xe0554a39f: 26,448 sims / 78.5k steps → 56–244 ms per block (corpus-high under rayon contention).
- Gate-heavy bursts, sims <1k but gate 24–153 ms: 10760 (152.7 ms gate of 153.6), 15012 (54.0 of 56.3), 26030 (58.7 of 59.5), 12935 (45.7), 10949 (23.5). All V3×V3×V3 with the giant-liquidity 0xe0554a39f (liq 1.23e28) at hop 3; the fat crossing tables at the middle pools blow up per-boundary compose/reduce cost.
- Peak block 25873270: CPU 10.5 s (phase wall 2.51 s) — one 244 ms walk path + five 24–56 ms gate paths, the p90 distortion.

### 4. Why p90 stays high

- The metrics p90 of the SOLVE CYCLE ≈ 5 s comes from the 72% gate CPU spread over 2.8k paths × 437 µs.
- The 5 slowest paths/block are stable pool identities. The 244 ms outlier is pure walk-sim count (dense crossing table walk per eval); most tail muscle is the envelope (compose + stage-1 sweep) on paths whose middle pool crossing tables carry thousands of tick ranges.

### 5. Candidate levers (loop-12)

1. Fat-crossing table compression for boundary composition (a compose target per range-count band — e.g. offsets from the giant stable pool).
2. Path ordering by previous-block sims count before rayon dispatch → heavy jobs start first, tail-shaving the p90/max (load-balance only, classic makespan fix).
3. Per-pool walk-sim memo across blocks when its crossing-table fingerprint and neighboring liquidity are unchanged (bounded by stale-guard scope).
4. Revisit gate.prune_stage1 (454 ms) with the corpus-shape-aware bounds once the giant-pool families above are captured into the replay corpus — prior wavefront/cascade designs lost on OLD pool shapes, not these.
