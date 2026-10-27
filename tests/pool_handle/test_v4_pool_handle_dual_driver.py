"""Dual-driver parity test for the prototype structural Pool handle (V4)."""

from __future__ import annotations

import pytest

from tests.helpers.pool_handle_factory import make_v4_handle


@pytest.fixture
def v4_pool():
    return make_v4_handle()


def test_structure_and_identity(v4_pool) -> None:
    assert v4_pool.structure() == "concentrated_liquidity"
    identity, variant = v4_pool.identity()
    assert identity == "concentrated_liquidity"
    assert variant == "uniswap_v4"


def test_concentrated_liquidity_view(v4_pool) -> None:
    concentrated_liquidity = v4_pool.concentrated_liquidity()

    assert concentrated_liquidity.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert concentrated_liquidity.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert concentrated_liquidity.fee == 3000
    assert concentrated_liquidity.tick_spacing == 60
    assert concentrated_liquidity.liquidity == 1_000_000
    assert concentrated_liquidity.tick == 0
    assert concentrated_liquidity.sqrt_price_x96 == 1 << 96


def test_calculate_tokens_out(v4_pool) -> None:
    out = v4_pool.calculate_tokens_out(zero_for_one=True, amount_in=1_000)

    assert out is not None
    assert out > 0
