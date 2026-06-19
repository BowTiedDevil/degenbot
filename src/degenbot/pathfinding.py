"""Pathfinding utilities for discovering arbitrage routes."""

import asyncio
import enum
import itertools
import time
from collections.abc import AsyncIterator, Iterable, Iterator, Sequence
from dataclasses import dataclass

import sqlalchemy
from eth_typing import ChecksumAddress
from networkx import MultiGraph
from sqlalchemy.orm import Session

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import LiquidityPoolTable, PoolManagerTable, UniswapV4PoolTable
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger

type PoolId = int
type TokenId = int


@dataclass(slots=True, frozen=True)
class PathStep:
    """PathStep class."""

    address: ChecksumAddress
    type: type[LiquidityPoolTable | UniswapV4PoolTable]
    hash: str | None = None


class Direction(enum.Enum):
    """Direction class."""

    FORWARD = enum.auto()
    FORWARD_AND_REVERSE = enum.auto()


def _compute_node_valid_depths(
    graph: MultiGraph,
    pool_type_per_depth: Sequence[set[type] | None],
) -> dict[int, set[int]]:
    """Pre-compute which depths each node can participate in.

    For each node in the graph, determine which depth positions its edges
    satisfy. A node can appear at depth d if it has at least one incident edge
    whose pool_type is in the allowed set at depth d.

    This enables "lookahead" pruning: before recursing into a neighbor, the DFS
    can check (in O(1)) whether that neighbor can continue at the next depth.
    For permutations like V3-V4-V3, this eliminates exploration of V3-to-V3-only
    tokens that could never bridge to the V4 depth.

    Returns:
        Mapping from token ID to the set of depth positions it can appear at.

    """
    node_valid_depths: dict[int, set[int]] = {}
    for node in graph.nodes():
        # Collect all pool types this node has edges for
        node_pool_types: set[type] = set()
        for edges_dict in graph[node].values():
            node_pool_types.update(attr["pool_type"] for attr in edges_dict.values())

        # Determine which depths this node can appear at
        valid_depths: set[int] = set()
        for d, allowed in enumerate(pool_type_per_depth):
            if allowed is None or any(issubclass(pt, tuple(allowed)) for pt in node_pool_types):
                valid_depths.add(d)

        node_valid_depths[node] = valid_depths
    return node_valid_depths


