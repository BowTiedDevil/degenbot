"""Tests for Bot.release_python_state() — the Python-state teardown handshake.

After the Rust engine takes ownership of canonical pool/token state, the Bot's
Python-side caches (tracker `_tracked_pools`/`_untracked_pools` + snapshots,
the pool + token registries) are scaffolding that should be dropped so the hot
loop only holds the engine + the async web3 handle. This encodes the 15-line
hand-rolled trim block from `main()` behind a single Bot method, so the example
stops reverse-engineering Bot internals.
"""

import pathlib
from threading import Lock
from unittest.mock import MagicMock

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
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


class _FakeTrackerWithSnapshot:
    """Minimal tracker exercising the unload_snapshot() conditional.

    Mirrors the shape AbstractPoolTracker concrete subclasses present:
    `_tracked_pools`, `_untracked_pools`, and an optional `unload_snapshot`.
    """

    def __init__(self) -> None:
        self._lock = Lock()
        self._tracked_pools: dict[str, object] = {"0xpool": object()}
        self._untracked_pools: set[str] = {"0xother"}
        self.snapshot_unloaded = False

    def unload_snapshot(self) -> None:
        self.snapshot_unloaded = True


class TestReleasePythonState:
    """Bot.release_python_state() — drop Python caches after Rust owns state."""

    def test_release_clears_tracker_caches(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        tracker = bot.add_tracker(UniswapV2PoolTracker, factory_address=factory)
        # seed non-empty caches
        assert tracker._tracked_pools == {}
        tracker._tracked_pools["0xdead"] = object()
        tracker._untracked_pools.add("0xbeef")

        bot.release_python_state()

        assert tracker._tracked_pools == {}
        assert tracker._untracked_pools == set()

    def test_release_calls_unload_snapshot_where_present(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        fake_key = get_checksum_address("0x0000000000000000000000000000000000000001")
        fake = _FakeTrackerWithSnapshot()
        bot._trackers[fake_key] = fake

        bot.release_python_state()

        assert fake.snapshot_unloaded is True
        assert fake._tracked_pools == {}
        assert fake._untracked_pools == set()

    def test_release_resets_pool_and_token_registries(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        # seed the registries with sentinel storage via their _storage()
        bot.pools.add(
            pool_address="0x0000000000000000000000000000000000000002",
            pool=object(),  # type: ignore[arg-type]
            chain_id=1,
        )
        # TokenRegistry has a different add signature; just assert reset clears
        assert len(bot.pools) >= 1

        bot.release_python_state()

        assert len(bot.pools) == 0
        assert len(bot.tokens) == 0

    def test_release_is_idempotent(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        )

        bot.release_python_state()
        # second call must not raise
        bot.release_python_state()
