"""Regression test for the `pool update` multi-chunk control loop.

Background: commit ``a67214ae`` routed the ``ExchangeTable.last_update_block``
stamp through the Rust ``db_set_exchange_last_update_block`` seam, which
writes on its **own** rusqlite connection. The ``pool_update`` loop drives
its chunk-advancement off the SQLAlchemy ORM ``exchange.last_update_block``
attribute. Under SQLite WAL snapshot isolation the long-lived session's read
snapshot does not see the Rust-written stamp, so from the third chunk onward
``exchanges_to_update`` goes empty (the membership predicate
``last_update_block + 1 == working_start_block`` fails against the stale ORM
value) and the loop terminates early — freezing ``last_update_block`` at the
end of the second chunk with zero pools discovered.

This test pins the loop-advancement behavior with a stub provider + a stub
event fetch (no RPC, no events → exercises only the control flow + the
stamp readback). With the bug present the loop stalls at block 19999 after
two 10000-block chunks; the fix lets it reach the requested ``--to-block``.
"""

from __future__ import annotations

import sqlite3
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest
from click.testing import CliRunner

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import pool as pool_mod
from degenbot.cli.pool import pool_update
from degenbot.database.models.base import ExchangeTable
from degenbot.database.operations import (
    create_new_sqlite_database,
    get_scoped_sqlite_session,
)
from degenbot.database.session_manager import DatabaseSessionManager

if TYPE_CHECKING:
    import pathlib

CHAIN = 1
UNISWAP_V2_FACTORY = get_checksum_address("0x" + "f" * 40)


class _StubProvider:
    """Minimal provider shim: ``chain_id`` + a ``get_block`` returning a tip
    past the requested ``--to-block`` so the "ahead of chain tip" guard
    passes."""

    chain_id = CHAIN

    def get_block(self, _tag: str) -> dict[str, int]:
        return {"number": 30_000}


class _StubBot:
    """The loop touches only ``bot.db()`` + ``bot.config.database.path``."""

    def __init__(self, db_manager: DatabaseSessionManager, path: str) -> None:
        self._db = db_manager
        self.config = SimpleNamespace(database=SimpleNamespace(path=path))

    def db(self):
        # Mirror production: ``bot.db`` is a DatabaseSessionManager; calling
        # it (``bot.db()``) yields a SQLAlchemy Session context manager.
        return self._db()


def _seed_exchange(db_path: pathlib.Path) -> DatabaseSessionManager:
    """Create a fresh file DB + activate a single ``uniswap_v2`` exchange.

    Returns a live ``DatabaseSessionManager`` over the file (the loop holds
    one long-lived session, matching the production code path).
    """
    create_new_sqlite_database(db_path)
    session = get_scoped_sqlite_session(db_path)
    with session() as s:
        s.add(
            ExchangeTable(
                chain_id=CHAIN,
                name="uniswap_v2",
                active=True,
                last_update_block=None,
                factory=UNISWAP_V2_FACTORY,
                deployer=None,
            ),
        )
        s.commit()
    return DatabaseSessionManager(session)


@pytest.fixture
def stub_bot(tmp_path: pathlib.Path) -> _StubBot:
    db_path = tmp_path / "pool_update.db"
    db_manager = _seed_exchange(db_path)
    return _StubBot(db_manager, str(db_path))


def test_pool_update_advances_past_second_chunk(
    stub_bot: _StubBot,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``pool update`` advances through all chunks to ``--to-block``.

    With a stub provider returning no ``PairCreated`` events, no pools are
    discovered — but the loop must still stamp ``last_update_block`` forward
    on every chunk. A three-chunk run (``--chunk 10000 --to-block 29999``)
    must end with ``last_update_block == 29999``; the stale-readback bug
    freezes it at ``19999`` (the end of chunk 2).
    """
    stub_bot  # noqa: B018 — fixture builds the seeded DB + bot

    def _provider(*, chain_id: int) -> _StubProvider:
        return _StubProvider()

    monkeypatch.setattr(pool_mod, "get_provider_from_config", _provider)
    monkeypatch.setattr(pool_mod, "get_events_from_contract", lambda **_: [])

    runner = CliRunner()
    result = runner.invoke(
        pool_update,  # type: ignore[arg-type]
        ["--to-block", "29999", "--chunk", "10000"],
        obj=stub_bot,
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output

    # Read ground-truth via a fresh sqlite connection: the long-lived
    # scoped_session's thread-local connection stays pinned to an early WAL
    # snapshot, so reading through SQLAlchemy returns a stale
    # ``last_update_block`` even after ``pool_update`` returns. A fresh
    # connection sees the Rust-written stamps.
    with sqlite3.connect(stub_bot.config.database.path) as conn:
        last_update_block = conn.execute(
            "SELECT last_update_block FROM exchanges WHERE name = 'uniswap_v2'",
        ).fetchone()[0]
    conn.close()
    assert last_update_block == 29999