def _dfs(
    *,
    start_token_id: TokenId,
    end_token_id: TokenId,
    working_path: list[tuple[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]],
    session: Session,
    include_reverse: bool,
    graph: MultiGraph,
    min_depth: int,
    max_depth: int | None,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    node_valid_depths: dict[int, set[int]] | None = None,
) -> Iterator[Sequence[PathStep]]:
    """Perform an iterative depth-first search from the start token to the end token.

    When a valid path is found, yield the result and backtrack one step to discover
    additional paths.

    Args:
        start_token_id: The ID of the token where the search begins.
        end_token_id: The ID of the token the path must return to.
        working_path: The sequence of ``(pool_id, pool_type)`` tuples currently
            being explored. This list is mutated in place during backtracking.
        session: An active SQLAlchemy ORM session for resolving pool addresses.
        include_reverse: If ``True``, yield each found path again in reverse
            order.
        graph: A networkx ``MultiGraph`` where nodes are token IDs and edges are
            liquidity pools.
        min_depth: The minimum number of hops a completed path must contain.
        max_depth: The maximum number of hops a completed path may contain, or
            ``None`` for no limit.
        pool_type_per_depth: If set, a sequence of allowed pool type sets at each
            depth. Depth 0 = first hop, depth 1 = second hop, etc. A ``None`` entry
            allows all pool types at that depth. Edges whose pool_type is not in
            the allowed set are pruned before recursion.
        node_valid_depths: If set, a mapping from token ID to the set of depth
            positions it can appear at (based on its edge types). Used for
            lookahead pruning: edges leading to nodes that can't continue at
            the next depth are skipped without recursion.

    Yields:
        Sequence[PathStep]: A valid path from start token to end token.

    """
    if start_token_id not in graph:
        logger.debug("returning early, token not in graph")
        return

    if start_token_id == end_token_id and len(working_path) >= min_depth:
        path_steps: list[PathStep] = []

        for pool_id, pool_type in working_path:
            if issubclass(pool_type, LiquidityPoolTable):
                path_steps.append(
                    PathStep(
                        address=session.scalars(
                            sqlalchemy.select(pool_type.address).where(pool_type.id == pool_id)
                        ).one(),
                        type=pool_type,
                    )
                )
            elif issubclass(pool_type, UniswapV4PoolTable):
                pool_address, pool_hash = session.execute(
                    sqlalchemy
                    .select(
                        PoolManagerTable.address,
                        pool_type.pool_hash,
                    )
                    .join(pool_type.manager)
                    .where(pool_type.id == pool_id)
                ).one()

                path_steps.append(
                    PathStep(
                        address=pool_address,
                        hash=pool_hash,
                        type=pool_type,
                    )
                )

        assert min_depth <= len(path_steps)
        if max_depth:
            assert len(path_steps) <= max_depth
        yield path_steps
        if include_reverse:
            yield path_steps[::-1]

    # Stop recursion if the working path has reached the maximum depth.
    # pool_type_per_depth implicitly sets a maximum depth equal to its length,
    # since there are no allowed pool types beyond its last entry.
    effective_max_depth = max_depth
    if pool_type_per_depth is not None and (
        effective_max_depth is None or len(pool_type_per_depth) < effective_max_depth
    ):
        effective_max_depth = len(pool_type_per_depth)
    if effective_max_depth is not None and len(working_path) >= effective_max_depth:
        return

    for neighbor_token_id, edges_dict in graph[start_token_id].items():
        for attr in edges_dict.values():
            pool_id = attr["pool_id"]
            pool_type = attr["pool_type"]

            if (pool_id, pool_type) not in working_path:
                # Prune edges that don't match the pool type filter at this depth
                if pool_type_per_depth is not None:
                    allowed = pool_type_per_depth[len(working_path)]
                    if allowed is not None and not issubclass(pool_type, tuple(allowed)):
                        continue

                    # Lookahead: skip if the neighbor can't continue at the
                    # next depth (e.g. a V3-only token at depth 0 when depth 1
                    # requires V4). This eliminates exploration of dead-end
                    # branches without recursing into them.
                    next_depth = len(working_path) + 1
                    if (
                        next_depth < len(pool_type_per_depth)
                        and node_valid_depths is not None
                        and next_depth not in node_valid_depths.get(neighbor_token_id, set())
                    ):
                        continue

                # Extend path
                working_path.append((pool_id, pool_type))

                yield from _dfs(
                    start_token_id=neighbor_token_id,
                    end_token_id=end_token_id,
                    working_path=working_path,
                    include_reverse=include_reverse,
                    session=session,
                    graph=graph,
                    min_depth=min_depth,
                    max_depth=max_depth,
                    pool_type_per_depth=pool_type_per_depth,
                    node_valid_depths=node_valid_depths,
                )

                # Backtrack
                working_path.pop()


