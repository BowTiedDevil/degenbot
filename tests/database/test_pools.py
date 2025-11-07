import time

from hexbytes import HexBytes
from sqlalchemy import case, distinct, func, or_, select

from degenbot.checksum_cache import get_checksum_address
from degenbot.database import db_session
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    AerodromeV2PoolTable,
    LiquidityPoolTable,
    PoolManagerTable,
    UniswapV4PoolTable,
)


def test_query_base_class():
    start = time.perf_counter()
    num_pools = db_session.scalar(select(func.count()).select_from(LiquidityPoolTable))
    print(f"Found {num_pools} pools (base table select) in {time.perf_counter() - start:.2f}s")


def test_get_pool_from_base_table():
    pool = db_session.scalar(
        select(LiquidityPoolTable).where(
            LiquidityPoolTable.address == "0x723AEf6543aecE026a15662Be4D3fb3424D502A9"
        )
    )
    assert isinstance(pool, AerodromeV2PoolTable)
    assert pool.token0.address == "0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b"
    assert pool.token1.address == "0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA"


def test_filter_by_token_id():
    start = time.perf_counter()

    weth = db_session.scalar(
        select(Erc20TokenTable).where(
            Erc20TokenTable.address == "0x4200000000000000000000000000000000000006",
            Erc20TokenTable.chain == 8453,
        )
    )

    num_pools = db_session.scalar(
        select(func.count())
        .select_from(LiquidityPoolTable)
        .where(
            or_(
                LiquidityPoolTable.token0_id == weth.id,
                LiquidityPoolTable.token1_id == weth.id,
            )
        )
    )

    print(
        f"Found {num_pools} WETH pairs (base table select with token_id filter) in {time.perf_counter() - start:.2f}s"
    )


def test_filter_by_token_relationship():
    start = time.perf_counter()

    weth = db_session.scalar(
        select(Erc20TokenTable).where(
            Erc20TokenTable.address == "0x4200000000000000000000000000000000000006",
            Erc20TokenTable.chain == 8453,
        )
    )

    num_pools = db_session.scalar(
        select(func.count())
        .select_from(LiquidityPoolTable)
        .where(
            or_(
                LiquidityPoolTable.token0.has(id=weth.id),
                LiquidityPoolTable.token1.has(id=weth.id),
            )
        )
    )

    print(
        f"Found {num_pools} WETH pairs (base table select with token relationhip .has() filter) in {time.perf_counter() - start:.2f}s"
    )


def test_find_unique_tokens_paired_with_weth():
    start = time.perf_counter()

    min_pairs = 2

    weth = db_session.scalar(
        select(Erc20TokenTable).where(
            Erc20TokenTable.address == "0x4200000000000000000000000000000000000006",
            Erc20TokenTable.chain == 8453,
        )
    )

    paired_tokens = db_session.scalars(
        select(
            distinct(
                db_session.query(
                    case(
                        (LiquidityPoolTable.token0_id == weth.id, LiquidityPoolTable.token1_id),
                        (LiquidityPoolTable.token1_id == weth.id, LiquidityPoolTable.token0_id),
                    ).label("other_token_id"),
                    func.count().label("cnt"),
                )
                .filter(
                    (LiquidityPoolTable.token0_id == weth.id)
                    | (LiquidityPoolTable.token1_id == weth.id)
                )
                .group_by("other_token_id")
                .having(func.count() >= min_pairs)
                .subquery()
                .columns["other_token_id"]
            )
        )
    ).all()

    print(
        f"Found {len(paired_tokens)} tokens with at least {min_pairs} WETH pairs in {time.perf_counter() - start:.2f}s"
    )


def test_get_uniswap_v4_pool():
    pool_hash = HexBytes("0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a")
    pool_manager_address = get_checksum_address("0x498581fF718922c3f8e6A244956aF099B2652b2b")
    chain_id = 8453

    start = time.perf_counter()

    pool_manager_in_db = db_session.scalar(
        select(PoolManagerTable).where(
            PoolManagerTable.address == pool_manager_address,
            PoolManagerTable.chain == chain_id,
        )
    )
    assert pool_manager_in_db is not None

    pool_in_db = db_session.scalar(
        select(UniswapV4PoolTable).where(
            UniswapV4PoolTable.pool_hash == pool_hash.to_0x_hex(),
            UniswapV4PoolTable.manager.has(id=pool_manager_in_db.id),
        ),
    )

    print(f"Query completed in {time.perf_counter() - start:.2f}s")

    assert pool_in_db.hooks == "0x0000000000000000000000000000000000000000"
    assert pool_in_db.currency0.address == "0x0000000000000000000000000000000000000000"
    assert pool_in_db.currency1.address == "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
