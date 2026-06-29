# Scaffold `degenbot-pathfinding` core crate
Create the new alloy-free, pyo3-free, zero-dependency leaf crate under `rust/crates/degenbot-pathfinding/`.

## Goal
- Establish the crate skeleton so all subsequent tasks build against a real workspace member
- Wire it into the workspace, the no-pyo3 gate, and the umbrella + binding layer feature graph

## Context
- The three-layer architecture (ADR-005, `rust/AGENTS.md` Three-Layer Pattern) requires a pure-Rust core leaf with zero `pyo3`
- Model after `rust/crates/degenbot-decoders/` (an alloy-only leaf) but even leaner: pathfinding needs no external crate at all (token IDs + pool IDs are plain `u64`)
- The `degenbot` umbrella crate (`rust/crates/degenbot/`) re-exports cores for standalone Rust consumers; add pathfinding there too
- `rust/crates/degenbot-python/` (the `degenbot_rs` cdylib) gets an optional path dep + `pathfinding` feature, mirroring how `decoders`/`uniswap` are wired

## Acceptance Criteria
- `rust/crates/degenbot-pathfinding/Cargo.toml` exists with `name = "degenbot-pathfinding"`, `publish = false`, the standard `[lints.rust]`/`[lints.clippy]` block (matching `degenbot-decoders/Cargo.toml`), and NO `[dependencies]` section (zero external deps)
- `rust/crates/degenbot-pathfinding/src/lib.rs` exists with a crate-level doc comment describing the leaf and a `pub mod graph;` declaration (the `graph` module file can be a stub with just the doc comment for now)
- `rust/crates/degenbot-pathfinding/src/graph.rs` exists as a near-empty stub with a module doc comment (no types yet)
- `rust/Cargo.toml`: `"crates/degenbot-pathfinding"` added to `workspace.members`
- `rust/Cargo.toml`: `[profile.release.package.degenbot-pathfinding]` block with `codegen-units = 16` added alongside the other core packages
- `rust/crates/degenbot/Cargo.toml`: `degenbot-pathfinding = { path = "../degenbot-pathfinding" }` added to `[dependencies]`
- `rust/crates/degenbot-python/Cargo.toml`: `degenbot-pathfinding = { path = "../degenbot-pathfinding", optional = true }` added to `[dependencies]`; `pathfinding = ["dep:degenbot-pathfinding"]` added to `[features]`; `"pathfinding"` added to the `default` feature list
- `justfile` `check-no-pyo3-in-cores` loop: add `degenbot-pathfinding` to the crate list
- `cargo check --manifest-path rust/Cargo.toml --workspace` passes
- `just check-no-pyo3-in-cores` passes
- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-pathfinding -- --deny warnings` is clean
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` passes

## Validation Gates
- `cargo check --manifest-path rust/Cargo.toml --workspace`
- `just check-no-pyo3-in-cores`
- `cargo clippy --manifest-path rust/Cargo.toml --all-features --all-targets -- --deny warnings`
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`
---
# Implement pure-Rust `PathGraph` + iterative DFS
Build the complete graph algorithm in the `degenbot-pathfinding` core leaf. This is sub-step A of the three-layer cutover (§3.1): pure-Rust core, zero pyo3, independently testable.

## Goal
- A fully functional, pure-Rust port of the Python pathfinding DFS that produces identical path sets
- The `#[cfg(test)]` unit tests serve as the first parity oracle; they mirror the in-memory fixture tests from `tests/pathfinding/test_permutation_filter_min_depth.py`

## Context
The Python module (`src/degenbot/pathfinding.py`) currently has these responsibilities that port to Rust:
1. **Graph construction** (`_prepare_graph`): build a multigraph (nodes = token IDs, edges = pools with `{pool_id, pool_kind}`). Python also does DB queries + dead-end pruning; the DB queries stay in Python (ORM), but the graph data structure + pruning move to Rust.
2. **Node valid depths** (`_compute_node_valid_depths`): precompute which depth positions each node can participate in, for lookahead pruning. Pure graph analysis.
3. **DFS** (`_dfs` / `_dfs_async`): recursive depth-first search from start token back to itself (cycles), with cycle detection, min/max depth bounds, `pool_type_per_depth` permutation filter, lookahead pruning, and `include_reverse` optimization. This is the hot path.