async def _dfs_async(
    *,
    start_token_id: TokenId,
    end_token_id: TokenId,
    working_path: list[tuple[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]],
    session: Session,
    include_reverse: bool,
    graph: MultiGraph,
    min_depth: int,
    max_depth: int | None,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    node_valid_depths: dict[int, set[int]] | None = None,
) -> AsyncIterator[Sequence[PathStep]]:
    """Perform an iterative depth-first search from the start token to the end token.

    When a valid path is found, yield the result and backtrack one step to discover
    additional paths.

    Args:
        start_token_id: The ID of the token where the search begins.
        end_token_id: The ID of the token the path must return to.
        working_path: The sequence of ``(pool_id, pool_type)`` tuples currently
            being explored. This list is mutated in place during backtracking.
        session: An active SQLAlchemy ORM session for resolving pool addresses.
        include_reverse: If ``True``, yield each found path again in reverse
            order.
        graph: A networkx ``MultiGraph`` where nodes are token IDs and edges are
            liquidity pools.
        min_depth: The minimum number of hops a completed path must contain.
        max_depth: The maximum number of hops a completed path may contain, or
            ``None`` for no limit.
        pool_type_per_depth: If set, a sequence of allowed pool type sets at each
            depth. Depth 0 = first hop, depth 1 = second hop, etc. A ``None`` entry
            allows all pool types at that depth. When provided, edges whose
            pool_type is not in the allowed set are pruned before recursion.
        node_valid_depths: If set, a mapping from token ID to the set of depth
            positions it can appear at (based on its edge types). Used for
            lookahead pruning: edges leading to nodes that can't continue at
            the next depth are skipped without recursion.

    Yields:
        Sequence[PathStep]: A valid path from start token to end token.

    """
    await asyncio.sleep(0)

    if start_token_id not in graph:
        logger.debug("returning early, token not in graph")
        return

    if start_token_id == end_token_id and len(working_path) >= min_depth:
        path_steps: list[PathStep] = []

        for pool_id, pool_type in working_path:
            if issubclass(pool_type, LiquidityPoolTable):
                path_steps.append(
                    PathStep(
                        address=session.scalars(
                            sqlalchemy.select(pool_type.address).where(pool_type.id == pool_id)
                        ).one(),
                        type=pool_type,
                    )
                )
            elif issubclass(pool_type, UniswapV4PoolTable):
                pool_address, pool_hash = session.execute(
                    sqlalchemy
                    .select(
                        PoolManagerTable.address,
                        pool_type.pool_hash,
                    )
                    .join(pool_type.manager)
                    .where(pool_type.id == pool_id)
                ).one()

                path_steps.append(
                    PathStep(
                        address=pool_address,
                        hash=pool_hash,
                        type=pool_type,
                    )
                )

        assert min_depth <= len(path_steps)
        if max_depth:
            assert len(path_steps) <= max_depth
        yield path_steps
        if include_reverse:
            yield path_steps[::-1]

    # Stop recursion if the working path has reached the maximum depth.
    # pool_type_per_depth implicitly sets a maximum depth equal to its length,
    # since there are no allowed pool types beyond its last entry.
    effective_max_depth = max_depth
    if pool_type_per_depth is not None and (
        effective_max_depth is None or len(pool_type_per_depth) < effective_max_depth
    ):
        effective_max_depth = len(pool_type_per_depth)
    if effective_max_depth is not None and len(working_path) >= effective_max_depth:
        return

    for neighbor_token_id, edges_dict in graph[start_token_id].items():
        for attr in edges_dict.values():
            pool_id = attr["pool_id"]
            pool_type = attr["pool_type"]

            if (pool_id, pool_type) not in working_path:
                # Prune edges that don't match the pool type filter at this depth
                if pool_type_per_depth is not None:
                    allowed = pool_type_per_depth[len(working_path)]
                    if allowed is not None and not issubclass(pool_type, tuple(allowed)):
                        continue

                    # Lookahead: skip if the neighbor can't continue at the
                    # next depth (e.g. a V3-only token at depth 0 when depth 1
                    # requires V4). This eliminates exploration of dead-end
                    # branches without recursing into them.
                    next_depth = len(working_path) + 1
                    if (
                        next_depth < len(pool_type_per_depth)
                        and node_valid_depths is not None
                        and next_depth not in node_valid_depths.get(neighbor_token_id, set())
                    ):
                        continue

                # Extend path
                working_path.append((pool_id, pool_type))

                async for step in _dfs_async(
                    start_token_id=neighbor_token_id,
                    end_token_id=end_token_id,
                    working_path=working_path,
                    include_reverse=include_reverse,
                    session=session,
                    graph=graph,
                    min_depth=min_depth,
                    max_depth=max_depth,
                    pool_type_per_depth=pool_type_per_depth,
                    node_valid_depths=node_valid_depths,
                ):
                    yield step

                # Backtrack
                working_path.pop()


