"""Dual-driver parity test for the prototype structural Pool handle (V3)."""

from __future__ import annotations

import pytest

from tests.helpers.pool_handle_factory import make_v3_handle


@pytest.fixture
def v3_pool():
    return make_v3_handle()


def test_structure_and_identity(v3_pool) -> None:
    assert v3_pool.structure() == "concentrated_liquidity"
    identity, variant = v3_pool.identity()
    assert identity == "concentrated_liquidity"
    assert variant == "uniswap_v3"


def test_concentrated_liquidity_view(v3_pool) -> None:
    concentrated_liquidity = v3_pool.concentrated_liquidity()

    assert concentrated_liquidity.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert concentrated_liquidity.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert concentrated_liquidity.fee == 3000
    assert concentrated_liquidity.tick_spacing == 60
    assert concentrated_liquidity.liquidity == 1_000_000
    assert concentrated_liquidity.tick == 0
    assert concentrated_liquidity.sqrt_price_x96 == 1 << 96


def test_calculate_tokens_out(v3_pool) -> None:
    out = v3_pool.calculate_tokens_out(zero_for_one=True, amount_in=1_000)

    assert out is not None
    assert out > 0


def test_dex_name_resolved_from_known_uniswap_deployment() -> None:
    # Uniswap V3 mainnet factory (chain 1) → "uniswap".
    handle = make_v3_handle(
        address="0xeb9e45330b5cf3209a2bfdce8b6eb6f52247290c",
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
    )
    assert handle.dex_name == "uniswap"


def test_dex_name_unknown_deployment_is_none() -> None:
    handle = make_v3_handle(
        address="0x4444444444444444444444444444444444444444",
        factory="0x1111111111111111111111111111111111111111",
    )
    assert handle.dex_name is None
