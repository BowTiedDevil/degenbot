"""Tests for `build_hops_from_pools` — HopInfo construction from pool objects.

Behavior tests (red/green driven): the function builds the correct HopInfo
subtype from a concrete pool object + direction, reading attributes off the
pool rather than requiring the caller to hand-stitch per-hop dicts.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from examples.eth_backrun_helpers import (
    PathInfo,
    SolidlyHopInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
    build_hops_from_pools,
)
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.types.test_concrete_pool_construction import (
    PANCAKESWAP_V2_FACTORY,
    SUSHISWAP_V2_FACTORY,
    UNISWAP_V2_FACTORY,
    _make_sushi,
    _make_uniswap_v2_pool,
    _make_uniswap_v3_pool,
    _make_uniswap_v4_pool,
    _make_usdc,
    _make_weth,
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


def test_build_hops_from_camelot_stable_swap_pool() -> None:
    """A Camelot V2 pool in ``stable_swap`` mode builds a SolidlyHopInfo.

    Camelot stable pools live under ``UniswapV2Pool`` (the V2 identity carries
    ``stable_swap=True``); the engine's ``derive_hop_type`` classifies these as
    the Solidly hop family. The hop's variant tag is ``camelot-v2-stable``.
    """
    from fractions import Fraction

    weth = _make_weth()
    usdc = _make_usdc()
    pool = make_v2_pool(
        address="0x1f8CCa4F4aBEcdFC2aC82e7E7D7A3F3B5D8C0aB4",
        token0=usdc,
        token1=weth,
        factory=SUSHISWAP_V2_FACTORY,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=1000 * 10**18,
        state_block=18_000_000,
        stable_swap=True,
        fee_denominator=10_000,
        variant="camelot-v2-stable",
    )

    hops = build_hops_from_pools([(pool, True)])

    assert len(hops) == 1
    hop = hops[0]
    assert isinstance(hop, SolidlyHopInfo)
    assert hop.stable is True
    assert hop.variant == "camelot-v2-stable"
    assert hop.zfo is True
    assert hop.fee == pool.fee_token0


def test_path_type_recognizes_solidly_hops() -> None:
    """``PathInfo.path_type`` emits ``Solidly`` for SolidlyHopInfo hops."""
    weth = _make_weth()
    usdc = _make_usdc()
    aerodrome = make_aerodrome_v2_pool(
        address="0xAE7FFAe65eC9eA34741B0FbA1E5dBc4F0eC5Ea6F",
        token0=usdc,
        token1=weth,
        factory="0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        fee=Fraction(3, 1000),
        stable=True,
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=1000 * 10**18,
    )
    v3 = _make_uniswap_v3_pool()

    hops = build_hops_from_pools([(aerodrome, True), (v3, False)])

    # (Solidly + V3 is NOT a registerable path itself — V3+Solidly is rejected
    # by scope at solve time. This test only exercises path_type building.)
    pinfo = PathInfo(hops=hops)
    assert pinfo.path_type == "Solidly-V3"


def test_build_hops_raises_on_unsupported_pool_type() -> None:
    """An object that is not V2/V3/V4/Solidly raises TypeError naming its type."""

    class NotAPool:
        pass

    with pytest.raises(TypeError, match="Unsupported pool type: NotAPool"):
        build_hops_from_pools([(NotAPool(), True)])


def test_build_hops_from_aerodrome_stable_pool() -> None:
    """An Aerodrome V2 stable pool builds a SolidlyHopInfo.

    The hop's variant tag mirrors the Rust DexVariant kebab-case and ``stable``
    matches the pool's flag, so the engine (which derives the Solidly family
    at register_path time) and the encoder stay in sync.
    """
    weth = _make_weth()
    usdc = _make_usdc()
    pool = make_aerodrome_v2_pool(
        address="0xAE7FFAe65eC9eA34741B0FbA1E5dBc4F0eC5Ea6F",
        token0=usdc,
        token1=weth,
        factory="0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        fee=__import__("fractions").Fraction(3, 1000),
        stable=True,
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=1000 * 10**18,
    )

    hops = build_hops_from_pools([(pool, True)])

    assert len(hops) == 1
    hop = hops[0]
    assert isinstance(hop, SolidlyHopInfo)
    assert hop.pool_address == pool.address
    assert hop.token0_address == pool.token0.address
    assert hop.token1_address == pool.token1.address
    assert hop.stable is True
    assert hop.variant == "aerodrome-v2-stable"
    assert hop.zfo is True
    # Directional fee: zfo=True picks fee_token0; Aerodrome pools swap
    # symmetrically so fee_token0 == fee_token1.
    assert hop.fee == pool.fee_token0


def test_build_hops_from_aerodrome_volatile_pool_stays_solidly() -> None:
    """An Aerodrome VOLATILE pool also routes to SolidlyHopInfo (variant tagged
    volatile) — the engine solves the volatile path via calc_exact_in_volatile."""  # noqa: E501
    weth = _make_weth()
    usdc = _make_usdc()
    pool = make_aerodrome_v2_pool(
        address="0x3a96FFa26a3D11a9d54242B1AE0fB3Ea7e7Dc12E",
        token0=usdc,
        token1=weth,
        factory="0x9008d19f58aabd9ed0d60971565aa8510560ab41",
        fee=__import__("fractions").Fraction(3, 1000),
        stable=False,
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=1000 * 10**18,
    )

    hops = build_hops_from_pools([(pool, False)])

    assert len(hops) == 1
    hop = hops[0]
    assert isinstance(hop, SolidlyHopInfo)
    assert hop.stable is False
    assert hop.variant == "aerodrome-v2-volatile"
    assert hop.zfo is False
    assert hop.fee == pool.fee_token1
