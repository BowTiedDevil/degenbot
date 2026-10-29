"""Calculation mixin for Aerodrome V2 pools.

Provides calculation methods that operate on state held by AerodromeV2PoolState.

The swap verbs delegate to the pool's `swap_quoter`, which wires the species'
leaves at construction (stable: the Solidly invariant; volatile: constant
product, with the hypothetical arm over the Rust swap-simulation door) — no
runtime `if self._stable` dispatch in hot paths. The pricing verbs below are pure
functions over state and stay here.

Two calculation strategies:
- Volatile: standard constant-product (x*y=k)
- Stable: Solidly stable invariant (x^3*y + y^3*x >= k)
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.exceptions import DegenbotValueError

if TYPE_CHECKING:
    from degenbot.aerodrome.types import AerodromeV2PoolState
    from degenbot.aerodrome.v2_pool_quoter import AerodromeV2SwapQuoter
    from degenbot.erc20 import Erc20Token


class AerodromeV2PoolCalc:
    """Calculation methods for Aerodrome V2 pools.

    The swap verbs delegate to the per-species quoter the concrete pool builds;
    the pricing verbs read state directly.
    """

    # Attributes provided by AerodromeV2PoolState in the MRO
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: Fraction

    # Read-only views provided by the concrete pool class in the MRO (see
    # UniswapV2PoolCalc for the rationale).
    @property
    def reserves_token0(self) -> int:
        """Current token0 reserve (concrete pool provides)."""
        raise NotImplementedError

    @property
    def reserves_token1(self) -> int:
        """Current token1 reserve (concrete pool provides)."""
        raise NotImplementedError

    @property
    def state(self) -> AerodromeV2PoolState:
        """The pool's current state (concrete pool provides)."""
        raise NotImplementedError

    tokens: tuple[Erc20Token, Erc20Token]

    @property
    def update_block(self) -> int:
        """Block of the last state update (concrete pool provides)."""
        raise NotImplementedError

    @property
    def swap_quoter(self) -> AerodromeV2SwapQuoter:
        """The pool's swap quoter (concrete pool provides)."""
        raise NotImplementedError

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """Calculate the required token INPUT for a target OUTPUT at current pool reserves.

        Returns:
            The required input token amount.

        """
        return self.swap_quoter.exact_out(
            token_out,
            token_out_quantity,
            override_state=override_state,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current pool reserves.

        Returns:
            The expected output token amount.

        """
        return self.swap_quoter.exact_in(
            token_in,
            token_in_quantity,
            override_state=override_state,
        )

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
            DegenbotValueError: If the token is not held by this pool.

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
