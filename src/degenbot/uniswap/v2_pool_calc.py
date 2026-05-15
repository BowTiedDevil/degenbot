"""Calculation mixin for Uniswap V2-style constant-product pools.

Provides read-only calculation methods that operate on state held by
V2PoolState (or a compatible state mixin). All methods are non-mutating
and delegate to standalone calculation functions where possible.

Pools with different calculation paths (e.g., Aerodrome's stable mode,
Camelot's k invariant) define their own calc mixin instead of using this one.
"""

from __future__ import annotations

import dataclasses
from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    InvalidSwapInputAmount,
    LiquidityPoolError,
)
from degenbot.uniswap.v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
)

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class UniswapV2PoolCalc:
    """Calculation methods matching the Uniswap V2 contract.

    All methods operate on state held by the concrete class via V2PoolState.
    Subclasses override as needed for contract-specific differences.

    Class variables with Uniswap V2 defaults:
        FEE: Directional fee rate (default 0.3%)
        RESERVES_STRUCT_TYPES: ABI struct types for reserve decoding
    """

    # These can be overridden by subclasses (e.g., PancakeSwap uses different fee)
    FEE: Fraction = Fraction(3, 1000)
    RESERVES_STRUCT_TYPES: tuple[str, ...] = ("uint112", "uint112")

    def calculate_tokens_in_from_ratio_out(
        self,
        token_in: Erc20Token,
        ratio_absolute: Fraction,
    ) -> int:
        """Calculate the maximum token input for the target output ratio after fees."""
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Token in {token_in} not held by this pool.")

        if token_in == self._token0:
            return max(
                0,
                int(
                    self.reserves_token1 / ratio_absolute
                    - self.reserves_token0 / (1 - self._fee_token0)
                ),
            )

        return max(
            0,
            int(
                self.reserves_token0 / ratio_absolute
                - self.reserves_token1 / (1 - self._fee_token1)
            ),
        )

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the required token INPUT for a target OUTPUT at current pool reserves."""
        if token_out_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if token_out == self._token1:
            reserves_in = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )
            reserves_out = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )
            fee = self._fee_token0
        elif token_out == self._token0:
            reserves_in = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )
            reserves_out = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )
            fee = self._fee_token1
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_out: {token_out}! This pool holds: {self._token0} {self._token1}"  # noqa:E501
            )

        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"  # noqa:E501
            )

        return constant_product_calc_exact_out(
            amount_out=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current pool reserves."""
        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if token_in == self._token0:
            reserves_in = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )
            reserves_out = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )
            fee = self._fee_token0
        elif token_in == self._token1:
            reserves_in = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )
            reserves_out = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )
            fee = self._fee_token1
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_in: {token_in}! Pool holds: {self._token0} {self._token1}"  # noqa:E501
            )

        return constant_product_calc_exact_in(
            amount_in=token_in_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
        )

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the absolute price for the given token, expressed in units of the other."""
        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the absolute exchange rate for the given token."""
        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            Fraction(state.reserves_token1, state.reserves_token0)
            if token == self._token1
            else Fraction(state.reserves_token0, state.reserves_token1)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the nominal price for the given token, corrected for decimals."""
        return 1 / self.get_nominal_exchange_rate(token=token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the nominal exchange rate, corrected for decimal place values."""
        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return self._fee_token0 if zero_for_one else self._fee_token1
