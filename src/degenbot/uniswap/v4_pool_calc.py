"""
Calculation mixin for Uniswap V4 concentrated-liquidity pools.

Provides pricing and fee calculation methods that operate on state held
by V4PoolState. The swap calculation methods (calculate_tokens_in/out)
stay in the pool class because they're tightly coupled to V4-specific
internals (SwapDelta, Hooks, HookedPoolResult).
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.v3_functions import exchange_rate_from_sqrt_price_x96

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.uniswap.v4_types import UniswapV4PoolState


class UniswapV4PoolCalc:
    """
    Pricing and fee methods for Uniswap V4 pools.

    Class variables that V4 subclasses may override:
    - FEE_DENOMINATOR
    - SLOT0_STRUCT_TYPES
    - TICK_LIQUIDITY_STRUCT_TYPES
    """

    # MRO-provided attributes (declared for type checking;
    # actual definitions live on V4PoolState and the pool class)
    tokens: tuple[Erc20Token, Erc20Token]
    state: UniswapV4PoolState
    _token0: Erc20Token
    _token1: Erc20Token
    fee: int

    FEE_DENOMINATOR = 1_000_000

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """Get the absolute price for the given token, expressed in units of the other."""
        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """Get the absolute exchange rate for the given token."""
        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            exchange_rate_from_sqrt_price_x96(state.sqrt_price_x96)
            if token == self._token1
            else 1 / exchange_rate_from_sqrt_price_x96(state.sqrt_price_x96)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """Get the nominal price, corrected for decimals."""
        return 1 / self.get_nominal_exchange_rate(token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """Get the nominal rate, corrected for decimal place values."""
        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        """Extract fee."""
        return Fraction(self.fee, self.FEE_DENOMINATOR)

        """Extract fee."""
        return None