def _get_tokens_with_min_degree(
    degree: int,
    session: Session,
    chain_id: int,
    pool_types: Sequence[type],
) -> set[TokenId]:
    token_count_selects: list[sqlalchemy.Select[tuple[TokenId]]] = []
    for pool_type in pool_types:
        if issubclass(pool_type, LiquidityPoolTable):
            token_count_selects.extend((
                sqlalchemy.select(pool_type.token0_id.label("token_id")).where(
                    pool_type.chain == chain_id
                ),
                sqlalchemy.select(pool_type.token1_id.label("token_id")).where(
                    pool_type.chain == chain_id
                ),
            ))
        if issubclass(pool_type, UniswapV4PoolTable):
            token_count_selects.extend((
                sqlalchemy.select(pool_type.currency0_id.label("token_id")).where(
                    pool_type.manager.has(chain=chain_id)
                ),
                sqlalchemy.select(pool_type.currency1_id.label("token_id")).where(
                    pool_type.manager.has(chain=chain_id)
                ),
            ))
    token_count_subq = sqlalchemy.union_all(*token_count_selects).subquery()
    token_counts_greater_than_two_subq = (
        sqlalchemy
        .select(
            token_count_subq.columns["token_id"],
            sqlalchemy.func.count().label("pool_count"),
        )
        .group_by(token_count_subq.columns["token_id"])
        .having(sqlalchemy.func.count() >= degree)
        .subquery()
    )
    return set(
        session.scalars(
            sqlalchemy.select(token_counts_greater_than_two_subq.columns["token_id"])
        ).all()
    )


