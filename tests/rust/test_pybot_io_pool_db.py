"""Parity + functional tests for the `PyBotIo` pool-builder DB seam (QVMWQC).

`PyBotIo.fetch_pool_row` / `fetch_pool_kind` / `fetch_token_by_id` /
`fetch_exchange` / `fetch_liquidity_positions` / `fetch_initialization_maps`
route the sync pool builders' construction-time DB reads through
`degenbot-db`, replacing the `SQLAlchemy` ORM `session.scalar(select(...))`
+ lazy-loaded relationship traversal. These tests seed a temp DB + assert
that the Rust seam reads the same rows the ORM does (§4.2 parity):

- `fetch_pool_row` returns a `LiquidityPoolRow` whose scalar + FK-id columns
  match the ORM `LiquidityPoolTable` row.
- `fetch_pool_kind` returns the per-DEX subclass row (V3 `tick_spacing` +
  fees + liquidity-update marker).
- `fetch_token_by_id` / `fetch_exchange` hydrate the FK relationships.
- `fetch_liquidity_positions` / `fetch_initialization_maps` hydrate the V3
  tick snapshot (tick/liquidity_net/gross + word/bitmap), U256 as `int`.
- the no-`database_path` skip returns `None` / empty (mirrors the prior
  `contextlib.suppress(Exception)` skip).
"""

from __future__ import annotations

from sqlalchemy import create_engine, select
from sqlalchemy.orm import Session

from degenbot.database.models import Base
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    InitializationMapTable,
    LiquidityPoolTable,
    LiquidityPositionTable,
    UniswapV3PoolTable,
)
from degenbot.degenbot_rs import PyBotIo, db_create_new_database

CHAIN = 1
POOL_ADDR = "0x" + "12" * 20
TOK0_ADDR = "0x" + "34" * 20
TOK1_ADDR = "0x" + "56" * 20
FACTORY = "0x" + "78" * 20
EXCHANGE_NAME = "uniswap_v3"


def _seed_v3_pool(database_path: str) -> int:
    """Seed a V3 pool + exchange + tokens + subclass row + tick snapshot; return pool id."""
    engine = create_engine(f"sqlite:///{database_path}")
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        exch = ExchangeTable(
            id=1,
            chain_id=CHAIN,
            name=EXCHANGE_NAME,
            active=True,
            factory=FACTORY,
            deployer=None,
        )
        t0 = Erc20TokenTable(
            id=1, chain=CHAIN, address=TOK0_ADDR, name="T0", symbol="T0SYM", decimals=6
        )
        t1 = Erc20TokenTable(
            id=2, chain=CHAIN, address=TOK1_ADDR, name="T1", symbol="T1SYM", decimals=18
        )
        session.add_all([exch, t0, t1])
        session.flush()

        # Joined-table inheritance: construct the concrete V3 subclass as a
        # single object (carries both the base `pools` columns + the V3
        # subclass columns). SQLAlchemy writes both rows + sets the polymorphic
        # identity (`uniswap_v3`).
        pool = UniswapV3PoolTable(
            id=10,
            chain=CHAIN,
            address=POOL_ADDR,
            kind="uniswap_v3",
            token0_id=t0.id,
            token1_id=t1.id,
            exchange_id=exch.id,
            tick_spacing=60,
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
        )
        session.add(pool)
        session.flush()
        pool_id = pool.id

        # Tick snapshot: one init-map word + one liquidity position.
        session.add(
            InitializationMapTable(
                id=1, pool_id=pool_id, word=-3, bitmap=str(2**128 + 7)
            )
        )
        session.add(
            LiquidityPositionTable(
                id=1,
                pool_id=pool_id,
                tick=-100,
                liquidity_net=str(2**128 - 1),  # a big uint128
                liquidity_gross=str(123456),
            )
        )
        session.commit()
    engine.dispose()
    return pool_id


def _io(database_path: str) -> PyBotIo:
    return PyBotIo(provider=object(), database_path=database_path)


