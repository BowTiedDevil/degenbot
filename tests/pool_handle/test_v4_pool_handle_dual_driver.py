"""Dual-driver parity test for the prototype structural Pool handle (V4)."""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot


@pytest.fixture
def v4_pool():
    bot = Bot(1)
    tick_data = {
        -60: (1_000_000, 1_000_000, 0),
        60: (1_000_000, -1_000_000, 0),
    }
    pool_id = bot.register_v4_pool(
        pool_manager="0x1111111111111111111111111111111111111111",
        pool_id_hex="0x" + "00" * 32,
        currency0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        currency1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        fee=3000,
        tick_spacing=60,
        hook_address=None,
        sqrt_price_x96=1 << 96,
        liquidity=1_000_000,
        tick=0,
        block=100,
        tick_data=tick_data,
        coverage="tracked",
    )
    return bot.py_pool(pool_id)


def test_structure_and_identity(v4_pool) -> None:
    assert v4_pool.structure() == "concentrated_liquidity"
    identity, variant = v4_pool.identity()
    assert identity == "concentrated_liquidity"
    assert variant == "uniswap_v4"


def test_concentrated_liquidity_view(v4_pool) -> None:
    cl = v4_pool.concentrated_liquidity()

    assert cl.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert cl.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert cl.fee == 3000
    assert cl.tick_spacing == 60
    assert cl.liquidity == 1_000_000
    assert cl.tick == 0
    assert cl.sqrt_price_x96 == 1 << 96


def test_calculate_tokens_out(v4_pool) -> None:
    out = v4_pool.calculate_tokens_out(True, 1_000)

    assert out is not None
    assert out > 0
