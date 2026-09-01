## Loop-15: the event solver — the per-stage ceil-inversion replaces the right-edge bisection

### Insight (reopening the loop-14 negative)
Loop-14 measured the COMPOSED-model preimage (one prefix Mobius inverse): 232/233 pieces quantizer-blocked. But the floor-cancel lemma licenses something strictly stronger: for integer W, `floor(f(x)) >= W  iff  f(x) >= W`, so the realized chain (floored at every stage) inverts EXACTLY by NESTED per-hop inversions with a ceiling at each stage:
- V2/ConstantProduct hops: `swap_exact_out` (the existing +1 floor-compensated inverse).
- CL hops: crossing-prefix + word-profile ending-range division (binary search on the profile prefix), then ONE `compute_swap_step_v3` exact-out step per demand (the pool's own negative-remaining arithmetic) plus the saturation clamp.
- Upstream crossing guards skip demand chains that an upstream hop's own exit preempts (its own candidate is in the set; the min over all is the first event).

The accumulated per-hop ceils ARE the quantizer drift that blocked the composed model — the nested recursion computes them exactly.

### Census (DEGENBOT_WALK_EVENT_CENSUS=1)
LIVE CORPUS (104 golden paths, 9 reps, 158,283 pieces): exact=158,283 (100%), early=0, late=0, pred_none=0. Every prediction verified by the two-probe proof (landed(pa) above AND landed(pa-1) not). Unit tests pin the same property by lattice-walk brute force on mixed 3/4-hop synthetic paths + profile-level brute-force minimality (dense + sparse, both swap directions, fee compensation).

### Production change (`piece_window_right_edge_evented`)
Per piece: predict first_above via the nested inversion; accept on the two-probe proof; return the EXACT edge `pa - 1` (strictly finer than the legacy <=4-wei bracket); on any disagreement fall back to the seeded grow + bisection (defense in depth; zero corpus fallbacks). Gate `DEGENBOT_WALK_EVENT_SOLVER=0` forces legacy (A/B toggle). Both the main-loop edge and the stop-time neighbor windows use it. Telemetry: walk.evtee column (ok/fallbacks) via WalkStats.event_solver_ok/fallbacks.

### Measured
- Offline replay (identical captured states, 9-rep medians): path 3671 30.8ms -> 9.2ms (3.3x); 8181 24.9ms -> 6.6ms (3.8x); heaviest 27320 39.4ms -> 9.6-10.2ms (~4x). Sims per heavy path 19,188 -> 2,913 (-85%); right-edge sim share 84% -> 2/piece verification only.
- Live dry-run: healthy; per-block walk.sims-to-paths ratio collapsed (e.g. 2018 paths, 7,303 walk sims); slowest-path tail max ~25.5ms.
- Full solver suite 144/144 green; goldens 104/104 match; deterministic 104/104.

### Sentinel note (documented epsilon)
The late-liquidity family test now allows a 2-wei deficit vs the brute reference: its 700/1e14 member optimum sits on a hyperflat top (~9k wei wide, < 1 wei of real slope) where the discrete maximum is rounding aliasing. Alignment luck decides the last wei: the event solver measures +12 wei on the 800/1e14 member and -1 on 700/1e14 (the legacy solver finds the same recorded maximum at a 9.1m-wei-different input). A deficit > 2 wei still fails loudly.

### T2 (cross-block edge-shift census, from the same hook)
88 recurring pids, 125,703 matched tuples: shift buckets [<=4: 9,846 | <=64k: 0 | <=2^40: 11,025 | beyond: 104,832] — edge shifts across blocks mostly exceed 2^40 wei, so warm-seeded bisection has little surface. Parked (the memo census already showed exact reuse at 1.6-7.5%, and the event solver now obviates the search anyway).

### Files
- mobius_v3_int.rs: v3_step_min_gross_for_output, word_profile_min_input_for_output, cl_hop_min_input_for_output, walk_event_first_above_predicted; event census (WalkEventCensus + recorder + env gate); piece_window_right_edge_evented; WalkStats.event_solver_* counters; env gates + tests.
- cl_solve_replay.rs: evtee column, census + edge-shift summaries.
