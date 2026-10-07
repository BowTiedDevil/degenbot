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
    pool_family_tags,
    prepare_traversal_plan,
    resolve_directions,
)

# Ordering contract: both inner modules resolve names through this
# partially-initialized package, so the `_ffi` bindings above MUST precede
# them, and `_kinds` MUST precede `_pathfinding`. `_kinds` takes
# `PoolKind`/`pool_family_tags` as bound here and mints the family taxonomy
# from the core's exported tag list rather than re-declaring it;
# `._pathfinding` in turn consumes the minted names. The order is also
# isort's preferred one (first-party absolute import, then the relative
# imports sorted), so no import-order suppression belongs on these lines.
from ._kinds import (
    ALL_POOL_KINDS,
    FAMILY_TAG_TO_POOL_KIND,
    POOL_FAMILY_TAGS,
    POOL_KIND_TAG,
)
from ._pathfinding import PathfindingRequest, PathStep, find_paths, find_paths_async

__all__ = [
    "ALL_POOL_KINDS",
    "FAMILY_TAG_TO_POOL_KIND",
    "POOL_FAMILY_TAGS",
    "POOL_KIND_TAG",
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
    "pool_family_tags",
    "prepare_traversal_plan",
    "resolve_directions",
]