def test_fetch_pool_row_parity(tmp_path):
    database_path = str(tmp_path / "pool_seam.db")
    db_create_new_database(database_path)
    pool_id = _seed_v3_pool(database_path)

    io = _io(database_path)
    row = io.fetch_pool_row(chain_id=CHAIN, address=POOL_ADDR)
    assert row is not None
    assert row.id == pool_id
    assert row.chain == CHAIN
    assert row.address.lower() == POOL_ADDR.lower()
    assert row.kind == "uniswap_v3"

    # Parity: the ORM row has the same FK ids.
    engine = create_engine(f"sqlite:///{database_path}")
    try:
        with Session(engine) as session:
            orm = session.scalar(
                select(LiquidityPoolTable).where(
                    LiquidityPoolTable.address == POOL_ADDR,
                    LiquidityPoolTable.chain == CHAIN,
                )
            )
            assert orm is not None
            assert orm.token0_id == row.token0_id
            assert orm.token1_id == row.token1_id
            assert orm.exchange_id == row.exchange_id
    finally:
        engine.dispose()


def test_fetch_pool_kind_v3(tmp_path):
    database_path = str(tmp_path / "poolkind_seam.db")
    db_create_new_database(database_path)
    pool_id = _seed_v3_pool(database_path)

    io = _io(database_path)
    row = io.fetch_pool_row(chain_id=CHAIN, address=POOL_ADDR)
    assert row is not None
    kind = io.fetch_pool_kind(kind=row.kind, pool_id=pool_id)
    assert kind is not None
    assert kind.variant == "v3"
    assert kind.tick_spacing == 60
    assert kind.fee_token0 == 3
    assert kind.fee_token1 == 3
    assert kind.fee_denominator == 1000
    assert kind.liquidity_update_block is None  # not set in the fixture


def test_fetch_token_by_id_and_exchange(tmp_path):
    database_path = str(tmp_path / "fk_seam.db")
    db_create_new_database(database_path)
    _seed_v3_pool(database_path)

    io = _io(database_path)
    pool = io.fetch_pool_row(chain_id=CHAIN, address=POOL_ADDR)
    assert pool is not None

    t0 = io.fetch_token_by_id(pool.token0_id)
    t1 = io.fetch_token_by_id(pool.token1_id)
    assert t0 is not None and t1 is not None
    assert t0.address.lower() == TOK0_ADDR.lower()
    assert t0.name == "T0"
    assert t0.symbol == "T0SYM"
    assert t0.decimals == 6
    assert t1.decimals == 18

    exch = io.fetch_exchange(pool.exchange_id)
    assert exch is not None
    assert exch.factory.lower() == FACTORY.lower()
    assert exch.name == EXCHANGE_NAME
    assert exch.deployer is None


def test_fetch_tick_snapshot(tmp_path):
    database_path = str(tmp_path / "ticksnap_seam.db")
    db_create_new_database(database_path)
    pool_id = _seed_v3_pool(database_path)

    io = _io(database_path)
    positions = io.fetch_liquidity_positions(pool_id)
    init_maps = io.fetch_initialization_maps(pool_id)
    assert len(positions) == 1
    assert len(init_maps) == 1

    pos = positions[0]
    assert pos.tick == -100
    assert int(pos.liquidity_net) == 2**128 - 1
    assert int(pos.liquidity_gross) == 123456

    im = init_maps[0]
    assert im.word == -3
    assert int(im.bitmap) == 2**128 + 7


def test_no_database_path_skips_pool_reads():
    io = PyBotIo(provider=object(), database_path=None)
    assert io.fetch_pool_row(CHAIN, POOL_ADDR) is None
    assert io.fetch_pool_kind("uniswap_v3", 1) is None
    assert io.fetch_token_by_id(1) is None
    assert io.fetch_exchange(1) is None
    assert io.fetch_liquidity_positions(1) == []
    assert io.fetch_initialization_maps(1) == []