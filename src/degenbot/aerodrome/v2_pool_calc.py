"""Calculation mixin for Aerodrome V2 pools.

Provides calculation methods that operate on state held by AerodromeV2PoolState.

The swap verbs delegate to the pool's `swap_quoter`, which wires the species'
leaves at construction (stable: the Solidly invariant; volatile: constant
product, with the hypothetical arm over the Rust swap-simulation door) — no
runtime `if self._stable` dispatch in hot paths. The pricing verbs come from
`PriceLadder`; the reserve-ratio leaf is below, shared by both species (the
invariant does not change the reserve relation a price reads).

Two calculation strategies:
- Volatile: standard constant-product (x*y=k)
- Stable: Solidly stable invariant (x^3*y + y^3*x >= k)
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, cast

from degenbot.types.price_ladder import PriceLadder

if TYPE_CHECKING:
    from degenbot.aerodrome.types import AerodromeV2PoolState
    from degenbot.aerodrome.v2_pool_quoter import AerodromeV2SwapQuoter
    from degenbot.erc20 import Erc20Token, NativeToken
    from degenbot.types.abstract import AbstractPoolState


class AerodromeV2PoolCalc(PriceLadder):
    """Calculation methods for Aerodrome V2 pools.

    The swap verbs delegate to the per-species quoter the concrete pool builds;
    the pricing verbs read state directly.
    """

    # Attributes provided by AerodromeV2PoolState in the MRO
    _token0: Erc20Token | NativeToken
    _token1: Erc20Token | NativeToken
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

    def _absolute_rate(self, token: Erc20Token, state: AbstractPoolState) -> Fraction:
        """Return the raw reserve ratio — an Aerodrome pool encodes the rate in its reserves.

        Returns:
            The absolute exchange rate as a Fraction.

        """
        reserves = cast("AerodromeV2PoolState", state)
        return (
            Fraction(reserves.reserves_token1, reserves.reserves_token0)
            if token == self._token1
            else Fraction(reserves.reserves_token0, reserves.reserves_token1)
        )
