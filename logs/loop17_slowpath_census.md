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

1. Slow paths = 3-hop dense-CL. NOT dominated by rebuild (2.8ms once per
   pool/block) nor the gate (1.7ms): the walk carries it, and its top bucket
   is the **sims themselves** — 3,321 probes at ~0.64µs (≈13 probes per piece
   over 274 pieces: 5 anchor±2, ~3 left-edge straddle, 1 skipped-tuple check,
   2 right-edge verifies, 2 direction probes).
2. The cached pipeline additionally pays ~2.4ms of refill/assembly per solve
   outside the walk — the lab strategies (S1..S7, byte-equal proven) address
   exactly this slice.

## Recommended T3 sequence

1. **Per-sim cost** (0.64µs = 3 hops × profile query: binary search + one
   partial-landing `compute_swap_step_v3`): a specialized narrow-mulDiv path
   could halve it. Byte-identical, no golden risk — the safest first strike.
2. **Probe-count reduction** (~13/piece): the left-edge straddle probes and
   the anchor ±2 sweep revisit landing-derivable quantities. WARNING: probe
   grid changes shift goldens (1-wei plateau alignment, cf. the loop-15
   flat-top lesson) — deliberate regeneration + differential discipline.
3. **Refill assembly** (~2.4ms): wire the best lab strategy (S1/S4) into the
   production refill path.
4. Anchor coefficient memoization across consecutive pieces (shared tuple
   prefixes) for the 0.7ms anchor slice.
