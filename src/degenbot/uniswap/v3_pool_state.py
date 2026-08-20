"""State mixin for Uniswap V3 concentrated-liquidity pools.

Holds all data attributes (immutable and mutable) and read-only properties
that access them. No calculation logic — calculations live in
UniswapV3PoolCalc.

V3 state is significantly different from V2:
- Concentrated liquidity: tick-based, not reserves-based
- Has tick_bitmap and tick_data for the liquidity distribution
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token


class V3PoolState:
    """State for V3-style concentrated-liquidity pools.

    Immutable data set at construction:
    - _token0, _token1: the paired ERC-20 tokens
    - _fee: pool fee in hundredths of a bip (e.g. 3000 = 0.3%)
    - _tick_spacing: granularity of the tick bitmap

    Mutable state (CL scalars + liquidity distribution):
    - liquidity, sqrt_price_x96, tick, tick_bitmap, tick_data
    """

    # Immutable — set once at construction
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: int
    _tick_spacing: int
    # The CL handle (set by the companion's _from_py_pool); the
    # sparse_liquidity_map property reads Rust coverage through it.
    _py_pool: Any

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
        """Fee."""
        return self._fee

    @property
    def tick_spacing(self) -> int:
        """Tick spacing."""
        return self._tick_spacing

    @property
    def sparse_liquidity_map(self) -> bool:
        """Determine sparse liquidity map.

        Rust ``coverage`` is the fact (T2 FBJTUM: the double-tracked Python
        flag is retired; the V3/V4 state owns the read).

        """
        return self._py_pool.coverage == "sparse"

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """Tokens."""
        return self._token0, self._token1