def _prepare_graph(
    chain_id: int,
    pool_types: Sequence[type],
    session: Session,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> MultiGraph:

    start = time.perf_counter()

    candidate_tokens = _get_tokens_with_min_degree(
        degree=2,
        session=session,
        chain_id=chain_id,
        pool_types=pool_types,
    )
    logger.debug(f"Found {len(candidate_tokens)} candidate tokens held by 2 or more pools")

    if allowed_intermediate_tokens is not None:
        before = len(candidate_tokens)
        candidate_tokens &= allowed_intermediate_tokens
        logger.debug(
            f"Token whitelist applied: {before} → {len(candidate_tokens)} candidate tokens"
        )

    # Build the graph by creating edges (pools) connecting nodes (tokens) in the
    # candidate set
    graph = MultiGraph()
    for pool_type in pool_types:
        if issubclass(pool_type, LiquidityPoolTable):
            graph.add_edges_from(
                (
                    token0_id,
                    token1_id,
                    {"pool_id": pool_id, "pool_type": pool_type},
                )
                for pool_id, token0_id, token1_id in session.execute(
                    sqlalchemy.select(
                        pool_type.id,
                        pool_type.token0_id,
                        pool_type.token1_id,
                    ).where(pool_type.chain == chain_id)
                ).all()
                if (token0_id in candidate_tokens and token1_id in candidate_tokens)
            )
        elif issubclass(pool_type, UniswapV4PoolTable):
            graph.add_edges_from(
                (
                    currency0_id,
                    currency1_id,
                    {"pool_id": pool_id, "pool_type": pool_type},
                )
                for pool_id, currency0_id, currency1_id in session.execute(
                    sqlalchemy.select(
                        pool_type.id,
                        pool_type.currency0_id,
                        pool_type.currency1_id,
                    ).where(pool_type.manager.has(chain=chain_id))
                )
                if (currency0_id in candidate_tokens and currency1_id in candidate_tokens)
            )
        logger.debug(f"Added edges for pool type {pool_type.__name__}")

    logger.debug(
        f"Built graph at +{time.perf_counter() - start:.1f}s: "
        f"{graph.number_of_nodes()} tokens, {graph.number_of_edges()} pools"
    )

    # Prune dead end tokens
    while tokens_to_prune := tuple(token for token, degree in graph.degree() if degree <= 1):
        graph.remove_nodes_from(tokens_to_prune)
    logger.debug(
        f"Pruned graph at +{time.perf_counter() - start:.1f}s: "
        f"{graph.number_of_nodes()} tokens, {graph.number_of_edges()} pools"
    )

    return graph


def _prepare_traversal_plan(
    start_tokens: set[ChecksumAddress],
    end_tokens: set[ChecksumAddress],
) -> dict[tuple[ChecksumAddress, ChecksumAddress], Direction]:
    """Prepare a traversal plan that will cover all combinations from the given starting and ending.

    sets.

    Returns:
        The computed value.

    """
    # Assemble an exhaustive plan based on the Cartesian product of all start and end nodes:
    # e.g. P(a|b -> a|b) == P(a->a) + P(a->b) + P(b->a) + P(b->b)
    traversal_plan: dict[
        tuple[ChecksumAddress, ChecksumAddress],
        Direction,
    ] = dict.fromkeys(
        itertools.product(start_tokens, end_tokens),
        Direction.FORWARD,
    )

    # Optimize traversal plan by consolidating parallel forward paths for token pairs found in the
    # starting and ending sets. If a forward path is known, the parallel reverse path can be
    # efficiently included without performing another traversal of the graph:
    # e.g. P(a->b) + P(b->a) == P(a<->b)
    tokens_used_for_start_and_end = start_tokens & end_tokens
    if len(tokens_used_for_start_and_end) > 1:
        logger.debug("Optimizing traversal plan.")
        for start_token, end_token in itertools.combinations(tokens_used_for_start_and_end, 2):
            traversal_plan[start_token, end_token] = Direction.FORWARD_AND_REVERSE
            del traversal_plan[end_token, start_token]

    return traversal_plan


def find_paths(
    *,
    chain_id: int,
    start_tokens: Iterable[ChecksumAddress | str],
    end_tokens: Iterable[ChecksumAddress | str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = (LiquidityPoolTable, UniswapV4PoolTable),
    db: DatabaseSessionManager,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    allowed_intermediate_tokens: Iterable[ChecksumAddress | str] | None = None,
) -> Iterator[Sequence[PathStep]]:
    """Find paths from each of the given start tokens to each of the given end tokens.

    Uses a depth-first search strategy. The search will exhaustively discover paths
    from a minimum depth to an optional maximum.

    Paths may be constrained to a subset of pool types. If not specified, all valid
    pool types will be included.

    Args:
        chain_id: The chain ID to restrict pool and token queries.
        start_tokens: Token addresses to use as the start of each path.
        end_tokens: Token addresses to use as the end of each path.
        min_depth: The minimum number of hops in yielded paths.
        max_depth: The optional maximum number of hops in yielded paths.
        pool_types: Database model classes for the pool types to include in the
            graph (default: ``LiquidityPoolTable`` and ``UniswapV4PoolTable``).
        db: The database session manager used to open a read session.
        pool_type_per_depth: If set, a sequence of allowed pool type sets at each
            depth. Depth 0 = first hop, depth 1 = second hop, etc. A ``None`` entry
            allows all pool types at that depth. When provided, edges whose
            pool_type is not in the allowed set are pruned before recursion.
        allowed_intermediate_tokens: If set, restrict the graph to only these token
            addresses as intermediate nodes. Pools connecting any non-whitelisted
            intermediate token are excluded from the graph. Use this to filter out
            tax tokens, fee-on-transfer tokens, and low-quality pairs that would
            waste simulation gas.

    Yields:
        Sequence[PathStep]: A valid arbitrage path from a start token to an end token.

    Raises:
        DegenbotValueError: If no pools are found for the given chain ID or tokens.

    """
    # @dev Liquidity pool lookups using a token ID are implicitly filtered for the chain ID, since
    # token addresses are unique to the chain. WHERE clauses can therefore be omitted from SELECTs.

    start = time.perf_counter()

    with db() as session:
        allowed_token_ids: set[TokenId] | None = None
        if allowed_intermediate_tokens is not None:
            allowed_token_ids = set(
                session.scalars(
                    sqlalchemy.select(Erc20TokenTable.id).where(
                        Erc20TokenTable.address.in_({
                            get_checksum_address(t) for t in allowed_intermediate_tokens
                        }),
                        Erc20TokenTable.chain == chain_id,
                    )
                ).all()
            )

        graph = _prepare_graph(
            chain_id=chain_id,
            pool_types=pool_types,
            session=session,
            allowed_intermediate_tokens=allowed_token_ids,
        )

        # Pre-compute valid depths per node for lookahead pruning when
        # pool_type_per_depth is set. This eliminates exploration of
        # dead-end branches (e.g. V3-only tokens when depth 1 requires V4).
        node_valid_depths: dict[int, set[int]] | None = None
        if pool_type_per_depth is not None:
            node_valid_depths = _compute_node_valid_depths(graph, pool_type_per_depth)
            logger.debug(
                f"Computed node valid depths for lookahead pruning: "
                f"{sum(1 for v in node_valid_depths.values() if len(v) > 1)} bridge nodes"
            )

        traversal_plan = _prepare_traversal_plan(
            start_tokens={get_checksum_address(token) for token in start_tokens},
            end_tokens={get_checksum_address(token) for token in end_tokens},
        )

        for (start_token, end_token), direction in traversal_plan.items():
            start_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == start_token,
                    Erc20TokenTable.chain == chain_id,
                )
            )
            if start_token_id is None:
                msg = f"Start token {start_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            end_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == end_token,
                    Erc20TokenTable.chain == chain_id,
                )
            )
            if end_token_id is None:
                msg = f"End token {end_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            working_path: list[tuple[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]] = []

            logger.debug(
                f"Finding paths from {start_token} "
                f"(id {start_token_id}) -> {end_token} (id {end_token_id})"
            )

            logger.debug(f"Performing generic {max_depth}-pool path search")

            yield from _dfs(
                start_token_id=start_token_id,
                end_token_id=end_token_id,
                working_path=working_path,
                include_reverse=(direction == direction.FORWARD_AND_REVERSE),
                session=session,
                graph=graph,
                min_depth=min_depth,
                max_depth=max_depth,
                pool_type_per_depth=pool_type_per_depth,
                node_valid_depths=node_valid_depths,
            )

            logger.debug(
                f"Completed structured generic search (max depth {max_depth}) "
                f"at +{time.perf_counter() - start:.1f}s"
            )


