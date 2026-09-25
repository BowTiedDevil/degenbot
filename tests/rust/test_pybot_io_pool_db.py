"""Functional tests for the `BotIo` pool-builder database seam."""

from __future__ import annotations

from degenbot._ffi import BotIo
from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot.db import db_create_new_database
from tests.helpers.database import sqlite_connection

CHAIN = 1
_POOL_OFFLINE_JSON = '{"chain_id":1,"block_number":1,"timestamp":1,"calls":{},"code":{}}'
POOL_ADDR = "0x" + "12" * 20
TOK0_ADDR = "0x" + "34" * 20
TOK1_ADDR = "0x" + "56" * 20
FACTORY = "0x" + "78" * 20
EXCHANGE_NAME = "uniswap_v3"


def _offline_provider() -> RustAlloyProvider:
    return RustAlloyProvider.offline_from_json_string(_POOL_OFFLINE_JSON)


def _seed_v3_pool(database_path: str) -> int:
    with sqlite_connection(database_path) as connection:
        connection.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES (1, ?, ?, 1, ?)",
            (CHAIN, EXCHANGE_NAME, FACTORY),
        )
        connection.executemany(
            "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            [
                (1, CHAIN, TOK0_ADDR, "T0", "T0SYM", 6),
                (2, CHAIN, TOK1_ADDR, "T1", "T1SYM", 18),
            ],
        )
        connection.execute(
            "INSERT INTO pools (id, chain, address, kind, token0_id, token1_id, exchange_id) "
            "VALUES (10, ?, ?, 'uniswap_v3', 1, 2, 1)",
            (CHAIN, POOL_ADDR),
        )
        connection.execute(
            "INSERT INTO uniswap_v3_pools (pool_id, tick_spacing, fee_token0, fee_token1, "
            "fee_denominator) VALUES (10, 60, 3, 3, 1000)"
        )
        connection.execute(
            "INSERT INTO initialization_maps (id, pool_id, word, bitmap) VALUES (1, 10, -3, ?)",
            (str(2**128 + 7),),
        )
        connection.execute(
            "INSERT INTO liquidity_positions (id, pool_id, tick, liquidity_net, liquidity_gross) "
            "VALUES (1, 10, -100, ?, ?)",
            (str(2**128 - 1), "123456"),
        )
        return 10


def _io(database_path: str) -> BotIo:
    return BotIo(provider=_offline_provider(), database_path=database_path)


def test_fetch_pool_row_returns_seeded_pool(tmp_path):
    database_path = str(tmp_path / "pool_seam.db")
    db_create_new_database(database_path)
    pool_id = _seed_v3_pool(database_path)

    row = _io(database_path).fetch_pool_row(chain_id=CHAIN, address=POOL_ADDR)
    assert row is not None
    assert row.id == pool_id
    assert row.chain == CHAIN
    assert row.address.lower() == POOL_ADDR.lower()
    assert row.kind == "uniswap_v3"
    assert row.token0_id == 1
    assert row.token1_id == 2
    assert row.exchange_id == 1


def test_fetch_exchange(tmp_path):
    database_path = str(tmp_path / "fk_seam.db")
    db_create_new_database(database_path)
    _seed_v3_pool(database_path)

    io = _io(database_path)
    pool = io.fetch_pool_row(chain_id=CHAIN, address=POOL_ADDR)
    assert pool is not None
    exchange = io.fetch_exchange(pool.exchange_id)
    assert exchange is not None
    assert exchange.factory.lower() == FACTORY.lower()
    assert exchange.name == EXCHANGE_NAME
    assert exchange.deployer is None


def test_no_database_path_skips_pool_reads():
    io = BotIo(provider=_offline_provider(), database_path=None)
    assert io.fetch_pool_row(CHAIN, POOL_ADDR) is None
    assert io.fetch_exchange(1) is None