Key design decisions (resolved):
- **Pool type representation**: Python uses `type[LiquidityPoolTable | UniswapV4PoolTable]` (2 variants). Rust uses `pub enum PoolKind { V2V3, V4 }`. The mapping is 2-variant and stable. Python passes pool kinds as `u8` discriminants (0 = V2V3, 1 = V4).
- **Graph storage**: `HashMap<u64, Vec<Edge>>` adjacency list (not NetworkX dict-of-dict-of-dict). An `Edge` is `{ pool_id: u64, pool_kind: PoolKind }`. Multi-edges (parallel pools between the same token pair) are naturally supported by `Vec<Edge>`.
- **Cycle detection**: Python does `not in working_path` (linear scan, O(depth) per edge — quadratic in path length). Rust uses a `HashSet<(u64, PoolKind)>` visited set for O(1) membership test. This is a correctness-preserving optimization: the set of (pool_id, pool_kind) pairs on the current path is identical to the Python `working_path` list's contents.
- **DFS iteration**: iterative (explicit stack), not recursive. Avoids Python function-call overhead and recursion limits. The visit order must match the Python recursive DFS exactly (process neighbors in insertion order, yield complete paths, backtrack one step). The Python code iterates `graph[start_token_id].items()` — NetworkX preserves insertion order for adjacency dicts. Rust must preserve edge insertion order in the adjacency list `Vec`.
- **`include_reverse`**: when a path is yielded, also yield its reverse (`path_steps[::-1]`). Port this to Rust — it's a trivial in-place reversal before yielding.
- **`pool_type_per_depth`**: a `Vec<Option<Vec<PoolKind>>>` (length = exact hop depth). A `None` entry allows all pool kinds at that depth. Edges whose `pool_kind` is not in the allowed set are pruned before recursion. The filter implicitly caps `effective_max_depth` at its length.
- **`node_valid_depths`**: `HashMap<u64, Vec<bool>>` (or `HashSet<u64>` per node of valid depth indices). The lookahead prunse: before recursing into a neighbor, if the neighbor can't participate at the next depth, skip without recursing.

What stays in Python (I/O + orchestration):
- DB queries (SQLAlchemy) to fetch pools, tokens, addresses
- Traversal plan computation (`_prepare_traversal_plan` — Cartesian product of start/end tokens, FORWARD_AND_REVERSE optimization)
- Address resolution (map pool_id → ChecksumAddress / (manager_address, pool_hash))
- Token ID resolution (start_token → token_id lookup)
- `PathStep` dataclass construction

## API (Rust core)
```rust
/// Discriminant for the two pool-table families.
/// 0 = LiquidityPoolTable (V2/V3), 1 = UniswapV4PoolTable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PoolKind { V2V3, V4 }

/// A pool edge in the graph: which pool connects two tokens.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Edge { pub pool_id: u64, pub pool_kind: PoolKind }

/// A multigraph: token IDs are nodes, pools are edges.
pub struct PathGraph { /* adjacency list: HashMap<u64, Vec<Edge>> */ }

impl PathGraph {
    /// Build from a flat list of (token0_id, token1_id, pool_id, pool_kind) edges.
    /// Edges are added in order; neighbor iteration preserves this order.
    pub fn from_edges(edges: Vec<(u64, u64, u64, PoolKind)>) -> Self;

    /// Remove nodes with degree <= 1, repeating until no such nodes remain.
    pub fn prune_dead_ends(&mut self);

    /// Precompute valid depth positions per node, for lookahead pruning.
    /// Returns Some(map) only when pool_type_per_depth is provided.
    pub fn compute_node_valid_depths(
        &self,
        pool_type_per_depth: &[Option<Vec<PoolKind>>],
    ) -> Vec<(u64, Vec<bool>)>;

    /// Depth-first search for all valid paths from start back to end (cycles).
    /// Yields paths as Vec<(pool_id, PoolKind)> in the same order the Python DFS would.
    pub fn find_paths(
        &self,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<&[Option<Vec<PoolKind>>]>,
        node_valid_depths: Option<&HashMap<u64, Vec<bool>>>,
    ) -> Vec<Vec<(u64, PoolKind)>>;
}
```

