"""Benchmark: old NetworkX-backed pathfinding vs new Rust-backed pathfinding.

This benchmark isolates the algorithmic speedup by using an in-memory
synthetic graph -- no DB queries. It compares:

1. **Old**: NetworkX MultiGraph construction + recursive Python DFS
   (reconstructed from the pre-Rust implementation, git history
   ``2ae63a51``).
2. **New**: flat edge-list construction + ``find_paths_rust`` lazy iterator
   (the current Rust-backed implementation).

Both implementations find the same set of paths. The benchmark measures:

- Graph construction time
- Search time (DFS traversal only)
- Total time (construction + search)
- Paths found (correctness parity)

The synthetic graph is a fully-connected graph of N tokens with M
parallel V2 pools between each pair, producing a large number of cycles.
This stresses the DFS and makes the speedup visible.

Usage::

    uv run python tests/perf/bench_pathfinding.py
"""

from __future__ import annotations

import time
from typing import TYPE_CHECKING

from degenbot.degenbot_rs import find_paths_rust

if TYPE_CHECKING:
    from collections.abc import Iterator

    from networkx import MultiGraph

# -- Pool-kind discriminants (must match Rust `PoolKind`) -------------------
V2 = 0  # UniswapV2PoolTableBase
V3 = 1  # UniswapV3PoolTableBase
V4 = 2  # UniswapV4PoolTable


def build_synthetic_edges(n_tokens: int, n_parallel_pools: int) -> list[tuple[int, int, int, int]]:
    """Build a synthetic edge list for a fully-connected graph.

    Every pair of tokens (i, j) where i < j is connected by
    ``n_parallel_pools`` parallel V2 pools. This produces a dense graph
    with many cycles -- ideal for stressing the DFS.

    Returns:
        A list of ``(token0_id, token1_id, pool_id, pool_kind_u8)`` tuples.
    """
    edges: list[tuple[int, int, int, int]] = []
    pool_id = 1000  # start at a non-zero base to look like DB IDs
    for i in range(n_tokens):
        for j in range(i + 1, n_tokens):
            for _ in range(n_parallel_pools):
                edges.append((i, j, pool_id, V2))
                pool_id += 1
    return edges


# ---------------------------------------------------------------------------
# Old implementation (NetworkX + recursive Python DFS)
# Reconstructed from git history (2ae63a51:src/degenbot/pathfinding.py).
# No DB queries -- operates on a pre-built NetworkX MultiGraph.
# ---------------------------------------------------------------------------


def build_networkx_graph(
    edges: list[tuple[int, int, int, int]],
) -> MultiGraph:
    """Build a NetworkX MultiGraph from the flat edge list."""
    import networkx as nx  # benchmark-only import

    graph: MultiGraph = nx.MultiGraph()
    for token0, token1, pool_id, pool_kind_u8 in edges:
        graph.add_edge(
            token0,
            token1,
            pool_id=pool_id,
            pool_kind_u8=pool_kind_u8,
        )
    return graph


def _dfs_old(
    *,
    start: int,
    end: int,
    working_path: list[tuple[int, int]],
    graph: MultiGraph,
    min_depth: int,
    max_depth: int | None,
    include_reverse: bool,
) -> Iterator[list[tuple[int, int]]]:
    """Recursive Python DFS -- a faithful port of the old ``_dfs`` generator.

    Yields paths as lists of ``(pool_id, pool_kind_u8)`` tuples.
    No DB queries (the original did per-pool address lookups; we skip that
    since the Rust version also defers address resolution).
    """
    if start not in graph:
        return

    if start == end and len(working_path) >= min_depth:
        yield list(working_path)
        if include_reverse:
            yield list(reversed(working_path))

    effective_max_depth = max_depth
    if effective_max_depth is not None and len(working_path) >= effective_max_depth:
        return

    for neighbor, edges_dict in graph[start].items():
        for attr in edges_dict.values():
            pool_id = attr["pool_id"]
            pool_kind_u8 = attr["pool_kind_u8"]
            edge_key = (pool_id, pool_kind_u8)

            if edge_key not in working_path:
                working_path.append(edge_key)
                yield from _dfs_old(
                    start=neighbor,
                    end=end,
                    working_path=working_path,
                    graph=graph,
                    min_depth=min_depth,
                    max_depth=max_depth,
                    include_reverse=include_reverse,
                )
                working_path.pop()


def find_paths_old(
    edges: list[tuple[int, int, int, int]],
    start_token_id: int,
    end_token_id: int,
    min_depth: int,
    max_depth: int | None,
    *,
    include_reverse: bool,
) -> list[list[tuple[int, int]]]:
    """Old implementation: NetworkX graph + recursive DFS."""
    graph = build_networkx_graph(edges)
    return list(
        _dfs_old(
            start=start_token_id,
            end=end_token_id,
            working_path=[],
            graph=graph,
            min_depth=min_depth,
            max_depth=max_depth,
            include_reverse=include_reverse,
        )
    )


# ---------------------------------------------------------------------------
# New implementation (Rust-backed lazy iterator)
# ---------------------------------------------------------------------------


def find_paths_new(
    edges: list[tuple[int, int, int, int]],
    start_token_id: int,
    end_token_id: int,
    min_depth: int,
    max_depth: int | None,
    *,
    include_reverse: bool,
) -> list[list[tuple[int, int]]]:
    """New implementation: flat edge list + Rust lazy DFS iterator."""
    path_iter = find_paths_rust(
        edges,
        start_token_id,
        end_token_id,
        min_depth,
        max_depth,
        include_reverse,
        None,  # no pool_type_per_depth filter
    )
    return [list(path) for path in path_iter]


