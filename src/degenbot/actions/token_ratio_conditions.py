from decimal import Decimal
from fractions import Fraction

from ..erc20_token import Erc20Token
from ..uniswap.v2_liquidity_pool import LiquidityPool
from ..uniswap.v3_liquidity_pool import V3LiquidityPool
from .baseclasses import BaseCondition


class TokenRatioCondition(BaseCondition):
    def __init__(
        self,
        token: Erc20Token,
        pool: LiquidityPool | V3LiquidityPool,
        target: int | float | Decimal | Fraction,
    ):
        """
        An abstract condition that can access the instantaneous rate of exchange (ratio) of `token`
        in terms of the other token held by `pool`. The price is absolute, i.e. it reflects the
        full decimal precision for the ERC-20 token contract.

        Derived classes should override the `__call__` method to implement boolean conditions
        related to this price.
        """

        self.token = token
        self.pool = pool
        self.target = target

    @property
    def exchange_rate(self) -> Fraction:
        return self.pool.get_absolute_rate(self.token)

    def update_target(
        self,
        price: int | float | Decimal | Fraction,
    ) -> None:
        self.target = price


class TokenRatioLessThan(TokenRatioCondition):
    def __call__(self) -> bool:
        return self.exchange_rate < self.target


class TokenRatioLessThanOrEqual(TokenRatioCondition):
    def __call__(self) -> bool:
        return self.exchange_rate <= self.target


class TokenRatioEquals(TokenRatioCondition):
    def __call__(self) -> bool:
        return self.exchange_rate == self.target


class TokenRatioGreaterThan(TokenRatioCondition):
    def __call__(self) -> bool:
        return self.exchange_rate > self.target


class TokenRatioGreaterThanOrEqual(TokenRatioCondition):
    def __call__(self) -> bool:
        return self.exchange_rate > self.target
