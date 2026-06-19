"""Integration tests for EngineRegistry.register_path pool-driven API.

register_path takes a sequence of (pool, zero_for_one) pairs, builds HopInfo
via build_hops_from_pools, resolves each pool's engine key from the registry's
key maps, dispatches (key, zfo) pairs to the engine, and stores the built
PathInfo. These tests use a Fake engine (AGENTS.md prefers Fakes over mocks)
and seed the key maps directly, so no live Rust engine / BotState is needed.
"""

from __future__ import annotations

import pytest

import examples.eth_backrun_v2_v3_v4_rust as runner
from degenbot.degenbot_rs import PyBot, UniswapArbEngine
from examples.eth_backrun_helpers import build_hops_from_pools
from tests.types.test_concrete_pool_construction import (
    _make_uniswap_v2_pool,
    _make_uniswap_v3_pool,
    _make_uniswap_v4_pool,
)


class FakeUniswapArbEngine:
    """Records register_and_solve_path calls; returns monotonic path ids."""

    def __init__(self) -> None:
        self.calls: list[list[tuple[int, bool]]] = []
        self._next_id = 1

    def register_and_solve_path(self, hops: list[tuple[int, bool]]) -> int:
        self.calls.append(list(hops))
        path_id = self._next_id
        self._next_id += 1
        return path_id


def test_register_path_dispatches_keys_and_stores_built_hops() -> None:
    """register_path maps each pool to its engine key + direction and stores
    the HopInfo list built from pool attributes."""
    fake = FakeUniswapArbEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    v2 = _make_uniswap_v2_pool()
    v3 = _make_uniswap_v3_pool()
    registry._v2_keys[v2.address] = 100
    registry._v3_keys[v3.address] = 200

    pools_and_zfos = [(v2, True), (v3, False)]

    path_id = registry.register_path(pools_and_zfos)

    assert fake.calls == [[(100, True), (200, False)]]
    assert registry.paths[path_id].hops == build_hops_from_pools(pools_and_zfos)


def test_register_path_v4_keyed_by_pool_id_hex() -> None:
    """V4 pools resolve their engine key from _v4_keys[pool_id_hex]."""
    fake = FakeUniswapArbEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    v4 = _make_uniswap_v4_pool()
    pool_id_hex = v4.pool_id.to_0x_hex()
    registry._v4_keys[pool_id_hex] = 999

    path_id = registry.register_path([(v4, True)])

    assert fake.calls == [[(999, True)]]
    assert registry.paths[path_id].hops[0].pool_id_hex == pool_id_hex


class _FakeBot:
    """Minimal Bot double exposing ``_py_bot`` for the production construction path.

    ``EngineRegistry(bot=...)`` dereferences ``bot._py_bot`` to build the real
    engine against the bot's shared BotState (ADR-006 D1). A bare ``PyBot()`` is
    the offline stand-in — ``test_shared_state_topology`` proves the shared-core
    topology with this exact pair, no RPC/anvil needed.
    """

    def __init__(self) -> None:
        self._py_bot = PyBot()


def test_bot_none_without_engine_raises() -> None:
    """Production path requires a bot when no engine is supplied."""
    with pytest.raises(ValueError, match=r"engine.*bot|bot.*engine"):
        runner.EngineRegistry(bot=None)


def test_bot_supplies_py_bot_to_real_engine() -> None:
    """EngineRegistry(bot=bot) constructs the real engine against bot._py_bot."""
    bot = _FakeBot()
    registry = runner.EngineRegistry(bot=bot)

    assert isinstance(registry.engine, UniswapArbEngine)
    # The engine shares the bot's BotState (ADR-006 D1): registering a V2 pool
    # against the same py_bot must be visible to the engine's pool_count.
    registry.engine.register_v2_pool(
        address="0x0000000000000000000000000000000000000001",
        reserve0=1_000_000,
        reserve1=1_000_000,
        gamma_numer=997,
        fee_denom=1000,
    )
    assert registry.engine.v2_pool_count() == 1
