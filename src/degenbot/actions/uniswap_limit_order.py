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
from .token_ratio_conditions import (
    TokenRatioEquals,
    TokenRatioGreaterThan,
    TokenRatioGreaterThanOrEqual,
    TokenRatioLessThan,
    TokenRatioLessThanOrEqual,
)


class ComparisonModes(enum.Enum):
    LESS_THAN = enum.auto()
    LESS_THAN_OR_EQUAL = enum.auto()
    EQUALS = enum.auto()
    GREATER_THAN_OR_EQUAL = enum.auto()
    GREATER_THAN = enum.auto()


class UniswapPriceLimitOrder(ConditionalAction):
    def __init__(
        self,
        pool: LiquidityPool | V3LiquidityPool,
        buy_token: Erc20Token,
        comparison: ComparisonModes,
        target: int | float | Decimal | Fraction,
        actions: Sequence[Callable[[], Any]],
    ):
        """
        A Uniswap pool limit order, conditionally executed against the *nominal* price of
        `buy_token` in the given `pool`. The nominal price is expressed ignoring the decimal
        multiplier set by the token contract, e.g. 1 WETH / DAI instead of
        1*10**18 WETH / 1*10**18 DAI
        """

        self.buy_token = buy_token
        self.pool = pool

        if buy_token not in pool.tokens:
            raise ValueError(f"{buy_token} not found in {pool}")

        if isinstance(target, Decimal):
            target = Fraction.from_decimal(target)
        elif isinstance(target, float):
            target = Fraction.from_float(target)

        # Price conditionals are evaluated against the absolute price, so convert from nominal
        if buy_token == pool.token0:
            absolute_price_target = target * Fraction(
                10**pool.token1.decimals,
                10**pool.token0.decimals,
            )
        else:
            absolute_price_target = target * Fraction(
                10**pool.token0.decimals,
                10**pool.token1.decimals,
            )

        match comparison:
            case ComparisonModes.LESS_THAN:
                self.condition = TokenPriceLessThan(
                    token=buy_token,
                    pool=pool,
                    target=absolute_price_target,
                )
            case ComparisonModes.LESS_THAN_OR_EQUAL:
                self.condition = TokenPriceLessThanOrEqual(
                    token=buy_token,
                    pool=pool,
                    target=absolute_price_target,
                )
            case ComparisonModes.EQUALS:
                self.condition = TokenPriceEquals(
                    token=buy_token,
                    pool=pool,
                    target=absolute_price_target,
                )
            case ComparisonModes.GREATER_THAN_OR_EQUAL:
                self.condition = TokenPriceGreaterThanOrEqual(
                    token=buy_token,
                    pool=pool,
                    target=absolute_price_target,
                )
            case ComparisonModes.GREATER_THAN:
                self.condition = TokenPriceGreaterThan(
                    token=buy_token,
                    pool=pool,
                    target=absolute_price_target,
                )
            case _:
                raise ValueError(f"Unknown price mode {comparison} specified")

        self.actions = actions

    def update_target(
        self,
        target: int | float | Decimal | Fraction,
    ) -> None:
        """
        Set an updated price target.
        """

        if isinstance(target, Decimal):
            target = Fraction.from_decimal(target)
        elif isinstance(target, float):
            target = Fraction.from_float(target)

        # Price conditionals are evaluated against the absolute price, so convert from nominal
        if self.buy_token == self.pool.token0:
            absolute_price_target = target * Fraction(
                10**self.pool.token1.decimals,
                10**self.pool.token0.decimals,
            )
        else:
            absolute_price_target = target * Fraction(
                10**self.pool.token0.decimals,
                10**self.pool.token1.decimals,
            )

        self.condition.update_target(absolute_price_target)


class UniswapRatioLimitOrder(ConditionalAction):
    def __init__(
        self,
        pool: LiquidityPool | V3LiquidityPool,
        buy_token: Erc20Token,
        comparison: ComparisonModes,
        target: int | float | Decimal | Fraction,
        actions: Sequence[Callable[[], Any]],
    ):
        """
        A Uniswap pool limit order, conditionally executed against the *nominal* rate of exchange
        (ratio) of `buy_token` in the given `pool`. The nominal ratio is expressed ignoring the
        decimal multiplier set by the token contract, e.g. 1 WETH / DAI instead of
        1*10**18 WETH / 1*10**18 DAI
        """

        self.buy_token = buy_token
        self.pool = pool

        if buy_token not in pool.tokens:
            raise ValueError(f"{buy_token} not found in {pool}")

        if isinstance(target, Decimal):
            target = Fraction.from_decimal(target)
        elif isinstance(target, float):
            target = Fraction.from_float(target)

        # Ratio conditionals are evaluated against the absolute ratio, so convert from nominal
        if buy_token == pool.token0:
            absolute_ratio_target = target * Fraction(
                10**pool.token0.decimals,
                10**pool.token1.decimals,
            )
        else:
            absolute_ratio_target = target * Fraction(
                10**pool.token1.decimals,
                10**pool.token0.decimals,
            )

        match comparison:
            case ComparisonModes.LESS_THAN:
                self.condition = TokenRatioLessThan(
                    token=buy_token,
                    pool=pool,
                    target=absolute_ratio_target,
                )
            case ComparisonModes.LESS_THAN_OR_EQUAL:
                self.condition = TokenRatioLessThanOrEqual(
                    token=buy_token,
                    pool=pool,
                    target=absolute_ratio_target,
                )
            case ComparisonModes.EQUALS:
                self.condition = TokenRatioEquals(
                    token=buy_token,
                    pool=pool,
                    target=absolute_ratio_target,
                )
            case ComparisonModes.GREATER_THAN_OR_EQUAL:
                self.condition = TokenRatioGreaterThanOrEqual(
                    token=buy_token,
                    pool=pool,
                    target=absolute_ratio_target,
                )
            case ComparisonModes.GREATER_THAN:
                self.condition = TokenRatioGreaterThan(
                    token=buy_token,
                    pool=pool,
                    target=absolute_ratio_target,
                )
            case _:
                raise ValueError(f"Unknown price mode {comparison} specified")

        self.actions = actions

    def update_target(
        self,
        target: int | float | Decimal | Fraction,
    ) -> None:
        """
        Set an updated ratio target.
        """

        if isinstance(target, Decimal):
            target = Fraction.from_decimal(target)
        elif isinstance(target, float):
            target = Fraction.from_float(target)

        # Ratio conditionals are evaluated against the absolute ratio, so convert from nominal
        if self.buy_token == self.pool.token0:
            absolute_ratio_target = target * Fraction(
                10**self.pool.token0.decimals,
                10**self.pool.token1.decimals,
            )
        else:
            absolute_ratio_target = target * Fraction(
                10**self.pool.token1.decimals,
                10**self.pool.token0.decimals,
            )

        self.condition.update_target(absolute_ratio_target)
