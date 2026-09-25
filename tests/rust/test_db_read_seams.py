"""Focused Python binding tests for the production DB read seams."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.db import (
    db_create_new_database,
    db_fetch_graph_edition,
    db_resolve_token_ids,
)
from tests.helpers.database import sqlite_connection

if TYPE_CHECKING:
    from pathlib import Path


ADDRESS_A = "0x1111111111111111111111111111111111111111"
ADDRESS_B = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
MISSING_ADDRESS = "0x3333333333333333333333333333333333333333"


def _seed_database(path: Path) -> None:
    db_create_new_database(str(path))
    with sqlite_connection(path) as connection:
        connection.executemany(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (?, ?, ?)",
            [(1, 1, ADDRESS_A), (2, 1, ADDRESS_B), (3, 10, ADDRESS_A)],
        )
        connection.executemany(
            "INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES (?, ?, ?, ?, ?)",
            [(1, 1, "chain-one", 1, ADDRESS_A), (2, 10, "chain-ten", 1, ADDRESS_A)],
        )
        connection.executemany(
            "INSERT INTO pools "
            "(id, address, chain, kind, token0_id, token1_id, exchange_id) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (2, ADDRESS_A, 1, "uniswap_v2", 1, 2, 1),
                (7, ADDRESS_B, 1, "uniswap_v3", 1, 2, 1),
                (100, MISSING_ADDRESS, 10, "uniswap_v2", 3, 3, 2),
            ],
        )
        connection.executemany(
            "INSERT INTO pool_managers (id, address, chain, kind, exchange_id) "
            "VALUES (?, ?, ?, ?, ?)",
            [
                (20, ADDRESS_A, 1, "uniswap_v4", 1),
                (21, ADDRESS_A, 10, "uniswap_v4", 2),
            ],
        )
        connection.executemany(
            "INSERT INTO managed_pools (id, kind, manager_id) VALUES (?, ?, ?)",
            [(4, "uniswap_v4", 20), (9, "uniswap_v4", 20), (100, "uniswap_v4", 21)],
        )


def test_resolve_token_ids_preserves_missing_duplicate_and_chain_semantics(
    tmp_path: Path,
) -> None:
    path = tmp_path / "tokens.db"
    _seed_database(path)

    assert db_resolve_token_ids(str(path), 1, []) == {}
    assert db_resolve_token_ids("/path/that/does/not/exist.db", 1, []) == {}
    assert db_resolve_token_ids(
        str(path),
        1,
        [ADDRESS_A, MISSING_ADDRESS, ADDRESS_A, ADDRESS_B.lower()],
    ) == {ADDRESS_A: 1, ADDRESS_B: 2}
    assert db_resolve_token_ids(str(path), 10, [ADDRESS_A, ADDRESS_B]) == {ADDRESS_A: 3}


def test_fetch_graph_edition_returns_v2_v3_v4_chain_fingerprint(tmp_path: Path) -> None:
    path = tmp_path / "graph.db"
    _seed_database(path)

    assert db_fetch_graph_edition(str(path), 1) == (2, 7, 2, 9)
    assert db_fetch_graph_edition(str(path), 10) == (1, 100, 1, 100)
    assert db_fetch_graph_edition(str(path), 999) == (0, 0, 0, 0)


def test_read_seams_map_invalid_input_and_database_errors_to_value_error(
    tmp_path: Path,
) -> None:
    with pytest.raises(ValueError, match="Invalid address"):
        db_resolve_token_ids(str(tmp_path / "unused.db"), 1, ["not-an-address"])

    foreign_db = tmp_path / "foreign.db"
    foreign_db.write_text("not a sqlite database")
    with pytest.raises(ValueError, match="file is not a database"):
        db_fetch_graph_edition(str(foreign_db), 1)
