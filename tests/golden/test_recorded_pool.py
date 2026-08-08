"""Regression guard for the recorded-pool golden harness (T0, offline).

These tests run in CI (replay mode, no RPC/anvil) and re-assert the same fixed
outputs the fork-backed pool-math tests check, from the committed golden files.
If a harness refactor breaks reconstruction, or a golden is re-recorded against
the wrong block, these fail loudly offline.
"""

from pathlib import Path

import pytest

from tests.golden.recorded_pool import PoolGoldenError, load_pool

V2_WBTC_WETH = Path("tests/golden/data/uniswap/v2/wbtc_weth/17600000.json")
V3_WBTC_WETH = Path("tests/golden/data/uniswap/v3/wbtc_weth/17600000.json")

BLOCK = 17_600_000


def test_v2_round_trip_reproduces_exact_output() -> None:
    pool = load_pool(V2_WBTC_WETH, chain_id=1, block=BLOCK)
    # Same constants as test_calculate_tokens_out_from_tokens_in (v2 liquidity_pool).
    assert (
        pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token0,
            token_in_quantity=8_000_000_000,
        )
        == 847228560678214929944
    )


def test_v3_round_trip_reproduces_exact_output() -> None:
    pool = load_pool(V3_WBTC_WETH, chain_id=1, block=BLOCK)
    # Same constants as test_calculate_tokens_in_from_tokens_out (v3 liquidity_pool).
    assert (
        pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token1,
            token_out_quantity=10**18,
        )
        == 6325394
    )


def test_chain_mismatch_is_rejected() -> None:
    with pytest.raises(PoolGoldenError, match="chain_id"):
        load_pool(V2_WBTC_WETH, chain_id=137, block=BLOCK)


def test_block_mismatch_is_rejected() -> None:
    with pytest.raises(PoolGoldenError, match="block"):
        load_pool(V2_WBTC_WETH, chain_id=1, block=BLOCK + 1)


def test_v3_golden_carries_full_tick_data() -> None:
    pool = load_pool(V3_WBTC_WETH, chain_id=1, block=BLOCK)
    assert len(pool.tick_data) > 100
