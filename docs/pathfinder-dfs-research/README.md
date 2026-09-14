# Autoresearch archive: pathfinder DFS speedup (session 01a09292)

Salvaged from the `autoresearch/01a09292-459c-7539-a0d9-e6d24f2fdeb0` worktree
and branch before removal (2026-09-14). The branch ran the harness
2026-09-11 and is superseded; its load-bearing conclusions LANDED in main:

- `4bbee9caa` perf(pathfinding): linear 2-core prune, CSR adjacency,
  multiply-shift interning (the branch-side twins: d74f1c22c, ef4e76644).

## Structure-cache recommendation (RESOLVED, 2026-09-14: measured unwarranted)

The search-method alternatives analysis (see `autoresearch.md`) rejected
BFS/DP/Johnson/MITM and recommended a **structure cache at the runner layer**.
Promoted to ergo task XJOMWG and resolved by measurement: the latch landed
as 2d3937700 already eliminates the repeated-unbounded re-enumeration the
recommendation targeted (the 74k-per-24-min re-yield soak was a manual
discover loop, now short-circuited to a ~50ms edition probe); discovery
runs once at startup and trigger_discovery has no automated caller, so
categories the cache would serve (repeated identical bounded triggers,
post-trim replay) are unexercised. Full Phase A measurement table in the
XJOMWG ergo body. Side-finding worth noting: the async per-yield sleep(0)
path makes the first sweep ~3x slower than sync find_paths (106.9s vs
36.2s on the real chain-1 graph).

## Files

- `autoresearch.md` — objective, correctness contract, metrics, method
- `EXPERIMENTS.md` / `INVESTIGATIONS.md` — the experiment ledgers
- `autoresearch.ideas.md` — the idea backlog
- `autoresearch.jsonl` — raw harness ledger
