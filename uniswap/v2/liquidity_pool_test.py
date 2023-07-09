from fractions import Fraction

import pytest

from degenbot import Erc20Token, LiquidityPool
from degenbot.exceptions import ZeroSwapError
from degenbot.uniswap.v2.liquidity_pool import (
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


class MockLiquidityPool(LiquidityPool):
    def __init__(self):
        pass


# Test is based on the WBTC-WETH Uniswap V2 pool on Ethereum mainnet,
# evaluated against the results from the Uniswap V2 Router 2 contract
# functions `getAmountsOut` and `getAmountsIn`
#
# Pool address: 0xBb2b8038a1640196FbE3e38816F3e67Cba72D940
# Router address: 0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D


token0 = MockErc20Token()
token0.address = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
token0.decimals = 8
token0.name = "Wrapped BTC"
token0.symbol = "WBTC"

token1 = MockErc20Token()
token1.address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
token1.decimals = 18
token1.name = "Wrapped Ether"
token1.symbol = "WETH"

lp = MockLiquidityPool()
lp.name = "WBTC-WETH (V2, 0.30%)"
lp.address = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
lp.factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
lp.fee = None
lp.fee_token0 = Fraction(3, 1000)
lp.fee_token1 = Fraction(3, 1000)
lp.reserves_token0 = 16231137593
lp.reserves_token1 = 2571336301536722443178
lp.token0 = token0
lp.token1 = token1


def test_calculate_tokens_out_from_tokens_in():
    # Reserve values for this test are taken at block height 17,600,000

    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token0,
            8000000000,
        )
        == 847228560678214929944
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token1,
            1200000000000000000000,
        )
        == 5154005339
    )


def test_calculate_tokens_out_from_tokens_in_with_override():
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == 864834865217768537471
    )


def test_calculate_tokens_in_from_tokens_out():
    # Reserve values for this test are taken at block height 17,600,000

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            8000000000,
            lp.token1,
        )
        == 2506650866141614297072
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            1200000000000000000000,
            lp.token0,
        )
        == 14245938804
    )


def test_calculate_tokens_in_from_tokens_out_with_override():
    # Overridden reserve values for this test are taken at block height 17,650,000
    # token0 reserves: 16027096956
    # token1 reserves: 2602647332090181827846

    pool_state_override = UniswapV2PoolState(
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            token_in=lp.token0,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == 13752842264
    )


def test_simulations():
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-847228560678214929944,
    )

    # token_in = lp.token0 should have same result as token_out = lp.token1
    assert (
        lp.simulate_swap(
            token_in=lp.token0,
            token_in_quantity=8000000000,
        )
        == sim_result
    )
    assert (
        lp.simulate_swap(
            token_out=lp.token1,
            token_in_quantity=8000000000,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=-5154005339,
        amount1_delta=1200000000000000000000,
    )

    assert (
        lp.simulate_swap(
            token_in=lp.token1,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )

    assert (
        lp.simulate_swap(
            token_out=lp.token0,
            token_in_quantity=1200000000000000000000,
        )
        == sim_result
    )


def test_simulations_with_override():
    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=8000000000,
        amount1_delta=-864834865217768537471,
    )

    pool_state_override = UniswapV2PoolState(
        reserves_token0=16027096956,
        reserves_token1=2602647332090181827846,
    )

    assert (
        lp.simulate_swap(
            token_in=lp.token0,
            token_in_quantity=8000000000,
            override_state=pool_state_override,
        )
        == sim_result
    )

    sim_result = UniswapV2PoolSimulationResult(
        amount0_delta=13752842264,
        amount1_delta=-1200000000000000000000,
    )

    assert (
        lp.simulate_swap(
            token_out=lp.token1,
            token_out_quantity=1200000000000000000000,
            override_state=pool_state_override,
        )
        == sim_result
    )


def test_zero_swaps():
    with pytest.raises(ZeroSwapError):
        assert (
            lp.calculate_tokens_out_from_tokens_in(
                lp.token0,
                0,
            )
            == 0
        )

    with pytest.raises(ZeroSwapError):
        assert (
            lp.calculate_tokens_out_from_tokens_in(
                lp.token1,
                0,
            )
            == 0
        )


def test_swap_for_all():
    # The last token in a pool can never be swapped for
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token1,
            2**256 - 1,
        )
        == lp.reserves_token0 - 1
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token0,
            2**256 - 1,
        )
        == lp.reserves_token1 - 1
    )
