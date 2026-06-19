"""Tests for `build_hops_from_pools` — HopInfo construction from pool objects.

Behavior tests (red/green driven): the function builds the correct HopInfo
subtype from a concrete pool object + direction, reading attributes off the
pool rather than requiring the caller to hand-stitch per-hop dicts.
"""

from __future__ import annotations

import pytest

from examples.eth_backrun_helpers import (
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
    build_hops_from_pools,
)
from tests.types.test_concrete_pool_construction import (
    _make_uniswap_v2_pool,
    _make_uniswap_v3_pool,
    _make_uniswap_v4_pool,
)


def test_build_hops_from_v2_pool() -> None:
    """A V2 pool + direction builds a V2HopInfo from pool attributes.

    zfo=True sells token0, so the directional fee comes from fee_token0
    scaled to bips-of-10000 (Fraction(3,1000) -> 30).
    """
    pool = _make_uniswap_v2_pool()

    hops = build_hops_from_pools([(pool, True)])

    assert len(hops) == 1
    hop = hops[0]
    assert isinstance(hop, V2HopInfo)
    assert hop.pool_address == pool.address
    assert hop.token0_address == pool.token0.address
    assert hop.token1_address == pool.token1.address
    assert hop.fee == 30
    assert hop.zfo is True


def test_build_hops_from_v3_pool() -> None:
    """A V3 pool + direction builds a V3HopInfo from pool attributes."""
    pool = _make_uniswap_v3_pool()

    hops = build_hops_from_pools([(pool, False)])

    assert len(hops) == 1
    hop = hops[0]
    assert isinstance(hop, V3HopInfo)
    assert hop.pool_address == pool.address
    assert hop.token0_address == pool.token0.address
    assert hop.token1_address == pool.token1.address
    assert hop.fee == pool.fee
    assert hop.zfo is False


def test_build_hops_from_v4_pool() -> None:
    """A V4 pool + direction builds a V4HopInfo from pool attributes."""
    pool = _make_uniswap_v4_pool()

    hops = build_hops_from_pools([(pool, True)])

    assert len(hops) == 1
    hop = hops[0]
    assert isinstance(hop, V4HopInfo)
    assert hop.pool_manager_address == pool.address
    assert hop.pool_id_hex == pool.pool_id.to_0x_hex()
    assert hop.currency0_address == pool.token0.address
    assert hop.currency1_address == pool.token1.address
    assert hop.fee == pool.fee
    assert hop.tick_spacing == pool.tick_spacing
    assert hop.hook_address == pool.hook_address
    assert hop.zfo is True


def test_build_hops_preserves_order_and_directional_v2_fee() -> None:
    """A multi-hop list yields hops in input order with correct types.

    The V2 directional fee must follow the per-hop direction: zfo=True picks
    fee_token0, zfo=False picks fee_token1 — both scaled to bips-of-10000.
    """
    v2 = _make_uniswap_v2_pool()  # fee_token0 = fee_token1 = 3/1000 -> 30
    v3 = _make_uniswap_v3_pool()

    hops = build_hops_from_pools([(v2, True), (v3, False)])

    assert len(hops) == 2
    assert isinstance(hops[0], V2HopInfo)
    assert isinstance(hops[1], V3HopInfo)
    assert hops[0].zfo is True
    assert hops[1].zfo is False
    assert hops[0].fee == 30
    assert hops[1].pool_address == v3.address


def test_build_hops_raises_on_unsupported_pool_type() -> None:
    """An object that is not V2/V3/V4 raises TypeError naming its type."""

    class NotAPool:
        pass

    with pytest.raises(TypeError, match="Unsupported pool type: NotAPool"):
        build_hops_from_pools([(NotAPool(), True)])
