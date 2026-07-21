"""Tests for the single-chain Bot facade (ADR-006 D5).

One Bot per chain. ``Bot(config, provider=)`` enforces the connected RPC's
``chain_id`` matches ``config.default_chain_id`` (fail-fast on a
misconfigured endpoint). All per-method ``chain_id=`` params are gone — the
Bot has exactly one chain. Two ``Bot`` instances in one process are fully
independent (separate ``PyBot``s, separate providers, no shared registry).
"""

import pathlib

import pytest

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import AlloyProvider, OfflineProvider


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={},
        default_chain_id=chain_id,
    )


def _fake_provider(chain_id: int = 1) -> OfflineProvider:
    """A real offline provider (recorded JSON, no RPC) with the given chain_id.

    `Bot.__init__` reads `provider.chain_id` (the recorded chain_id) to enforce
    config/chain alignment; no RPC is issued at construction, so an offline
    provider over an in-memory Rust transport suffices — no MagicMock double
    (see O3).
    """
    return OfflineProvider(
        chain_id=chain_id,
        blocks={"1": {"timestamp": 1, "calls": {}, "code": {}}},
    )


class TestSingleChainBot:
    """Bot.__init__ takes one chain; rejects a provider whose chain_id mismatches."""

    def test_requires_chain_id_in_config(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        config.default_chain_id = None
        with pytest.raises(DegenbotValueError):
            Bot(config, provider=_fake_provider(1))

    def test_rejects_chain_id_mismatch(self, tmp_path: pathlib.Path) -> None:
        # config says chain 1, provider reports chain 137 — fail fast.
        config = _make_test_config(tmp_path, chain_id=1)
        with pytest.raises(DegenbotValueError, match="chain"):
            Bot(config, provider=_fake_provider(137))

    def test_accepts_matching_provider(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path, chain_id=1)
        bot = Bot(config, provider=_fake_provider(1))
        assert bot.chain_id == 1
        assert isinstance(bot.provider, (AlloyProvider, OfflineProvider))


class TestTwoBotsAreIndependent:
    """D5 positive proof: two Bots in one process share nothing."""

    def test_separate_providers_and_py_bots(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "a", chain_id=1), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "b", chain_id=137), provider=_fake_provider(137))
        assert bot1.chain_id == 1
        assert bot2.chain_id == 137
        assert bot1.provider is not bot2.provider
        assert bot1.pools is not bot2.pools
        assert bot1.tokens is not bot2.tokens
        assert bot1._py_bot is not bot2._py_bot
