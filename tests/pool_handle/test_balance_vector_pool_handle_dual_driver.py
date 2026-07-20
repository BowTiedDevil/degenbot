"""Dual-driver parity test for the prototype structural Pool handle (balance-vector)."""

from __future__ import annotations

import pytest

from degenbot._ffi import PyBot


@pytest.fixture
def curve_pool():
    bot = PyBot(1)
    pool_id = bot.register_curve_pool(
        address="0x1111111111111111111111111111111111111111",
        tokens=[
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "0xcccccccccccccccccccccccccccccccccccccccc",
        ],
        a_coefficient=100,
        fee=4_000_000,
        admin_fee=0,
        rate_multipliers=[10**18, 10**18, 10**18],
        balances=[1_000_000_000, 2_000_000_000, 3_000_000_000],
        update_block=100,
    )
    return bot.py_pool(pool_id)


def test_structure_and_identity(curve_pool) -> None:
    assert curve_pool.structure() == "balance_vector"
    identity, variant = curve_pool.identity()
    assert identity == "balance_vector"
    assert variant == "curve"


def test_balance_vector_view(curve_pool) -> None:
    bv = curve_pool.balance_vector()

    assert bv.n_tokens == 3
    assert len(bv.tokens) == 3
    assert len(bv.balances) == 3
    assert bv.tokens[0].lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert bv.balances[0] == 1_000_000_000


def test_calculate_tokens_out_returns_sentinel(curve_pool) -> None:
    # Curve swap math is not yet dispatched from simulate_swap.
    out = curve_pool.calculate_tokens_out(True, 1_000_000)

    assert out is not None
    assert out == 0
