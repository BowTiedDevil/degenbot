# Loop-17: slow-path outliers — census

## T1 capture (live, ~11 min run)

217 heavy paths captured (`DEGENBOT_SOLVER_CAPTURE=1`, MIN_US=20ms, CAP=48) to
tests/fixtures/live_capture_loop17.jsonl. The slow class is uniform: **3-hop
all-CL paths through dense pools** (range lens like [315,87,305], [106,x,312],
[408,x,8], [264,67,186] — recurring families around hub pools). Several of the
captured paths are gate-skipped (sims=0) — pure gate time on dense hops.

## T2 decomposition (warm replay + cache-lab, heaviest path 3201 = [315,87,305])

| component | per-path | notes |
|---|---|---|
| full table+profile rebuild (uncached) | ~2.8 ms | once per pool/block |
| gate (cacheless, loop-16 optimized) | ~1.7 ms | separate phase |
| strategy refill + hop assembly | ~2.4 ms | cached pipeline, outside the walk |
| cached walk solve | ~4.75 ms | measured inside `solve_active_set_path` |
| — walk sims (3,321 at ~0.64µs) | ~2.12 ms | **45% of the walk — the top bucket** |
| — climb machinery | ~1.8 ms | 38% — not yet sub-attributed |
| — anchor computations (274 pieces) | ~0.71 ms | 15% |
| — event predictions | ~0.13 ms | 3% |

Census instrumentation (process-wide atomics, no TLS cost): WALK_SIM_US_TOTAL,
WALK_ANCHOR_US_TOTAL, WALK_PRED_US_TOTAL, WALK_SOLVE_US_TOTAL; the cache-lab
prints them alongside per-strategy refill+solve timings.

## Key findings

## Key findings (updated after the T3 per-sim strike)

1. Slow paths = 3-hop dense-CL. NOT dominated by rebuild (2.8ms once per
   pool/block) nor the gate (1.7ms): the walk carries it.
2. Accurate ns-resolution census (216 lab solves, heaviest quartet):

   | bucket | per solve | share |
   |---|---|---|
   | walk probes (sims) | ~2.69 ms | **69%** |
   | — anchor ±2 probe sweep | ~0.98 ms | 25% (the top single slice) |
   | — direction test + advancement | ~0.52 ms | 13% |
   | — right-edge verifies | ~0.42 ms | 11% |
   | — refine grids | ~0.47 ms | 12% |
   | — left-edge straddle | ~0.29 ms | 7% |
   | anchor compose + isqrt | ~0.82 ms | 21% |
   | climb machinery (non-sim) | ~0.26 ms | 7% (redge 0.19) |

   (The earlier "4.2ms machinery" figure was a census artifact — the sim
   timer truncated to whole µs, hiding sub-µs sims. All census timers now
   store ns.)
3. Per-sim is what it is: ~0.77µs = 3×(crossing partition_point ≈10ns +
   profile partition + one `compute_swap_step_v3`).

## Recommended T3 sequence — updated state

1. ~~Per-sim cost~~ **DONE** (`a9ae5983e`): byte-identical fast division
   paths in `compute_swap_step_v3` — power-of-two denominators become
   shifts (the Q96 sites), narrow operands take 256/256 division, and all
   rounding-up helpers fuse their remainder pass. Step 278→213ns; walk-sim
   −29%; reference event 9.82→8.5ms; zero divergences; 2696/2696 suites.
2. **Probe-count reduction** (~13/piece): the anchor ±2 sweep (~0.98ms/solve)
   and landed_beyond in the direction test are the landing-derivable
   candidates. WARNING: probe grid changes shift goldens (1-wei plateau
   alignment, cf. the loop-15 flat-top lesson) — deliberate regeneration +
   differential discipline. **Fork: needs a dedicated campaign.**
3. **Refill assembly** (~2.4ms cached-pipeline slice): wire the best lab
   strategy (S1/S4) into the production refill path.
4. Anchor coefficient memoization across consecutive pieces (shared tuple
   prefixes) for the 0.82ms anchor-compose slice (pure fn → byte-identical).

## T4 — end-to-end verification (post-campaign, live bot run)

13-minute dry-run (`run_bot.sh`, capture threshold lowered to 5ms):

- **Stability**: zero missed WS pongs, zero errors/panics; 499 heavy-path
  captures (>5ms) in 13 min.
- **Solver correctness**: `cl_solve_replay` golden match 217/217 (old
  capture) and **499/499** (new capture), deterministic across iterations.
- **Heaviest medians**: old fixture 9064µs, new epoch 9013µs (the replay is
  build+gate+walk per rep; the walk share dropped far more — see lab
  numbers above — while build ~2.8ms and gate ~1.7ms now floor the
  replay). Slowest-path telemetry now shows the **gate** as the top slice
  on the worst paths (e.g. path 719: 19.1ms total, 19.1ms gate, sims=0).
- **Skip-gate note**: fresh-epoch replays show 2/499 FALSE SKIPs at a
  1e12-wei hypothetical floor (0 at the 1e9 production floor — the loop-16
  floor discipline holds).

Campaign totals vs loop-16 baseline (same lab harness, heaviest quartet):
reference event 9.82 -> 7.26 ms (-26%), cached walk solve 4.75 -> ~3.8 ms.
NEXT loop candidates: gate (dominates worst-path telemetry), then the
golden-fork probe-set campaign.
