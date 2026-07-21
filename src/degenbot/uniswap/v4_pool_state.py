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

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token


class V4PoolState:
    """State for V4-style concentrated-liquidity pools.

    Immutable data set at construction:
    - _token0, _token1: the paired ERC-20 tokens
    - _pool_id: the pool identifier (HexBytes)
    - _pool_key: the V4 PoolKey struct
    - _sparse_liquidity_map: whether full tick data was provided at construction
    """

    # Immutable — set once at construction
    _token0: Erc20Token
    _token1: Erc20Token
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
    def sparse_liquidity_map(self) -> bool:
        """Determine sparse liquidity map."""
        return self._sparse_liquidity_map

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """Tokens."""
        return self._token0, self._token1
