"""
State mixin for Uniswap V3 concentrated-liquidity pools.

Holds all data attributes (immutable and mutable) and read-only properties
that access them. No calculation logic — calculations live in
UniswapV3PoolCalc.

V3 state is significantly different from V2:
- Concentrated liquidity: tick-based, not reserves-based
- State managed by ConcentratedLiquidityStateManager
- Has tick_bitmap and tick_data for the liquidity distribution
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token


class V3PoolState:
    """
    State for V3-style concentrated-liquidity pools.

    Immutable data set at construction:
    - _token0, _token1: the paired ERC-20 tokens
    - _fee: pool fee in hundredths of a bip (e.g. 3000 = 0.3%)
    - _tick_spacing: granularity of the tick bitmap
    - _sparse_liquidity_map: whether full tick data was provided at construction

    Mutable state (via ConcentratedLiquidityStateManager):
    - liquidity, sqrt_price_x96, tick, tick_bitmap, tick_data
    """

    # Immutable — set once at construction
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: int
    _tick_spacing: int
    _sparse_liquidity_map: bool

    @property
    def token0(self) -> Erc20Token:
        """Token0."""
        return self._token0

    @property
    def token1(self) -> Erc20Token:
        """Token1."""
        return self._token1

    @property
    def fee(self) -> int:
        """Return fee."""
        return self._fee

    @property
    def tick_spacing(self) -> int:
        """Return tick spacing."""
        return self._tick_spacing

    @property
    def sparse_liquidity_map(self) -> bool:
        """Determine sparse liquidity map."""
        return self._sparse_liquidity_map

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """Tokens."""
        return self._token0, self._token1
