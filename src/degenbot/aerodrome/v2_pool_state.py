"""State mixin for Aerodrome V2 pools.

Holds all data attributes (immutable and mutable) and their properties.
Different from V2PoolState: Aerodrome has a unidirectional fee and a stable flag
instead of directional fees.

No calculation logic — calculations live in AerodromeV2PoolCalc.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20 import Erc20Token


class AerodromeV2PoolState:
    """State for Aerodrome V2 pools with unidirectional fee and stable/volatile mode.

    Matches the Aerodrome V2 contract's data model:
    - token0, token1: the paired ERC-20 tokens (immutable)
    - fee: unidirectional swap fee (immutable)
    - stable: whether the pool uses the Solidly stable invariant (immutable)
    - reserves_token0, reserves_token1: current reserves (mutable via external_update)
    """

    # Immutable — set once at construction
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: Fraction
    _stable: bool

    @property
    def token0(self) -> Erc20Token:
        """Token0."""
        return self._token0

    @property
    def token1(self) -> Erc20Token:
        """Token1."""
        return self._token1

    @property
    def fee(self) -> Fraction:
        """Fee."""
        return self._fee

    @property
    def fee_token0(self) -> Fraction:
        """Return the fee for token0 → token1 swaps (same as fee_token1 for Aerodrome)."""
        return self._fee

    @property
    def fee_token1(self) -> Fraction:
        """Return the fee for token1 → token0 swaps (same as fee_token0 for Aerodrome)."""
        return self._fee

    @property
    def stable(self) -> bool:
        """Determine stable."""
        return self._stable

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """Tokens."""
        return self._token0, self._token1
