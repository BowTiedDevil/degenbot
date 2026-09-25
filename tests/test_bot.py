"""Tests for the Bot class (single-chain facade, ADR-006 D5).

Design note — public-seam testing. Every double enters Bot through a
constructor parameter or a public method: no mock/``patch`` of module
privates, no assignment to ``bot._*`` attributes.

Seams used here (all additive to ``Bot.__init__`` / ``from_config_file``;
omitted kwargs keep today's production bindings, matching the runner's
``SimSubmitPipeline`` candidate_builder/simulator/renderer/submitter and
``_submit_batch_records`` submitter/relay_providers precedents):

- ``from_config_file(config=..., provider=...)`` — a real config + a real
  ``OfflineProvider`` are handed in, replacing a patch of the private
  ``_init_config`` / ``get_provider_from_config`` module factories.
- ``Bot(py_bot=..., io=..., erc20_builder=...)`` — engine / I/O / token-builder
  doubles for the build-path tests. An injected object skips the
  construction-time wiring that belongs to the default instance (engine DB
  snapshot load + ``ConstructionIo`` attach; ``BotIo`` ``ConstructionIo``
  attach), so the doubles need no spec scaffolding.
- The V2/V4 parity tests drive the PUBLIC build entries (``build_pool`` /
  ``build_managed_pool``), which reach the same delegated-build parity guards
  the direct private calls did.

Rejected alternatives:

- Driving ``_build_delegated`` directly for the V2 parity test was feasible
  via the constructor seam, but the public ``build_pool`` entry reaches the
  same guard once the io double satisfies type resolution (no DB row, an
  unregistered factory, and a probe answer of V2) — the extra choreography is
  the same public surface the facade itself exercises, so the test covers the
  dispatch into the delegated path at no assertion cost.
- Stubbing ``_make_v4_tick_data_fetcher`` proved unnecessary: the real
  factory is lazy (no I/O at creation) and its closure is never invoked on
  the parity-mismatch path, so the real one runs untouched.
- ``MagicMock(spec=AlloyProvider)`` provider doubles: nothing on the paths
  under test touches the provider beyond ``chain_id``, so the real
  ``OfflineProvider`` (recorded JSON, no RPC at construction) is both
  stricter and dependency-free.
"""

import pathlib
from types import SimpleNamespace

import pytest

from degenbot._ffi import Bot as _Engine
from degenbot.bot import Bot
from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.provider import OfflineProvider
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.types.pool_type import PoolProbe
from degenbot.uniswap.trackers import UniswapV2PoolTracker
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

