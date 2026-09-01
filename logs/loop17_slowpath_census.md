# Loop-17: slow-path outliers — census

## T1 capture (live, ~11 min run)

217 heavy paths captured (`DEGENBOT_SOLVER_CAPTURE=1`, MIN_US=20ms, CAP=48) to
tests/fixtures/live_capture_loop17.jsonl. The slow class is uniform: **3-hop
all-CL paths through dense pools** (range lens like [315,87,305], [106,x,312],
[408,x,8], [264,67,186] — recurring families around hub pools). Several of the
captured paths have `sims=0, pieces=0` (gate-skipped, pure gate time on dense
hops).

## T2 decomposition (warm replay + cache-lab timings)

Heaviest path (3201, [315,87,305] ranges, 707 total):

| component | per-path | share of cached solve |
|---|---|---|
| full table+profile rebuild (uncached) | ~2.8 ms | first touch per pool/block |
| gate (cacheless, loop-16 optimized) | ~1.7 ms | excluded (separate phase) |
| cached solve (walk) total | ~7.2 ms | 100% |
| — walk sims (3,321 at ~0.7µs) | ~2.2 ms | 33% |
| — anchor computations (274 pieces) | ~0.7 ms | 11% |
| — event predictions | ~0.1 ms | 2% |
| — remaining climb machinery | ~4.2 ms | **56%** |

Census instrumentation (process-wide atomics, no TLS cost): WALK_SIM_US_TOTAL,
WALK_ANCHOR_US_TOTAL, WALK_PRED_US_TOTAL; the cache-lab prints them.

## Key findings

1. The slow paths are NOT dominated by the profile/table rebuild (2.8ms once
   per pool/block) nor by the gate (1.7ms) — the **walk solve itself** (~7.2ms
   cached) carries it, and inside it the single largest bucket is the
   **un-instrumented climb machinery** (~4.2ms): the per-piece loop work
   outside sims/anchors/predictions — candidates: `walk_piece_anchor_transitional`,
   per-piece window bookkeeping, landed-beyond scans, recorder pushes.
2. Sims are ~0.7µs each (3 hops × profile binary-search + one
   `compute_swap_step_v3` partial landing) — not a fat target anymore.
3. The cache-lab strategies (S1..S7, byte-equal proven) cut re-fills to
   0.05–3ms per transition vs 2.8–9.2ms full rebuild — worth wiring into
   production only after the walk machinery is understood (rebuild is
   amortized across paths per block today).

## Recommended T3 sequence

1. Instrument the remaining climb machinery (phase timers inside
   `solve_active_set_path`: per-piece window setup vs transitional anchors vs
   climb loop accounting) — the 4.2ms must be attributed before surgery.
2. Likely structural targets, in order of expected value:
   a. `walk_piece_anchor_transitional` + `build_shifted_piece_hops` per-piece
      re-composition — the shifted Möbius coefficients per piece tuple could be
      memoized per (path, tuple-prefix) since consecutive pieces share prefixes
      (incremental compose instead of from scratch per piece).
   b. Recorder/bookkeeping overhead in the climb loop.
   c. Only then: probe-count reduction (changes grid alignment — sentinel
      sensitivity, cf. the loop-15 flat-top lesson).