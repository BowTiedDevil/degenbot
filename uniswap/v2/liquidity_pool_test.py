from fractions import Fraction

import pytest

from degenbot import Erc20Token, LiquidityPool
from degenbot.exceptions import ZeroSwapError


def test_liquidity_pool_calculate_tokens_out_from_tokens_in():
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
    # Reserve values taken at block height 17,600,000

    token0 = MockErc20Token()
    token0.name = "Wrapped BTC"
    token0.symbol = "WBTC"
    token0.address = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    token0.decimals = 8

    token1 = MockErc20Token()
    token1.decimals = 18
    token1.name = "Wrapped Ether"
    token1.symbol = "WETH"
    token1.address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

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

    assert (
        lp.calculate_tokens_out_from_tokens_in(lp.token0, 8000000000)
        == 847228560678214929944
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(
            lp.token1, 1200000000000000000000
        )
        == 5154005339
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(8000000000, lp.token1)
        == 2506650866141614297072
    )

    assert (
        lp.calculate_tokens_in_from_tokens_out(
            1200000000000000000000, lp.token0
        )
        == 14245938804
    )

    with pytest.raises(ZeroSwapError):
        assert lp.calculate_tokens_out_from_tokens_in(lp.token0, 0) == 0

    with pytest.raises(ZeroSwapError):
        assert lp.calculate_tokens_out_from_tokens_in(lp.token1, 0) == 0

    # The last token in a pool can never be swapped for
    assert (
        lp.calculate_tokens_out_from_tokens_in(lp.token1, 2**256 - 1)
        == lp.reserves_token0 - 1
    )
    assert (
        lp.calculate_tokens_out_from_tokens_in(lp.token0, 2**256 - 1)
        == lp.reserves_token1 - 1
    )
