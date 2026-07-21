"""CLI tests for the `exchange activate/deactivate` Rust-seam cutover (MXAEWD).

The ~30 commands in `src/degenbot/cli/exchange.py` are thin delegating shells
over the Rust core substrate (`degenbot._ffi.db_upsert_exchange` /
`db_set_exchange_active` / `db_fetch_exchange_by_name` /
`db_upsert_pool_manager`). These tests pin the user-facing behavior end to
end: activation inserts an `active=False` row then flips it `True`;
re-activation short-circuits with "already activated"; deactivation flips
`False` / short-circuits "already deactivated"; deactivating a never-seen
exchange prints "no entry". The V4 activate additionally seeds (idempotently
reasserts) a `pool_managers` row.

The ground-truth readback is via a fresh `sqlite3` connection (matching
`test_pool_update_loop.py`) so the Rust-written state is visible (a fresh
connection → a fresh WAL snapshot, vs. a stale long-lived SQLAlchemy session).
"""

from __future__ import annotations

import sqlite3
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest
from click.testing import CliRunner

from degenbot.cli.exchange import activate, deactivate
from degenbot.database.operations import create_new_sqlite_database
from degenbot.db import db_fetch_exchange_by_name
from degenbot.uniswap.deployments import (
    EthereumMainnetUniswapV2,
    EthereumMainnetUniswapV4,
)

if TYPE_CHECKING:
    import pathlib


class _StubBot:
    """The exchange CLI touches only `bot.config.database.path`."""

    def __init__(self, path: str) -> None:
        self.config = SimpleNamespace(database=SimpleNamespace(path=path))


@pytest.fixture
def stub_bot(tmp_path: pathlib.Path) -> _StubBot:
    db_path = tmp_path / "exchange_cli.db"
    create_new_sqlite_database(db_path)
    db_path.chmod(0o644)
    return _StubBot(str(db_path))


def _run(runner: CliRunner, cmd, args: list[str], bot: _StubBot):
    return runner.invoke(cmd, args, obj=bot, catch_exceptions=False)


# ── V2 activate / re-activate / deactivate / re-deactivate ───────────────


def test_activate_inserts_active_true_factory_round_trips(stub_bot: _StubBot) -> None:
    runner = CliRunner()
    result = _run(runner, activate, ["ethereum_uniswap_v2"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "Activated Uniswap V2 on Ethereum" in result.output

    row = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v2",
    )
    assert row is not None
    assert row.active is True
    assert row.factory == EthereumMainnetUniswapV2.factory.address
    assert row.last_update_block is None


def test_reactivate_is_already_activated_noop(stub_bot: _StubBot) -> None:
    runner = CliRunner()
    _run(runner, activate, ["ethereum_uniswap_v2"], stub_bot)
    first = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v2",
    )
    assert first is not None

    result = _run(runner, activate, ["ethereum_uniswap_v2"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "already activated" in result.output

    second = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v2",
    )
    assert second is not None
    assert second.id == first.id  # no new insert
    assert second.active is True


def test_deactivate_flips_active_false(stub_bot: _StubBot) -> None:
    runner = CliRunner()
    _run(runner, activate, ["ethereum_uniswap_v2"], stub_bot)

    result = _run(runner, deactivate, ["ethereum_uniswap_v2"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "Deactivated Uniswap V2 on Ethereum" in result.output

    row = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v2",
    )
    assert row is not None
    assert row.active is False


def test_redeactivate_is_already_deactivated_noop(stub_bot: _StubBot) -> None:
    runner = CliRunner()
    _run(runner, activate, ["ethereum_uniswap_v2"], stub_bot)
    _run(runner, deactivate, ["ethereum_uniswap_v2"], stub_bot)

    result = _run(runner, deactivate, ["ethereum_uniswap_v2"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "already deactivated" in result.output

    row = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v2",
    )
    assert row is not None
    assert row.active is False


def test_deactivate_missing_exchange_prints_no_entry(stub_bot: _StubBot) -> None:
    # ethereum_uniswap_v3 is never activated → "no entry"
    runner = CliRunner()
    result = _run(runner, deactivate, ["ethereum_uniswap_v3"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "no entry" in result.output.lower()
    assert "Uniswap V3" in result.output


# ── V4 activate seeds + idempotently reasserts the pool manager ───────────


def _pool_manager_row(db_path: str) -> tuple[int, str, str] | None:
    """Read the single `pool_managers` row (id, address, state_view) via a
    fresh sqlite3 connection (ground-truth)."""
    with sqlite3.connect(db_path) as conn:
        row = conn.execute(
            "SELECT id, address, state_view FROM pool_managers",
        ).fetchone()
    conn.close()
    if row is None:
        return None
    return row[0], row[1], row[2]


def test_activate_v4_seeds_pool_manager_and_is_idempotent(stub_bot: _StubBot) -> None:
    runner = CliRunner()
    result = _run(runner, activate, ["ethereum_uniswap_v4"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "Activated Uniswap V4 on Ethereum" in result.output

    # exchange active + factory == pool_manager.address
    row = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v4",
    )
    assert row is not None
    assert row.active is True
    assert row.factory == EthereumMainnetUniswapV4.pool_manager.address

    # a pool_managers row exists at the manager address.
    pm = _pool_manager_row(stub_bot.config.database.path)
    assert pm is not None
    pm_id, pm_addr, pm_state_view = pm
    assert pm_addr == EthereumMainnetUniswapV4.pool_manager.address
    assert pm_state_view == EthereumMainnetUniswapV4.state_view.address

    # re-activate → idempotent: exchange stays active (already-activated
    # short-circuit), pool manager row unchanged (same id, same address).
    result2 = _run(runner, activate, ["ethereum_uniswap_v4"], stub_bot)
    assert result2.exit_code == 0, result2.output
    assert "already activated" in result2.output

    pm_after = _pool_manager_row(stub_bot.config.database.path)
    assert pm_after is not None
    assert pm_after[0] == pm_id
    assert pm_after[1] == EthereumMainnetUniswapV4.pool_manager.address


def test_deactivate_v4_then_reactivate_reasserts_manager_unchanged(
    stub_bot: _StubBot,
) -> None:
    runner = CliRunner()
    _run(runner, activate, ["ethereum_uniswap_v4"], stub_bot)
    _run(runner, deactivate, ["ethereum_uniswap_v4"], stub_bot)

    # row now inactive; pool manager row still present.
    row = db_fetch_exchange_by_name(
        database_path=stub_bot.config.database.path,
        chain_id=1,
        name="uniswap_v4",
    )
    assert row is not None
    assert row.active is False

    # re-activate an inactive V4 exchange: the shell re-runs
    # db_upsert_pool_manager (an idempotent ON CONFLICT reassert). The manager
    # row must be unchanged (same id, same address/state_view).
    pm_before = _pool_manager_row(stub_bot.config.database.path)
    assert pm_before is not None

    result = _run(runner, activate, ["ethereum_uniswap_v4"], stub_bot)
    assert result.exit_code == 0, result.output
    assert "Activated Uniswap V4 on Ethereum" in result.output

    pm_after = _pool_manager_row(stub_bot.config.database.path)
    assert pm_after is not None
    assert pm_after == pm_before
