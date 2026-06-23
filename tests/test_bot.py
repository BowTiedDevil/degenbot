"""Tests for the Bot class (single-chain facade, ADR-006 D5)."""

import pathlib
from unittest.mock import MagicMock, patch

import pytest

from degenbot.async_bot import AsyncBot
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.degenbot_rs import PyBot
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.provider import ProviderAdapter
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.uniswap.trackers import UniswapV2PoolTracker


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    """Create a DegenbotConfig pointing at a temporary database."""
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
        default_chain_id=chain_id,
    )


def _fake_provider(chain_id: int = 1) -> ProviderAdapter:
    provider = MagicMock(spec=ProviderAdapter)
    provider.chain_id = chain_id
    return provider


class TestBotInit:
    """Bot constructor tests (single-chain)."""

    def test_bot_creates_database_session_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot.db, DatabaseSessionManager)

    def test_bot_creates_pool_registry(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot.pools, PoolRegistry)

    def test_bot_creates_token_registry(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot.tokens, TokenRegistry)

    def test_bot_creates_managed_pool_registry(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot.managed_pools, ManagedPoolRegistry)

    def test_bot_stores_config(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert bot.config is config

    def test_bot_trackers_empty_at_start(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert bot._trackers == {}

    def test_bot_exposes_chain_id_and_provider(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path, chain_id=1)
        provider = _fake_provider(1)
        bot = Bot(config, provider=provider)
        assert bot.chain_id == 1
        assert bot.provider is provider


class TestBotPyBotHandle:
    """Bot constructs and owns a PyO3 PyBot handle (ADR-005)."""

    def test_bot_constructs_py_bot(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot._py_bot, PyBot)

    def test_each_bot_has_independent_py_bot(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "bot2"), provider=_fake_provider(1))
        assert isinstance(bot1._py_bot, PyBot)
        assert isinstance(bot2._py_bot, PyBot)
        assert bot1._py_bot is not bot2._py_bot

    def test_py_bot_carries_configured_chain_id(self, tmp_path: pathlib.Path) -> None:
        """The Bot facade wires its ``default_chain_id`` into the Rust ``PyBot``
        (ADR-006 D4: ``Bot::new(chain_id)``). No more ``chain_id = 0`` placeholder.
        """
        config = _make_test_config(tmp_path, chain_id=1)
        bot = Bot(config, provider=_fake_provider(1))
        assert bot._py_bot.chain_id == 1

    def test_py_bot_chain_id_follows_config(self, tmp_path: pathlib.Path) -> None:
        """A non-default ``default_chain_id`` propagates to the ``PyBot`` (the
        wiring is real, not a hard-coded constant).
        """
        config = _make_test_config(tmp_path, chain_id=10)
        bot = Bot(config, provider=_fake_provider(10))
        assert bot._py_bot.chain_id == 10


class TestBotFromConfigFile:
    """Bot.from_config_file() tests."""

    def test_from_config_file_creates_bot(self, tmp_path: pathlib.Path) -> None:
        with patch("degenbot.bot._init_config") as mock_init:
            mock_init.return_value = _make_test_config(tmp_path)
            with patch("degenbot.bot.get_provider_from_config") as mock_factory:
                mock_factory.return_value = _fake_provider(1)
                bot = Bot.from_config_file()
                assert isinstance(bot, Bot)
                mock_init.assert_called_once()
                mock_factory.assert_called_once()


class TestBotAddTracker:
    """Bot.add_tracker() tests (single-chain — no chain_id arg)."""

    def test_add_tracker_stores_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        manager = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        )
        assert isinstance(manager, UniswapV2PoolTracker)
        key = get_checksum_address("0x5C69bEe701ef814E44274f655e7632cB715C14B6")
        assert key in bot._trackers
        assert bot._trackers[key] is manager

    def test_add_tracker_rejects_duplicate(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        bot.add_tracker(UniswapV2PoolTracker, factory_address=factory)

        with pytest.raises(TrackerAlreadyInitialized):
            bot.add_tracker(UniswapV2PoolTracker, factory_address=factory)


class TestMultipleBots:
    """Multiple Bot instances must have independent state."""

    def test_independent_registries(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "bot2"), provider=_fake_provider(1))

        assert bot1.pools is not bot2.pools
        assert bot1.tokens is not bot2.tokens
        assert bot1.managed_pools is not bot2.managed_pools
        assert bot1.provider is not bot2.provider
        assert bot1.db is not bot2.db

    def test_independent_trackers(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "bot2"), provider=_fake_provider(1))

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        manager1 = bot1.add_tracker(UniswapV2PoolTracker, factory_address=factory)
        # Second bot can add a manager for the same factory without error
        manager2 = bot2.add_tracker(UniswapV2PoolTracker, factory_address=factory)
        assert manager1 is not manager2


class TestAsyncBotInit:
    """AsyncBot constructor tests."""

    def test_async_bot_creates_database_session_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config, provider=_fake_provider(1))
        assert isinstance(bot.db, DatabaseSessionManager)

    def test_async_bot_stores_config(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config, provider=_fake_provider(1))
        assert bot.config is config
