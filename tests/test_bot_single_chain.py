"""Tests for the single-chain Bot facade (ADR-006 D5).

One Bot per chain. ``Bot(config, provider=)`` enforces the connected RPC's
``chain_id`` matches ``config.default_chain_id`` (fail-fast on a
misconfigured endpoint). All per-method ``chain_id=`` params are gone — the
Bot has exactly one chain. Two ``Bot`` instances in one process are fully
independent (separate ``PyBot``s, separate providers, no shared registry).
"""

import pathlib
from unittest.mock import AsyncMock, MagicMock

import pytest

from degenbot.async_bot import AsyncBot
from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import ProviderAdapter


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={},
        default_chain_id=chain_id,
    )


def _fake_provider(chain_id: int = 1) -> ProviderAdapter:
    """A minimal fake provider exposing only ``chain_id`` (no network).

    The Bot's chain-enforcement only reads ``provider.chain_id`` at
    construction, so a MagicMock suffices to exercise the match/mismatch
    branches without spinning up an OfflineProvider.
    """
    provider = MagicMock(spec=ProviderAdapter)
    provider.chain_id = chain_id
    return provider


def _fake_async_provider(chain_id: int = 1) -> MagicMock:
    """A minimal fake async provider exposing ``get_chain_id`` (no network).

    Async providers can't read ``chain_id`` synchronously (their sync property
    raises NotImplementedError); the seam awaits ``get_chain_id()``.
    """

    provider = MagicMock()
    provider.get_chain_id = AsyncMock(return_value=chain_id)
    return provider


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
        assert isinstance(bot.provider, ProviderAdapter)


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


class TestAsyncBotSingleChain:
    """AsyncBot.from_provider enforces chain via an awaited get_chain_id()."""

    @pytest.mark.asyncio
    async def test_from_provider_accepts_matching_chain(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path, chain_id=1)
        bot = await AsyncBot.from_provider(config, provider=_fake_async_provider(1))
        assert bot.chain_id == 1

    @pytest.mark.asyncio
    async def test_from_provider_rejects_chain_mismatch(self, tmp_path: pathlib.Path) -> None:
        # config says chain 1, the async provider reports chain 137 → fail fast.
        config = _make_test_config(tmp_path, chain_id=1)
        with pytest.raises(DegenbotValueError, match="chain"):
            await AsyncBot.from_provider(config, provider=_fake_async_provider(137))

    @pytest.mark.asyncio
    async def test_from_provider_requires_chain_id_in_config(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        config.default_chain_id = None
        with pytest.raises(DegenbotValueError):
            await AsyncBot.from_provider(config, provider=_fake_async_provider(1))
