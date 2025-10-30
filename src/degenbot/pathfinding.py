import itertools
from collections.abc import Iterator, Sequence

import sqlalchemy
from eth_typing import ChecksumAddress

from degenbot.database import db_session
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import LiquidityPoolTable, ManagedLiquidityPoolTable
from degenbot.exceptions.base import DegenbotValueError


def find_paths(
    chain_id: int,
    start_token: ChecksumAddress,
    end_token: ChecksumAddress,
    max_depth: int | None = None,
    pool_types: Sequence[LiquidityPoolTable | ManagedLiquidityPoolTable] = (
        LiquidityPoolTable,
        ManagedLiquidityPoolTable,
    ),
) -> Iterator[Sequence[ChecksumAddress]]:
    """
    Find paths from `start_token` to `end_token`, formatted as an iterator of pool objects
    constructed from the database.

    Paths are discovered using the breadth-first search strategy with an optional maximum depth.

    Paths may be limited to a subset of pool types. If not specified, all valid pool types will be
    included.
    """

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

    num_standalone_pools = db_session.scalar(
        sqlalchemy.select(sqlalchemy.func.count())
        .select_from(
            *(pool_type for pool_type in pool_types if issubclass(pool_type, LiquidityPoolTable))
        )
        .where(LiquidityPoolTable.chain == chain_id)
    )
    num_managed_pools = db_session.scalar(
        sqlalchemy.select(sqlalchemy.func.count())
        .select_from(
            *(
                pool_type
                for pool_type in pool_types
                if issubclass(pool_type, ManagedLiquidityPoolTable)
            )
        )
        .where(ManagedLiquidityPoolTable.manager.has(chain=chain_id))
    )

    num_pools = num_standalone_pools + num_managed_pools

    print(f"Finding paths from {num_standalone_pools} standalone pools in database")
    print(f"Finding paths from {num_managed_pools} managed pools in database")
    print(f"Finding paths from {num_pools} known pools in database")

    match max_depth:
        case 2:
            assert start_token == end_token
            cycle_token = start_token_in_db

            candidate_token_ids = db_session.scalars(
                sqlalchemy.select(
                    sqlalchemy.union_all(
                        sqlalchemy.select(
                            LiquidityPoolTable.token1_id.label("forward_token_id")
                        ).where(LiquidityPoolTable.token0_id == cycle_token.id),
                        sqlalchemy.select(
                            LiquidityPoolTable.token0_id.label("forward_token_id")
                        ).where(LiquidityPoolTable.token1_id == cycle_token.id),
                    )
                    .subquery()
                    .columns["forward_token_id"]
                )
            ).all()

            print(f"Found {len(candidate_token_ids)} cycle token neighbors")

            # Identify tokens paired with the cycle token in at least two pools
            paired_token_ids = db_session.scalars(
                sqlalchemy.select(
                    sqlalchemy.union_all(
                        sqlalchemy.select(
                            LiquidityPoolTable.token1_id.label("forward_token_id")
                        ).where(LiquidityPoolTable.token0_id == cycle_token.id),
                        sqlalchemy.select(
                            LiquidityPoolTable.token0_id.label("forward_token_id")
                        ).where(LiquidityPoolTable.token1_id == cycle_token.id),
                    )
                    .subquery()
                    .columns["forward_token_id"]
                )
                .group_by("forward_token_id")
                .having(sqlalchemy.func.count() >= 2)
            ).all()
            assert len(set(paired_token_ids)) == len(paired_token_ids)

            print(f"Found {len(paired_token_ids)} candidate tokens")

            for token_id in paired_token_ids:
                candidate_pools = db_session.scalars(
                    sqlalchemy.union_all(
                        sqlalchemy.select(LiquidityPoolTable.address).where(
                            LiquidityPoolTable.token0_id == token_id,
                            LiquidityPoolTable.token1_id == start_token_in_db.id,
                        ),
                        sqlalchemy.select(LiquidityPoolTable.address).where(
                            LiquidityPoolTable.token0_id == start_token_in_db.id,
                            LiquidityPoolTable.token1_id == token_id,
                        ),
                    )
                ).all()

                yield from itertools.permutations(candidate_pools, 2)

        case 3:
            ...
        case None:
            ...
        case _:
            msg = "Invalid max_depth!"
            raise DegenbotValueError(msg)
