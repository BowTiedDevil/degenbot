"""State mixin for V2-style constant-product pools.

Holds all data attributes (immutable and mutable) and their properties.
No calculation logic — calculations live in UniswapV2PoolCalc.

This mixin is designed to be composed with:
- UniswapV2PoolCalc (calculation methods)
- AbstractLiquidityPool (address, name, simulate_swap)
- PublisherMixin, PoolPickleMixin (infrastructure)

Pools with different state shapes (e.g., Aerodrome's unidirectional fee
and stable flag) define their own state mixin instead of using this one.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20 import Erc20Token


class V2PoolState:
    """State for V2-style constant-product pools with directional fees.

    Matches the Uniswap V2 contract's data model:
    - token0, token1: the paired ERC-20 tokens (immutable)
    - fee_token0, fee_token1: directional swap fees (immutable)
    - reserves_token0, reserves_token1: current reserves (mutable via external_update)
    - block: last update block number (mutable via external_update)

    All attributes are set by the concrete pool's __init__.
    Properties provide read access; mutation happens through external_update().
    """

    # Immutable — set once at construction
    _token0: Erc20Token
    _token1: Erc20Token
    _fee_token0: Fraction
    _fee_token1: Fraction

    @property
    def token0(self) -> Erc20Token:
        return self._token0

    @property
    def token1(self) -> Erc20Token:
        return self._token1

    @property
    def fee_token0(self) -> Fraction:
        return self._fee_token0

    @property
    def fee_token1(self) -> Fraction:
        return self._fee_token1

    @property
    def fee(self) -> tuple[Fraction, Fraction]:
        """Return the pool fees as a tuple (fee_token0, fee_token1)."""
        return (self._fee_token0, self._fee_token1)

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self._token0, self._token1
