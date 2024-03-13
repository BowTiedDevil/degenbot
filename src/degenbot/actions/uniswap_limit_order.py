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
    LESS_THAN = enum.auto()
    LESS_THAN_OR_EQUAL = enum.auto()
    EQUALS = enum.auto()
    GREATER_THAN_OR_EQUAL = enum.auto()
    GREATER_THAN = enum.auto()


class UniswapLimitOrder(ConditionalAction):
    def __init__(
        self,
        pool: LiquidityPool | V3LiquidityPool,
        buy_token: Erc20Token,
        price_mode: PriceModes,
        price_target: int | float | Decimal | Fraction,
        actions: Sequence[Callable[[], Any]],
    ):
        """
        A Uniswap limit order, triggered by conditions involving the token price of `token` in the given `pool`
        """

        match price_mode:
            case PriceModes.LESS_THAN:
                self.condition = TokenPriceLessThan(token=buy_token, pool=pool, target=price_target)
            case PriceModes.LESS_THAN_OR_EQUAL:
                self.condition = TokenPriceLessThanOrEqual(
                    token=buy_token, pool=pool, target=price_target
                )
            case PriceModes.EQUALS:
                self.condition = TokenPriceEquals(token=buy_token, pool=pool, target=price_target)
            case PriceModes.GREATER_THAN_OR_EQUAL:
                self.condition = TokenPriceGreaterThanOrEqual(
                    token=buy_token, pool=pool, target=price_target
                )
            case PriceModes.GREATER_THAN:
                self.condition = TokenPriceGreaterThan(
                    token=buy_token, pool=pool, target=price_target
                )
            case _:
                raise ValueError(f"Unknown price mode {price_mode} specified")

        self.actions = actions

    @classmethod
    def from_nominal_price(
        cls,
        pool: LiquidityPool | V3LiquidityPool,
        buy_token: Erc20Token,
        price_mode: PriceModes,
        price_target: int | float | Decimal | Fraction,
        actions: Sequence[Callable[[], Any]],
    ) -> "UniswapLimitOrder":
        """
        Build a Uniswap limit order using nominal prices. Translates nominal values to absolute
        values for the token.

        e.g. a price target of 100.00 USDC is translated to 100.00 * 10**6
        """

        absolute_target = price_target * 10**buy_token.decimals
        return cls(
            pool=pool,
            buy_token=buy_token,
            price_mode=price_mode,
            price_target=absolute_target,
            actions=actions,
        )
