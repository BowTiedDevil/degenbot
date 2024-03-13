import functools
from decimal import Decimal
from fractions import Fraction

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

    NOMINAL_PRICE_TARGET = Decimal("0.05")  # Nominal price of 0.05 WBTC/WETH
    ABSOLUTE_PRICE_TARGET = NOMINAL_PRICE_TARGET * 10**wbtc.decimals

    for price_mode in PriceModes:
        order = UniswapLimitOrder.from_nominal_price(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            price_target=NOMINAL_PRICE_TARGET,
            price_mode=price_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenPriceCondition)
        assert order.condition.target == ABSOLUTE_PRICE_TARGET

    for price_mode in PriceModes:
        order = UniswapLimitOrder(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            price_target=ABSOLUTE_PRICE_TARGET,
            price_mode=price_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenPriceCondition)
        assert order.condition.target == ABSOLUTE_PRICE_TARGET

    # Test with alternative target formats
    for target in [0.05, Fraction(1, 20)]:
        order = UniswapLimitOrder.from_nominal_price(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            price_target=target,
            price_mode=price_mode,
            actions=[dummy_action],
        )
        order.check()
