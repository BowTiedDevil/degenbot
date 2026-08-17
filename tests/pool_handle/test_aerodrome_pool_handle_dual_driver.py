"""Dual-driver parity test for the prototype structural Pool handle (Aerodrome V2)."""

from __future__ import annotations

import pytest

from degenbot._ffi import RustBot


@pytest.fixture
def aerodrome_pool():
    bot = RustBot(1)
    pool_id = bot.register_aerodrome_pool(
        address="0x1111111111111111111111111111111111111111",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        factory="0x2222222222222222222222222222222222222222",
        variant="aerodrome-v2-volatile",
        stable=False,
        fee_numer=3,
        fee_denom=10_000,
        token0_decimals=18,
        token1_decimals=18,
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


@pytest.fixture
def aerodrome_stable_pool():
    """Aerodrome V2 **stable**-mode pool (Solidly invariant) fixture.

    Mirrors the Rust `pool_handle_aerodrome.rs` `aerodrome_stable_swap_*`
    fixtures: equal 1e18 reserves, both 18 decimals, fee (3, 10000) = 0.03%.
    """
    bot = RustBot(1)
    pool_id = bot.register_aerodrome_pool(
        address="0x3333333333333333333333333333333333333333",
        token0="0xcccccccccccccccccccccccccccccccccccccccc",
        token1="0xdddddddddddddddddddddddddddddddddddddddd",
        factory="0x4444444444444444444444444444444444444444",
        variant="aerodrome-v2-stable",
        stable=True,
        fee_numer=3,
        fee_denom=10_000,
        token0_decimals=18,
        token1_decimals=18,
        reserve0=1_000_000_000_000_000_000,
        reserve1=1_000_000_000_000_000_000,
        update_block=100,
    )
    return bot.py_pool(pool_id)


def test_aerodrome_stable_swap_matches_recorded_constant(aerodrome_stable_pool) -> None:
    # Solidly stable invariant (x^3y + y^3x >= k) via calc_exact_in_stable_solidly,
    # swap 1e18 token0→token1. The recorded constant 753627265063405946 is
    # cross-checked against the independent Rust `simulate_swap` stable arm (same
    # leaf, independent marshalling) — the dual-driver pair
    # test_aerodrome_stable_swap_is_monotonic lives in pool_handle_aerodrome.rs.
    out = aerodrome_stable_pool.calculate_tokens_out(True, 1_000_000_000_000_000_000)
    assert out is not None
    assert out == 753_627_265_063_405_946

    # Symmetry: equal balances + equal decimals ⇒ identical output both ways.
    out_rev = aerodrome_stable_pool.calculate_tokens_out(False, 1_000_000_000_000_000_000)
    assert out_rev == out, "symmetric direction on equal balances"


def test_aerodrome_stable_swap_is_monotonic(aerodrome_stable_pool) -> None:
    small = aerodrome_stable_pool.calculate_tokens_out(True, 1_000_000_000_000_000_000)
    large = aerodrome_stable_pool.calculate_tokens_out(True, 2_000_000_000_000_000_000)
    assert small is not None
    assert large is not None
    assert large > small, "output must increase with input"
    assert large < 2_000_000_000_000_000_000, "output bounded below input (fee + slippage)"
