from fractions import Fraction

import web3

from degenbot import Erc20Token
from degenbot.arbitrage import FlashBorrowToLpSwapNew
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool


class MockErc20Token(Erc20Token):
    def __init__(self):
        pass


class MockLiquidityPool(LiquidityPool):
    def __init__(self):
        pass


wbtc = MockErc20Token()
wbtc.address = web3.Web3.toChecksumAddress(
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
)
wbtc.decimals = 8
wbtc.name = "Wrapped BTC"
wbtc.symbol = "WBTC"

weth = MockErc20Token()
weth.address = web3.Web3.toChecksumAddress(
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
)
weth.decimals = 18
weth.name = "Wrapped Ether"
weth.symbol = "WETH"

uni_v2_lp = MockLiquidityPool()
uni_v2_lp.name = "WBTC-WETH (UniV2, 0.30%)"
uni_v2_lp.address = web3.Web3.toChecksumAddress(
    "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
)
uni_v2_lp.factory = web3.Web3.toChecksumAddress(
    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
)
uni_v2_lp.fee = None
uni_v2_lp.fee_token0 = Fraction(3, 1000)
uni_v2_lp.fee_token1 = Fraction(3, 1000)
uni_v2_lp.reserves_token0 = 20000000000
uni_v2_lp.reserves_token1 = 3000000000000000000000
uni_v2_lp.token0 = wbtc
uni_v2_lp.token1 = weth
uni_v2_lp.new_reserves = True
uni_v2_lp._update_pool_state()

sushi_v2_lp = MockLiquidityPool()
sushi_v2_lp.name = "WBTC-WETH (SushiV2, 0.30%)"
sushi_v2_lp.address = web3.Web3.toChecksumAddress(
    "0xCEfF51756c56CeFFCA006cD410B03FFC46dd3a58"
)
sushi_v2_lp.factory = web3.Web3.toChecksumAddress(
    "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
)
sushi_v2_lp.fee = None
sushi_v2_lp.fee_token0 = Fraction(3, 1000)
sushi_v2_lp.fee_token1 = Fraction(3, 1000)
sushi_v2_lp.reserves_token0 = 20000000000
sushi_v2_lp.reserves_token1 = 3000000000000000000000
sushi_v2_lp.token0 = wbtc
sushi_v2_lp.token1 = weth
sushi_v2_lp.new_reserves = True
sushi_v2_lp._update_pool_state()


arb = FlashBorrowToLpSwapNew(
    borrow_pool=uni_v2_lp,
    borrow_token=wbtc,
    repay_token=weth,
    swap_pools=[sushi_v2_lp],
    update_method="external",
)


def test_type_checks():
    # Need to ensure that the mocked helpers will pass the type checks
    # inside various methods
    assert isinstance(uni_v2_lp, LiquidityPool)
    assert isinstance(sushi_v2_lp, LiquidityPool)
    assert isinstance(weth, Erc20Token)
    assert isinstance(wbtc, Erc20Token)


def test_arbitrage():
    uni_v2_lp.new_reserves = True
    sushi_v2_lp.new_reserves = True

    arb.update_reserves()

    # no profit expected, both pools have the same reserves
    assert arb.best["borrow_amount"] == 0
    assert arb.best["borrow_pool_amounts"] == []
    assert arb.best["profit_amount"] == 0
    assert arb.best["repay_amount"] == 0
    assert arb.best["swap_pool_amounts"] == []

    # best_future should be empty (no overrides were provided)
    assert arb.best_future["borrow_amount"] == 0
    assert arb.best_future["borrow_pool_amounts"] == []
    assert arb.best_future["profit_amount"] == 0
    assert arb.best_future["repay_amount"] == 0
    assert arb.best_future["swap_pool_amounts"] == []


def test_arbitrage_with_overrides():
    uni_v2_lp.new_reserves = True
    sushi_v2_lp.new_reserves = True

    arb.update_reserves(
        override_future=True,
        override_future_borrow_pool_reserves_token0=20000000000,
        override_future_borrow_pool_reserves_token1=4000000000000000000000
        // 2,
    )

    # non-override state should be the same
    assert arb.best["borrow_amount"] == 0
    assert arb.best["borrow_pool_amounts"] == []
    assert arb.best["profit_amount"] == 0
    assert arb.best["repay_amount"] == 0
    assert arb.best["swap_pool_amounts"] == []

    # override state should reflect a profit opportunity from the severe
    # mismatch in pool reserves (+33% WETH reserves in Sushi pool)
    assert arb.best_future["borrow_amount"] == 1993359746
    assert arb.best_future["borrow_pool_amounts"] == [1993359746, 0]
    assert arb.best_future["profit_amount"] == 49092923683591028736
    assert arb.best_future["repay_amount"] == 222068946927979774742
    assert arb.best_future["swap_pool_amounts"] == [[0, 271161870611570793739]]
