# Changelog

All notable changes to the Rust workspace crates are recorded here.

## Unreleased

### Changed

- **ADR-084 capability-surface cut (W2 - the solve door):** the
  `degenbot-pools` solve tree is curated behind intent-named doors. The
  published-path removals, with their replacements:

  | Was | Is |
  | --- | --- |
  | `degenbot_pools::solve::mixed::solve_path` | `degenbot_pools::solve::solve_path` (umbrella: `degenbot::solvers::solve_path`) |
  | `degenbot_pools::solve::mixed::solve_path_with_min_profit` | `degenbot_pools::solve::solve_path_with_min_profit` |
  | `degenbot_pools::solve::mixed::solve::SolveOutcome` | `degenbot_pools::solve::SolveOutcome` |
  | `degenbot_pools::solve::mixed::{ResolvedMixedPath, ResolvedHop, SolvePathResult, HopType, MixedPoolRef, PoolHop, MixedPath, SolveArmOutcome, UnsupportedHopIdentity, V3SnapshotData, V4SnapshotData, INT128_MAX}` | the same names at `degenbot_pools::solve::*` |
  | `degenbot_pools::solve::mixed::{BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, SolidlyHopState}` | the same names at `degenbot_pools::solve::*` (homes unchanged in `families`) |
  | `degenbot_pools::solve::mixed::simulate_solidly_path` | `degenbot_pools::solve::simulate_solidly_path` |
  | `degenbot_pools::solve::profit_envelope::{GateDeps, Envelope, GateStats, GateSkipCause, DegeneratePathSink, PrefixCache, HopMath, Line, PathBoundLines, path_*_bound*}` | `degenbot_pools::solve::gate::*` (the door module; the numeric kernel, line derivation, and capture serializer stay internal to the `profit_envelope` home) |
  | `degenbot_pools::solve::affected_keys::AffectedKey` | `degenbot_pools::affected::AffectedKey` (moved out of the solve tree) |
  | `degenbot_pools::solve::capture_fixture::read_fixture` | `degenbot_pools::solve::fixtures::read_fixture` (feature `capture-fixtures`) |

  Everything else under `solve/` (the mobius kernels, `bounded_brent`, the
  intake internals) is now `pub(crate)`: no external crate can name it. The
  door's external naming surface is pinned by
  `architecture_gates.rs::solve_door_census_is_the_external_surface` over
  `rust/crates/facade/degenbot/tests/solve_door_census.txt`.

- **ADR-084 W5 (pools namespacing, phase 1):** the 34 root modules of
  `degenbot-pools` moved under the four capability modules. The was-to-is
  rule is uniform: every `degenbot_pools::<module>` path (and every
  `crate::<module>` path inside the crate) gained its capability segment -
  `state::` for the per-family value structs, storage slots, tick map/bitmap,
  spec bounds, journal data, and family-capability declarations; `sim::` for
  the simulators, `PoolEntry`/registry, seam traits, and registration
  refusals; `families::` for the event plumbing (`buffered_event`,
  `liquidity_event`, `liquidity_event_buffer`, `state_update`); `solve::` for
  `affected`. The crate root keeps the umbrella-grade re-exports (`TickInfo`,
  the storage-slot encoders, the seam traits, `CurveInputsError`, the
  buffered-event/`PoolStateUpdate` types) so a flat import continues to work
  for the major types; module-qualified paths must add the segment. The root
  `pub mod` surface is 4, pinned by `pools_root_surface_is_pinned`.

- **ADR-084 D3 (W3 - `capture-fixtures`):** `zstd` (the C-build fixture
  dependency) is optional in `degenbot-pools` behind the non-default
  `capture-fixtures` feature; the default dependency tree of every crate -
  including `degenbot-execution`, `degenbot-rpc`, and the other seam crates -
  compiles no C library for fixtures. Test lanes enable
  `degenbot-pools/capture-fixtures` and `degenbot-bot/executor-ab-probe`
  explicitly, so the golden corpora stay executed. The dependency charter
  (`just check-dependency-charter`, ratchet #7) denies `pyo3` (outside the
  binding shell) and `zstd`/`zstd-sys` (default features) workspace-wide.

- **ADR-084 D4 (W4 - capability layers):** `degenbot-ingestion`,
  `degenbot-runs`, and `degenbot-pool-updater` moved from
  `crates/integrations/` to `crates/foundation/` (pure `git mv`; crate names
  and APIs unchanged). Zero upward role edges remain, enforced by
  `role_edge_direction_is_enforced`.

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
