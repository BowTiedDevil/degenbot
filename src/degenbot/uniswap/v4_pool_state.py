"""State mixin for Uniswap V4 concentrated-liquidity pools.

Holds all data attributes (immutable and mutable) and read-only properties
that access them. No calculation logic — calculations live in
UniswapV4PoolCalc.

V4-specific state:
- pool_id: the pool identifier tracked by the V4 PoolManager contract
- pool_key: the V4 PoolKey struct (currency0, currency1, fee, tick_spacing, hooks)
- hook_address: the hooks contract address
- active_hooks: which hook flags are set
- protocol_fee: directional protocol fees
- lp_fee: the LP fee
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.uniswap.concentrated_liquidity_pool_state import ConcentratedLiquidityPoolState

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token, NativeToken


class V4PoolState(ConcentratedLiquidityPoolState):
    """State for V4-style concentrated-liquidity pools.

    Immutable data set at construction:
    - _token0, _token1: the paired token companions (an ERC-20 or the
      conceptual native token on a V4 pair — ADR-097)
    - _pool_id: the pool identifier (bytes)
    - _pool_key: the V4 PoolKey struct
    (sparseness is no longer stored here — Rust coverage is the fact)
    """

    # Immutable — set once at construction
    _token0: Erc20Token | NativeToken
    _token1: Erc20Token | NativeToken

    @property
    def token0(self) -> Erc20Token | NativeToken:
        """Token0."""
        return self._token0

    @property
    def token1(self) -> Erc20Token | NativeToken:
        """Token1."""
        return self._token1

    @property
    def tokens(self) -> tuple[Erc20Token | NativeToken, Erc20Token | NativeToken]:
        """Tokens."""
        return self._token0, self._token1
