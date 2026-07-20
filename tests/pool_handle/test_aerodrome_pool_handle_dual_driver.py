"""Dual-driver parity test for the prototype structural Pool handle (Aerodrome V2)."""

from __future__ import annotations

import pytest

from degenbot._ffi import PyBot


@pytest.fixture
def aerodrome_pool():
    bot = PyBot(1)
    pool_id = bot.register_aerodrome_pool(
        address="0x1111111111111111111111111111111111111111",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        factory="0x2222222222222222222222222222222222222222",
        variant="aerodrome-v2-volatile",
        stable=False,
        fee_numer=3,
        fee_denom=10_000,
        reserve0=1_000_000_000,
        reserve1=2_000_000_000,
        update_block=100,
    )
    return bot.py_pool(pool_id)


def test_structure_and_identity(aerodrome_pool) -> None:
    assert aerodrome_pool.structure() == "reserve_pair"
    identity, variant = aerodrome_pool.identity()
    assert identity == "reserve_pair"
    assert variant == "aerodrome_v2_volatile"


def test_reserve_pair_view(aerodrome_pool) -> None:
    rp = aerodrome_pool.reserve_pair()

    assert rp.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert rp.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert rp.reserve0 == 1_000_000_000
    assert rp.reserve1 == 2_000_000_000


def test_calculate_tokens_out(aerodrome_pool) -> None:
    # Aerodrome volatile swap math dispatches through the Rust solidly math
    # (`calc_exact_in_volatile`) — pure constant-product math, no decimals
    # needed.
    out = aerodrome_pool.calculate_tokens_out(True, 1_000_000)

    assert out is not None
    assert out > 0
