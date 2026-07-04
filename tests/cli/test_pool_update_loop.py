"""Tests for the `pool update` CLI command's boot + hand-off shape.

Task ``JJ232N`` (epic ``2SFL6I``): ``pool_update`` is now a thin boot +
hand-off to the Rust-owned chunk loop (``degenbot_rs.run_pool_update``,
Task ``QZHNZQ``). The previous test pinned the multi-chunk control flow
(commit ``a67214ae``'s stale-readback regression); with the loop now in
Rust, the regression test moves to the Rust core's
``apply_chunk_writes_on_conn_*`` tests (Task ``CKXCOB`` 3c). This file now
asserts the *hand-off*: the CLI builds a ``CancelHandle``, installs a
SIGINT handler, threads a progress callback + the resolved ``to_block`` +
``rpc_url``, calls ``degenbot_rs.run_pool_update`` exactly once per
chain, + echoes the returned report ``dict``.
"""

from __future__ import annotations

import sqlite3
import types
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
RPC_URL = "http://test.invalid:8545"


class _StubBot:
    """The ``pool_update`` hand-off touches only ``bot.config`` (the chain id,
    the database path, + the RPC cascade). No SQLAlchemy session is opened."""

    def __init__(self, db_path: pathlib.Path) -> None:
        self.config = types.SimpleNamespace(
            database=types.SimpleNamespace(path=str(db_path)),
            default_chain_id=CHAIN,
            rpc={CHAIN: RPC_URL},
        )


def _seed_exchange(db_path: pathlib.Path) -> DatabaseSessionManager:
    """Create a fresh file DB + activate a single ``uniswap_v2`` exchange.

    Returns a ``DatabaseSessionManager`` so the autouse ``dispose_all`` teardown
    in ``tests/conftest.py`` releases the SQLAlchemy engine.
    """
    create_new_sqlite_database(db_path)
    db_manager = DatabaseSessionManager(get_scoped_sqlite_session(db_path))
    with db_manager() as s:
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
    return db_manager


@pytest.fixture
def stub_bot(tmp_path: pathlib.Path) -> _StubBot:
    db_path = tmp_path / "pool_update.db"
    _seed_exchange(db_path)
    return _StubBot(db_path)


def test_pool_update_handoff_calls_run_pool_update_once_per_chain(
    stub_bot: _StubBot,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``pool update`` delegates to ``degenbot_rs.run_pool_update`` with the
    resolved config + a ``CancelHandle``, then echoes the report.

    The stub ``run_pool_update`` records the call args + returns a fixed
    report dict; no live RPC is touched. Asserts: (a) called exactly once,
    (b) the database path / chain id / chunk size / rpc_url are threaded,
    (c) a fresh ``CancelHandle`` is passed (the SIGINT bridge), (d) the
    progress callback is a callable, (e) the returned report is echoed.
    """
    stub_bot  # noqa: B018 -- fixture seeds the DB + bot

    captured: dict[str, object] = {}

    def _fake_run_pool_update(
        *,
        database_path: str,
        chain_id: int,
        to_block: int | None,
        chunk_size: int,
        rpc_url: str,
        progress_callback: object,
        cancel_handle: object,
    ) -> dict[str, object]:
        captured.update(
            database_path=database_path,
            chain_id=chain_id,
            to_block=to_block,
            chunk_size=chunk_size,
            rpc_url=rpc_url,
            progress_callback=progress_callback,
            cancel_handle=cancel_handle,
        )
        # Fire the progress callback once (proves tqdm-on-callback wiring).
        assert callable(progress_callback)
        progress_callback(
            {
                "chain_id": chain_id,
                "chunk_start": 1,
                "chunk_end": 10_000,
                "pools_written": 0,
                "liquidity_apply_count": 0,
                "committed": True,
            }
        )
        return {
            "chain_id": chain_id,
            "from_block": 1,
            "to_block": 10_000,
            "chunks_committed": 1,
            "total_pools_written": 0,
            "total_liquidity_applies": 0,
        }

    monkeypatch.setattr(pool_mod, "run_pool_update", _fake_run_pool_update)
    # Avoid a real provider build for tag+offset resolution: pass a concrete
    # `--to-block` int (no tag+offset => no provider round-trip).
    monkeypatch.setattr(pool_mod, "resolve_http_rpc_uri", lambda *a, **k: RPC_URL)

    runner = CliRunner()
    result = runner.invoke(
        pool_update,  # type: ignore[arg-type]
        ["--to-block", "10000", "--chunk", "10000"],
        obj=stub_bot,
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output

    # (a) exactly one call.
    assert "database_path" in captured, "run_pool_update was not called"
    # (b) the config is threaded.
    assert captured["database_path"] == stub_bot.config.database.path
    assert captured["chain_id"] == CHAIN
    assert captured["chunk_size"] == 10_000
    assert captured["rpc_url"] == RPC_URL
    assert captured["to_block"] == 10_000
    # (c) a CancelHandle instance is threaded (the SIGINT bridge).
    cancel_handle = captured["cancel_handle"]
    assert hasattr(cancel_handle, "cancel"), "cancel_handle has no .cancel()"
    assert hasattr(cancel_handle, "is_cancelled"), "cancel_handle has no .is_cancelled()"
    # (d) the progress callback is callable (checked inside the stub above).
    # (e) the report is echoed.
    assert "10000" in result.output
    assert "1 chunks" in result.output
    # The exchange stamp is NOT set by the Python shell (the Rust core owns it);
    # the hand-off doesn't touch the DB directly.
    conn = sqlite3.connect(stub_bot.config.database.path)
    try:
        row = conn.execute(
            "SELECT last_update_block FROM exchanges WHERE name = 'uniswap_v2'",
        ).fetchone()
    finally:
        conn.close()
    # The fake run_pool_update didn't write, so the stamp stays at its seeded
    # value (None). This proves the shell doesn't stamp -- only the core does.
    assert row[0] is None
