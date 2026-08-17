"""Tests for the Bot class (single-chain facade, ADR-006 D5)."""

import pathlib
from unittest.mock import patch

import pytest

from degenbot.bot import Bot, RustBot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.provider import OfflineProvider
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.uniswap.trackers import UniswapV2PoolTracker
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    """Create a DegenbotConfig pointing at a temporary database."""
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
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
    """Bot constructs and owns a PyO3 RustBot handle (ADR-005)."""

    def test_bot_constructs_py_bot(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot._py_bot, RustBot)

    def test_each_bot_has_independent_py_bot(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "bot2"), provider=_fake_provider(1))
        assert isinstance(bot1._py_bot, RustBot)
        assert isinstance(bot2._py_bot, RustBot)
        assert bot1._py_bot is not bot2._py_bot

    def test_py_bot_carries_configured_chain_id(self, tmp_path: pathlib.Path) -> None:
        """The Bot facade wires its ``default_chain_id`` into the Rust ``RustBot``
        (ADR-006 D4: ``Bot::new(chain_id)``). No more ``chain_id = 0`` placeholder.
        """
        config = _make_test_config(tmp_path, chain_id=1)
        bot = Bot(config, provider=_fake_provider(1))
        assert bot._py_bot.chain_id == 1

    def test_py_bot_chain_id_follows_config(self, tmp_path: pathlib.Path) -> None:
        """A non-default ``default_chain_id`` propagates to the ``RustBot`` (the
        wiring is real, not a hard-coded constant).
        """
        config = _make_test_config(tmp_path, chain_id=10)
        bot = Bot(config, provider=_fake_provider(10))
        assert bot._py_bot.chain_id == 10


class TestBotFromConfigFile:
    """Bot.from_config_file() tests."""

    def test_from_config_file_creates_bot(self, tmp_path: pathlib.Path) -> None:
        with patch("degenbot.bot._bot._init_config") as mock_init:
            mock_init.return_value = _make_test_config(tmp_path)
            with patch("degenbot.bot._bot.get_provider_from_config") as mock_factory:
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


class TestBuildDelegatedIdentityReturnSurface:
    """TF7RZB-S1: build_pool's Rust-delegated V2/V3 path returns a typed
    identity `(pool_id, token0, token1, address, family)` from the builder and
    asserts parity against the registered handle (a divergence is a genuine
    core/driver seam bug and must fail loudly, not silently re-derive)."""

    def test_build_delegated_v2_parity_mismatch_raises(self, tmp_path) -> None:
        """A V2 builder identity that diverges from the registered handle's
        tokens raises — the return-surface parity guard."""
        from types import SimpleNamespace
        from unittest.mock import MagicMock

        import pytest

        from degenbot.bot import Bot
        from degenbot.builders.request import BuildPoolRequest
        from degenbot.config import DatabaseSettings, DegenbotConfig
        from degenbot.exceptions.base import DegenbotValueError
        from degenbot.provider import AlloyProvider
        from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
        from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

        config = DegenbotConfig(
            database=DatabaseSettings(path=str(tmp_path / "t.db")),
            rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
            default_chain_id=1,
        )
        provider = MagicMock(spec=AlloyProvider)
        provider.chain_id = 1
        bot = Bot(config, provider=provider)

        # Stub the Rust-owned surface: build_v2_pool returns the tuple return
        # surface (core identity), get_pool returns a handle whose tokens DIFFER
        # -> the parity guard must raise.
        handle = SimpleNamespace(token0_address="0x" + "a" * 40, token1_address="0x" + "b" * 40)
        fake_py = SimpleNamespace(
            build_v2_pool=lambda address, block=None: (
                7,
                "0x" + "C" * 40,
                "0x" + "D" * 40,
                "0x" + "E" * 40,
                "uniswap-v2",
            ),
            get_pool=lambda pid: handle,
        )
        bot._py_bot = fake_py  # type: ignore[assignment]
        bot._io = SimpleNamespace(get_block_number=lambda: 100)  # type: ignore[assignment]

        request = BuildPoolRequest()
        with pytest.raises(DegenbotValueError):
            bot._build_delegated(UniswapV2Pool, "0x" + "e" * 40, 1, request)


