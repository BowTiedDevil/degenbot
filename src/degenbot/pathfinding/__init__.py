"""Pathfinding utilities for discovering arbitrage routes.

Barrier module (ADR-013: the Pydantic barrier): bridges the Rust
``build_path_graph`` / ``find_paths_rust`` seams from ``_ffi`` and re-exports
the deep pathfinding logic from :mod:`._pathfinding`. Importers should use::

    from degenbot.pathfinding import find_paths, find_paths_async

rather than reaching into ``degenbot._ffi`` directly.
"""

from degenbot._ffi import (
    PathGraph,
    PathStepBuilder,
    PoolKind,
    build_path_graph,
    call_blocking_on_ambient_runtime,
    classify_pool_kind,
    classify_pool_kinds,
    convert_pool_type_filter,
    find_paths_async_rust,
    find_paths_rust,
    prepare_traversal_plan,
    resolve_directions,
)

from ._pathfinding import PathfindingRequest, PathStep, find_paths, find_paths_async

__all__ = [
    "PathGraph",
    "PathStep",
    "PathStepBuilder",
    "PathfindingRequest",
    "PoolKind",
    "build_path_graph",
    "call_blocking_on_ambient_runtime",
    "classify_pool_kind",
    "classify_pool_kinds",
    "convert_pool_type_filter",
    "find_paths",
    "find_paths_async",
    "find_paths_async_rust",
    "find_paths_rust",
    "prepare_traversal_plan",
    "resolve_directions",
]
