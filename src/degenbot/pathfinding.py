import enum
import itertools
import time
from collections.abc import Iterable, Iterator, Sequence
from dataclasses import dataclass

import sqlalchemy
from eth_typing import ChecksumAddress
from networkx import MultiGraph

from degenbot.checksum_cache import get_checksum_address
from degenbot.database import db_session
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import LiquidityPoolTable, PoolManagerTable, UniswapV4PoolTable
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger

type PoolId = int
type TokenId = int


@dataclass(slots=True, frozen=True)
class PathStep:
    address: ChecksumAddress
    type: type[LiquidityPoolTable | UniswapV4PoolTable]
    hash: str | None = None


class Direction(enum.Enum):
    FORWARD = enum.auto()
    FORWARD_AND_REVERSE = enum.auto()


def find_paths(
    chain_id: int,
    start_tokens: Iterable[str],
    end_tokens: Iterable[str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = [LiquidityPoolTable, UniswapV4PoolTable],
) -> Iterator[Sequence[PathStep]]:
    """
    Find paths from each of the given start tokens to each of the given end tokens using a
    depth-first search strategy. The search will exhaustively discover paths from a minimum depth
    to an optional maximum.

    Paths may be constrained to a subset of pool types. If not specified, all valid pool types will
    be included.

    The function is a generator which yields results one-by-one as they are discovered. Callers
    must consume the results or capture the yielded results in a container.

    The path search assumes this strategy:
        Beginning at an arbitrary start token, T_s, perform successive swaps through a sequence of
        pools. Swaps between successive pools require a common forward token F_n between.
        The final swap yields an arbitrary end token, T_e.

        POOL    TOKEN PAIR
        0       T_s  - T_f0
        1       T_f0 - T_f1
        2       T_f1 - T_f2
        3       T_f2 - T_e
    """

    def dfs(
        start_token_id: TokenId,
        end_token_id: TokenId,
        working_path: list[tuple[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]],
        *,
        include_reverse: bool,
    ) -> Iterator[Sequence[PathStep]]:
        """
        Perform an iterative depth-first search from the start token to the end token. When a valid
        path is found, yield the result and backtrack one step to discover additional paths.
        """

        if start_token_id not in graph:
            logger.debug("returning early, token not in graph")
            return

        if start_token_id == end_token_id and len(working_path) >= min_depth:
            _path: list[PathStep] = []

            for pool_id, pool_type in working_path:
                if issubclass(pool_type, LiquidityPoolTable):
                    _path.append(
                        PathStep(
                            address=db_session.scalars(
                                sqlalchemy.select(pool_type.address).where(pool_type.id == pool_id)
                            ).one(),
                            type=pool_type,
                        )
                    )
                elif issubclass(pool_type, UniswapV4PoolTable):
                    pool_address, pool_hash = db_session.execute(
                        sqlalchemy.select(
                            PoolManagerTable.address,
                            pool_type.pool_hash,
                        )
                        .join(pool_type.manager)
                        .where(pool_type.id == pool_id)
                    ).one()

                    _path.append(
                        PathStep(
                            address=pool_address,
                            hash=pool_hash,
                            type=pool_type,
                        )
                    )

            assert min_depth <= len(_path)
            if max_depth:
                assert len(_path) <= max_depth
            yield _path
            if include_reverse:
                yield _path[::-1]

        # Stop recursion if the working path has reached the maximum depth
        if len(working_path) == max_depth:
            return

        for neighbor_token_id, edges_dict in graph[start_token_id].items():
            for attr in edges_dict.values():
                pool_id = attr["pool_id"]
                pool_type = attr["pool_type"]

                if (pool_id, pool_type) not in working_path:
                    # Extend path
                    working_path.append((pool_id, pool_type))

                    yield from dfs(
                        start_token_id=neighbor_token_id,
                        end_token_id=end_token_id,
                        working_path=working_path,
                        include_reverse=include_reverse,
                    )

                    # Backtrack
                    working_path.pop()

    def get_tokens_with_min_degree(degree: int) -> set[TokenId]:
        token_count_selects: list[sqlalchemy.Select[tuple[TokenId]]] = []
        for pool_type in pool_types:
            if issubclass(pool_type, LiquidityPoolTable):
                token_count_selects.append(
                    sqlalchemy.select(pool_type.token0_id.label("token_id")).where(
                        pool_type.chain == chain_id
                    )
                )
                token_count_selects.append(
                    sqlalchemy.select(pool_type.token1_id.label("token_id")).where(
                        pool_type.chain == chain_id
                    )
                )
            if issubclass(pool_type, UniswapV4PoolTable):
                token_count_selects.append(
                    sqlalchemy.select(pool_type.currency0_id.label("token_id")).where(
                        pool_type.manager.has(chain=chain_id)
                    )
                )
                token_count_selects.append(
                    sqlalchemy.select(pool_type.currency1_id.label("token_id")).where(
                        pool_type.manager.has(chain=chain_id)
                    )
                )
        token_count_subq = sqlalchemy.union_all(*token_count_selects).subquery()
        token_counts_greater_than_two_subq = (
            sqlalchemy.select(
                token_count_subq.columns["token_id"],
                sqlalchemy.func.count().label("pool_count"),
            )
            .group_by(token_count_subq.columns["token_id"])
            .having(sqlalchemy.func.count() >= degree)
            .subquery()
        )
        return set(
            db_session.scalars(
                sqlalchemy.select(token_counts_greater_than_two_subq.columns["token_id"])
            ).all()
        )

    # @dev Liquidity pool lookups using a token ID are implicitly filtered for the chain ID, since
    # token addresses are unique to the chain. WHERE clauses can therefore be omitted from SELECTs.

    start = time.perf_counter()

    if pool_types is None:
        pool_types = [LiquidityPoolTable, UniswapV4PoolTable]

    candidate_tokens = get_tokens_with_min_degree(2)
    logger.debug(f"Found {len(candidate_tokens)} candidate tokens held by 2 or more pools")

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
                for pool_id, token0_id, token1_id in db_session.execute(
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
                for pool_id, currency0_id, currency1_id in db_session.execute(
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

    # Prepare an exhaustive traversal plan based on the Cartesian product of all start and end
    # nodes: e.g. P(a|b -> a|b) == P(a->a) + P(a->b) + P(b->a) + P(b->b)
    traversal_plan: dict[
        tuple[ChecksumAddress, ChecksumAddress],
        Direction,
    ] = {
        (get_checksum_address(start_token), get_checksum_address(end_token)): Direction.FORWARD
        for start_token, end_token in itertools.product(start_tokens, end_tokens)
    }

    tokens_used_for_start_and_end = set(start_tokens) & set(end_tokens)
    if len(tokens_used_for_start_and_end) > 1:
        logger.debug("Optimizing traversal plan.")
        # One traversal can be eliminated for every combination from tokens in the starting and
        # ending sets. The plan is reduced by consolidating: P(a->b) + P(b->a) == P(a<->b)
        for start_token, end_token in itertools.combinations(tokens_used_for_start_and_end, 2):
            traversal_plan[(start_token, end_token)] = Direction.FORWARD_AND_REVERSE
            del traversal_plan[(end_token, start_token)]

    for (start_token, end_token), direction in traversal_plan.items():
        start_token_id = db_session.scalar(
            sqlalchemy.select(Erc20TokenTable.id).where(
                Erc20TokenTable.address == start_token,
                Erc20TokenTable.chain == chain_id,
            )
        )
        if start_token_id is None:
            msg = f"Start token {start_token} was not found in the database."
            raise DegenbotValueError(msg)

        end_token_id = db_session.scalar(
            sqlalchemy.select(Erc20TokenTable.id).where(
                Erc20TokenTable.address == end_token,
                Erc20TokenTable.chain == chain_id,
            )
        )
        if end_token_id is None:
            msg = f"End token {end_token} was not found in the database."
            raise DegenbotValueError(msg)

        working_path: list[tuple[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]] = []

        logger.debug(
            f"Finding paths from {start_token} "
            f"(id {start_token_id}) -> {end_token} (id {end_token_id})"
        )

        logger.debug(f"Performing generic {max_depth}-pool path search")

        yield from dfs(
            start_token_id=start_token_id,
            end_token_id=end_token_id,
            working_path=working_path,
            include_reverse=direction == direction.FORWARD_AND_REVERSE,
        )

        logger.debug(
            f"Completed structured generic search (max depth {max_depth}) "
            f"at +{time.perf_counter() - start:.1f}s"
        )
