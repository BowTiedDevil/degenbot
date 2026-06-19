"""Tests for AsyncBot's async context-manager / aclose() resource lifecycle.

``async with AsyncBot.from_provider(...) as bot:`` must release the Python
handles AsyncBot owns on exit: provider connection (``provider.close()``),
the DB scoped session (``db.remove()``), and the tracker/registry caches
(``release_python_state()``). Mirrors the sync ``Bot`` ctx-manager tests.

Note: the async provider's ``close()`` is synchronous, so ``aclose()`` is
itself synchronous; only ``__aexit__`` is ``async`` (it calls ``aclose``).
"""

import pathlib
from unittest.mock import AsyncMock, MagicMock

import pytest

from degenbot.async_bot import AsyncBot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.uniswap.trackers import UniswapV2PoolTracker


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
        default_chain_id=chain_id,
    )


def _fake_async_provider(chain_id: int = 1) -> MagicMock:
    p = MagicMock()
    p.get_chain_id = AsyncMock(return_value=chain_id)
    return p


class _RecordingDb:
    def __init__(self) -> None:
        self.remove_calls = 0

    def remove(self) -> None:
        self.remove_calls += 1


class TestAsyncBotContextManager:
    async def test_context_manager_releases_all_handles_on_exit(
        self, tmp_path: pathlib.Path
    ) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_async_provider(1)

        bot = await AsyncBot.from_provider(config, provider=provider)
        # swap in a recording db + seed a tracker
        recording_db = _RecordingDb()
        bot.db = recording_db  # type: ignore[assignment]
        tracker = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        )
        tracker._tracked_pools["0xdead"] = object()
        tracker._untracked_pools.add("0xbeef")

        async with bot:
            pass

        provider.close.assert_called_once()
        assert recording_db.remove_calls == 1
        assert tracker._tracked_pools == {}
        assert tracker._untracked_pools == set()
        assert bot._closed is True

    async def test_aclose_is_idempotent(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_async_provider(1)
        bot = await AsyncBot.from_provider(config, provider=provider)
        bot.db = _RecordingDb()  # type: ignore[assignment]

        bot.aclose()
        bot.aclose()  # second call must be a no-op

        provider.close.assert_called_once()
        assert bot._closed is True

    async def test_aexit_does_not_suppress_exceptions(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_async_provider(1)
        boom_msg = "boom"
        with pytest.raises(RuntimeError, match=boom_msg):
            async with await AsyncBot.from_provider(config, provider=provider):
                raise RuntimeError(boom_msg)

        provider.close.assert_called_once()

    async def test_aclose_composes_release_python_state(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_async_provider(1)
        bot = await AsyncBot.from_provider(config, provider=provider)
        bot.db = _RecordingDb()  # type: ignore[assignment]
        tracker = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        )
        tracker._tracked_pools["0xdead"] = object()

        bot.release_python_state()
        assert tracker._tracked_pools == {}

        bot.aclose()

        provider.close.assert_called_once()
        assert bot._closed is True