class TestBuildManagedPoolIdentityReturnSurface:
    """TF7RZB-S2/S3: the V4 build path resolves identity core-side via
    `resolve_v4_identity` (DB two-step else overrides) then echoes it back
    through `build_v4_pool`; _build_v4_managed verifies the two agree."""

    def test_build_v4_parity_mismatch_raises(self, tmp_path) -> None:
        """A builder identity that diverges from the resolver identity raises
        — the return-surface parity guard."""
        from types import SimpleNamespace
        from unittest.mock import MagicMock

        import pytest

        from degenbot.bot import Bot
        from degenbot.config import DatabaseSettings, DegenbotConfig
        from degenbot.exceptions.base import DegenbotValueError
        from degenbot.provider import AlloyProvider
        from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

        config = DegenbotConfig(
            database=DatabaseSettings(path=str(tmp_path / "t.db")),
            rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
            default_chain_id=1,
        )
        provider = MagicMock(spec=AlloyProvider)
        provider.chain_id = 1
        bot = Bot(config, provider=provider)

        pm = "0x" + "aa" * 20
        pool_id_hex = "0x" + "11" * 32
        tokens = ["0x" + "cc" * 20, "0x" + "dd" * 20]  # cc<dd -> cc is currency0

        # Stub the io seam: only the companion scalars read + get_block_number
        # remain (identity resolution moved core-side).
        bot._io = SimpleNamespace(  # type: ignore[assignment]
            get_block_number=lambda: 100,
            fetch_v4_slot0_liquidity=lambda *a, **k: (1 << 96, 0, 0, 5000, 0),
            # CDJEPJ-2: batched metadata seam — return None per token so the
            # metadata path falls back to the stubbed `build` below.
            fetch_erc20_metadata_batch=lambda *a, **k: [None, None],
        )
        bot._erc20_builder.build = lambda *a, **k: SimpleNamespace(  # type: ignore[assignment]
            address="0x" + "cc" * 20
        )
        bot._make_v4_tick_data_fetcher = lambda *a, **k: None  # type: ignore[assignment]

        # Stub the Rust surface: the resolver returns identity A; build_v4_pool
        # echoes a DIFFERENT currency0 -> parity guard must raise.
        bot._py_bot = SimpleNamespace(  # type: ignore[assignment]
            resolve_v4_identity=lambda **k: (
                "0x" + "cc" * 20,
                "0x" + "dd" * 20,
                5000,
                1,
                0,
                "0x" + "bb" * 20,
                None,
            ),
            build_v4_pool=lambda **k: (
                7,
                "sparse",
                "0x" + "ee" * 20,  # currency0 mismatch vs resolver
                "0x" + "dd" * 20,
                pm,
                5000,
                1,
                0,
                pool_id_hex,
                5000,  # protocol_fee (CDJEPJ-1 return surface)
                0,  # lp_fee
            ),
        )

        with pytest.raises(DegenbotValueError):
            bot.build_managed_pool(
                pm,
                pool_id_hex,
                state_block=100,
                state_view_address="0x" + "bb" * 20,
                tokens=tokens,
                fee=5000,
                tick_spacing=1,
            )


class TestBuildManagedPoolResolveErrorMapping:
    """TF7RZB-S3: a core-identity-resolution failure (MissingIdentity → mapped
    to PyValueError at the seam) surfaces as DegenbotValueError."""

    def test_resolve_missing_identity_raises_degenbot(self, tmp_path) -> None:
        """When resolve_v4_identity raises ValueError (no DB row, no overrides),
        _build_v4_managed re-raises DegenbotValueError."""
        from types import SimpleNamespace
        from unittest.mock import MagicMock

        import pytest

        from degenbot.bot import Bot
        from degenbot.config import DatabaseSettings, DegenbotConfig
        from degenbot.exceptions.base import DegenbotValueError
        from degenbot.provider import AlloyProvider
        from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

        config = DegenbotConfig(
            database=DatabaseSettings(path=str(tmp_path / "t.db")),
            rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
            default_chain_id=1,
        )
        provider = MagicMock(spec=AlloyProvider)
        provider.chain_id = 1
        bot = Bot(config, provider=provider)

        pm = "0x" + "aa" * 20
        pool_id_hex = "0x" + "11" * 32

        bot._io = SimpleNamespace(  # type: ignore[assignment]
            get_block_number=lambda: 100,
        )
        bot._py_bot = SimpleNamespace(  # type: ignore[assignment]
            resolve_v4_identity=lambda **k: (_ for _ in ()).throw(
                ValueError("V4 identity incomplete: pool not in the database")
            ),
        )

        with pytest.raises(DegenbotValueError):
            bot.build_managed_pool(
                pm,
                pool_id_hex,
                state_block=100,
                # No state_view / fee / tick_spacing / tokens -> core rejects.
            )
