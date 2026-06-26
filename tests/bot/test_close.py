"""Tests for Bot's context-manager / close() resource lifecycle.

``with Bot(...) as bot:`` must release the Python handles Bot owns on exit:
the provider connection (``provider.close()``), the DB scoped session
(``db.remove()``), and the tracker/registry caches (``release_python_state()``).
``close()`` is idempotent and composes with an explicit ``release_python_state()``
call. ``__exit__`` never suppresses exceptions.
"""

import pathlib
from unittest.mock import MagicMock, patch

import pytest

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.provider import ProviderAdapter
from degenbot.uniswap.trackers import UniswapV2PoolTracker


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
        default_chain_id=chain_id,
    )


def _fake_provider(chain_id: int = 1) -> ProviderAdapter:
    provider = MagicMock(spec=ProviderAdapter)
    provider.chain_id = chain_id
    return provider


class _RecordingDb:
    """Stand-in for DatabaseSessionManager that records remove()/dispose() calls."""

    def __init__(self) -> None:
        self.remove_calls = 0
        self.dispose_calls = 0

    def remove(self) -> None:
        self.remove_calls += 1

    def dispose(self) -> None:
        self.dispose_calls += 1


class TestBotContextManager:
    def test_context_manager_releases_all_handles_on_exit(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_provider(1)

        with Bot(config, provider=provider) as bot:
            # swap in a recording db so we can observe remove()/dispose();
            # dispose the real engine built in __init__ first so it doesn't leak
            bot.db.dispose()  # type: ignore[attr-defined]
            recording_db = _RecordingDb()
            bot.db = recording_db  # type: ignore[assignment]
            # seed a tracker with caches
            tracker = bot.add_tracker(
                UniswapV2PoolTracker,
                factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
            )
            tracker._tracked_pools["0xdead"] = object()
            tracker._untracked_pools.add("0xbeef")
            assert len(bot.pools) == 0  # nothing registered yet

        # On exit: provider closed, db removed + disposed, python state released
        provider.close.assert_called_once()
        assert recording_db.remove_calls == 1
        assert recording_db.dispose_calls == 1
        assert tracker._tracked_pools == {}
        assert tracker._untracked_pools == set()
        assert bot._closed is True

    def test_close_is_idempotent(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_provider(1)
        bot = Bot(config, provider=provider)
        bot.db.dispose()  # release the real engine before swapping in the stand-in  # type: ignore[attr-defined]
        bot.db = _RecordingDb()  # type: ignore[assignment]

        bot.close()
        bot.close()  # second call must be a no-op

        provider.close.assert_called_once()
        assert bot._closed is True  # type: ignore[attr-defined]

    def test_exit_does_not_suppress_exceptions(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_provider(1)
        boom_msg = "boom"
        with pytest.raises(RuntimeError, match=boom_msg), Bot(config, provider=provider):
            raise RuntimeError(boom_msg)

        provider.close.assert_called_once()

    def test_close_disposes_real_database_engine(self, tmp_path: pathlib.Path) -> None:
        """close() must dispose the SQLAlchemy Engine bound by ``__init__``.

        ``db.remove()`` only returns the thread-local Session; the Engine's
        connection pool keeps the ``sqlite3.Connection`` alive, which surfaces
        as ``ResourceWarning: unclosed database`` under GC. close() must dispose.
        """
        config = _make_test_config(tmp_path)
        provider = _fake_provider(1)
        bot = Bot(config, provider=provider)
        engine = bot.db._engine
        assert engine is not None

        with patch.object(engine, "dispose", wraps=engine.dispose) as spy:
            bot.close()
            spy.assert_called_once_with()
        assert bot._closed is True  # type: ignore[attr-defined]

    def test_close_tolerates_db_without_dispose(self, tmp_path: pathlib.Path) -> None:
        """close() must not raise when a custom db stand-in lacks dispose()."""

        class _RemoveOnlyDb:
            def __init__(self) -> None:
                self.remove_calls = 0

            def remove(self) -> None:
                self.remove_calls += 1

        config = _make_test_config(tmp_path)
        provider = _fake_provider(1)
        bot = Bot(config, provider=provider)
        bot.db.dispose()  # release the real engine before swapping in the stand-in  # type: ignore[attr-defined]
        bot.db = _RemoveOnlyDb()  # type: ignore[assignment]

        bot.close()  # must not raise

        provider.close.assert_called_once()
        assert bot._closed is True  # type: ignore[attr-defined]

    def test_close_composes_release_python_state(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        provider = _fake_provider(1)
        bot = Bot(config, provider=provider)
        bot.db.dispose()  # release the real engine before swapping in the stand-in  # type: ignore[attr-defined]
        bot.db = _RecordingDb()  # type: ignore[assignment]
        tracker = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        )
        tracker._tracked_pools["0xdead"] = object()

        # explicit mid-lifecycle release first, then full close
        bot.release_python_state()
        assert tracker._tracked_pools == {}

        bot.close()  # must not raise despite state already released

        provider.close.assert_called_once()
        assert bot._closed is True  # type: ignore[attr-defined]
