"""Tests for the Bot class (Phase 1)."""

import pathlib
from unittest.mock import MagicMock, patch

import pytest

from degenbot.async_bot import AsyncBot
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.connection.async_connection_manager import AsyncConnectionManager
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.pool import ManagerAlreadyInitialized
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.uniswap.trackers import UniswapV2PoolTracker


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    """Create a DegenbotConfig pointing at a temporary database."""
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


class TestBotInit:
    """Bot constructor tests."""

    def test_bot_creates_connection_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert isinstance(bot.connections, ConnectionManager)

    def test_bot_creates_database_session_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert isinstance(bot.db, DatabaseSessionManager)

    def test_bot_creates_pool_registry(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert isinstance(bot.pools, PoolRegistry)

    def test_bot_creates_token_registry(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert isinstance(bot.tokens, TokenRegistry)

    def test_bot_creates_managed_pool_registry(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert isinstance(bot.managed_pools, ManagedPoolRegistry)

    def test_bot_stores_config(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert bot.config is config

    def test_bot_trackers_empty_at_start(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)
        assert bot._trackers == {}


class TestBotFromConfigFile:
    """Bot.from_config_file() tests."""

    def test_from_config_file_creates_bot(self, tmp_path: pathlib.Path) -> None:
        with patch("degenbot.bot._init_config") as mock_init:
            mock_init.return_value = _make_test_config(tmp_path)
            bot = Bot.from_config_file()
            assert isinstance(bot, Bot)
            mock_init.assert_called_once()


class TestBotAddTracker:
    """Bot.add_tracker() tests."""

    def test_add_tracker_stores_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        manager = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
            chain_id=1,
        )
        assert isinstance(manager, UniswapV2PoolTracker)
        assert ("0x5C69bEe701ef814E44274f655e7632cB715C14B6".lower(),) not in bot._trackers
        # Manager is stored keyed by (chain_id, factory_address)
        key = (1, get_checksum_address("0x5C69bEe701ef814E44274f655e7632cB715C14B6"))
        assert key in bot._trackers
        assert bot._trackers[key] is manager

    def test_add_tracker_rejects_duplicate(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        bot.add_tracker(UniswapV2PoolTracker, factory_address=factory, chain_id=1)

        with pytest.raises(ManagerAlreadyInitialized):
            bot.add_tracker(UniswapV2PoolTracker, factory_address=factory, chain_id=1)


class TestMultipleBots:
    """Multiple Bot instances must have independent state."""

    def test_independent_registries(self, tmp_path: pathlib.Path) -> None:
        config1 = _make_test_config(tmp_path / "bot1")
        config2 = _make_test_config(tmp_path / "bot2")

        bot1 = Bot(config1)
        bot2 = Bot(config2)

        assert bot1.pools is not bot2.pools
        assert bot1.tokens is not bot2.tokens
        assert bot1.managed_pools is not bot2.managed_pools
        assert bot1.connections is not bot2.connections
        assert bot1.db is not bot2.db

    def test_independent_trackers(self, tmp_path: pathlib.Path) -> None:
        config1 = _make_test_config(tmp_path / "bot1")
        config2 = _make_test_config(tmp_path / "bot2")

        bot1 = Bot(config1)
        bot2 = Bot(config2)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        bot1.connections.register_provider(provider)
        bot1.connections.set_default_chain(1)
        bot2.connections.register_provider(provider)
        bot2.connections.set_default_chain(1)

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        manager1 = bot1.add_tracker(UniswapV2PoolTracker, factory_address=factory, chain_id=1)
        # Second bot can add a manager for the same factory without error
        manager2 = bot2.add_tracker(UniswapV2PoolTracker, factory_address=factory, chain_id=1)
        assert manager1 is not manager2


class TestAsyncBotInit:
    """AsyncBot constructor tests."""

    def test_async_bot_creates_async_connection_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)
        assert isinstance(bot.connections, AsyncConnectionManager)

    def test_async_bot_creates_database_session_manager(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)
        assert isinstance(bot.db, DatabaseSessionManager)

    def test_async_bot_stores_config(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = AsyncBot(config)
        assert bot.config is config

    def test_async_bot_from_config_file(self, tmp_path: pathlib.Path) -> None:
        with patch("degenbot.async_bot._init_config") as mock_init:
            mock_init.return_value = _make_test_config(tmp_path)
            bot = AsyncBot.from_config_file()
            assert isinstance(bot, AsyncBot)
