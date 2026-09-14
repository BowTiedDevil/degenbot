# Deferred ideas — search-method alternatives (analysis, 2026-09-12)

Question: can a different search method (BFS, dynamic programming,
meet-in-the-middle, Johnson's circuit enumeration) emit the same result
faster than the current structurally-pruned DFS? Short answer: **no** —
the workload is output-bound and the state space cannot merge. Details:

## Why state-merging algorithms (DP/BFS/join rewriting) cannot win here

The output contract is: every closed walk start->...->start of length in
[min,max], with EVERY POOL USED AT MOST ONCE per walk (pool-disjointness).
Node revisits are legal (e.g. WETH-a-WETH-a-WETH via four distinct pools of
the two pairs) and parallel-pool alternation cycles are common on mainnet.

- Therefore two different prefixes that reach the same (node, depth) are NOT
  interchangeable: their used-pool sets differ, and the pool sets never
  coincide as walks diverge. There is nothing for a DP table to merge.
- Any exact method must hence enumerate every candidate walk — the same
  tree the DFS walks. The only remaining question is which representation
  walks that tree with the least overhead:
  - DFS, iterative, O(depth) live state, array-index visited-set:
    1M emissions in ~10-18ms on the real mainnet graph (~55-90ns/emission,
    i.e. buffer-write bandwidth territory).
  - Level-synchronous BFS/DP frontier: retains O(level-width) walks in
    memory (mainnet depth-2 frontier ~1.5M states x (node, 2 pools, parent
    ref) = tens of MB churn), then emits the same cycles by streaming from
    parents — strictly more memory traffic than the DFS O(depth) stack,
    zero output savings, same tree size.
  - Meet-in-the-middle (join halves): for d=3 the candidate-pair count
    equals the DFS tree size exactly (sum over 2-walks of closing-hop
    count), so it does the same work plus a hash join; for d=4 both halves
    (~1.5M x 1.5M join space) exceed DFS exploration by orders of
    magnitude at mainnet scale. Rejected on memory alone (gigabytes).
  - Johnson's elementary-circuit enumeration: dedups NODE revisits —
    would SKIP legal walks (parallel-pool alternation) and fail the
    emission-multiplicity parity gate. Not output-equivalent; rejected.
- Empirical anchor: the "dumb" spec oracle (same naive-DFS shape a
  BFS-flavored or DP rewrite typically ends up as, minus hybrid
  engineering) needs 20-45s for 1M emissions on the same real-graph config
  where the library does ~10-18ms => the ~2000-4000x gap is all in the
  structural pruning + compact-index engineering, none of which an
  algorithm-class swap would preserve.
- Conclusion: refresh the DFS, don't replace it. Remaining headroom is
  emission materialization and cache behavior, both already at
  bandwidth-tier rates.

## The dominating non-DFS move (recommended for merge-back, orchestration-level)

Discovery output depends ONLY on the graph structure (edge list + depth
bounds + filter), never on pool state/prices. The live bot re-runs the
discovery->registration pipeline as the crawl re-delivers the same
structure (W73FVY soak: 74,143 duplicate candidate re-yields absorbed by
registration memos, 59,847 build-error skips answered from memos). Those
memos are treating the symptom. Treating the cause: cache the discovered
cycle set at the orchestration seam keyed by (edges fingerprint, depth
bounds, filter spec) and replay registrations instantly; re-enumerate only
when the structural fingerprint changes (new pool added/removed). This
eliminates ~all re-discovery cost per crawl iteration, which dwarfs the
per-search constant factors above. NOT implemented this session (Python
runner is out of scope for the pathfinder perf work; also requires a
product decision on cache bounds/invalidations).
