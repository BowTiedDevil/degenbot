"""Integration tests for EngineRegistry.register_path pool-driven API.

register_path takes a sequence of (pool, zero_for_one) pairs, resolves each
pool's engine key from the registry's key maps, + dispatches (key, zfo) pairs
to the engine's register_and_solve_path. NXM2BF: the Python PathInfo relay is
retired — register_path no longer builds/stores a Python PathInfo; the candidate
resolves the encoder's composers::PathInfo from the returned path_id via
PyArbitrageEngine.path_info_for_core. These tests assert the key-dispatch
contract (the Python-side responsibility that remains). They use a Fake engine
(AGENTS.md prefers Fakes over mocks) + seed the key maps directly, so no live
Rust engine / BotState is needed.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot
from degenbot.arbitrage.engine_registry import ArbitrageEngine, EngineRegistry
from degenbot.utils.bytes import to_0x_hex
from tests.types.test_concrete_pool_construction import (
    _make_uniswap_v2_pool,
    _make_uniswap_v3_pool,
    _make_uniswap_v4_pool,
)


class FakeArbitrageEngine:
    """Records register_and_solve_path calls; returns monotonic path ids.

    Its identity lookups answer from a declared identity table, standing in for
    the SHARED BotState's registration tables (the core's identity owner). The
    registry itself holds no map: a test declares the identities it needs here,
    and the registry resolves each hop by asking.
    """

    def __init__(self) -> None:
        self.calls: list[list[tuple[int, bool]]] = []
        self.identity_asks: list[tuple[str, str]] = []
        self._identities: dict[tuple[str, str], int] = {}
        self._next_id = 1

    def register_identity(self, family_tag: str, address: str, pool_id: int) -> None:
        self._identities[family_tag, address.lower()] = pool_id

    def pool_id_for_pool(self, family_tag: str, address: str) -> int | None:
        self.identity_asks.append((family_tag, address.lower()))
        return self._identities.get((family_tag, address.lower()))

    def pool_id_for_v4_pool(self, pool_manager: str, pool_id_hex: str) -> int | None:
        self.identity_asks.append(("v4", f"{pool_manager}/{pool_id_hex}".lower()))
        return self._identities.get(("v4", f"{pool_manager}/{pool_id_hex}".lower()))

    def register_and_solve_path(self, hops: list[tuple[int, bool]]) -> int:
        self.calls.append(list(hops))
        path_id = self._next_id
        self._next_id += 1
        return path_id


def test_register_path_dispatches_keys_and_directions() -> None:
    """register_path maps each pool to its engine key + direction. NXM2BF: no
    Python PathInfo is built/stored — the returned path_id is the handle the
    candidate resolves composers::PathInfo from."""
    fake = FakeArbitrageEngine()
    registry = EngineRegistry(bot=None, engine=fake)

    v2 = _make_uniswap_v2_pool()
    v3 = _make_uniswap_v3_pool()
    fake.register_identity("v2", v2.address, 100)
    fake.register_identity("v3", v3.address, 200)

    pools_and_zfos = [(v2, True), (v3, False)]

    path_id = registry.register_path(pools_and_zfos)

    assert fake.calls == [[(100, True), (200, False)]]
    assert isinstance(path_id, int)
    # NXM2BF: the Python PathInfo relay is retired — no `paths` attribute.
    assert not hasattr(registry, "paths")


def test_register_path_v4_keyed_by_the_manager_and_pool_id_pair() -> None:
    """A V4 pool resolves its engine key by the (PoolManager, pool_id) pair.

    One manager hosts many pools, so the pair — not the pool id alone — is the
    identity the core answers with.
    """
    fake = FakeArbitrageEngine()
    registry = EngineRegistry(bot=None, engine=fake)

    v4 = _make_uniswap_v4_pool()
    pool_id_hex = to_0x_hex(v4.pool_id)
    fake.register_identity("v4", f"{v4.address}/{pool_id_hex}", 999)

    path_id = registry.register_path([(v4, True)])

    assert fake.calls == [[(999, True)]]
    assert fake.identity_asks == [("v4", f"{v4.address}/{pool_id_hex}".lower())]
    assert isinstance(path_id, int)


def test_register_path_refuses_an_unregistered_identity() -> None:
    """An unregistered identity is a typed ValueError, not a stale cached key."""
    fake = FakeArbitrageEngine()
    registry = EngineRegistry(bot=None, engine=fake)
    v2 = _make_uniswap_v2_pool()

    with pytest.raises(ValueError, match="Pool not registered"):
        registry.register_path([(v2, True)])


def test_pool_identity_is_derived_from_the_real_shared_bot_state() -> None:
    """Against the REAL pyclass: the id comes from the shared BotState.

    No Python map is seeded. The pool is registered in the core through
    ``py_bot.register_v2_pool`` and every identity read resolves it from there —
    the core is the only identity owner left.
    """
    bot = _FakeBot()
    registry = EngineRegistry(bot=bot)
    address = "0x00000000000000000000000000000000000000aa"
    key = bot._py_bot.register_v2_pool(
        address=address,
        token0="0x00000000000000000000000000000000000000b0",
        token1="0x00000000000000000000000000000000000000b1",
        reserve0=1_000_000,
        reserve1=1_000_000,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory="0x00000000000000000000000000000000000000b2",
    )

    assert registry.engine.pool_id_for_pool("v2", address) == key
    assert registry.knows_pool(address) is True
    # A family that is not registered at this address has no id of its own.
    assert registry.engine.pool_id_for_pool("v3", address) is None
    # And the registry holds no per-family identity map or claim table.
    assert not any(
        name.endswith(("_keys", "_inflight", "_claims")) for name in vars(registry)
    )


class _FakeBot:
    """Minimal Bot double exposing ``_py_bot`` for the production construction path.

    ``EngineRegistry(bot=...)`` dereferences ``bot._py_bot`` to build the real
    engine against the bot's shared BotState (ADR-006 D1). A bare ``Bot()`` is
    the offline stand-in — ``test_shared_state_topology`` proves the shared-core
    topology with this exact pair, no RPC/anvil needed.
    """

    def __init__(self) -> None:
        self._py_bot = Bot()


def test_bot_none_without_engine_raises() -> None:
    """Production path requires a bot when no engine is supplied."""
    with pytest.raises(ValueError, match=r"engine.*bot|bot.*engine"):
        EngineRegistry(bot=None)


def test_bot_supplies_py_bot_to_real_engine() -> None:
    """EngineRegistry(bot=bot) constructs the real engine against bot._py_bot.

    ADR-006 D1: the engine shares the bot's BotState. The live registration path
    is ``Bot.register_v*`` (ADR-006 D3 deleted the unreachable pyo3 engine
    ``register_*`` surface); registering a V2 pool against the bot's py_bot must
    be visible to the engine's ``v2_pool_count``.
    """
    bot = _FakeBot()
    registry = EngineRegistry(bot=bot)

    assert isinstance(registry.engine, ArbitrageEngine)
    bot._py_bot.register_v2_pool(
        address="0x0000000000000000000000000000000000000001",
        token0="0x0000000000000000000000000000000000000002",
        token1="0x0000000000000000000000000000000000000003",
        reserve0=1_000_000,
        reserve1=1_000_000,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory="0x0000000000000000000000000000000000000004",
    )
    assert registry.engine.v2_pool_count() == 1


def test_register_path_dispatches_aerodrome_key() -> None:
    """An Aerodrome pool routes its shared-core key through register_path.

    register_aerodrome_pool caches the pool_id (the engine's derive_hop_type
    classifies it as HopType::SolidlyStable at register_path time, so no engine-
    side tag passes through the (key, zfo) tuple). NXM2BF: the Solidly HopInfo
    build moved to the Rust projection (path_info_for_core returns
    UnsupportedHopType for Solidly — matching the pre-flatten encode gap), so
    the Python relay no longer classifies the hop family. Key-dispatch only.
    """
    from fractions import Fraction

    from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
    from tests.types.test_concrete_pool_construction import _make_usdc, _make_weth

    fake = FakeArbitrageEngine()
    registry = EngineRegistry(bot=None, engine=fake)

    usdc = _make_usdc()
    weth = _make_weth()
    aero = make_aerodrome_v2_pool(
        address="0xAE7FFAe65eC9eA34741B0FbA1E5dBc4F0eC5Ea6F",
        token0=usdc,
        token1=weth,
        factory="0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        fee=Fraction(3, 1000),
        stable=True,
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=1000 * 10**18,
    )
    # The pool's shared-core id, read off its core handle.
    aero_key = registry.register_aerodrome_pool(aero)
    assert aero_key == aero._py_pool.pool_id
    # The core resolves it back by the aerodrome family identity.
    fake.register_identity("aerodrome-v2", aero.address, aero_key)

    path_id = registry.register_path([(aero, False)])

    # The fake engine received the shared-core key + direction (no Solidly tag
    # — the engine derives the family from the BotState identity).
    assert fake.calls == [[(aero_key, False)]]
    assert isinstance(path_id, int)
