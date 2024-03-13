import enum
from decimal import Decimal
from fractions import Fraction
from typing import Any, Callable, Sequence

from ..erc20_token import Erc20Token
from ..uniswap.v2_liquidity_pool import LiquidityPool
from ..uniswap.v3_liquidity_pool import V3LiquidityPool
from .conditional_action import ConditionalAction
from .token_price_conditions import (
    TokenPriceEquals,
    TokenPriceGreaterThan,
    TokenPriceGreaterThanOrEqual,
    TokenPriceLessThan,
    TokenPriceLessThanOrEqual,
)


class PriceModes(enum.Enum):
    GREATER_THAN = enum.auto()
    GREATER_THAN_OR_EQUAL = enum.auto()
    LESS_THAN = enum.auto()
    LESS_THAN_OR_EQUAL = enum.auto()
    EQUALS = enum.auto()


class UniswapLimitOrder(ConditionalAction):
    def __init__(
        self,
        pool: LiquidityPool | V3LiquidityPool,
        token: Erc20Token,
        mode: PriceModes,
        target: int | float | Decimal | Fraction,
        actions: Sequence[Callable[[], Any]],
    ):
        if mode not in PriceModes:
            raise ValueError(f"Unknown price mode {mode} specified")

        match mode:
            case PriceModes.GREATER_THAN:
                self.condition = TokenPriceGreaterThan(token=token, pool=pool, target=target)
            case PriceModes.GREATER_THAN_OR_EQUAL:
                self.condition = TokenPriceGreaterThanOrEqual(token=token, pool=pool, target=target)
            case PriceModes.LESS_THAN:
                self.condition = TokenPriceLessThan(token=token, pool=pool, target=target)
            case PriceModes.LESS_THAN_OR_EQUAL:
                self.condition = TokenPriceLessThanOrEqual(token=token, pool=pool, target=target)
            case PriceModes.EQUALS:
                self.condition = TokenPriceEquals(token=token, pool=pool, target=target)

        self.actions = actions
