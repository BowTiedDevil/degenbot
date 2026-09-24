"""Public structural Pool cutover contract.

These tests exercise the registered ``Pool`` handle as the companion boundary:
reads are atomic structural views and writes are verb-shaped commands. Family
identity still comes from the handle so companion construction can remain a
single-argument seam.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

import degenbot._ffi as ffi
from degenbot._ffi import Bot, Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool


def _reserve_pair() -> Pool:
    bot = Bot(1)
    pool_id = bot.register_v2_pool_test_only(
        address="0x1111111111111111111111111111111111111111",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        reserve0=1_000,
        reserve1=2_000,
        gamma_numer0=997,
        fee_denom0=1_000,
        gamma_numer1=997,
        fee_denom1=1_000,
        factory="0x2222222222222222222222222222222222222222",
        update_block=100,
        variant="uniswap-v2",
        stable_swap=False,
        fee_denominator=None,
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None
    return handle


def _balance_vector() -> Pool:
    bot = Bot(1)
    pool_id = bot.register_balancer_weighted_pool(
        address="0x3333333333333333333333333333333333333333",
        vault="0x4444444444444444444444444444444444444444",
        pool_id_hex="0x" + "00" * 32,
        tokens=[
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ],
        weights=[10**18, 10**18],
        scaling_factors=[10**18, 10**18],
        swap_fee=0,
        pow_version=2,
        balances=[1_000, 2_000],
        update_block=100,
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None
    return handle


def test_get_pool_returns_the_structural_handle_and_retires_prototype_accessor() -> None:
    handle = _reserve_pair()

    assert type(handle) is Pool
    assert not hasattr(handle, "reserve0")
    assert not hasattr(handle, "snapshot")
    assert not hasattr(Bot(1), "py_pool")


def test_production_companion_uses_the_canonical_structural_handle() -> None:
    companion = make_v2_pool(
        address="0x" + "1" * 40,
        token0=make_erc20_for_pool("0x" + "2" * 40, "T0", 18),
        token1=make_erc20_for_pool("0x" + "3" * 40, "T1", 6),
        factory="0x" + "4" * 40,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=10**18,
        reserves_token1=2 * 10**18,
    )

    assert type(companion._py_pool) is Pool
    assert not hasattr(companion._py_pool, "snapshot")
    assert not hasattr(companion._py_pool, "sync_reserves")


def make_erc20_for_pool(address: str, symbol: str, decimals: int):
    return make_erc20(Bot(1), address, name=symbol, symbol=symbol, decimals=decimals)


def test_reserve_pair_view_is_the_atomic_read_and_apply_sync_is_the_command() -> None:
    handle = _reserve_pair()

    before = handle.reserve_pair()
    assert (before.reserve0, before.reserve1, before.update_block) == (1_000, 2_000, 100)

    handle.apply_sync(3_000, 4_000, 101)

    after = handle.reserve_pair()
    assert (after.reserve0, after.reserve1, after.update_block) == (3_000, 4_000, 101)


def test_balance_vector_view_is_the_atomic_read_and_apply_balances_is_the_command() -> None:
    handle = _balance_vector()

    before = handle.balance_vector()
    assert (list(before.balances), before.update_block) == ([1_000, 2_000], 100)

    handle.apply_balances([3_000, 4_000], 101)

    after = handle.balance_vector()
    assert (list(after.balances), after.update_block) == ([3_000, 4_000], 101)


def test_structural_command_refuses_the_wrong_family_loudly() -> None:
    handle = _balance_vector()

    with pytest.raises(ValueError, match="reserve-pair"):
        handle.apply_sync(3_000, 4_000, 101)


def test_legacy_liquidity_pool_handle_is_not_registered() -> None:
    assert not hasattr(ffi, "LiquidityPool")
