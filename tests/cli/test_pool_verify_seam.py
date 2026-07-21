"""Seam test for the `pool verify` CLI command + the verify-v3/v4 PyO3 stubs.

Locks the:
- `degenbot pool verify --family v3|v4` CLI hand-off to `degenbot._ffi.verify_v3/v4_liquidity_map`
  (the arg-threading + the GREEN/RED echo), via monkeypatching the Rust entry
  points (no live RPC node is touched).
- the named-divergence dict shape (`variant` + `tick`/`word` + `expected`/`actual`)
  the Rust seam returns — the bisect-able triage contract the pre-commit gate
  + this ad-hoc command share.

A network-gated test (skips without ``RPC_URL``) exercises a real V3 pool is
out of scope here (it needs a populated DB + a live node); the offline shape is
the contract this file guards.
"""

from __future__ import annotations

import types
from typing import TYPE_CHECKING

import pytest
from click.testing import CliRunner

from degenbot.cli import pool as pool_mod
from degenbot.cli.pool import pool_verify

if TYPE_CHECKING:
    import pathlib


CHAIN = 1


class _StubBot:
    """`pool_verify` touches only `bot.config.database.path` (the path is
    passed through to the Rust verify, which is monkeypatched in these tests —
    no real DB or RPC is opened)."""

    def __init__(self, db_path: pathlib.Path) -> None:
        self.config = types.SimpleNamespace(
            database=types.SimpleNamespace(path=str(db_path)),
            default_chain_id=CHAIN,
        )


@pytest.fixture
def stub_bot(tmp_path: pathlib.Path) -> _StubBot:
    return _StubBot(tmp_path / "verify.db")


def test_pool_verify_v3_green_echoes_green(
    stub_bot: _StubBot, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`pool verify --family v3` with zero divergences echoes GREEN."""
    called: dict[str, object] = {}

    def _fake_verify_v3(
        *,
        database_path: str,
        rpc_url: str,
        chain_id: int,
        pool_address: str,
        block_number: int,
    ) -> list[dict[str, object]]:
        called.update(
            database_path=database_path,
            rpc_url=rpc_url,
            chain_id=chain_id,
            pool_address=pool_address,
            block_number=block_number,
        )
        return []  # GREEN

    monkeypatch.setattr(pool_mod, "verify_v3_liquidity_map", _fake_verify_v3)

    result = CliRunner().invoke(
        pool_verify,  # type: ignore[arg-type]
        [
            "--rpc-url",
            "http://stub",
            "--chain",
            "1",
            "--block",
            "18000000",
            "--pool",
            "0x" + "ab" * 20,
            "--family",
            "v3",
        ],
        obj=stub_bot,
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output
    assert "GREEN" in result.output
    assert called["chain_id"] == 1
    assert called["block_number"] == 18000000
    assert called["pool_address"] == "0x" + "ab" * 20


def test_pool_verify_v3_red_lists_divergences(
    stub_bot: _StubBot, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`pool verify --family v3` with divergences echoes the named list."""

    def _fake_verify_v3(**_kwargs: object) -> list[dict[str, object]]:
        return [
            {
                "variant": "TickGross",
                "tick": 12345,
                "expected": "500",
                "actual": "0",
            },
            {
                "variant": "BitmapWord",
                "word": 0,
                "expected": "1",
                "actual": "0",
            },
        ]

    monkeypatch.setattr(pool_mod, "verify_v3_liquidity_map", _fake_verify_v3)

    result = CliRunner().invoke(
        pool_verify,  # type: ignore[arg-type]
        [
            "--rpc-url",
            "http://stub",
            "--chain",
            "1",
            "--block",
            "18000000",
            "--pool",
            "0x" + "cd" * 20,
            "--family",
            "v3",
        ],
        obj=stub_bot,
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output
    assert "RED" in result.output
    assert "2 divergence" in result.output
    # The named divergence surfaces (bisect-able triage).
    assert "12345" in result.output
    assert "TickGross" in result.output
    assert "BitmapWord" in result.output


def test_pool_verify_v4_requires_pool_manager(
    stub_bot: _StubBot, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`pool verify --family v4` without --pool-manager raises ValueError."""
    monkeypatch.setattr(pool_mod, "verify_v4_liquidity_map", lambda **_k: [])

    with pytest.raises(ValueError, match="pool-manager"):
        CliRunner().invoke(
            pool_verify,  # type: ignore[arg-type]
            [
                "--rpc-url",
                "http://stub",
                "--chain",
                "1",
                "--block",
                "18000000",
                "--pool",
                "0x" + "ef" * 32,
                "--family",
                "v4",
            ],
            obj=stub_bot,
            catch_exceptions=False,
        )


def test_pool_verify_v4_threads_pool_manager(
    stub_bot: _StubBot, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`pool verify --family v4 --pool-manager <addr>` threads the manager."""
    called: dict[str, object] = {}

    def _fake_verify_v4(
        *,
        database_path: str,
        rpc_url: str,
        chain_id: int,
        pool_hash: str,
        pool_manager_address: str,
        block_number: int,
    ) -> list[dict[str, object]]:
        called.update(
            pool_hash=pool_hash,
            pool_manager_address=pool_manager_address,
            block_number=block_number,
        )
        return []

    monkeypatch.setattr(pool_mod, "verify_v4_liquidity_map", _fake_verify_v4)

    manager = "0x" + "12" * 20
    pool_hash = "0x" + "ab" * 32
    result = CliRunner().invoke(
        pool_verify,  # type: ignore[arg-type]
        [
            "--rpc-url",
            "http://stub",
            "--chain",
            "1",
            "--block",
            "18000000",
            "--pool",
            pool_hash,
            "--family",
            "v4",
            "--pool-manager",
            manager,
        ],
        obj=stub_bot,
        catch_exceptions=False,
    )
    assert result.exit_code == 0, result.output
    assert "GREEN" in result.output
    assert called["pool_manager_address"] == manager
    assert called["pool_hash"] == pool_hash