## Acceptance Criteria
- `PoolKind`, `Edge`, `PathGraph` types defined in `graph.rs` with the API above
- `from_edges` builds a correct adjacency-list multigraph (parallel edges preserved, insertion order preserved)
- `prune_dead_ends` removes degree-0 and degree-1 nodes iteratively until fixpoint (matches Python's `while tokens_to_prune := ... graph.remove_nodes_from(...)`)
- `compute_node_valid_depths` produces identical results to the Python `_compute_node_valid_depths` for the same graph + filter
- `find_paths` produces paths in the **same order** as the Python `_dfs` for all test fixtures (edge insertion order == neighbor visit order)
- `find_paths` honors `min_depth` (no path shorter), `max_depth` (no path longer), `include_reverse` (yields reversed path after each forward path), `pool_type_per_depth` (prunes by hop-type + caps max depth), and `node_valid_depths` (lookahead pruning)
- `pool_type_per_depth` with fewer entries than `max_depth` does not panic (the implicit cap)
- `#[cfg(test)]` module in `graph.rs` with unit tests mirroring the in-memory fixture from `test_permutation_filter_min_depth.py`:
  - The 4-pool V2 graph (WETH-A parallel edges + A-B + B-WETH)
  - 3-depth V2-V2-V2 filter yields only 3-hop paths (no 2-hop leak)
  - 2-depth V2-V3 filter caps max depth
  - `include_reverse` doubles output
  - `min_depth` / `max_depth` bounds
  - `prune_dead_ends` removes dead-end tokens
- All Rust unit tests pass: `cargo test -p degenbot-pathfinding`
- Clippy clean: `cargo clippy -p degenbot-pathfinding -- --deny warnings`

## Validation Gates
- `cargo test --manifest-path rust/Cargo.toml -p degenbot-pathfinding`
- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-pathfinding --all-targets -- --deny warnings`
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`

## Notes for the implementer
- The Python DFS is in `src/degenbot/pathfinding.py` (`_dfs` function). Read it carefully before implementing — every branch (cycle yield, max-depth return, pool-type filter, lookahead) must be reproduced.
- The iterative DFS must process neighbors in **insertion order** (the order edges were added via `from_edges`). This is critical for path-ordering parity.
- Use a `HashSet<(u64, PoolKind)>` for the visited-set (cycle detection). Push/pop on yielding.
- The `find_paths` return type is `Vec<Vec<...>>` (eager, not an iterator) because Python will consume it in one PyO3 round-trip. An iterator would cross the PyO3 boundary per-yield.
---
# PyO3 wrapper: `find_paths_rust` function
Sub-step B of the three-layer cutover (§3.2): thin PyO3 translator over the core leaf.

## Goal
- Expose `PathGraph::find_paths` to Python via a single `#[pyfunction]` round-trip
- Register the symbol in the Python module so `pathfinding.py` can import it

## Context
- Binding layer lives in `rust/crates/degenbot-python/src/` (the `degenbot_rs` cdylib)
- Per-domain subdirs mirror the cores; create `rust/crates/degenbot-python/src/pathfinding/mod.rs`
- The wrapper is a thin translator: extract Python args (flat lists of ints) → build `PathGraph` → call `find_paths` → wrap result as nested Python lists
- No business logic in the wrapper (Rule 3)
- The graph construction (`from_edges` + `prune_dead_ends` + `compute_node_valid_depths` + `find_paths`) all happen in one `py.detach()` call — the entire search is GIL-released

## API (PyO3 wrapper)
```python
# Exposed as degenbot_rs._pathfinding.find_paths_rust (or similar)
def find_paths_rust(
    edges: list[tuple[int, int, int, int]],  # (token0_id, token1_id, pool_id, pool_kind_u8)
    start_token_id: int,
    end_token_id: int,
    min_depth: int,
    max_depth: int | None,
    include_reverse: bool,
    pool_type_per_depth: list[set[int] | None] | None,  # None = no filter
) -> list[list[tuple[int, int]]]  # (pool_id, pool_kind_u8) per hop
```

## Acceptance Criteria
- `rust/crates/degenbot-python/src/pathfinding/mod.rs` exists with the `#[pyfunction] find_paths_rust` wrapper
- `rust/crates/degenbot-python/src/lib.rs`: `#[cfg(feature = "pathfinding")] pub mod pathfinding;` added
- `rust/crates/degenbot-python/src/c_api.rs`: `#[cfg(feature = "pathfinding")]` block registering `find_paths_rust` via `wrap_pyfunction!`
- The wrapper:
  - Extracts `edges` as `Vec<(u64, u64, u64, u8)>` from the Python list
  - Builds `PathGraph::from_edges(...)` (mapping `u8` → `PoolKind`)
  - Calls `prune_dead_ends()` (matches the Python `_prepare_graph` behavior)
  - Optionally calls `compute_node_valid_depths()` if `pool_type_per_depth` is provided
  - Calls `find_paths(...)` inside `py.detach()` (GIL released during the search)
  - Maps the result `Vec<Vec<(u64, PoolKind)>>` → `list[list[tuple[int, int]]]` (mapping `PoolKind` → `u8`)
  - Returns the nested Python list
- `pool_type_per_depth` Python arg: each element is `None` or a `set`/`list`/`tuple` of `int` (pool kind discriminants). Empty set = no pool kinds allowed at that depth. Convert to `Vec<Option<Vec<PoolKind>>>`.
- `rust/crates/degenbot-python/tests/` has a basic import smoke test (or extend the existing python_integration test) confirming `find_paths_rust` is callable from Python
- `cargo test --manifest-path rust/Cargo.toml --workspace` passes (Rust unit + integration tests)
- `just test-rust-python` passes (PyO3-wrapped Python tests)
- Clippy clean across the workspace

## Validation Gates
- `cargo test --manifest-path rust/Cargo.toml --workspace`
- `cargo clippy --manifest-path rust/Cargo.toml --all-features --all-targets -- --deny warnings`
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`
- `uv run pytest tests/rust --ff -x -q --no-header`

## Notes for the implementer
- The wrapper must extract ALL input data into owned Rust types before `py.detach()`. No `Bound<'_, PyAny>` in the detached closure.
- Use `py.detach(|| { ... })` for the graph construction + search. The result construction (building the nested Python list) happens after, GIL held.
- `pool_type_per_depth` conversion: Python passes `list[set[int] | None] | None`. Convert each non-None element to `Vec<PoolKind>` by mapping `0 → V2V3, 1 → V4` (reject others with a ValueError).
- Follow the GIL-discipline patterns in `rust/crates/degenbot-python/src/abi/decoder.rs` as a reference for the extract → detach → wrap shape.
---
# Route Python companion through Rust + eliminate N+1 queries
Sub-step C of the three-layer cutover (§3.3): route `pathfinding.py` through the Rust seam, eliminate the N+1 address-resolution queries, and delete the dead Python DFS.

## Goal
- `find_paths` and `find_paths_async` produce identical results to the current implementation, but backed by Rust
- The per-path-per-pool SQLAlchemy address queries (the main bottleneck) are eliminated by bulk-preloading addresses into a lookup dict during graph construction
- NetworkX dependency removed from the codebase (only used in this module)

## Context
- The existing tests in `tests/pathfinding/` are the parity oracle (§4.2): they test the public `find_paths`/`find_paths_async` API. If the Rust-backed implementation passes them, parity is proven.
- The current `_prepare_graph` already queries all pools (token0_id, token1_id, pool_id) from the DB. Extend these same queries to also select `address` (V2/V3) or `manager.address, pool_hash` (V4), building `dict[int, ChecksumAddress]` and `dict[int, tuple[ChecksumAddress, str]]` lookup tables during graph construction — zero additional DB round-trips.
- The traversal plan (`_prepare_traversal_plan`) and token-ID resolution stay in Python (they're DB/ORM concerns).
- `PathStep` dataclass stays; it's the Python-facing result type.

## Acceptance Criteria
- `src/degenbot/pathfinding.py` rewritten:
  - `_prepare_graph` still queries pools from the DB, but now returns BOTH the flat edge list (`list[tuple[TokenId, TokenId, PoolId, int]]`) for Rust AND the address lookup dicts (`dict[PoolId, ChecksumAddress]` for V2/V3, `dict[PoolId, tuple[ChecksumAddress, str]]` for V4)
  - `_compute_node_valid_depths`, `_dfs`, `_dfs_async` are **deleted** (the Rust core handles these)
  - `find_paths` and `find_paths_async` build the traversal plan in Python (unchanged), then for each (start, end, direction) triple call `find_paths_rust(...)` (the PyO3 function) with the flat edge list + search options, then map the returned `(pool_id, kind_u8)` pairs to `PathStep` objects using the preloaded address lookup dicts
  - `PathStep` construction: for `V2V3` pools, `address = v2v3_address_lookup[pool_id]`; for `V4` pools, `address, hash = v4_address_lookup[pool_id]`
  - The public signatures of `find_paths` and `find_paths_async` are **unchanged** (same args, same return type `Iterator[Sequence[PathStep]]` / `AsyncIterator[Sequence[PathStep]]`)
  - `find_paths_async` can delegate to the sync Rust call (the search itself is fast + GIL-released; the async wrapper just yields results as an async iterator). No need for an async Rust path.
- `networkx` removed from `pyproject.toml` `[project.dependencies]`
- `uv lock` succeeds and no longer includes `networkx`
- `import networkx` appears nowhere in `src/degenbot/`
- All existing pathfinding tests pass unchanged:
  - `tests/pathfinding/test_pathfinding.py`
  - `tests/pathfinding/test_permutation_filter_min_depth.py`
  - `tests/pathfinding/test_pool_type_per_depth.py`
- `ruff check src/degenbot/pathfinding.py` and `ruff format --check src/degenbot/pathfinding.py` pass
- `ty check src/degenbot/pathfinding.py` passes

## Validation Gates
- `just test-python` (full suite — confirms no regressions)
- `uv run pytest tests/pathfinding -x -q --no-header` (slow tests included)
- `uv run pytest tests/pathfinding/test_permutation_filter_min_depth.py -x -q --no-header` (the deterministic in-memory fixture — fast, proves exact parity)
- `uv run ruff check --fix src/degenbot/pathfinding.py`
- `uv run ruff format --check src/`
- `uv run ty check --no-progress src/`

## Notes for the implementer
- The edge list passed to Rust must be in the same order the Python `_prepare_graph` added edges to the NetworkX graph: iterate `pool_types` in order, and within each pool type iterate the query result in DB order. This ordering is critical for path-output parity (DFS visit order depends on neighbor insertion order).
- The `pool_type_per_depth` Python arg uses `set[type]` (Python class objects). Convert to `list[set[int] | None]` by mapping each pool-type class to its `PoolKind` discriminant: `issubclass(t, LiquidityPoolTable) → 0`, `issubclass(t, UniswapV4PoolTable) → 1`.
- For `find_paths_async`: since the Rust call is sync and GIL-released, the async generator can just call it synchronously and yield results. The `asyncio.sleep(0)` calls in the old `_dfs_async` are no longer needed (they existed to yield control between recursion levels; the Rust call is a single blocking operation).
- The `Direction.FORWARD_AND_REVERSE` → `include_reverse` mapping stays: set `include_reverse=True` when direction is `FORWARD_AND_REVERSE`.
- Do NOT remove `networkx` from `pyproject.toml` until ALL tests pass with the Rust-backed implementation. Remove it as the final sub-step.
---
# Add Rust-pathfinding parity benchmark
Confirm the performance improvement and document it, mirroring `tests/perf/PROGRESS.md`.

## Goal
- A benchmark script comparing the old NetworkX-backed pathfinding against the new Rust-backed implementation, proving the speedup
- Update `tests/perf/PROGRESS.md` with the results

## Context
- `tests/perf/PROGRESS.md` already documents the Rust solver speedup investigation (2.91×); pathfinding is a parallel story
- The benchmark should use the in-memory fixture from `test_permutation_filter_min_depth.py` (deterministic, no DB dependency) to isolate the algorithmic speedup from DB I/O
- For a fair comparison, benchmark the DFS traversal only (graph construction + search), not the DB queries (which are unchanged)

## Acceptance Criteria
- `tests/perf/bench_pathfinding.py` exists, using the in-memory 4-pool V2 fixture (or a larger synthetic graph) to compare:
  - Old: NetworkX graph construction + recursive `_dfs` (can be reconstructed from the deleted code / git history if needed, or benchmark against a saved snapshot)
  - New: flat-edge-list construction + `find_paths_rust` round-trip
- Measures: graph construction time, search time, total time, paths found (correctness parity)
- `tests/perf/PROGRESS.md` updated with a "Pathfinding" section documenting the speedup
- The benchmark script runs cleanly: `uv run python tests/perf/bench_pathfinding.py`

## Validation Gates
- `uv run python tests/perf/bench_pathfinding.py` completes and prints a comparison table
- Speedup is documented in PROGRESS.md

## Notes for the implementer
- If the old implementation has already been deleted (in the prior task), reconstruct a minimal NetworkX baseline from git history (`git show HEAD~1:src/degenbot/pathfinding.py`) for the benchmark only. Keep it in the benchmark file, not in the source tree.
- The key metric is search time (the DFS traversal), since that's what moved to Rust. Graph construction will also improve (HashMap vs NetworkX dict-of-dict) but the DB queries dominate construction time in production.
- A larger synthetic graph (e.g., 100 tokens, 500 pools, depth-3 search) will show the speedup more clearly than the tiny 4-pool fixture. Generate one programmatically.