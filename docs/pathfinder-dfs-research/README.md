# Autoresearch archive: pathfinder DFS speedup (session 01a09292)

Salvaged from the `autoresearch/01a09292-459c-7539-a0d9-e6d24f2fdeb0` worktree
and branch before removal (2026-09-14). The branch ran the harness
2026-09-11 and is superseded; its load-bearing conclusions LANDED in main:

- `4bbee9caa` perf(pathfinding): linear 2-core prune, CSR adjacency,
  multiply-shift interning (the branch-side twins: d74f1c22c, ef4e76644).

## Open recommendation (NOT landed)

The search-method alternatives analysis (see `autoresearch.md`) rejected
BFS/DP/Johnson/MITM and recommended a **structure cache at the runner layer**
for the enumeration — that recommendation is unimplemented and tracked
separately in the ergo backlog.

## Files

- `autoresearch.md` — objective, correctness contract, metrics, method
- `EXPERIMENTS.md` / `INVESTIGATIONS.md` — the experiment ledgers
- `autoresearch.ideas.md` — the idea backlog
- `autoresearch.jsonl` — raw harness ledger
