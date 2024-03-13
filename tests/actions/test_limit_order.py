import functools

from degenbot.actions.token_price_conditions import TokenPriceCondition
from degenbot.actions.uniswap_limit_order import PriceModes, UniswapLimitOrder
from degenbot.erc20_token import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import LiquidityPool


def test_limit_order_creation() -> None:
    WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
    WBTC_WETH_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"

    wbtc_weth_pool = LiquidityPool(WBTC_WETH_POOL_ADDRESS)
    wbtc = Erc20Token(WBTC_ADDRESS)

    dummy_action = functools.partial(print, "action")

    for price_mode in PriceModes:
        order = UniswapLimitOrder(
            token=wbtc,
            pool=wbtc_weth_pool,
            target=100_000,
            mode=price_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenPriceCondition)
        order.check()
