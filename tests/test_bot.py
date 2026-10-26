"""Tests for the Bot class (single-chain facade, ADR-006 D5).

Design note — public-seam testing. Every double enters Bot through a
constructor parameter or a public method: no mock/``patch`` of module
privates, no assignment to ``bot._*`` attributes.

Seams used here (all additive to ``Bot.__init__``; omitted kwargs resolve from
the installed typed config, matching the runner's injectable actors and the
``_submit_batch_records`` submitter/relay_providers precedents):

- ``Bot(chain_id=..., database=..., provider=...)`` — the explicit override
  keywords plus a real ``OfflineProvider``, replacing a patch of the private
  ``resolve_*`` / ``get_provider_from_config`` module factories.
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
from collections.abc import Callable
from dataclasses import dataclass

import pytest

from degenbot._ffi import Bot as _Engine
from degenbot._ffi import PoolConstructionRequest
from degenbot.bot import Bot
from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.exceptions.base import DegenbotValueError
from degenbot.provider import OfflineProvider
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.types.pool_type import PoolProbe
from tests.fakes.session import session_registry_methods
from tests.fakes.tokens import FakeToken

# Not in the deployments registry, so the type resolver falls back to probing.
_UNREGISTERED_FACTORY = "0x" + "f" * 40


def _test_session(tmp_path: pathlib.Path, chain_id: int = 1) -> dict[str, object]:
    """The explicit keyword overrides a session under test runs with."""
    return {"chain_id": chain_id, "database": str(tmp_path / "test.db")}


def _fake_provider(chain_id: int = 1) -> OfflineProvider:
    """A real offline provider (recorded JSON, no RPC) with the given chain_id.

    `Bot.__init__` reads `provider.chain_id` (the recorded chain_id) to enforce
    session/chain alignment; no RPC is issued at construction, so an offline
    provider over an in-memory Rust transport suffices — no mock double.
    """
    return OfflineProvider(
        chain_id=chain_id,
        blocks={"1": {"timestamp": 1, "calls": {}, "code": {}}},
    )


@dataclass
class _ScriptedIo:
    """``BotIo`` double for construction reads; a test scripts only the reads
    its build path drives (an unscripted read is one the path never calls)."""

    get_block_number: Callable[[], int] = lambda: 100
    fetch_factory_address: Callable[[str], str] | None = None
    probe_pool_type: Callable[[str], int] | None = None


@dataclass(frozen=True)
class _PoolHandle:
    """The registered pool-handle slice the build parity guard reads."""

    token0_address: str
    token1_address: str


@dataclass
class _ConstructionOutcome:
    """The one construction answer the driver reads: handle + registered identity."""

    pool_id: int
    pool: _PoolHandle
    family: str
    variant: str
    token0: str
    token1: str
    tokens: list[str]


@dataclass
class _ScriptedEngine:
    """Engine (``py_bot``) double for build-path tests.

    A test scripts only the build-path methods its facade entry drives; the
    session-registry identity surface comes from a real ``Bot`` handle.
    """

    construct_pool: Callable[..., object] | None = None
    get_pool: Callable[..., object] | None = None

    def __post_init__(self) -> None:
        for name, method in session_registry_methods().items():
            setattr(self, name, method)


@dataclass
class _ScriptedErc20Builder:
    """Token-builder double: the real builder's per-token build needs DB/RPC reads."""

    build: Callable[..., object]
    build_many: Callable[..., object]


class TestBotInit:
    """Bot constructor tests (single-chain)."""

    def test_bot_exposes_database_path_without_legacy_session(self, tmp_path: pathlib.Path) -> None:
        session = _test_session(tmp_path)
        bot = Bot(**session, provider=_fake_provider(1))

        assert bot.database_path == tmp_path / "test.db"
        assert not hasattr(bot, "db")

    def test_bot_creates_pool_registry(self, tmp_path: pathlib.Path) -> None:
        session = _test_session(tmp_path)
        bot = Bot(**session, provider=_fake_provider(1))
        assert isinstance(bot.pools, PoolRegistry)

    def test_bot_creates_token_registry(self, tmp_path: pathlib.Path) -> None:
        session = _test_session(tmp_path)
        bot = Bot(**session, provider=_fake_provider(1))
        assert isinstance(bot.tokens, TokenRegistry)

    def test_bot_creates_managed_pool_registry(self, tmp_path: pathlib.Path) -> None:
        session = _test_session(tmp_path)
        bot = Bot(**session, provider=_fake_provider(1))
        assert isinstance(bot.managed_pools, ManagedPoolRegistry)

    def test_bot_carries_no_config_object(self, tmp_path: pathlib.Path) -> None:
        """The resolved values are the session's own; nothing holds a config.

        Python is a driver over the installed typed config, so no config value
        object is left on the session to drift from the core's.
        """
        session = _test_session(tmp_path)
        bot = Bot(**session, provider=_fake_provider(1))
        assert not hasattr(bot, "config")

    def test_bot_exposes_chain_id_and_provider(self, tmp_path: pathlib.Path) -> None:
        session = _test_session(tmp_path, chain_id=1)
        provider = _fake_provider(1)
        bot = Bot(**session, provider=provider)
        assert bot.chain_id == 1
        assert bot.provider is provider


