"""Pathfinding driver: DB/token I/O + Rust seam calls only.

Plan assembly (`prepare_traversal_plan`), the pool-kind table
(`classify_pool_kinds` / `convert_pool_type_filter`), and `PathStep`
construction (`PathStepBuilder`) all live in the Rust core.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from functools import partial
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import db_resolve_token_ids
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.pathfinding import (
    PathStepBuilder,
    PoolKind,
    build_path_graph,
    call_blocking_on_ambient_runtime,
    classify_pool_kinds,
    convert_pool_type_filter,
    find_paths_async_rust,
    find_paths_rust,
    prepare_traversal_plan,
)

if TYPE_CHECKING:
    import pathlib
    from collections.abc import AsyncGenerator, Iterable, Iterator, Sequence

    from degenbot.types.chain import ChecksummedAddress

type PoolId = int
type TokenId = int

# Minimum elapsed wall-clock between `find_paths_async` progress heartbeats.
_DISCOVERY_HEARTBEAT_INTERVAL_S: float = 15.0


@dataclass(slots=True, frozen=True)
class PathStep:
    """PathStep class."""

    address: ChecksummedAddress
    type: PoolKind
    hash: str | None = None


@dataclass(slots=True)
class _PreparedGraph:
    """The flat edge list + the Rust path-step builder from `_prepare_graph`."""

    edges: list[tuple[TokenId, TokenId, PoolId, PoolKind]]
    step_builder: PathStepBuilder


def _prepare_graph(
    chain_id: int,
    pool_types: Sequence[PoolKind],
    database_path: pathlib.Path,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> _PreparedGraph:
    """Build the flat edge list + step builder for the Rust DFS.

    The Rust `build_path_graph` seam opens the explicit file path.

    Returns:
        A ``_PreparedGraph`` with flat edges + the Rust step builder.

    """
    start = time.perf_counter()
    raw = build_path_graph(
        database_path=str(database_path),
        chain_id=chain_id,
        pool_kinds=classify_pool_kinds(pool_types),
        allowed_intermediate_token_ids=allowed_intermediate_tokens,
    )
    candidate_tokens: set[TokenId] = set(raw["candidate_tokens"])
    logger.debug(f"Found {len(candidate_tokens)} candidate tokens held by 2 or more pools")
    if allowed_intermediate_tokens is not None:
        logger.debug(f"Token whitelist applied: {len(candidate_tokens)} candidate tokens")

    step_builder = PathStepBuilder(
        v2v3_addresses=raw["v2v3_addresses"],
        v4_lookups=raw["v4_lookups"],
        step_cls=PathStep,
    )
    logger.debug(
        f"Built graph at +{time.perf_counter() - start:.1f}s: {len(raw['edges'])} edges",
    )
    return _PreparedGraph(edges=list(raw["edges"]), step_builder=step_builder)


@dataclass(slots=True, frozen=True)
class _Traversal:
    """One `(start, end, direction)` DFS traversal over a prepared graph."""

    prepared: _PreparedGraph
    start_token_id: TokenId
    end_token_id: TokenId
    include_reverse: bool
    min_depth: int
    pool_kind_filter: list[set[PoolKind] | None] | None


@dataclass(slots=True, frozen=True)
class PathfindingRequest:
    """Search parameters shared by `find_paths` and `find_paths_async`.

    Fields:
        chain_id: Chain ID restricting pool and token queries.
        start_tokens: Token addresses that begin a path.
        end_tokens: Token addresses that end a path.
        database_path: File-backed SQLite database opened by Rust read seams.
        min_depth: Minimum hops in yielded paths.
        max_depth: Optional maximum hops in yielded paths.
        pool_types: Typed pool families to include (default V2/V3/V4).
        pool_type_per_depth: Optional per-depth allowed pool-family sets; a
            ``None`` entry allows all kinds at that depth.
        allowed_intermediate_tokens: Optional intermediate-token whitelist.

    """

    chain_id: int
    start_tokens: Iterable[ChecksummedAddress | str]
    end_tokens: Iterable[ChecksummedAddress | str]
    database_path: pathlib.Path
    min_depth: int = 2
    max_depth: int | None = None
    pool_types: Sequence[PoolKind] = (PoolKind.V2, PoolKind.V3, PoolKind.V4)
    pool_type_per_depth: Sequence[set[PoolKind] | None] | None = None
    allowed_intermediate_tokens: Iterable[ChecksummedAddress | str] | None = None


def _resolve_boundary_token_ids(
    chain_id: int,
    tokens: set[ChecksummedAddress],
    database_path: pathlib.Path,
    label: str,
) -> list[TokenId]:
    """Resolve every boundary address to its token id, preserving set order.

    Returns:
        The token ids in the same iteration order as ``tokens``.

    Raises:
        DegenbotValueError: A boundary token is absent from the database.

    """
    resolved = db_resolve_token_ids(str(database_path), chain_id, list(tokens))
    ordered: list[TokenId] = []
    for token in tokens:
        token_id = resolved.get(token)
        if token_id is None:
            msg = f"{label} token {token} was not found in the database."
            raise DegenbotValueError(message=msg)
        ordered.append(token_id)
    return ordered


def _prepare_traversals(request: PathfindingRequest) -> list[_Traversal]:
    """Resolve boundary token ids, then build the graph + Rust traversal plan.

    Returns:
        One `_Traversal` per `(start, end, direction)` plan entry.

    """
    # @dev Liquidity pool lookups using a token ID are implicitly filtered for
    # the chain ID, since token addresses are unique to the chain.
    allowed_token_ids: set[TokenId] | None = None
    if request.allowed_intermediate_tokens is not None:
        allowed_token_ids = set(
            db_resolve_token_ids(
                str(request.database_path),
                request.chain_id,
                [get_checksum_address(tok) for tok in request.allowed_intermediate_tokens],
            ).values(),
        )

    prepared = _prepare_graph(
        chain_id=request.chain_id,
        pool_types=request.pool_types,
        database_path=request.database_path,
        allowed_intermediate_tokens=allowed_token_ids,
    )
    rust_filter = convert_pool_type_filter(request.pool_type_per_depth)
    start_ids = _resolve_boundary_token_ids(
        request.chain_id,
        {get_checksum_address(token) for token in request.start_tokens},
        request.database_path,
        "Start",
    )
    end_ids = _resolve_boundary_token_ids(
        request.chain_id,
        {get_checksum_address(token) for token in request.end_tokens},
        request.database_path,
        "End",
    )
    filter_len = (
        len(request.pool_type_per_depth) if request.pool_type_per_depth is not None else None
    )

    traversals: list[_Traversal] = []
    for start_id, end_id, include_reverse, min_depth in prepare_traversal_plan(
        start_ids, end_ids, request.min_depth, filter_len
    ):
        logger.debug(f"Finding paths from token {start_id} -> token {end_id}")
        logger.debug(f"Performing generic {request.max_depth}-pool path search")
        traversals.append(
            _Traversal(
                prepared=prepared,
                start_token_id=start_id,
                end_token_id=end_id,
                include_reverse=include_reverse,
                min_depth=min_depth,
                pool_kind_filter=rust_filter,
            )
        )
    return traversals


def find_paths(
    *,
    request: PathfindingRequest,
) -> Iterator[Sequence[PathStep]]:
    """Find paths from each start token to each end token via the Rust DFS.

    Args:
        request: The graph scope + traversal constraints for this search.

    Yields:
        A valid arbitrage path from a start token to an end token.

    """
    start = time.perf_counter()
    traversals = _prepare_traversals(request=request)
    for traversal in traversals:
        # One lazy Rust iterator per plan entry; `PathStep` construction in Rust.
        path_iter = find_paths_rust(
            traversal.prepared.edges,
            traversal.start_token_id,
            traversal.end_token_id,
            traversal.min_depth,
            request.max_depth,
            traversal.include_reverse,
            traversal.pool_kind_filter,
        )
        for raw_path in path_iter:
            yield traversal.prepared.step_builder.build(raw_path)
        logger.debug(
            f"Completed structured generic search (max depth {request.max_depth}) "
            f"at +{time.perf_counter() - start:.1f}s",
        )


async def find_paths_async(
    *,
    request: PathfindingRequest,
    batch_size: int = 1000,
) -> AsyncGenerator[Sequence[PathStep], None]:
    """Async `find_paths`, driving the Rust batched async iterator.

    The one-time prep runs on the shared tokio blocking pool, so neither the
    database resolution nor the Rust bulk read stalls the event loop.

    Args:
        request: The graph scope + traversal constraints for this search.
        batch_size: Paths per Rust delivery batch (default 1000), clamped `>= 1`.

    Yields:
        Sequences of PathStep objects representing arbitrage paths.

    Raises:
        The producer's exception, re-raised at the consumer.

    """
    discovery_start = time.perf_counter()
    discovery_yielded = 0
    discovery_last_log = discovery_start

    traversals = await call_blocking_on_ambient_runtime(
        partial(_prepare_traversals, request=request)
    )
    effective_batch = max(1, int(batch_size))

    for traversal in traversals:
        path_batches = find_paths_async_rust(
            traversal.prepared.edges,
            traversal.start_token_id,
            traversal.end_token_id,
            traversal.min_depth,
            request.max_depth,
            traversal.include_reverse,
            traversal.pool_kind_filter,
            effective_batch,
        )
        async for batch in path_batches:
            for raw_path in batch:
                yield traversal.prepared.step_builder.build(raw_path)
                discovery_yielded += 1
                now = time.perf_counter()
                if now - discovery_last_log >= _DISCOVERY_HEARTBEAT_INTERVAL_S:
                    logger.info(
                        "[pathfinding] discovery progress: paths_yielded=%d elapsed=%.1fs",
                        discovery_yielded,
                        now - discovery_start,
                    )
                    discovery_last_log = now

    logger.info(
        "[pathfinding] discovery complete: paths_yielded=%d elapsed=%.1fs",
        discovery_yielded,
        time.perf_counter() - discovery_start,
    )
