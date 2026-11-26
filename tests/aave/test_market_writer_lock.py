"""The PyO3 update seam takes the market's shared writer lock.

`degenbot._ffi.aave.run_aave_update` acquires `AaveMarketWriter` for
(database path, market id) BEFORE it builds the live transport, so the same
advisory file lock the CLI and every Rust consumer take also excludes the
Python seam. Python holds a competing `flock` on that exact lock file here: the
refusal proves the seam goes through the shared writer, and the dead RPC
endpoint proves the refusal lands before any transport work.
"""

from __future__ import annotations

import fcntl
from typing import TYPE_CHECKING

import pytest

from degenbot.db import db_upgrade_database
from tests.helpers.database import sqlite_connection
from tests.helpers.seam import CancelHandle, run_aave_update

if TYPE_CHECKING:
    from pathlib import Path

MARKET_ID = 1


def _seed_db(db_path: Path) -> None:
    db_upgrade_database(str(db_path))
    with sqlite_connection(db_path) as connection:
        connection.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) "
            "VALUES (1, 1, 'Aave Ethereum Market', 1, 1000)"
        )


def _lock_path(db_path: Path) -> Path:
    """The lock file one (database, market) pair names, exactly as
    `degenbot_aave::market_lock_path` builds it: a sibling of the database.

    The canonical parent directory is what the writer keys on, so the fixture
    resolves the temp path before naming the file it competes on.
    """
    canonical = db_path.resolve()
    return canonical.with_name(f"{canonical.name}.aave-market-{MARKET_ID}.lock")


def test_the_pyo3_seam_refuses_a_market_another_writer_holds(tmp_path: Path) -> None:
    db_path = tmp_path / "cand.db"
    _seed_db(db_path)
    lock_path = _lock_path(db_path)

    with lock_path.open("a+") as holder:
        fcntl.flock(holder.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(ValueError, match="held by another Aave writer"):
            run_aave_update(
                database_path=str(db_path),
                chain_id=1,
                market_id=MARKET_ID,
                to_block=1010,
                chunk_size=10,
                # Unreachable on purpose: the refusal must land before the
                # transport is built, so reaching the network here is a
                # failure of the contract under test, not a flake.
                rpc_url="http://127.0.0.1:1",
                cancel_handle=CancelHandle(),
            )
        fcntl.flock(holder.fileno(), fcntl.LOCK_UN)
