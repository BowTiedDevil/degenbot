"""Tests for the single-chain Bot facade (ADR-006 D5).

One Bot per chain. ``Bot(**session, provider=)`` enforces the connected RPC's
``chain_id`` matches the session chain the cascade resolved (fail-fast on a
misconfigured endpoint). All per-method ``chain_id=`` params are gone — the
Bot has exactly one chain. Two ``Bot`` instances in one process are fully
independent (separate ``Bot``s, separate providers, no shared registry).
"""

import pathlib

import pytest

from degenbot.bot import Bot
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import AlloyProvider, OfflineProvider
from tests.helpers.bot_factory import ProviderKind, bot_session, fake_provider


class TestSingleChainBot:
    """Bot.__init__ takes one chain; rejects a provider whose chain_id mismatches."""

    def test_an_unresolvable_session_chain_is_refused(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path
    ) -> None:
        """No layer names a chain: refuse rather than guess one."""
        monkeypatch.setattr(
            "degenbot.bot._bot.resolve_chain_id",
            lambda _chain_id=None: (_ for _ in ()).throw(ValueError("no layer named a chain")),
        )

        with pytest.raises(ValueError, match="no layer named a chain"):
            Bot(database=str(tmp_path / "t.db"), provider=fake_provider(ProviderKind.OFFLINE, 1))

    def test_rejects_chain_id_mismatch(self, tmp_path: pathlib.Path) -> None:
        # the session says chain 1, the provider reports 137 — fail fast.
        session = bot_session(tmp_path, chain_id=1)
        with pytest.raises(DegenbotValueError, match="chain"):
            Bot(**session, provider=fake_provider(ProviderKind.OFFLINE, 137))

    def test_accepts_matching_provider(self, tmp_path: pathlib.Path) -> None:
        session = bot_session(tmp_path, chain_id=1)
        bot = Bot(**session, provider=fake_provider(ProviderKind.OFFLINE, 1))
        assert bot.chain_id == 1
        assert isinstance(bot.provider, (AlloyProvider, OfflineProvider))


class TestTwoBotsAreIndependent:
    """D5 positive proof: two Bots in one process share nothing."""

    def test_separate_providers_and_py_bots(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(
            **bot_session(tmp_path / "a", chain_id=1),
            provider=fake_provider(ProviderKind.OFFLINE, 1),
        )
        bot2 = Bot(
            **bot_session(tmp_path / "b", chain_id=137),
            provider=fake_provider(ProviderKind.OFFLINE, 137),
        )
        assert bot1.chain_id == 1
        assert bot2.chain_id == 137
        assert bot1.provider is not bot2.provider
        assert bot1.pools is not bot2.pools
        assert bot1.tokens is not bot2.tokens
        assert bot1._py_bot is not bot2._py_bot
