"""
Calculation mixin for Camelot V2 pools.

Camelot pools can operate in volatile (constant-product) or stable mode,
determined at construction time. This mixin wires the correct calculation
strategy — no runtime `if self.stable_swap` dispatch in hot paths.

Camelot's stable mode uses its own k and get_y functions (k_camelot,
get_y_camelot) instead of the standard Solidly ones.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.calculations.camelot import get_y_camelot, k_camelot
from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.v2_pool_calc import UniswapV2PoolCalc

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20 import Erc20Token
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class CamelotPoolCalc(UniswapV2PoolCalc):
    """
    Camelot calculations — extends V2PoolCalc with stable swap support.

    Adds:
    - fee_denominator: Camelot uses integer fee values with a denominator
    - stable_swap: whether to use Camelot's stable invariant
    - _calculate_tokens_out_from_tokens_in_stable_swap: Camelot-specific
      stable swap calculation

    The if self.stable_swap dispatch in calculate_tokens_out_from_tokens_in
    is replaced by pre-wired _calc_tokens_out_fn set at construction time.
    """

    token0: Erc20Token
    token1: Erc20Token
    fee_denominator: int
    fee_token0: Fraction
    fee_token1: Fraction
    stable_swap: bool

    def _wire_camelot_calculations(self, *, stable_swap: bool) -> None:
        """Wire calculation functions based on the stable_swap flag."""
        self.stable_swap = stable_swap
        if stable_swap:
            self._calc_tokens_out_fn = self._calculate_tokens_out_from_tokens_in_stable_swap
        else:
            self._calc_tokens_out_fn = super().calculate_tokens_out_from_tokens_in

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate expected token output — delegates to pre-wired strategy."""
        return self._calc_tokens_out_fn(
            token_in=token_in,
            token_in_quantity=token_in_quantity,
            override_state=override_state,
        )

    def _calculate_tokens_out_from_tokens_in_stable_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Camelot-specific stable swap calculation."""
        if override_state is not None:  # pragma: no cover
            pass  # logger.debug(f"State overrides applied: {override_state}")

        if token_in_quantity <= 0:  # pragma: no cover
            raise DegenbotValueError(message="token_in_quantity must be positive")

        precision_multiplier_token0: int = 10**self.token0.decimals
        precision_multiplier_token1: int = 10**self.token1.decimals

        fee_percent = self.fee_denominator * (
            self.fee_token0 if token_in == self.token0 else self.fee_token1
        )

        reserves_token0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_token1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        # Remove fee from amount received
        token_in_quantity -= token_in_quantity * fee_percent // self.fee_denominator
        xy = k_camelot(
            balance_0=reserves_token0,
            balance_1=reserves_token1,
            decimals_0=precision_multiplier_token0,
            decimals_1=precision_multiplier_token1,
        )
        reserves_token0 = reserves_token0 * 10**18 // precision_multiplier_token0
        reserves_token1 = reserves_token1 * 10**18 // precision_multiplier_token1
        reserve_a, reserve_b = (
            (reserves_token0, reserves_token1)
            if token_in == self.token0
            else (reserves_token1, reserves_token0)
        )
        token_in_quantity = (
            token_in_quantity * 10**18 // precision_multiplier_token0
            if token_in == self.token0
            else token_in_quantity * 10**18 // precision_multiplier_token1
        )
        y = reserve_b - get_y_camelot(token_in_quantity + reserve_a, xy, reserve_b)

        return (
            y
            * (
                precision_multiplier_token1
                if token_in == self.token0
                else precision_multiplier_token0
            )
            // 10**18
        )
