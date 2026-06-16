"""Calculation mixin for Uniswap V3 concentrated-liquidity pools.

Provides calculation methods that operate on state held by V3PoolState.
All methods are read-only with respect to pool state — they compute
results based on current state but don't modify it.

These methods match the Uniswap V3 contract's math:
- _calculate_swap: core swap computation ported from UniswapV3Pool.sol
- calculate_tokens_out_from_tokens_in / calculate_tokens_in_from_tokens_out: degenbot interface
- get_absolute_price / get_absolute_exchange_rate / get_nominal_price / get_nominal_rate: pricing
- extract_fee: fee extraction for hop building
- _get_tick_ranges / _compute_tick_ranges: bounded-product optimization

See: contract_reference/uniswap/V3/UniswapV3Factory.sol
(UniswapV3Pool swap, Oracle, Tick, TickBitmap, SqrtPriceMath, SwapMath)
"""

from __future__ import annotations

from abc import abstractmethod
from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import EVMRevertError, IncompleteSwap, LiquidityPoolError
from degenbot.uniswap.v3_functions import exchange_rate_from_sqrt_price_x96
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.uniswap.v3_types import UniswapV3PoolState


class UniswapV3PoolCalc:
    """Calculation methods matching the Uniswap V3 contract.

    All methods operate on state held by the concrete class via V3PoolState.

    Class variables that V3 subclasses may override:
    - FEE_DENOMINATOR
    - TICK_STRUCT_TYPES
    - SLOT0_STRUCT_TYPES
    """

    # Attributes provided by sibling/child mixins in the MRO.
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: int

    @property
    @abstractmethod
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """Tokens.

        Returns:
            The token pair held by this pool.

        """
        ...

    @property
    @abstractmethod
    def state(self) -> UniswapV3PoolState:
        """State.

        Returns:
            The current pool state.

        """
        ...

    @abstractmethod
    def _calculate_swap(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        sqrt_price_limit_x96: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> tuple[int, int, int, int, int]: ...

    FEE_DENOMINATOR = 1_000_000

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Calculate expected token OUTPUT for a given INPUT at current pool state.

        Returns:
            The expected output token amount.

        Raises:
            DegenbotValueError: If token_in is not held by this pool.
            IncompleteSwap: If the swap cannot fulfill the full input amount.
            LiquidityPoolError: If the simulated execution reverts.

        """
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not found!")

        zero_for_one = token_in == self._token0

        try:
            amount0_delta, amount1_delta, *_ = self._calculate_swap(
                zero_for_one=zero_for_one,
                amount_specified=token_in_quantity,
                sqrt_price_limit_x96=(MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1),
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            if zero_for_one is True and amount0_delta < token_in_quantity:
                raise IncompleteSwap(amount_in=amount0_delta, amount_out=-amount1_delta)
            if zero_for_one is False and amount1_delta < token_in_quantity:
                raise IncompleteSwap(amount_in=amount1_delta, amount_out=-amount0_delta)

            return -amount1_delta if zero_for_one else -amount0_delta

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Calculate required token INPUT for a target OUTPUT at current pool state.

        Returns:
            The required input token amount.

        Raises:
            DegenbotValueError: If token_out is not held by this pool.
            IncompleteSwap: If the swap cannot fulfill the full output amount.
            LiquidityPoolError: If the simulated execution reverts.

        """
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_out not found!")

        is_zero_for_one = token_out == self._token1

        try:
            amount0_delta, amount1_delta, *_ = self._calculate_swap(
                zero_for_one=is_zero_for_one,
                amount_specified=-token_out_quantity,
                sqrt_price_limit_x96=(
                    MIN_SQRT_RATIO + 1 if is_zero_for_one else MAX_SQRT_RATIO - 1
                ),
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            if is_zero_for_one is True and -amount1_delta < token_out_quantity:
                raise IncompleteSwap(amount_in=amount0_delta, amount_out=-amount1_delta)
            if is_zero_for_one is False and -amount0_delta < token_out_quantity:
                raise IncompleteSwap(amount_in=amount1_delta, amount_out=-amount0_delta)

            return amount0_delta if is_zero_for_one else amount1_delta

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: UniswapV3PoolState | None = None,
    ) -> Fraction:
        """Get the absolute price for the given token, expressed in units of the other.

        Returns:
            The absolute price as a Fraction.

        """
        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV3PoolState | None = None,
    ) -> Fraction:
        """Get the absolute exchange rate for the given token.

        A V3 pool encodes the token1/token0 exchange rate in sqrt_price_x96,
        so it can be directly obtained.

        Returns:
            The absolute exchange rate as a Fraction.

        Raises:
            DegenbotValueError: If the token is not held by this pool.

        """
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
        override_state: UniswapV3PoolState | None = None,
    ) -> Fraction:
        """Get the nominal price, corrected for decimals.

        Returns:
            The nominal price as a Fraction.

        """
        return 1 / self.get_nominal_rate(token, override_state=override_state)

    def get_nominal_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV3PoolState | None = None,
    ) -> Fraction:
        """Get the nominal rate of exchange, corrected for decimal place values.

        Returns:
            The nominal exchange rate as a Fraction.

        """
        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        """Extract fee.

        Returns:
            The fee as a Fraction.

        """
        return Fraction(self._fee, self.FEE_DENOMINATOR)
