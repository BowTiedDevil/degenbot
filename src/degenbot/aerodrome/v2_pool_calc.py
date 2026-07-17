"""Calculation mixin for Aerodrome V2 pools.

Provides calculation methods that operate on state held by AerodromeV2PoolState.
At construction time, the stable flag determines which underlying calculation
functions are wired — no runtime `if self._stable` dispatch in hot paths.

Two calculation strategies:
- Volatile: standard constant-product (x*y=k)
- Stable: Solidly stable invariant (x^3*y + y^3*x >= k)
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Literal

from degenbot.aerodrome.math import (
    calc_exact_in_stable_solidly as _rs_calc_exact_in_stable_solidly,
)
from degenbot.aerodrome.math import (
    calc_exact_in_volatile as _rs_calc_exact_in_volatile,
)
from degenbot.aerodrome.math import (
    calc_exact_out_stable_solidly as _rs_calc_exact_out_stable_solidly,
)
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    InvalidSwapInputAmount,
    LiquidityPoolError,
)
from degenbot.uniswap.v2_functions import constant_product_calc_exact_out

if TYPE_CHECKING:
    from degenbot.aerodrome.types import AerodromeV2PoolState
    from degenbot.erc20 import Erc20Token


class AerodromeV2PoolCalc:
    """Calculation methods for Aerodrome V2 pools.

    Wires the correct calculation function at construction time based on
    the stable flag. All methods delegate to the pre-bound calculation
    function — no runtime `if self._stable` dispatch.

    The following attributes are set in __init_subclass__ or at construction:
    - _calc_tokens_in_from_tokens_out: bound calc function for exact-out
    """

    # Attributes provided by AerodromeV2PoolState in the MRO
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: Fraction

    # Attributes provided by the concrete pool class in the MRO
    reserves_token0: int
    reserves_token1: int
    state: AerodromeV2PoolState
    tokens: tuple[Erc20Token, Erc20Token]

    def _wire_stable_calculations(self, *, stable: bool) -> None:
        """Wire calculation functions based on the stable flag.

        Called by the pool's __init__ after setting self._stable.
        """
        self._stable_calc_mode = stable
        if stable:
            self._calc_tokens_in_from_tokens_out = self._calc_tokens_in_stable
        else:
            self._calc_tokens_in_from_tokens_out = self._calc_tokens_in_volatile

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """Calculate the required token INPUT for a target OUTPUT at current pool reserves.

        Returns:
            The computed integer value.

        Raises:
            DegenbotValueError: See function documentation.
            InvalidSwapInputAmount: See function documentation.
            LiquidityPoolError: See function documentation.

        """
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
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_out: {token_out}! This pool holds: {self._token0} {self._token1}",  # noqa:E501
            )

        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})",  # noqa:E501
            )

        # Canonical (token0, token1) reserves + decimals needed by the
        # Solidly-stable exact-out leaf (the volatile path ignores them).
        reserves_0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )
        # token_in direction (0 or 1): the OPPOSITE side of token_out.
        token_in: Literal[0, 1] = 0 if token_out == self._token1 else 1
        decimals_0 = 10**self._token0.decimals
        decimals_1 = 10**self._token1.decimals

        return self._calc_tokens_in_from_tokens_out(
            token_out_quantity=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=self._fee,
            reserves_0=reserves_0,
            reserves_1=reserves_1,
            decimals_0=decimals_0,
            decimals_1=decimals_1,
            token_in=token_in,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current pool reserves.

        Returns:
            The computed integer value.

        Raises:
            DegenbotValueError: See function documentation.
            InvalidSwapInputAmount: See function documentation.

        """
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not recognized.")

        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        reserves_0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        token_in_dir: Literal[0, 1] = 0 if token_in == self._token0 else 1

        common_kwargs = {
            "amount_in": token_in_quantity,
            "token_in": token_in_dir,
            "reserves0": reserves_0,
            "reserves1": reserves_1,
            "fee": self._fee,
        }
        if self._stable_calc_mode:
            return self._calc_tokens_out_stable(
                **common_kwargs,
                decimals0=10**self._token0.decimals,
                decimals1=10**self._token1.decimals,
            )
        return self._calc_tokens_out_volatile(**common_kwargs)

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: AerodromeV2PoolState | None = None,
    ) -> Fraction:
        """Get the absolute price for the given token, expressed in units of the other.

        Returns:
            The computed value.

        """
        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: AerodromeV2PoolState | None = None,
    ) -> Fraction:
        """Get the absolute exchange rate for the given token.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: See function documentation.

        """
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
        override_state: AerodromeV2PoolState | None = None,
    ) -> Fraction:
        """Get the nominal price, corrected for decimals.

        Returns:
            The computed value.

        """
        return 1 / self.get_nominal_exchange_rate(token=token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: AerodromeV2PoolState | None = None,
    ) -> Fraction:
        """Get the nominal exchange rate, corrected for decimal place values.

        Returns:
            The computed value.

        """
        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
        )

    def extract_fee(
        self,
        *,
        zero_for_one: bool,  # ruff: ignore[ARG002]
    ) -> Fraction:
        """Extract fee.

        Returns:
            The computed value.

        """
        return self._fee

    # ── Private: calculation strategy methods ──

    @staticmethod
    def _calc_tokens_out_volatile(
        *,
        amount_in: int,
        token_in: Literal[0, 1],
        reserves0: int,
        reserves1: int,
        fee: Fraction,
    ) -> int:
        """Volatile (constant-product) exact-in calculation.

        Returns:
            The computed integer value.

        """
        return _rs_calc_exact_in_volatile(
            amount_in,
            token_in,
            reserves0,
            reserves1,
            fee.numerator,
            fee.denominator,
        )

    @staticmethod
    def _calc_tokens_out_stable(
        *,
        amount_in: int,
        token_in: Literal[0, 1],
        reserves0: int,
        reserves1: int,
        decimals0: int,
        decimals1: int,
        fee: Fraction,
    ) -> int:
        """Stable (Solidly invariant) exact-in calculation.

        Returns:
            The computed integer value.

        """
        return _rs_calc_exact_in_stable_solidly(
            amount_in,
            token_in,
            reserves0,
            reserves1,
            decimals0,
            decimals1,
            fee.numerator,
            fee.denominator,
        )

    @staticmethod
    def _calc_tokens_in_volatile(
        *,
        token_out_quantity: int,
        reserves_in: int,
        reserves_out: int,
        fee: Fraction,
        # Stable-only context (ignored by the volatile path).
        reserves_0: int,  # noqa: ARG004
        reserves_1: int,  # noqa: ARG004
        decimals_0: int,  # noqa: ARG004
        decimals_1: int,  # noqa: ARG004
        token_in: Literal[0, 1],  # noqa: ARG004
    ) -> int:
        """Volatile (constant-product) exact-out calculation.

        Returns:
            The computed integer value.

        """
        return constant_product_calc_exact_out(
            amount_out=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
        )

    @staticmethod
    def _calc_tokens_in_stable(
        *,
        token_out_quantity: int,
        reserves_in: int,  # noqa: ARG004
        reserves_out: int,  # noqa: ARG004
        fee: Fraction,
        reserves_0: int,
        reserves_1: int,
        decimals_0: int,
        decimals_1: int,
        token_in: Literal[0, 1],
    ) -> int:
        """Stable (Solidly invariant) exact-out calculation.

        Delegates to the Rust ``calc_exact_out_stable_solidly`` leaf — the
        inverse of ``calc_exact_in_stable_solidly`` over the Solidly/Aerodrome
        invariant ``f(x0, y) = x0*y*(x0**2 + y**2)``. The post-fee input is
        recovered by inverting the invariant via the solver's symmetry and
        then ceiling-dividing the fee (getAmountIn rounding convention).

        Returns:
            The computed integer value.

        """
        return _rs_calc_exact_out_stable_solidly(
            token_out_quantity,
            token_in,
            reserves_0,
            reserves_1,
            decimals_0,
            decimals_1,
            fee.numerator,
            fee.denominator,
        )
