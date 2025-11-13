import itertools
from collections.abc import Iterator, Sequence
from typing import TYPE_CHECKING

import sqlalchemy
from eth_typing import ChecksumAddress

from degenbot.database import db_session
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import LiquidityPoolTable, UniswapV4PoolTable
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger


def find_paths(
    chain_id: int,
    start_token: ChecksumAddress,
    end_token: ChecksumAddress,
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = [LiquidityPoolTable, UniswapV4PoolTable],
    equivalent_tokens: Sequence[tuple[ChecksumAddress, ChecksumAddress]] | None = None,
) -> Iterator[Sequence[LiquidityPoolTable | UniswapV4PoolTable]]:
    """
    Find paths from `start_token` to `end_token`, formatted as an iterator of pool objects
    constructed from the database.

    Paths are yielded using a breadth-first search strategy starting from a minimum depth up to an
    optional maximum.

    Paths may be constrained to a subset of pool types. If not specified, all valid pool types will
    be included.

    Pairs of equivalent profit tokens may be provided as a tuple of addresses. These tokens are
    assumed be convertible by some other means outside of the swap path, e.g. native Ether and WETH
    can be converted at 1:1 through the WETH contract.
    """

    # @dev chain_id is only used to look up the start and end tokens. Liquidity pool lookups use
    # these IDs, thus they are implicitly filtered for the target chain ID without needing a
    # dedicated WHERE clause.

    start_token_in_db = db_session.scalar(
        sqlalchemy.select(Erc20TokenTable).where(
            Erc20TokenTable.address == start_token,
            Erc20TokenTable.chain == chain_id,
        )
    )
    if start_token_in_db is None:
        msg = "The start token was not found in the database."
        raise DegenbotValueError(msg)

    end_token_in_db = db_session.scalar(
        sqlalchemy.select(Erc20TokenTable).where(
            Erc20TokenTable.address == end_token,
            Erc20TokenTable.chain == chain_id,
        )
    )
    if end_token_in_db is None:
        msg = "The end token was not found in the database."
        raise DegenbotValueError(msg)

    equivalent: dict[ChecksumAddress, ChecksumAddress] = {}
    if equivalent_tokens is not None:
        for token0, token1 in equivalent_tokens:
            equivalent[token0] = token1
            equivalent[token1] = token0

    pools: list[LiquidityPoolTable | UniswapV4PoolTable]
    forward_token_ids: set[int]
    token_id_selects: list[sqlalchemy.Select[tuple[int]]]

    match max_depth, start_token == end_token:
        case 2, True:
            """
            Cycle a profit token P through two pools with a common forward token X:
                Pool A holding a P-X pair
                Pool B holding a P-X pair

            e.g. cycle WETH through two WETH-X pairs.
            """

            if equivalent_tokens is not None:
                logger.warning(
                    "One or more equivalent token pairings were provided. Equivalent tokens are "
                    "not relevant for a two-pool cycle with a single profit token, and will be "
                    "ignored. To find paths with equivalent profit tokens, call this function "
                    "again with differing start and end tokens."
                )

            cycle_token = start_token_in_db

            # Assemble the token ID selections for the in-scope pools
            token_id_selects = []
            for pool_type in pool_types:
                if issubclass(pool_type, LiquidityPoolTable):
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.token1_id.label("forward_token_id"),
                        ).where(
                            pool_type.token0_id == cycle_token.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.token0_id.label("forward_token_id"),
                        ).where(
                            pool_type.token1_id == cycle_token.id,
                        )
                    )
                if issubclass(pool_type, UniswapV4PoolTable):
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.currency1_id.label("forward_token_id"),
                        ).where(
                            pool_type.currency0_id == cycle_token.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.currency0_id.label("forward_token_id"),
                        ).where(
                            pool_type.currency1_id == cycle_token.id,
                        )
                    )

            # Identify tokens paired with the cycle token in at least two pools
            forward_token_ids = set(
                db_session.scalars(
                    sqlalchemy.select(
                        sqlalchemy.union_all(*token_id_selects)
                        .subquery()
                        .columns["forward_token_id"]
                    )
                    .group_by("forward_token_id")
                    .having(sqlalchemy.func.count() > 1)
                ).all()
            )
            forward_token_ids.discard(cycle_token.id)
            logger.debug(f"Found {len(forward_token_ids)} forward tokens")

            for forward_token_id in forward_token_ids:
                forward_token = db_session.scalar(
                    sqlalchemy.select(Erc20TokenTable).where(Erc20TokenTable.id == forward_token_id)
                )
                if TYPE_CHECKING:
                    assert forward_token is not None

                if forward_token.address.lower() < cycle_token.address.lower():
                    token0_id, token1_id = forward_token.id, cycle_token.id
                else:
                    token0_id, token1_id = cycle_token.id, forward_token.id

                pools = []

                for pool_type in pool_types:
                    if issubclass(pool_type, LiquidityPoolTable):
                        pools.extend(
                            db_session.scalars(
                                sqlalchemy.select(pool_type).where(
                                    pool_type.token0_id == token0_id,
                                    pool_type.token1_id == token1_id,
                                )
                            ).all()
                        )
                    if issubclass(pool_type, UniswapV4PoolTable):
                        pools.extend(
                            db_session.scalars(
                                sqlalchemy.select(pool_type).where(
                                    pool_type.currency0_id == token0_id,
                                    pool_type.currency1_id == token1_id,
                                )
                            ).all()
                        )

                yield from itertools.permutations(pools, 2)

        case 2, False:
            """
            Cycle two equivalent profit tokens (P, P') through two pools with a common forward token
            X:
                Pool A holding a P-X pair
                Pool B holding a P'-X pair

            e.g. cycle WETH through a WETH-X pair and an Ether-X pair, or cycle Ether through an
            Ether-X and WETH-X pair.
            """

            # For a 2-pool arb with differing cycle start & end tokens, they must be equivalent
            # e.g. WETH <--> Ether
            assert equivalent[start_token] == end_token
            assert equivalent[end_token] == start_token

            cycle_token_start = start_token_in_db
            cycle_token_end = end_token_in_db

            # Assemble the token ID selections for the in-scope pools
            token_id_selects = []
            for pool_type in pool_types:
                if issubclass(pool_type, LiquidityPoolTable):
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.token0_id.label("forward_token_id"),
                        ).where(
                            pool_type.token1_id == cycle_token_start.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.token1_id.label("forward_token_id"),
                        ).where(
                            pool_type.token0_id == cycle_token_start.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.token0_id.label("forward_token_id"),
                        ).where(
                            pool_type.token1_id == cycle_token_end.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.token1_id.label("forward_token_id"),
                        ).where(
                            pool_type.token0_id == cycle_token_end.id,
                        )
                    )
                if issubclass(pool_type, UniswapV4PoolTable):
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.currency1_id.label("forward_token_id"),
                        ).where(
                            pool_type.currency0_id == cycle_token_start.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.currency0_id.label("forward_token_id"),
                        ).where(
                            pool_type.currency1_id == cycle_token_start.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.currency1_id.label("forward_token_id"),
                        ).where(
                            pool_type.currency0_id == cycle_token_end.id,
                        )
                    )
                    token_id_selects.append(
                        sqlalchemy.select(
                            pool_type.currency0_id.label("forward_token_id"),
                        ).where(
                            pool_type.currency1_id == cycle_token_end.id,
                        )
                    )

            # Identify tokens paired with the cycle token (or equivalent) in at least two pools
            forward_token_ids = set(
                db_session.scalars(
                    sqlalchemy.select(
                        sqlalchemy.union_all(*token_id_selects)
                        .subquery()
                        .columns["forward_token_id"]
                    )
                    .group_by("forward_token_id")
                    .having(sqlalchemy.func.count() > 1)
                ).all()
            )
            # Exclude both of the cycle tokens from use as a forward token
            # e.g. a WETH-Ether pool charging a fee is strictly worse than converting via the
            # wrapper contract, assuming 1:1 reserves
            forward_token_ids.discard(cycle_token_start.id)
            forward_token_ids.discard(cycle_token_end.id)
            logger.debug(f"Found {len(forward_token_ids)} forward tokens")

            for forward_token_id in forward_token_ids:
                forward_token = db_session.scalar(
                    sqlalchemy.select(Erc20TokenTable).where(
                        Erc20TokenTable.id == forward_token_id,
                    )
                )
                if TYPE_CHECKING:
                    assert forward_token is not None

                pools = []

                # Get P-X pools
                token0_id, token1_id = (
                    (forward_token_id, cycle_token_start.id)
                    if forward_token.address.lower() < cycle_token_start.address.lower()
                    else (cycle_token_start.id, forward_token_id)
                )
                for pool_type in pool_types:
                    if issubclass(pool_type, LiquidityPoolTable):
                        pools.extend(
                            db_session.scalars(
                                sqlalchemy.select(pool_type).where(
                                    pool_type.token0_id == token0_id,
                                    pool_type.token1_id == token1_id,
                                )
                            ).all()
                        )
                    if issubclass(pool_type, UniswapV4PoolTable):
                        pools.extend(
                            db_session.scalars(
                                sqlalchemy.select(pool_type).where(
                                    pool_type.currency0_id == token0_id,
                                    pool_type.currency1_id == token1_id,
                                )
                            ).all()
                        )

                # Get P'-X pools
                token0_id, token1_id = (
                    (forward_token_id, cycle_token_end.id)
                    if forward_token.address.lower() < cycle_token_end.address.lower()
                    else (cycle_token_end.id, forward_token_id)
                )
                for pool_type in pool_types:
                    if issubclass(pool_type, LiquidityPoolTable):
                        pools.extend(
                            db_session.scalars(
                                sqlalchemy.select(pool_type).where(
                                    pool_type.token0_id == token0_id,
                                    pool_type.token1_id == token1_id,
                                )
                            ).all()
                        )
                    if issubclass(pool_type, UniswapV4PoolTable):
                        pools.extend(
                            db_session.scalars(
                                sqlalchemy.select(pool_type).where(
                                    pool_type.currency0_id == token0_id,
                                    pool_type.currency1_id == token1_id,
                                )
                            ).all()
                        )

                yield from itertools.permutations(pools, 2)

        case 3:
            raise NotImplementedError
        case None:
            raise NotImplementedError
        case _:
            msg = "Invalid max_depth!"
            raise DegenbotValueError(msg)
