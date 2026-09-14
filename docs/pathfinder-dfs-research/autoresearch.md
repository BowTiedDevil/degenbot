# Autoresearch: speed up the pathfinder DFS while preserving correctness

## Objective

Reduce the discovery time of the Rust pathfinding DFS
(`rust/crates/degenbot-pathfinding/src/graph.rs` → `OwnedPathFinder`) used by
the MEV bot's arbitrage-path discovery. The DFS enumerates simple pool-cycle
paths from `start` back to `end` with min/max depth bounds and an optional
per-depth pool-kind filter.

**Correctness contract (per project owner):** enumeration ORDER is NOT a
contract and may change freely. The emitted path SET must exactly match the
current behavior — **no skipped paths and no duplicate emissions**. (Note:
unless a final change alters the FFI-visible surface, nothing Python-side
should need updating; if a final change DOES alter enumeration order in a way
a Python regression test pins down, re-run the full Python suite during
merge-back and update such tests deliberately.)

## Metrics

- **Primary**: `search_ms` (ms, lower is better) — total median DFS iteration
  time across 9 synthetic configs, measured by
  `rust/crates/degenbot-pathfinding/src/bin/pathbench.rs` (9 reps/config,
  median per config). This exercises the PRODUCTION entry point
  (`OwnedPathFinder`, not `PathGraph::find_paths`).
- **Secondary**: `construct_ms` (graph build + prune), `prepare_ms`
  (`OwnedPathFinder::new`: end-edge index + node-valid-depths). Don't
  regress these wildly to win `search_ms` — report them every run.
- **Target**: none fixed — keep improving until ideas are exhausted; report
  the best achieved speedup vs baseline at the end.

## Correctness gates (hard)

1. `pathbench` parity: every config's deduplicated emitted-cycle set must
   equal the oracle's (a simple reference DFS in the bench binary written
   against the spec). Missing `CHECK parity_all=1` → skipped paths → FAIL.
2. Emission multiplicity: with `include_reverse=false` each unordered cycle
   pair {C, reverse(C)} must be emitted exactly 2×; with `include_reverse=true`
   exactly 4×. Missing `CHECK dup_all=1` → duplicates → FAIL.
3. `autoresearch.checks.sh`: `cargo test -p degenbot-pathfinding --release`
   plus clippy (workspace lints: `warnings = deny`, pedantic clean).

## How to Run

`bash autoresearch.sh` — builds the leaf crate (fast, zero deps) and runs
`pathbench` in release mode.

## Files in Scope

- `rust/crates/degenbot-pathfinding/src/graph.rs` — the DFS + graph. Main
  target. `PathFinder` (borrowed) and `OwnedPathFinder` duplicate the
  algorithm; prefer extract-shared-core refactors so both stay correct, or
  update both consistently. `OwnedPathFinder` is the production path.
- `rust/crates/degenbot-pathfinding/src/lib.rs` — crate exports/docs.
- `rust/crates/degenbot-pathfinding/src/bin/pathbench.rs` — benchmark +
  oracle; may EXTEND configs closer to production shapes, but NOT rewrite
  the oracle or loosen checks to flatter a change (no overfitting/cheating).
- `autoresearch.sh`, `autoresearch.checks.sh`, `autoresearch.md`.

## Off Limits

- `rust/crates/degenbot-python/**` (PyO3 layer) — API shape frozen for this
  exercise; changes there require `.so` rebuild + Python tests (merge-back only).
- Python side (`src/degenbot/**`, `tests/**`) — same.
- The oracle + checks in `pathbench.rs` — they are the correctness anchors.
- Deleting/skipping gates to force a green run = cheating.

## Constraints

- No new external dependencies (the crate is a **zero-dependency leaf**; the
  bench binary must not add deps either). `std` only.
- Parallelism IS now permitted (order is free) — e.g. `std::thread` — but the
  emitted path SET and multiplicity must be preserved exactly, and no deps
  (so no rayon).
- Workspace lint policy: `warnings = deny`, clippy pedantic clean.

## What's Been Tried

(baseline — unmodified algorithm; awaiting first experiments)
