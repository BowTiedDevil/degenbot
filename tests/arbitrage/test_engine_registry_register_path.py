"""Integration tests for EngineRegistry.register_path pool-driven API.

register_path takes a sequence of (pool, zero_for_one) pairs, builds HopInfo
via build_hops_from_pools, resolves each pool's engine key from the registry's
key maps, dispatches (key, zfo) pairs to the engine, and stores the built
PathInfo. These tests use a Fake engine (AGENTS.md prefers Fakes over mocks)
and seed the key maps directly, so no live Rust engine / BotState is needed.
"""

from __future__ import annotations

import examples.eth_backrun_v2_v3_v4_rust as runner
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
    registry = runner.EngineRegistry(py_bot=None, engine=fake)

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
    registry = runner.EngineRegistry(py_bot=None, engine=fake)

    v4 = _make_uniswap_v4_pool()
    pool_id_hex = v4.pool_id.to_0x_hex()
    registry._v4_keys[pool_id_hex] = 999

    path_id = registry.register_path([(v4, True)])

    assert fake.calls == [[(999, True)]]
    assert registry.paths[path_id].hops[0].pool_id_hex == pool_id_hex