async def find_paths_async(
    *,
    chain_id: int,
    start_tokens: Iterable[ChecksumAddress | str],
    end_tokens: Iterable[ChecksumAddress | str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = [LiquidityPoolTable, UniswapV4PoolTable],
    db: DatabaseSessionManager,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    allowed_intermediate_tokens: Iterable[ChecksumAddress | str] | None = None,
) -> AsyncIterator[Sequence[PathStep]]:
    """Async version of ``find_paths``.

    Args:
        chain_id: The chain ID to restrict pool and token queries.
        start_tokens: Token addresses to use as the start of each path.
        end_tokens: Token addresses to use as the end of each path.
        min_depth: The minimum number of hops in yielded paths.
        max_depth: The optional maximum number of hops in yielded paths.
        pool_types: Database model classes for the pool types to include in the
            graph (default: ``LiquidityPoolTable`` and ``UniswapV4PoolTable``).
        db: The database session manager used to open a read session.
        pool_type_per_depth: If set, a sequence of allowed pool type sets at each
            depth. Depth 0 = first hop, depth 1 = second hop, etc. A ``None`` entry
            allows all pool types at that depth. When provided, edges whose
            pool_type is not in the allowed set are pruned before recursion.
        allowed_intermediate_tokens: If set, restrict the graph to only these token
            addresses as intermediate nodes. Pools connecting any non-whitelisted
            intermediate token are excluded from the graph. Use this to filter out
            tax tokens, fee-on-transfer tokens, and low-quality pairs that would
            waste simulation gas.

    Yields:
        Sequences of PathStep objects representing arbitrage paths.

    Raises:
        DegenbotValueError: If no pools are found for the given chain ID or tokens.

    """
    start = time.perf_counter()

    # NOTE: A ``with`` block wrapping a ``yield`` in an async generator does not
    # guarantee prompt cleanup if the consumer abandons iteration early (see
    # ruff ASYNC119). Manage the session lifetime explicitly with try/finally so
    # ``aclose()``/garbage collection always releases the session.
    session = db()
    try:
        allowed_token_ids: set[TokenId] | None = None
        if allowed_intermediate_tokens is not None:
            allowed_token_ids = set(
                session.scalars(
                    sqlalchemy.select(Erc20TokenTable.id).where(
                        Erc20TokenTable.address.in_({
                            get_checksum_address(t) for t in allowed_intermediate_tokens
                        }),
                        Erc20TokenTable.chain == chain_id,
                    )
                ).all()
            )

        graph = _prepare_graph(
            chain_id=chain_id,
            pool_types=pool_types,
            session=session,
            allowed_intermediate_tokens=allowed_token_ids,
        )

        # Pre-compute valid depths per node for lookahead pruning when
        # pool_type_per_depth is set. This eliminates exploration of
        # dead-end branches (e.g. V3-only tokens when depth 1 requires V4).
        node_valid_depths: dict[int, set[int]] | None = None
        if pool_type_per_depth is not None:
            node_valid_depths = _compute_node_valid_depths(graph, pool_type_per_depth)
            logger.debug(
                f"Computed node valid depths for lookahead pruning: "
                f"{sum(1 for v in node_valid_depths.values() if len(v) > 1)} bridge nodes"
            )

        traversal_plan = _prepare_traversal_plan(
            start_tokens={get_checksum_address(token) for token in start_tokens},
            end_tokens={get_checksum_address(token) for token in end_tokens},
        )

        for (start_token, end_token), direction in traversal_plan.items():
            start_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == start_token,
                    Erc20TokenTable.chain == chain_id,
                )
            )
            if start_token_id is None:
                msg = f"Start token {start_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            end_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == end_token,
                    Erc20TokenTable.chain == chain_id,
                )
            )
            if end_token_id is None:
                msg = f"End token {end_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            working_path: list[tuple[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]] = []

            logger.debug(
                f"Finding paths from {start_token} "
                f"(id {start_token_id}) -> {end_token} (id {end_token_id})"
            )

            logger.debug(f"Performing generic {max_depth}-pool path search")

            async for path in _dfs_async(
                start_token_id=start_token_id,
                end_token_id=end_token_id,
                working_path=working_path,
                include_reverse=(direction == direction.FORWARD_AND_REVERSE),
                session=session,
                graph=graph,
                min_depth=min_depth,
                max_depth=max_depth,
                pool_type_per_depth=pool_type_per_depth,
                node_valid_depths=node_valid_depths,
            ):
                yield path

            logger.debug(
                f"Completed structured generic search (max depth {max_depth}) "
                f"at +{time.perf_counter() - start:.1f}s"
            )
    finally:
        session.close()
