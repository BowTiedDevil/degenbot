from abc import abstractmethod
from decimal import Decimal
from fractions import Fraction

from ..erc20_token import Erc20Token
from ..uniswap.v2_liquidity_pool import LiquidityPool
from ..uniswap.v3_liquidity_pool import V3LiquidityPool


class BaseCondition:
    # Derived classes must implement a `__call__` method so the condition can be evaluated as a
    # callable.
    @abstractmethod
    def __call__(self) -> bool: ...


class TokenPriceCondition(BaseCondition):
    def __init__(
        self,
        token: Erc20Token,
        pool: LiquidityPool | V3LiquidityPool,
        target: int | float | Decimal | Fraction,
    ):
        self.token = token
        self.pool = pool
        self.target = target

    @property
    def price(self) -> Fraction:
        return self.pool.get_absolute_price(self.token)

    def update_target(self, target: int | float | Decimal | Fraction) -> None:
        self.target = target


class TokenPriceLessThan(TokenPriceCondition):
    def __call__(self) -> bool:
        return self.price < self.target


class TokenPriceLessThanOrEqual(TokenPriceCondition):
    def __call__(self) -> bool:
        return self.price <= self.target


class TokenPriceGreaterThan(TokenPriceCondition):
    def __call__(self) -> bool:
        return self.price > self.target


class TokenPriceGreaterThanOrEqual(TokenPriceCondition):
    def __call__(self) -> bool:
        return self.price > self.target


class TokenPriceEquals(TokenPriceCondition):
    def __call__(self) -> bool:
        return self.price == self.target
