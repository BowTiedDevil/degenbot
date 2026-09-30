# Changelog

All notable changes to the Rust workspace crates are recorded here.

## Unreleased

### Changed

- **degenbot-pathfinding:** the core now floors the effective minimum depth at a
  per-depth pool-kind filter's length (`effective_min_depth = max(min_depth,
  filter.len())` in `BundledSearch::with_params`). Per policy D1, a filter of
  length N pins an exactly-N-hop permutation regardless of kind constraints —
  all-`None` filters included. This is an enumeration-semantics change for
  direct callers of `find_paths` / `find_paths_iter` / `OwnedPathFinder::new`
  passing `min_depth < filter.len()`: walks shorter than the filter that
  prefix-matched its early depths no longer leak through. Callers via the
  Python driver are unaffected — the plan layer (`prepare_traversal_plan`)
  already applied the same floor, and re-applying it is an idempotent `max()`.
  The two benches that passed a redundant all-`None` filter
  (`pathfinding_sweep/grid_4x4_d5`, `pathfinding_snapshot/native_min2_emd3`)
  now pass `max_depth` alone (their paired `node_valid_depths` tables dropped
  with the filters) and enumerate exactly the path sets they enumerated before.