# Not in the deployments registry, so the type resolver falls back to probing.
_UNREGISTERED_FACTORY = "0x" + "f" * 40


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
    provider over an in-memory Rust transport suffices — no mock double.
    """
    return OfflineProvider(
        chain_id=chain_id,
        blocks={"1": {"timestamp": 1, "calls": {}, "code": {}}},
    )


class TestBotInit:
    """Bot constructor tests (single-chain)."""

    def test_bot_exposes_database_path_without_sqlalchemy_session(
        self, tmp_path: pathlib.Path
    ) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        assert bot.database_path == config.database.path
        assert not hasattr(bot, "db")

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
    """Bot constructs and owns a PyO3 _Engine handle (ADR-005)."""

    def test_bot_constructs_py_bot(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        assert isinstance(bot._py_bot, _Engine)

    def test_each_bot_has_independent_py_bot(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "bot2"), provider=_fake_provider(1))
        assert isinstance(bot1._py_bot, _Engine)
        assert isinstance(bot2._py_bot, _Engine)
        assert bot1._py_bot is not bot2._py_bot

    def test_py_bot_carries_configured_chain_id(self, tmp_path: pathlib.Path) -> None:
        """The Bot facade wires its ``default_chain_id`` into the Rust ``_Engine``
        (ADR-006 D4: ``Bot::new(chain_id)``). No more ``chain_id = 0`` placeholder.
        """
        config = _make_test_config(tmp_path, chain_id=1)
        bot = Bot(config, provider=_fake_provider(1))
        assert bot._py_bot.chain_id == 1

    def test_py_bot_chain_id_follows_config(self, tmp_path: pathlib.Path) -> None:
        """A non-default ``default_chain_id`` propagates to the ``_Engine`` (the
        wiring is real, not a hard-coded constant).
        """
        config = _make_test_config(tmp_path, chain_id=10)
        bot = Bot(config, provider=_fake_provider(10))
        assert bot._py_bot.chain_id == 10


class TestBotFromConfigFile:
    """Bot.from_config_file() tests."""

    def test_from_config_file_creates_bot(self, tmp_path: pathlib.Path) -> None:
        # Real config + real provider through the from_config_file DI seams —
        # the default-argument path (file discovery + provider factory) is
        # unchanged production behavior.
        bot = Bot.from_config_file(
            config=_make_test_config(tmp_path),
            provider=_fake_provider(1),
        )
        assert isinstance(bot, Bot)


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
        assert bot1.database_path != bot2.database_path

    def test_independent_trackers(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(_make_test_config(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(_make_test_config(tmp_path / "bot2"), provider=_fake_provider(1))

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        manager1 = bot1.add_tracker(UniswapV2PoolTracker, factory_address=factory)
        # Second bot can add a manager for the same factory without error
        manager2 = bot2.add_tracker(UniswapV2PoolTracker, factory_address=factory)
        assert manager1 is not manager2


class TestBuildDelegatedIdentityReturnSurface:
    """build_pool's Rust-delegated V2/V3 path returns a typed identity
    ``(pool_id, token0, token1, address, family)`` from the builder and asserts
    parity against the registered handle (a divergence is a genuine
    core/driver seam bug and must fail loudly, not silently re-derive)."""

    def test_build_delegated_v2_parity_mismatch_raises(self, tmp_path: pathlib.Path) -> None:
        """A V2 builder identity that diverges from the registered handle's
        tokens raises — the return-surface parity guard, reached through the
        public ``build_pool`` entry: the io double routes type resolution to
        the V2 delegated path (no DB row, unregistered factory, probe says V2).
        """
        config = DegenbotConfig(
            database=DatabaseSettings(path=str(tmp_path / "t.db")),
            rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
            default_chain_id=1,
        )
        io = SimpleNamespace(
            get_block_number=lambda: 100,
            fetch_factory_address=lambda address: _UNREGISTERED_FACTORY,
            # BotIo.probe_pool_type returns the 1-based PoolProbe code; V2 = 1.
            probe_pool_type=lambda address: int(PoolProbe.V2),
        )

        # Engine double: build_v2_pool returns the tuple return surface (core
        # identity), get_pool returns a handle whose tokens DIFFER -> the
        # parity guard must raise.
        handle = SimpleNamespace(token0_address="0x" + "a" * 40, token1_address="0x" + "b" * 40)
        py_bot = SimpleNamespace(
            build_v2_pool=lambda address, block=None: (
                7,
                "0x" + "C" * 40,
                "0x" + "D" * 40,
                "0x" + "E" * 40,
                "uniswap-v2",
            ),
            get_pool=lambda pid: handle,
        )
        bot = Bot(config, provider=_fake_provider(1), py_bot=py_bot, io=io)

        with pytest.raises(DegenbotValueError):
            bot.build_pool("0x" + "e" * 40)


class TestBuildManagedPoolIdentityReturnSurface:
    """The V4 build path resolves identity core-side via ``resolve_v4_identity``
    (DB two-step else overrides) then echoes it back through ``build_v4_pool``;
    the build verifies the two agree."""

    def test_build_v4_parity_mismatch_raises(self, tmp_path: pathlib.Path) -> None:
        """A builder identity that diverges from the resolver identity raises
        — the return-surface parity guard."""
        config = DegenbotConfig(
            database=DatabaseSettings(path=str(tmp_path / "t.db")),
            rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
            default_chain_id=1,
        )
        io = SimpleNamespace(get_block_number=lambda: 100)

        # Token-builder double: the real Erc20Builder's per-token build needs
        # DB/RPC reads the io double does not carry.
        token = SimpleNamespace(address="0x" + "cc" * 20)
        erc20_builder = SimpleNamespace(
            build=lambda *a, **k: token,
            build_many=lambda *a, **k: [token, token],
        )

        pm = "0x" + "aa" * 20
        pool_id_hex = "0x" + "11" * 32
        tokens = ["0x" + "cc" * 20, "0x" + "dd" * 20]  # cc<dd -> cc is currency0

        # Engine double: the resolver returns identity A; build_v4_pool echoes
        # a DIFFERENT currency0 -> parity guard must raise. Return surface:
        # (pool_id, coverage, currency0, currency1, pool_manager, fee,
        # tick_spacing, hook_flags, pool_id_hex, protocol_fee, lp_fee).
        py_bot = SimpleNamespace(
            resolve_v4_identity=lambda **k: (
                "0x" + "cc" * 20,
                "0x" + "dd" * 20,
                5000,
                1,
                0,  # hook flag mask 0 (hook address defaults to ZERO)
                "0x" + "00" * 20,
                "0x" + "bb" * 20,  # state-view caller override
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
                5000,  # protocol_fee
                0,  # lp_fee
            ),
        )
        bot = Bot(
            config,
            provider=_fake_provider(1),
            py_bot=py_bot,
            io=io,
            erc20_builder=erc20_builder,
        )

        with pytest.raises(DegenbotValueError):
            bot.build_managed_pool(
                pm,
                BuildManagedPoolRequest(
                    pool_id=pool_id_hex,
                    state_block=100,
                    state_view_address="0x" + "bb" * 20,
                    tokens=tokens,
                    fee=5000,
                    tick_spacing=1,
                ),
            )


class TestBuildManagedPoolResolveErrorMapping:
    """A core-identity-resolution failure (MissingIdentity -> mapped to
    PyValueError at the seam) surfaces as DegenbotValueError."""

    def test_resolve_missing_identity_raises_degenbot(self, tmp_path: pathlib.Path) -> None:
        """When resolve_v4_identity raises ValueError (no DB row, no overrides),
        the V4 build path re-raises DegenbotValueError."""
        config = DegenbotConfig(
            database=DatabaseSettings(path=str(tmp_path / "t.db")),
            rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
            default_chain_id=1,
        )
        io = SimpleNamespace(get_block_number=lambda: 100)
        py_bot = SimpleNamespace(
            resolve_v4_identity=lambda **k: (_ for _ in ()).throw(
                ValueError("V4 identity incomplete: pool not in the database")
            ),
        )
        bot = Bot(config, provider=_fake_provider(1), py_bot=py_bot, io=io)

        pm = "0x" + "aa" * 20
        pool_id_hex = "0x" + "11" * 32

        with pytest.raises(DegenbotValueError):
            bot.build_managed_pool(
                pm,
                BuildManagedPoolRequest(
                    pool_id=pool_id_hex,
                    state_block=100,
                    # No state_view / fee / tick_spacing / tokens -> core rejects.
                ),
            )
