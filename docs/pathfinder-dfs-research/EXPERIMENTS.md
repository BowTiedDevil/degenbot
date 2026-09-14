# Pathfinder DFS Optimization — Experiment Ledger

Contract: emitted path SET must match baseline exactly. No skipped paths,
no duplicate emissions. Enumeration ORDER is free (per project owner).
Gates: synthetic multiset+multiplicity parity vs oracle; real-graph count
parity vs cap-identical oracle; cargo test; clippy (workspace pedantic).

## Harness

- pathbench: 8 synthetic configs (full multiset parity) + 4 real-mainnet
  configs from edges.json (742,837 edges dumped from ~/.config/degenbot/
  degenbot.db chain=1 mirroring fetch_path_graph_edges; V4 ids offset 2^32).
  Hub token = 2 (degree ~580k pre-prune). Real configs capped at 5M emissions
  (count parity vs cap-identical oracle); synthetic full enumeration.
- Baseline B0 (commit 288b8c26f): search_ms=432.27 · construct_ms=56,332.7
  (dominated by prune_dead_ends on the real graph — production startup cost!)
  · prepare_ms=22.77 · parity_all=1 · dup_all=1 (after gate aggregation fix).

## Experiments

### B0 baseline (kept) — unmodified algorithm
search_ms=432.27. Real per-config search: ~91-125ms for 5M emissions each.

### Exp1 dist-to-end BFS pruning (DISCARDED, reverted)
Precompute BFS hops-to-end per node; prune neighbor if dist[nb] + L + 1 > emd.
Result: search_ms=467.52 (+8.2% WORSE). At depth-3 from start==end, the
penultimate-hop condition (dist[x]<=1) coincides with the existing
end_edges/penultimate-hop prune, so the extra per-edge check is pure overhead.
Reverted in full. Idea closed for depth-3 hub searches; would need deeper
searches (d4+) on non-hub starts to matter — not production shapes here.

### Exp2 linear-time 2-core prune (KEPT, commit pending)
Degree-array + FIFO peel, single ordered retain rebuild. Result:
construct_ms 56,332.7 -> 130.4 (from_edges 113.9 + prune 16.5), 432x.
search_ms 438.98 vs 432.27 (within noise; prune does not affect the DFS).
Gates: parity_all=1 dup_all=1 incl. real-graph count parity vs oracle on the
RAW edge list (proves the 2-core keeps every cycle). Wins production session
startup (bot was spending ~1min building the pathfinding graph per session).

