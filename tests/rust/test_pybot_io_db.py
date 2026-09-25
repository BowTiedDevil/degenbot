"""Functional tests for the `BotIo` construction-time token database seam."""

from __future__ import annotations

from degenbot._ffi import BotIo
from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import db_create_new_database
from tests.helpers.database import sqlite_connection

CHAIN = 1
ADDR = get_checksum_address("0x" + "ab" * 20)
_MINIMAL_OFFLINE_JSON = '{"chain_id":1,"block_number":1,"timestamp":1,"calls":{},"code":{}}'


def _offline_provider() -> RustAlloyProvider:
    return RustAlloyProvider.offline_from_json_string(_MINIMAL_OFFLINE_JSON)


def _seed_token_row(database_path: str) -> int:
    with sqlite_connection(database_path) as connection:
        cursor = connection.execute(
            "INSERT INTO erc20_tokens (chain, address) VALUES (?, ?)",
            (CHAIN, ADDR),
        )
        assert cursor.lastrowid is not None
        return int(cursor.lastrowid)


def _read_token_row(database_path: str) -> tuple[object, ...]:
    with sqlite_connection(database_path) as connection:
        row = connection.execute(
            "SELECT id, chain, address, name, symbol, decimals "
            "FROM erc20_tokens WHERE address = ? AND chain = ?",
            (ADDR, CHAIN),
        ).fetchone()
        assert row is not None
        return row


def test_fetch_erc20_token_returns_seeded_row(tmp_path):
    database_path = str(tmp_path / "erc20_seam.db")
    db_create_new_database(database_path)
    expected_id = _seed_token_row(database_path)

    row = BotIo(provider=_offline_provider(), database_path=database_path).fetch_erc20_token(
        chain_id=CHAIN, address=ADDR
    )

    assert row is not None
    assert row.id == expected_id
    assert row.chain == CHAIN
    assert row.address.lower() == ADDR.lower()
    assert row.name is None
    assert row.symbol is None
    assert row.decimals is None

    raw = _read_token_row(database_path)
    assert raw[0] == row.id
    assert raw[1] == row.chain
    assert raw[2].lower() == row.address.lower()
    assert raw[3:] == (None, None, None)


def test_fetch_erc20_token_missing_row_returns_none(tmp_path):
    database_path = str(tmp_path / "erc20_seam_missing.db")
    db_create_new_database(database_path)
    io = BotIo(provider=_offline_provider(), database_path=database_path)
    assert io.fetch_erc20_token(chain_id=CHAIN, address="0x" + "00" * 20) is None


def test_update_erc20_token_metadata_lands_update(tmp_path):
    database_path = str(tmp_path / "erc20_seam_writeback.db")
    db_create_new_database(database_path)
    _seed_token_row(database_path)

    io = BotIo(provider=_offline_provider(), database_path=database_path)
    result = io.update_erc20_token_metadata(
        chain_id=CHAIN, address=ADDR, name="Test", symbol="TST", decimals=6
    )
    assert result is None
    assert _read_token_row(database_path)[3:] == ("Test", "TST", 6)


def test_no_database_path_skips_db_read():
    io = BotIo(provider=_offline_provider(), database_path=None)
    assert io.database_path is None
    assert io.fetch_erc20_token(chain_id=CHAIN, address=ADDR) is None
    assert io.update_erc20_token_metadata(CHAIN, ADDR, "x", "y", 1) is None
