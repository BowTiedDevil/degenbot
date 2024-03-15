import functools
from decimal import Decimal
from fractions import Fraction

import pytest
from degenbot.actions.token_price_conditions import TokenPriceCondition
from degenbot.actions.token_ratio_conditions import TokenRatioCondition
from degenbot.actions.uniswap_limit_order import (
    ComparisonModes,
    UniswapPriceLimitOrder,
    UniswapRatioLimitOrder,
)
from degenbot.erc20_token import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import LiquidityPool

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRSS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_WETH_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"


@pytest.fixture
def weth() -> Erc20Token:
    return Erc20Token(WETH_ADDRSS)


@pytest.fixture
def wbtc() -> Erc20Token:
    return Erc20Token(WBTC_ADDRESS)


@pytest.fixture
def wbtc_weth_pool() -> LiquidityPool:
    return LiquidityPool(WBTC_WETH_POOL_ADDRESS)


def test_price_limit_order_creation(
    weth: Erc20Token,
    wbtc: Erc20Token,
    wbtc_weth_pool: LiquidityPool,
) -> None:
    dummy_action = functools.partial(print, "action")

    NOMINAL_PRICE_TARGET = Decimal("20.0")  # Nominal price of 20 WETH/WBTC
    ABSOLUTE_PRICE_TARGET = Fraction.from_decimal(NOMINAL_PRICE_TARGET) * Fraction(
        10**weth.decimals, 10**wbtc.decimals
    )

    for comparison_mode in ComparisonModes:
        order = UniswapPriceLimitOrder(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            target=NOMINAL_PRICE_TARGET,
            comparison=comparison_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenPriceCondition)
        assert order.condition.target == ABSOLUTE_PRICE_TARGET

    # Test other target formats
    for target in [
        20,
        20.0,
        Decimal("20.0"),
        Fraction(20, 1),
    ]:
        order = UniswapPriceLimitOrder(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            target=target,
            comparison=comparison_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenPriceCondition)


def test_ratio_limit_order_creation(
    weth: Erc20Token,
    wbtc: Erc20Token,
    wbtc_weth_pool: LiquidityPool,
) -> None:
    dummy_action = functools.partial(print, "action")

    NOMINAL_RATIO_TARGET = Decimal("0.05")  # Nominal ratio of 0.05 WBTC/WETH
    ABSOLUTE_RATIO_TARGET = Fraction.from_decimal(NOMINAL_RATIO_TARGET) * Fraction(
        10**wbtc.decimals,
        10**weth.decimals,
    )

    for comparison_mode in ComparisonModes:
        order = UniswapRatioLimitOrder(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            target=NOMINAL_RATIO_TARGET,
            comparison=comparison_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenRatioCondition)
        assert order.condition.target == ABSOLUTE_RATIO_TARGET

    # Test other target formats
    for target in [
        0,
        0.05,
        Decimal("0.05"),
        Fraction(1, 20),
    ]:
        order = UniswapRatioLimitOrder(
            pool=wbtc_weth_pool,
            buy_token=wbtc,
            target=target,
            comparison=comparison_mode,
            actions=[dummy_action],
        )
        assert isinstance(order.condition, TokenRatioCondition)