### Exp3 production yield path (KEPT, aec50004a)
Bench phase 3 switched to \`next_path_indices_into\` (buffer reuse). Aggregate
search_ms 416.46 at the old 5M cap — modest measurement correction, not an
impl change.

### Exp4 parallel first-hop partition (DISCARDED, reverted)
`new_shared` + `with_partition` (std::thread fan-out over first-hop pool
edges). Correctness holds (set+multiplicity preserved; count parity green),
BUT at production-shaped per-chunk work serial is 13x faster: 9.2ms vs
120.7ms for the real configs at 100k emissions. Live discovery yields small
lazy chunks; thread prep + fan-out overhead swamp the 2ms/config searches.
Idea closed unless discovery switches to big batched enumerations.

## Harness iteration speed (fixed)
- Pathbench alone took 24.5 min/run: the materializing oracle spent ~20 min
  re-deriving counts for the capped real configs (5M canonical vector
  sorts). Replaced with an allocation-free counting oracle with correct
  emission semantics (mult=1 or 2 with reverses); CHECK_CAP 5M -> 100k.
- Whole loop now ~29s (was ~20-25 min). Phase timing INFO lines added.

## Current baseline (for future experiments)
- construct_ms=127.6 (from_edges ~110 + prune ~16), prepare_ms=22.8,
  search_ms=9.4 (real configs: 2.6/1.4/2.6/2.3ms per 100k emissions,
  reps=9).

### Exp5 CSR adjacency + two-pass construction (KEPT, ef4e76644)
`Vec<Vec<CompactEdge>>` -> flat `adj_flat`+`adj_offsets` (per-node cursor
fill preserves insertion order). construct 127.6 -> 106.4ms (from_edges
130->92), search 9.38 -> 8.65. Gates + 19 unit tests green (count parity on
the real graph proves the fill order matches).

### Exp6 FxHash-style u64 hasher for token interning (KEPT, this commit)
std SipHash -> single multiply-xor round (`U64Hasher`+`U64BuildHasher`) on the ~1.5M `intern_token` lookups. from_edges 92.3 ->
58.7ms (-36%); construct 106.4 -> 72.0. Keys are internal IDs (no adversarial
input), so the removed DoS hardening is immaterial. Gates green.

## Session cumulative (real mainnet graph, 742,837 edges)
- construct (session startup): 56,333 -> 72.0 ms   (~780x)
- search (100k emissions/config): baseline-equivalent 12.7 -> 7.9 ms (-38%,
  incl. CSR cache locality; parallel fan-out rejected at this chunk scale)
- All correctness gates green throughout: synthetic multiset+multiplicity
  parity vs an independent oracle, real-graph count parity, cargo test,
  clippy pedantic (workspace warnings=deny).

### Exp7 oracle-count cache + 1M cap (KEPT, a3715c47a)
Counting-oracle results are pure functions of (edges, config, cap, oracle
semantics) — independent of the impl under test — so they are cached in
autoresearch.oracle-cache.txt keyed by an FNV of edges content + config +
cap + a manual ORACLE_FINGERPRINT (bump on oracle changes). First run paid
~136s populating; subsequent loops run in <1s (from 18-25 min at session
start). Real-config workload raised to 1M emissions for timing precision.

### Exp8 cache-dense pool_kinds byte (DISCARDED, reverted)
Separate `Vec<u8>` of pool kinds for hot filter checks. Measured WORSE
(72.2 vs 59-68 search_ms): the fast-path double-check pessimized the common
case, and filter branches are sparse in the real configs. Reverted.

## Integration validation (merge-back gate)
Fresh .so built from the worktree via `uv run maturin develop`; build
receipt reports "extension is fresh"; tests/test_build_info.py +
tests/arbitrage/test_synthetic_v2_round_trip.py: 3 passed. The PyO3 layer is
unchanged and the leaf's public API is identical, so the CSR/prune/hasher
changes are ABI-transparent for Python callers.

## Merge-ready commits (main..HEAD on the worktree branch)
- b3e4337c8 perf(pathfinding): linear-time 2-core dead-end prune (432x)
- d74f1c22c perf(pathfinding): multiply-shift u64 hasher (from_edges 92->59ms)
- ef4e76644 perf(pathfinding): CSR adjacency + two-pass construction
- (plus autoresearch harness commits — merge at reviewer's option; the
  pathfinding perf commits are the deliverable)

## Remaining ideas (diminishing returns, would need fresh signal)
- advance() micro-opts below the ~15-20% noise band of the 1M-emission runs
- from_edges: remaining ~54ms is edge iteration + arena; sort-based
  interning looked strictly worse than the cheap hash
- NOTE: main now has W73FVY runner changes (d7979c228) + possibly more;
  rebase the three perf commits onto latest main and re-run the full
  Python arbitrage suite at merge time.
- Notes: subagent W73FVY work (runner registration memos) committed to main
  as d7979c228; merge-back later must not clobber src/degenbot/** changes.

## Depth coverage beyond 3 hops (post-merge validation, on the landed graph.rs)
Added real-graph configs real_d4 (strict 4-hop) and real_d2_4_open
(min 2, max 4 — the open range also covers node-revisit "alternation"
cycles, which only exist at depth >= 4). Count parity vs the independent
oracle PASSES at 1M emissions for BOTH on the real mainnet graph (oracle
population ~64s once, then cached). Combined with the synthetic multiset
gates at depths 3, 4, 5 + filter + reverse, the three optimizations are
verified depth-agnostic. Rationale: prune is a depth-agnostic fixpoint
(identical fixpoint to the previous round-based prune); CSR is a
layout-only change whose per-node cursor fill provably preserves
enumeration order; the hasher never affects emitted values.

## Search-method alternatives (analyzed 2026-09-12, REJECTED — see
## autoresearch.ideas.md for the full argument)
- BFS/DP/level-synchronous frontiers: pool-disjointness makes walk states
  unmergeable, so any exact method walks the same tree; BFS adds O(width)
  memory churn with zero output savings. Workload is OUTPUT-BOUND
  (~55-90ns/emission incl. buffer writes; naive reference: 20-45s for the
  same 1M emissions => ~2000-4000x gap is engineering, not algorithm class).
- Johnson's circuit enumeration: node-simple only — skips legal
  parallel-pool-alternation walks (emission-multiplicity contract broken).
- Meet-in-the-middle: d=3 join count equals the DFS tree size; d=4 exceeds
  it by orders of magnitude with gigabyte frontiers.
- The dominating remaining move is ORCHESTRATION-level: cache the discovered
  cycle set keyed by the structural edge fingerprint (discovery output never
  depends on pool state/prices; live soak shows 74k duplicate re-yields per
  24-min window). Belongs to the runner layer — out of scope for the Rust
  leaf, recommended at merge-back.