# ---------------------------------------------------------------------------
# Benchmark runner
# ---------------------------------------------------------------------------


def _fmt_ms(ns: float) -> str:
    return f"{ns / 1_000_000:,.1f}"


def bench(
    label: str,
    n_tokens: int,
    n_parallel_pools: int,
    depth: int,
    *,
    include_reverse: bool = False,
) -> None:
    edges = build_synthetic_edges(n_tokens, n_parallel_pools)
    start_token = 0
    end_token = 0  # cycle back to start

    print(f"\n{'=' * 70}")
    print(f"  {label}")
    print(
        f"  graph: {n_tokens} tokens, {n_parallel_pools} parallel pools/pair, "
        f"{len(edges)} total edges"
    )
    print(
        f"  search: start={start_token}, end={end_token}, "
        f"min_depth={depth}, max_depth={depth}, "
        f"include_reverse={include_reverse}"
    )
    print(f"{'=' * 70}")

    # --- Old (NetworkX + Python DFS) ---
    # Graph construction
    t0 = time.perf_counter_ns()
    graph = build_networkx_graph(edges)
    t_construct_old = time.perf_counter_ns() - t0

    # Search
    t0 = time.perf_counter_ns()
    old_paths = list(
        _dfs_old(
            start=start_token,
            end=end_token,
            working_path=[],
            graph=graph,
            min_depth=depth,
            max_depth=depth,
            include_reverse=include_reverse,
        )
    )
    t_search_old = time.perf_counter_ns() - t0
    t_total_old = t_construct_old + t_search_old

    # --- New (Rust-backed) ---
    # Graph construction is implicit (find_paths_rust builds it internally);
    # we measure the full Rust call (construction + search) as one, then
    # also measure search-only by constructing the graph separately.
    # The Rust API doesn't expose graph construction separately, so we
    # measure the total find_paths_rust call and subtract by measuring
    # construction + search as one unit.

    t0 = time.perf_counter_ns()
    path_iter = find_paths_rust(
        edges,
        start_token,
        end_token,
        depth,
        depth,
        include_reverse,
        None,
    )
    new_paths = [list(p) for p in path_iter]
    t_total_new = time.perf_counter_ns() - t0

    # --- Correctness parity ---
    # Both should find the same number of paths. Compare as sets of
    # sorted tuples to handle ordering differences.
    old_set = {tuple(sorted(p)) for p in old_paths}
    new_set = {tuple(sorted(p)) for p in new_paths}

    parity_ok = old_set == new_set

    print(f"\n  {'Metric':<30} {'Old (NetworkX)':>15} {'New (Rust)':>15} {'Speedup':>10}")
    print(f"  {'-' * 70}")
    print(f"  {'Construction (ms)':<30} {_fmt_ms(t_construct_old):>15} {'--':>15}")
    print(f"  {'Search (ms)':<30} {_fmt_ms(t_search_old):>15} {'--':>15}")
    print(
        f"  {'Total (ms)':<30} {_fmt_ms(t_total_old):>15} {_fmt_ms(t_total_new):>15} "
        f"{t_total_old / t_total_new:>9.1f}x"
    )
    print(f"  {'Paths found':<30} {len(old_paths):>15} {len(new_paths):>15}")
    print(f"  {'Parity (set match)':<30} {'OK' if parity_ok else 'MISMATCH':>15}")

    if not parity_ok:
        only_old = old_set - new_set
        only_new = new_set - old_set
        if only_old:
            print(f"  Only in old ({len(only_old)}): e.g. {list(only_old)[:2]}")
        if only_new:
            print(f"  Only in new ({len(only_new)}): e.g. {list(only_new)[:2]}")


def main() -> None:
    print("Pathfinding Benchmark: NetworkX (old) vs Rust (new)")
    print("No DB queries -- pure algorithm benchmark on synthetic graphs.")

    bench(
        "Small graph (3 tokens, 2 parallel pools, depth 3)",
        n_tokens=3,
        n_parallel_pools=2,
        depth=3,
    )

    bench(
        "Medium graph (5 tokens, 3 parallel pools, depth 3)",
        n_tokens=5,
        n_parallel_pools=3,
        depth=3,
    )

    bench(
        "Dense graph (6 tokens, 4 parallel pools, depth 3)",
        n_tokens=6,
        n_parallel_pools=4,
        depth=3,
    )

    bench(
        "Depth-4 (5 tokens, 3 parallel pools, depth 4)",
        n_tokens=5,
        n_parallel_pools=3,
        depth=4,
    )

    bench(
        "With reverse (5 tokens, 3 parallel pools, depth 3, include_reverse)",
        n_tokens=5,
        n_parallel_pools=3,
        depth=3,
        include_reverse=True,
    )


if __name__ == "__main__":
    import sys

    # The old implementation uses NetworkX, which was removed from deps.
    # We try a best-effort import; if not available, skip the old benchmark.
    try:
        import networkx  # noqa: F401 -- availability check

        has_networkx = True
    except ImportError:
        has_networkx = False
        print(
            "WARNING: networkx is not installed (it was removed from deps). "
            "The old (NetworkX) benchmark will be skipped.",
            file=sys.stderr,
        )

    if has_networkx:
        main()
    else:
        # Just run the new implementation to confirm it works
        print("\nRunning Rust-only benchmark (NetworkX not available):")
        edges = build_synthetic_edges(5, 3)
        path_iter = find_paths_rust(
            edges, 0, 0, 3, 3, include_reverse=False, pool_type_per_depth=None
        )
        paths = [list(p) for p in path_iter]
        print(f"  5 tokens, 3 parallel pools, depth 3: {len(paths)} paths found")
