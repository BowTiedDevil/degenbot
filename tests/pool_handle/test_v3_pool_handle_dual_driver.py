"""Dual-driver parity test for the prototype structural Pool handle (V3)."""

from __future__ import annotations

import pytest

from degenbot._ffi import PyBot


@pytest.fixture
def v3_pool():
    bot = PyBot(1)
    tick_data = {
        -60: (1_000_000, 1_000_000, 0),
        60: (1_000_000, -1_000_000, 0),
    }
    pool_id = bot.register_v3_pool(
        address="0x1111111111111111111111111111111111111111",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        fee=3000,
        tick_spacing=60,
        factory="0x2222222222222222222222222222222222222222",
        sqrt_price_x96=1 << 96,
        liquidity=1_000_000,
        tick=0,
        tick_data=tick_data,
        update_block=100,
        coverage="tracked",
    )
    return bot.py_pool(pool_id)


def test_structure_and_identity(v3_pool) -> None:
    assert v3_pool.structure() == "concentrated_liquidity"
    identity, variant = v3_pool.identity()
    assert identity == "concentrated_liquidity"
    assert variant == "uniswap_v3"


def test_concentrated_liquidity_view(v3_pool) -> None:
    cl = v3_pool.concentrated_liquidity()

    assert cl.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert cl.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert cl.fee == 3000
    assert cl.tick_spacing == 60
    assert cl.liquidity == 1_000_000
    assert cl.tick == 0
    assert cl.sqrt_price_x96 == 1 << 96


def test_calculate_tokens_out(v3_pool) -> None:
    out = v3_pool.calculate_tokens_out(True, 1_000)

    assert out is not None
    assert out > 0