class TestBotPyBotHandle:
    """Bot constructs and owns a PyO3 _Engine handle (ADR-005)."""

    def test_bot_constructs_py_bot(self, tmp_path: pathlib.Path) -> None:
        session = _test_session(tmp_path)
        bot = Bot(**session, provider=_fake_provider(1))
        assert isinstance(bot._py_bot, _Engine)

    def test_each_bot_has_independent_py_bot(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(**_test_session(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(**_test_session(tmp_path / "bot2"), provider=_fake_provider(1))
        assert isinstance(bot1._py_bot, _Engine)
        assert isinstance(bot2._py_bot, _Engine)
        assert bot1._py_bot is not bot2._py_bot

    def test_py_bot_carries_the_resolved_chain_id(self, tmp_path: pathlib.Path) -> None:
        """The Bot facade wires its resolved session chain into the Rust
        ``_Engine`` (ADR-006 D4: ``Bot::new(chain_id)``), with no
        ``chain_id = 0`` placeholder.
        """
        session = _test_session(tmp_path, chain_id=1)
        bot = Bot(**session, provider=_fake_provider(1))
        assert bot._py_bot.chain_id == 1

    def test_py_bot_chain_id_follows_the_override(self, tmp_path: pathlib.Path) -> None:
        """A non-default session chain propagates to the ``_Engine`` (the wiring
        is real, not a hard-coded constant).
        """
        session = _test_session(tmp_path, chain_id=10)
        bot = Bot(**session, provider=_fake_provider(10))
        assert bot._py_bot.chain_id == 10


class TestMultipleBots:
    """Multiple Bot instances must have independent state."""

    def test_independent_registries(self, tmp_path: pathlib.Path) -> None:
        bot1 = Bot(**_test_session(tmp_path / "bot1"), provider=_fake_provider(1))
        bot2 = Bot(**_test_session(tmp_path / "bot2"), provider=_fake_provider(1))

        assert bot1.pools is not bot2.pools
        assert bot1.tokens is not bot2.tokens
        assert bot1.managed_pools is not bot2.managed_pools
        assert bot1.provider is not bot2.provider
        assert bot1.database_path != bot2.database_path


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
        session = _test_session(tmp_path)
        io = _ScriptedIo(
            fetch_factory_address=lambda address: _UNREGISTERED_FACTORY,
            # BotIo.probe_pool_type returns the 1-based PoolProbe code; V2 = 1.
            probe_pool_type=lambda address: int(PoolProbe.V2),
        )

        # Engine double: the construction answer carries an identity (core echo)
        # whose tokens DIFFER from the handle's -> the parity guard must raise.
        handle = _PoolHandle(token0_address="0x" + "a" * 40, token1_address="0x" + "b" * 40)
        py_bot = _ScriptedEngine(
            construct_pool=lambda request: _ConstructionOutcome(
                pool_id=7,
                pool=handle,
                family=request.family,
                variant="uniswap-v2",
                token0="0x" + "C" * 40,
                token1="0x" + "D" * 40,
                tokens=["0x" + "C" * 40, "0x" + "D" * 40],
            ),
            get_pool=lambda pid: handle,
        )
        bot = Bot(**session, provider=_fake_provider(1), py_bot=py_bot, io=io)

        with pytest.raises(DegenbotValueError):
            bot.build_pool("0x" + "e" * 40)


class TestBuildManagedPoolRequestShape:
    """``build_managed_pool`` issues ONE construction request: the
    ``managed_v4`` family carrying the pool id and the caller-supplied
    identity overrides. The engine double captures the request at the seam
    and halts the build."""

    @staticmethod
    def _capturing_engine(captured: dict[str, object]) -> _ScriptedEngine:
        def construct_pool(request: object) -> object:
            captured.update(
                address=request.address,
                family=request.family,
                pool_id=request.pool_id,
                tokens=request.tokens,
                fee=request.fee,
                tick_spacing=request.tick_spacing,
                hook_address=request.hook_address,
                state_view_address=request.state_view_address,
                block=request.block,
                db=request.db,
            )
            raise _SeamCaptured

        return _ScriptedEngine(construct_pool=construct_pool)

    def _bot(self, tmp_path: pathlib.Path, captured: dict[str, object]) -> Bot:
        token = FakeToken(address="0x" + "cc" * 20)
        erc20_builder = _ScriptedErc20Builder(
            build=lambda *a, **k: token,
            build_many=lambda *a, **k: [token, token],
        )
        return Bot(
            **_test_session(tmp_path),
            provider=_fake_provider(1),
            py_bot=self._capturing_engine(captured),
            io=_ScriptedIo(),
            erc20_builder=erc20_builder,
        )

    def test_request_carries_the_managed_v4_vocabulary(self, tmp_path: pathlib.Path) -> None:
        """The request is family-tagged ``managed_v4`` with the pool id (the
        constructor validates + normalizes it) and the identity overrides
        threaded verbatim."""
        captured: dict[str, object] = {}
        with pytest.raises(_SeamCaptured):
            self._bot(tmp_path, captured).build_managed_pool(
                "0x" + "aa" * 20,
                BuildManagedPoolRequest(
                    pool_id="0x" + "11" * 32,
                    state_block=100,
                    state_view_address="0x" + "bb" * 20,
                    tokens=["0x" + "cc" * 20, "0x" + "dd" * 20],
                    fee=5000,
                    tick_spacing=1,
                    hook_address=None,
                ),
            )
        assert captured["family"] == "managed_v4"
        # The driver checksums the manager before the request.
        assert captured["address"].lower() == "0x" + "aa" * 20
        assert captured["pool_id"] == "0x" + "11" * 32
        assert captured["tokens"] == ["0x" + "cc" * 20, "0x" + "dd" * 20]
        assert captured["fee"] == 5000
        assert captured["tick_spacing"] == 1
        assert captured["state_view_address"] == "0x" + "bb" * 20

    def test_malformed_pool_id_refuses_at_the_request(self) -> None:
        """The pool id is validated where the family is chosen (a required
        hint is an argument, ADR-085 D3) — at the constructor, before any
        verb or registry ever sees it."""
        with pytest.raises(ValueError, match="Pool ID hex string"):
            PoolConstructionRequest.managed_v4(
                "0x" + "aa" * 20,
                "0x" + "11" * 8,  # 8 bytes, not 32
            )


class TestBuildManagedPoolResolveErrorMapping:
    """A core-identity-resolution failure (MissingIdentity -> mapped to
    PyValueError at the seam) surfaces as DegenbotValueError."""

    def test_resolve_missing_identity_raises_degenbot(self, tmp_path: pathlib.Path) -> None:
        """When the ONE construction verb raises ValueError (no DB row, no
        overrides), the V4 build path re-raises DegenbotValueError."""
        session = _test_session(tmp_path)
        io = _ScriptedIo()
        py_bot = _ScriptedEngine(
            construct_pool=lambda request: (_ for _ in ()).throw(
                ValueError("V4 identity incomplete: pool not in the database")
            ),
        )
        bot = Bot(**session, provider=_fake_provider(1), py_bot=py_bot, io=io)

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


class _SeamCaptured(Exception):
    """Halts a build right after the captured FFI call records its kwargs."""


class TestBuildPoolDbArmEligibility:
    """``build_pool`` forwards the DB-arm eligibility flag to the ONE core
    construction entry.

    The DB liquidity map is a CURRENT-state cache (no per-block history), so it
    can only seed a HEAD construction. A pinned ``state_block`` behind the
    current head is a HISTORICAL construction and must skip the DB arm
    (``db=False`` — the core's Chain-arm Sparse path assembles the tick map
    from the chain at the pinned block); otherwise the core's chain-truth
    intake gate refuses the head-stamped rows (any position minted or burned
    since the pinned block), and before that gate existed they silently
    produced a mixed-state pool (head tick map under historical scalars).

    The engine double records the ``construct_pool`` kwargs and halts the
    build at the seam — the decision under test is made before any
    post-construction choreography.
    """

    _POOL = "0x" + "e" * 40

    @staticmethod
    def _capturing_engine(captured: dict[str, object]) -> _ScriptedEngine:
        def construct_pool(request: object) -> object:
            captured.update(
                address=request.address,
                family=request.family,
                variant=request.variant,
                block=request.block,
                db=request.db,
                factories=request.factories,
                registries=request.registries,
                vault=request.vault,
                invariant_version=request.invariant_version,
            )
            raise _SeamCaptured

        return _ScriptedEngine(construct_pool=construct_pool)

    def _bot(self, tmp_path: pathlib.Path, captured: dict[str, object]) -> Bot:
        io = _ScriptedIo(
            fetch_factory_address=lambda address: _UNREGISTERED_FACTORY,
            probe_pool_type=lambda address: int(PoolProbe.V3),
        )
        return Bot(
            **_test_session(tmp_path),
            provider=_fake_provider(1),
            py_bot=self._capturing_engine(captured),
            io=io,
        )

    def test_head_default_keeps_db_arm(self, tmp_path: pathlib.Path) -> None:
        captured: dict[str, object] = {}
        with pytest.raises(_SeamCaptured):
            self._bot(tmp_path, captured).build_pool(self._POOL)
        assert captured["db"] is True
        assert captured["block"] == 100  # the io double's head

    def test_pinned_head_state_block_keeps_db_arm(self, tmp_path: pathlib.Path) -> None:
        captured: dict[str, object] = {}
        with pytest.raises(_SeamCaptured):
            self._bot(tmp_path, captured).build_pool(self._POOL, state_block=100)
        assert captured["db"] is True
        assert captured["block"] == 100

    def test_historical_state_block_skips_db_arm(self, tmp_path: pathlib.Path) -> None:
        captured: dict[str, object] = {}
        with pytest.raises(_SeamCaptured):
            self._bot(tmp_path, captured).build_pool(self._POOL, state_block=50)
        assert captured["db"] is False
        assert captured["block"] == 50


class TestBuildManagedPoolDbArmEligibility:
    """The V4 twin of ``TestBuildPoolDbArmEligibility``: a pinned historical
    ``state_block`` forwards ``db=False`` to the ONE construction verb
    (Chain-arm Sparse tick map at the pinned block); a head build keeps
    ``db=True``."""

    @staticmethod
    def _bot(tmp_path: pathlib.Path, captured: dict[str, object]) -> Bot:
        token = FakeToken(address="0x" + "cc" * 20)
        erc20_builder = _ScriptedErc20Builder(
            build=lambda *a, **k: token,
            build_many=lambda *a, **k: [token, token],
        )

        def construct_pool(request: object) -> object:
            captured.update(db=request.db, block=request.block)
            raise _SeamCaptured

        py_bot = _ScriptedEngine(construct_pool=construct_pool)
        return Bot(
            **_test_session(tmp_path),
            provider=_fake_provider(1),
            py_bot=py_bot,
            io=_ScriptedIo(),
            erc20_builder=erc20_builder,
        )

    def _request(self, state_block: int | None) -> BuildManagedPoolRequest:
        return BuildManagedPoolRequest(
            pool_id="0x" + "11" * 32,
            state_block=state_block,
            state_view_address="0x" + "bb" * 20,
            tokens=["0x" + "cc" * 20, "0x" + "dd" * 20],
            fee=5000,
            tick_spacing=1,
        )

    def test_head_default_keeps_db_arm(self, tmp_path: pathlib.Path) -> None:
        captured: dict[str, object] = {}
        with pytest.raises(_SeamCaptured):
            self._bot(tmp_path, captured).build_managed_pool(
                "0x" + "aa" * 20,
                self._request(None),
            )
        assert captured["db"] is True
        assert captured["block"] == 100  # the io double's head

    def test_historical_state_block_skips_db_arm(self, tmp_path: pathlib.Path) -> None:
        captured: dict[str, object] = {}
        with pytest.raises(_SeamCaptured):
            self._bot(tmp_path, captured).build_managed_pool(
                "0x" + "aa" * 20,
                self._request(50),
            )
        assert captured["db"] is False
        assert captured["block"] == 50
