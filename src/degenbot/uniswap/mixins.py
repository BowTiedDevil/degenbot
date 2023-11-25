from fractions import Fraction
from typing import Iterator, Optional, Protocol, Set

from ..baseclasses import ArbitrageHelper
from ..erc20_token import Erc20Token
from ..exceptions import ZeroSwapError
from ..logging import logger
from .v2_dataclasses import UniswapV2PoolState


class Subscriber(Protocol):
    """
    Can be notified
    """

    def notify(self, subscriber) -> None:  # pragma: no cover
        ...


class Publisher(Protocol):
    """
    Can publish updates and accept subscriptions
    """

    _subscribers: Set[Subscriber]


class SubscriptionMixin:
    def get_arbitrage_helpers(self: Publisher) -> Iterator[ArbitrageHelper]:
        return (
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, (ArbitrageHelper))
        )

    def subscribe(self: Publisher, subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber) -> None:
        self._subscribers.discard(subscriber)

    def _notify_subscribers(self: Publisher):
        for subscriber in self._subscribers:
            subscriber.notify(self)


class CamelotStablePoolMixin:
    token0: Erc20Token
    token1: Erc20Token
    fee_token0: Fraction
    fee_token1: Fraction
    fee_denominator: int
    reserves_token0: int
    reserves_token1: int
    stable_swap: bool

    def _calculate_tokens_out_from_tokens_in_stable_swap(
        self,
        token_in: "Erc20Token",
        token_in_quantity: int,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        if override_state is not None:
            logger.debug("Reserve overrides applied:")
            logger.debug(f"token0: {override_state.reserves_token0}")
            logger.debug(f"token1: {override_state.reserves_token1}")

        if token_in_quantity <= 0:
            raise ZeroSwapError("token_in_quantity must be positive")

        precision_multiplier_token0: int = 10**self.token0.decimals
        precision_multiplier_token1: int = 10**self.token1.decimals

        def _k(balance_0, balance_1) -> int:
            _x: int = balance_0 * 10**18 // precision_multiplier_token0
            _y: int = balance_1 * 10**18 // precision_multiplier_token1
            _a: int = _x * _y // 10**18
            _b: int = (_x * _x // 10**18) + (_y * _y // 10**18)
            return _a * _b // 10**18  # x^3*y+y^3*x >= k

        def _get_y(x_0: int, xy: int, y: int) -> int:
            for _ in range(255):
                y_prev = y
                k = _f(x_0, y)
                if k < xy:
                    dy = (xy - k) * 10**18 // _d(x_0, y)
                    y = y + dy
                else:
                    dy = (k - xy) * 10**18 // _d(x_0, y)
                    y = y - dy

                if y > y_prev:
                    if y - y_prev <= 1:
                        return y
                else:
                    if y_prev - y <= 1:
                        return y

            return y

        def _f(x_0: int, y: int) -> int:
            return (
                x_0 * (y * y // 10**18 * y // 10**18) // 10**18
                + (x_0 * x_0 // 10**18 * x_0 // 10**18) * y // 10**18
            )

        def _d(x_0: int, y: int) -> int:
            return 3 * x_0 * (y * y // 10**18) // 10**18 + (x_0 * x_0 // 10**18 * x_0 // 10**18)

        # fee_percent is stored as a uint16 in the contract, but as a Fraction
        # in the superclass, so it must be converted.
        #
        # e.g. 0.04% fee = Fraction(1,2500) in the helper, fee = 40 in the
        # contract. To convert, multiply the fraction by the `FEE_DENOMINATOR`,
        # so fee_percent = 1/2500 * 100_000 = 40

        fee_percent = (
            self.fee_token0 if token_in is self.token0 else self.fee_token1
        ) * self.fee_denominator

        reserves_token0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_token1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        # remove fee from amount received
        token_in_quantity -= token_in_quantity * fee_percent // self.fee_denominator
        xy = _k(reserves_token0, reserves_token1)
        reserves_token0 = reserves_token0 * 10**18 // precision_multiplier_token0
        reserves_token1 = reserves_token1 * 10**18 // precision_multiplier_token1
        reserve_a, reserve_b = (
            (reserves_token0, reserves_token1)
            if token_in is self.token0
            else (reserves_token1, reserves_token0)
        )
        token_in_quantity = (
            token_in_quantity * 10**18 // precision_multiplier_token0
            if token_in is self.token0
            else token_in_quantity * 10**18 // precision_multiplier_token1
        )
        y = reserve_b - _get_y(token_in_quantity + reserve_a, xy, reserve_b)

        return (
            y
            * (
                precision_multiplier_token1
                if token_in is self.token0
                else precision_multiplier_token0
            )
            // 10**18
        )
