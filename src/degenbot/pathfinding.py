"""Pathfinding utilities for discovering arbitrage routes."""

import asyncio
import enum
import itertools
import time
from collections.abc import AsyncIterator, Iterable, Iterator, Sequence
from dataclasses import dataclass

import sqlalchemy
from eth_typing import ChecksumAddress
from sqlalchemy.orm import Session

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import LiquidityPoolTable, PoolManagerTable, UniswapV4PoolTable
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.degenbot_rs import find_paths_rust
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger

type PoolId = int
type TokenId = int

# Pool-kind discriminants passed to the Rust `find_paths_rust` function.
# These match the `PoolKind::from_u8` mapping in the Rust core leaf.
_POOL_KIND_V2V3: int = 0
_POOL_KIND_V4: int = 1


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


def _pool_kind_for_type(pool_type: type) -> int:
    """Map a pool-table class to its u8 discriminant for the Rust core.

    Returns:
        0 for V2/V3 pools (``LiquidityPoolTable``), 1 for V4 pools.

    Raises:
        DegenbotValueError: If ``pool_type`` is not a recognized pool-table class.

    """
    if issubclass(pool_type, UniswapV4PoolTable):
        return _POOL_KIND_V4
    if issubclass(pool_type, LiquidityPoolTable):
        return _POOL_KIND_V2V3
    msg = f"Unsupported pool type: {pool_type}"
    raise DegenbotValueError(message=msg)


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
                    pool_type.chain == chain_id,
                ),
                sqlalchemy.select(pool_type.token1_id.label("token_id")).where(
                    pool_type.chain == chain_id,
                ),
            ))
        if issubclass(pool_type, UniswapV4PoolTable):
            token_count_selects.extend((
                sqlalchemy.select(pool_type.currency0_id.label("token_id")).where(
                    pool_type.manager.has(chain=chain_id),
                ),
                sqlalchemy.select(pool_type.currency1_id.label("token_id")).where(
                    pool_type.manager.has(chain=chain_id),
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
            sqlalchemy.select(token_counts_greater_than_two_subq.columns["token_id"]),
        ).all(),
    )


@dataclass(slots=True)
class _PreparedGraph:
    """The flat edge list + address lookups produced by ``_prepare_graph``."""

    edges: list[tuple[TokenId, TokenId, PoolId, int]]
    v2v3_addresses: dict[PoolId, ChecksumAddress]
    v4_lookups: dict[PoolId, tuple[ChecksumAddress, str]]


def _prepare_graph(
    chain_id: int,
    pool_types: Sequence[type],
    session: Session,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> _PreparedGraph:
    """Build the flat edge list + address lookup dicts for the Rust DFS.

    This replaces the old NetworkX ``MultiGraph`` construction. The graph is
    represented as a flat list of ``(token0, token1, pool_id, pool_kind_u8)``
    tuples passed directly to ``find_paths_rust``. Address resolution (the
    former N+1 query bottleneck — one query per pool per yielded path) is
    eliminated by bulk-preloading all pool addresses during this single
    construction pass.

    Returns:
        A ``_PreparedGraph`` with flat edges + address lookup dicts.

    """
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
            f"Token whitelist applied: {before} → {len(candidate_tokens)} candidate tokens",
        )

    edges: list[tuple[TokenId, TokenId, PoolId, int]] = []
    v2v3_addresses: dict[PoolId, ChecksumAddress] = {}
    v4_lookups: dict[PoolId, tuple[ChecksumAddress, str]] = {}

    for pool_type in pool_types:
        if issubclass(pool_type, LiquidityPoolTable):
            for pool_id, token0_id, token1_id, address in session.execute(
                sqlalchemy.select(
                    pool_type.id,
                    pool_type.token0_id,
                    pool_type.token1_id,
                    pool_type.address,
                ).where(pool_type.chain == chain_id),
            ).all():
                if token0_id in candidate_tokens and token1_id in candidate_tokens:
                    edges.append((token0_id, token1_id, pool_id, _POOL_KIND_V2V3))
                    v2v3_addresses[pool_id] = address

        elif issubclass(pool_type, UniswapV4PoolTable):
            for pool_id, currency0_id, currency1_id, manager_address, pool_hash in session.execute(
                sqlalchemy
                .select(
                    pool_type.id,
                    pool_type.currency0_id,
                    pool_type.currency1_id,
                    PoolManagerTable.address,
                    pool_type.pool_hash,
                )
                .join(pool_type.manager)
                .where(pool_type.manager.has(chain=chain_id)),
            ).all():
                if currency0_id in candidate_tokens and currency1_id in candidate_tokens:
                    edges.append((currency0_id, currency1_id, pool_id, _POOL_KIND_V4))
                    v4_lookups[pool_id] = (manager_address, pool_hash)

        logger.debug(f"Added edges for pool type {pool_type.__name__}")

    logger.debug(
        f"Built graph at +{time.perf_counter() - start:.1f}s: {len(edges)} edges",
    )

    return _PreparedGraph(edges=edges, v2v3_addresses=v2v3_addresses, v4_lookups=v4_lookups)


def _build_path_steps(
    path: list[tuple[PoolId, int]],
    v2v3_addresses: dict[PoolId, ChecksumAddress],
    v4_lookups: dict[PoolId, tuple[ChecksumAddress, str]],
    pool_types: Sequence[type],
) -> list[PathStep]:
    """Convert a raw Rust path ``(pool_id, pool_kind_u8)`` into ``PathStep`` objects.

    The ``pool_kind_u8`` discriminant only tells us V2V3 vs V4. To recover the
    exact ``pool_type`` class for each pool, we look it up from the
    ``pool_types`` sequence passed by the caller — but since all V2/V3
    subclasses share one table, we can use the first V2/V3 type (or V4 type)
    from the caller's list.

    Returns:
        A list of ``PathStep`` objects with resolved addresses + hashes.

    """
    # Find representative pool-type classes for each kind.
    v2v3_type: type | None = None
    v4_type: type | None = None
    for pt in pool_types:
        if issubclass(pt, UniswapV4PoolTable) and v4_type is None:
            v4_type = pt
        elif issubclass(pt, LiquidityPoolTable) and v2v3_type is None:
            v2v3_type = pt

    steps: list[PathStep] = []
    for pool_id, pool_kind_u8 in path:
        if pool_kind_u8 == _POOL_KIND_V4:
            assert v4_type is not None
            manager_address, pool_hash = v4_lookups[pool_id]
            steps.append(PathStep(address=manager_address, hash=pool_hash, type=v4_type))
        else:
            assert v2v3_type is not None
            steps.append(PathStep(address=v2v3_addresses[pool_id], type=v2v3_type))

    return steps


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


def _convert_pool_type_filter(
    pool_type_per_depth: Sequence[set[type] | None] | None,
) -> list[set[int] | None] | None:
    """Convert the Python ``set[type]`` per-depth filter to Rust u8 discriminants.

    Returns:
        A list of ``None`` / ``set[int]`` entries, or ``None`` if no filter.

    """
    if pool_type_per_depth is None:
        return None
    result: list[set[int] | None] = []
    for allowed in pool_type_per_depth:
        if allowed is None:
            result.append(None)
        else:
            result.append({_pool_kind_for_type(pt) for pt in allowed})
    return result


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
                    ),
                ).all(),
            )

        prepared = _prepare_graph(
            chain_id=chain_id,
            pool_types=pool_types,
            session=session,
            allowed_intermediate_tokens=allowed_token_ids,
        )

        rust_filter = _convert_pool_type_filter(pool_type_per_depth)

        traversal_plan = _prepare_traversal_plan(
            start_tokens={get_checksum_address(token) for token in start_tokens},
            end_tokens={get_checksum_address(token) for token in end_tokens},
        )

        for (start_token, end_token), direction in traversal_plan.items():
            start_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == start_token,
                    Erc20TokenTable.chain == chain_id,
                ),
            )
            if start_token_id is None:
                msg = f"Start token {start_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            end_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == end_token,
                    Erc20TokenTable.chain == chain_id,
                ),
            )
            if end_token_id is None:
                msg = f"End token {end_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            logger.debug(
                f"Finding paths from {start_token} "
                f"(id {start_token_id}) -> {end_token} (id {end_token_id})",
            )

            # A permutation filter implies an exact hop depth: don't yield
            # shorter cycles that merely prefix-match the first N depths
            # (e.g. a 3-depth V3-V3-V2 filter must not leak 2-hop V3-V3).
            effective_min_depth = (
                min_depth
                if pool_type_per_depth is None
                else max(min_depth, len(pool_type_per_depth))
            )

            logger.debug(f"Performing generic {max_depth}-pool path search")

            # The Rust DFS returns a lazy iterator — paths are yielded one at
            # a time, so memory is bounded even for graphs that produce
            # millions of paths.
            path_iter = find_paths_rust(
                prepared.edges,
                start_token_id,
                end_token_id,
                effective_min_depth,
                max_depth,
                direction == Direction.FORWARD_AND_REVERSE,
                rust_filter,
            )

            for raw_path in path_iter:
                yield _build_path_steps(
                    raw_path,
                    prepared.v2v3_addresses,
                    prepared.v4_lookups,
                    pool_types,
                )

            logger.debug(
                f"Completed structured generic search (max depth {max_depth}) "
                f"at +{time.perf_counter() - start:.1f}s",
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

    The Rust DFS runs in a single GIL-released call and returns all results at
    once. This async wrapper yields them individually so consumers can iterate
    asynchronously. Periodic ``asyncio.sleep(0)`` calls give other tasks a
    chance to run during long searches.

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

    """
    # The sync `find_paths` generator is driven by the async event loop: we
    # iterate it synchronously and yield each result. The Rust DFS call inside
    # `find_paths` releases the GIL via `py.detach()`, so other Python tasks
    # can run during the search.

    for path in find_paths(
        chain_id=chain_id,
        start_tokens=start_tokens,
        end_tokens=end_tokens,
        min_depth=min_depth,
        max_depth=max_depth,
        pool_types=pool_types,
        db=db,
        pool_type_per_depth=pool_type_per_depth,
        allowed_intermediate_tokens=allowed_intermediate_tokens,
    ):
        await asyncio.sleep(0)
        yield path
