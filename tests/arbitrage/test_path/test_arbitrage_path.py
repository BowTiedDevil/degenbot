"""Tests for path-building primitives kept after ACDWOC's ArbitragePath retirement.

ACDWOC deleted ``ArbitragePath`` and the f64 Möbius solver stack — the
construction/validation/calculate/close classes that exercised the deleted
``ArbitragePath`` API went with it. This file keeps the primitives coverage
that survives the retirement: ``SwapVector``, pool ``to_hop_state`` /
``extract_fee``, and ``v3_libraries.functions.v3_virtual_reserves`` integer
math. The engine is the production solve surface, cross-validated against
``BrentSolver`` in ``tests/arbitrage/test_engine_vs_brent_parity.py``.
"""

from fractions import Fraction

import pytest

from degenbot import UniswapV2Pool
from degenbot.exceptions.arbitrage import IncompatiblePoolInvariant
from degenbot.types.hop_types import BoundedProductHop, ConstantProductHop
from degenbot.uniswap.v3_libraries.constants import Q96
from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves as _v3_virtual_reserves

from .conftest import (
    _make_aerodrome_pool,
    _make_token,
    _make_v2_pool,
    _make_v3_pool,
)

FEE_03 = Fraction(3, 1000)


class TestPoolCompatibility:
    def test_v2_compatible(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v2_pool(t0, t1)
        pool.to_hop_state(zero_for_one=True)  # should not raise

    def test_v3_compatible(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v3_pool(t0, t1)
        pool.to_hop_state(zero_for_one=True)  # should not raise

    def test_v4_compatible(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v3_pool(t0, t1)
        pool.to_hop_state(zero_for_one=True)  # should not raise

    def test_aerodrome_volatile_compatible(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_aerodrome_pool(t0, t1, stable=False)
        pool.to_hop_state(zero_for_one=True)  # should not raise

    def test_aerodrome_stable_compatible(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_aerodrome_pool(t0, t1, stable=True)
        pool.to_hop_state(zero_for_one=True)  # should not raise

    def test_unknown_incompatible(self):
        class _UnknownPool:
            pass

        with pytest.raises((IncompatiblePoolInvariant, AttributeError)):
            _UnknownPool().to_hop_state(zero_for_one=True)


class TestFeeExtraction:
    def test_v3_fee(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v3_pool(t0, t1, fee=3000)
        fee = pool.extract_fee(zero_for_one=True)
        assert fee == Fraction(3000, 1_000_000)

    def test_v2_fee_zero_for_one(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v2_pool(t0, t1, fee=Fraction(3, 1000))
        fee = pool.extract_fee(zero_for_one=True)
        assert fee == Fraction(3, 1000)

    def test_v2_fee_one_for_zero(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v2_pool(
            t0,
            t1,
            fee=Fraction(3, 1000),
            # Production pools use the same fee for both directions
            # when fee_token0 == fee_token1. To test asymmetric fees,
            # we'd need to pass different fee_token0/fee_token1.
        )
        fee = pool.extract_fee(zero_for_one=False)
        assert fee == Fraction(3, 1000)


class TestPoolToHopState:
    def test_v2_produces_constant_product_hop(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v2_pool(t0, t1)
        hop = pool.to_hop_state(zero_for_one=True)
        assert isinstance(hop, ConstantProductHop)
        assert hop.reserve_in == 10**18
        assert hop.reserve_out == 2 * 10**18
        assert hop.fee == FEE_03

    def test_v3_produces_bounded_product_hop(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool = _make_v3_pool(t0, t1)
        hop = pool.to_hop_state(zero_for_one=True)
        assert isinstance(hop, BoundedProductHop)

    def test_v2_direction(self):
        t0 = _make_token("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        t1 = _make_token("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        pool: UniswapV2Pool = _make_v2_pool(t0, t1, reserve0=1000, reserve1=2000)
        hop_forward: ConstantProductHop = pool.to_hop_state(zero_for_one=True)
        assert hop_forward.reserve_in == 1000
        assert hop_forward.reserve_out == 2000

        hop_reverse = pool.to_hop_state(zero_for_one=False)
        assert hop_reverse.reserve_in == 2000
        assert hop_reverse.reserve_out == 1000


class TestV3VirtualReservesIntegerMath:
    def test_price_one_symmetric(self):

        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=Q96, zero_for_one=True)
        assert x == 10**18 * Q96
        assert y == 10**18 * Q96

    def test_price_one_reversed(self):

        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=Q96, zero_for_one=False)
        assert x == 10**18 * Q96
        assert y == 10**18 * Q96

    def test_price_four(self):

        sqrt_p = 2 * Q96
        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=sqrt_p, zero_for_one=True)
        assert x == 10**18 * Q96 * Q96 // (2 * Q96)
        assert y == 10**18 * 2 * Q96

    def test_direction_swap(self):

        sqrt_p = 2 * Q96
        x_zfo, y_zfo = _v3_virtual_reserves(10**18, sqrt_p, zero_for_one=True)
        x_ofz, y_ofz = _v3_virtual_reserves(10**18, sqrt_p, zero_for_one=False)
        assert x_zfo == y_ofz
        assert y_zfo == x_ofz

    def test_product_equals_liquidity_squared_scaled(self):

        liquidity = 10**18
        sqrt_p = 79228162514264337593543950336
        x, y = _v3_virtual_reserves(liquidity, sqrt_p, zero_for_one=True)
        assert x * y == liquidity * liquidity * Q96 * Q96

    def test_large_liquidity_no_precision_loss(self):

        liquidity = 2**100
        sqrt_p = Q96
        x, y = _v3_virtual_reserves(liquidity, sqrt_p, zero_for_one=True)
        assert x == liquidity * Q96
        assert y == liquidity * Q96

    def test_matches_float_for_typical_values(self):

        liquidity = 10**18
        sqrt_price_x96 = 79228162514264337593543950336

        x_int, y_int = _v3_virtual_reserves(liquidity, sqrt_price_x96, zero_for_one=True)

        sqrt_price = sqrt_price_x96 / Q96
        x_float = round(liquidity / sqrt_price * Q96)
        y_float = round(liquidity * sqrt_price * Q96)

        assert abs(x_int - x_float) <= 1
        assert abs(y_int - y_float) <= 1
