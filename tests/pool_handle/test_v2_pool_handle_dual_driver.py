"""Dual-driver parity test for the prototype structural Pool handle (V2)."""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot


@pytest.fixture
def v2_pool():
    bot = Bot(1)
    pool_id = bot.register_v2_pool_test_only(
        address="0x1111111111111111111111111111111111111111",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        reserve0=1_000_000_000,
        reserve1=2_000_000_000,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory="0x2222222222222222222222222222222222222222",
        update_block=100,
        variant="uniswap-v2",
        stable_swap=False,
        fee_denominator=None,
    )
    return bot.py_pool(pool_id)


def test_structure_and_identity(v2_pool) -> None:
    assert v2_pool.structure() == "reserve_pair"
    identity, variant = v2_pool.identity()
    assert identity == "reserve_pair"
    assert variant == "uniswap_v2"


def test_reserve_pair_view(v2_pool) -> None:
    rp = v2_pool.reserve_pair()

    assert rp.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert rp.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert rp.reserve0 == 1_000_000_000
    assert rp.reserve1 == 2_000_000_000


def test_calculate_tokens_out(v2_pool) -> None:
    out = v2_pool.calculate_tokens_out(True, 1_000_000)

    assert out is not None
    assert out > 0


def test_dex_name_resolved_from_known_sushiswap_deployment() -> None:
    # SushiSwap V2 mainnet factory (chain 1) → "sushiswap".
    bot = Bot(1)
    pool_id = bot.register_v2_pool_test_only(
        address="0x3333333333333333333333333333333333333333",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        reserve0=1_000_000_000,
        reserve1=2_000_000_000,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
        update_block=100,
        variant="uniswap-v2",
        stable_swap=False,
        fee_denominator=None,
    )
    handle = bot.py_pool(pool_id)
    assert handle is not None
    assert handle.dex_name == "sushiswap"


def test_dex_name_unknown_deployment_is_none() -> None:
    # Synthetic factory absent from deployments.json → None (generic, no error).
    bot = Bot(1)
    pool_id = bot.register_v2_pool_test_only(
        address="0x4444444444444444444444444444444444444444",
        token0="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        token1="0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        reserve0=1_000_000_000,
        reserve1=2_000_000_000,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory="0x1111111111111111111111111111111111111111",
        update_block=100,
        variant="uniswap-v2",
        stable_swap=False,
        fee_denominator=None,
    )
    handle = bot.py_pool(pool_id)
    assert handle is not None
    assert handle.dex_name is None
