"""Calculation mixin for Uniswap V4 concentrated-liquidity pools.

Provides the fee surface and the concentrated-liquidity rate leaf that operate
on state held by V4PoolState; the price/rate verbs themselves are inherited from
`PriceLadder`. The swap calculation methods (calculate_tokens_in/out) stay in the
pool class because they're tightly coupled to V4-specific internals (SwapDelta,
Hooks, HookedPoolResult).

See: contract_reference/uniswap/V4/PoolManager.sol
(PoolManager swap/modifyLiquidity, Pool, Hooks, SqrtPriceMath, SwapMath)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

from degenbot.types.price_ladder import PriceLadder
from degenbot.uniswap.v3_functions import exchange_rate_from_sqrt_price_x96

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20 import Erc20Token
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.uniswap.v4_types import UniswapV4PoolState


class UniswapV4PoolCalc(PriceLadder):
    """Pricing and fee methods for Uniswap V4 pools.

    Class variables that V4 subclasses may override:
    - FEE_DENOMINATOR
    """

    # MRO-provided attributes (declared for type checking;
    # actual definitions live on V4PoolState and the pool class)
    _token0: Erc20Token
    _token1: Erc20Token

    # Read-only views provided by the concrete pool class (see
    # UniswapV2PoolCalc for the rationale).
    @property
    def state(self) -> UniswapV4PoolState:
        """The pool's current state (concrete pool provides)."""
        raise NotImplementedError

    @property
    def fee(self) -> int:
        """The pool's fee (concrete pool provides)."""
        raise NotImplementedError

    FEE_DENOMINATOR = 1_000_000

    def _absolute_rate(self, token: Erc20Token, state: AbstractPoolState) -> Fraction:
        """Read the rate out of `sqrt_price_x96` — the concentrated-liquidity encoding.

        Returns:
            The absolute exchange rate as a Fraction.

        """
        sqrt_price_x96 = cast("UniswapV4PoolState", state).sqrt_price_x96
        return (
            exchange_rate_from_sqrt_price_x96(sqrt_price_x96)
            if token == self._token1
            else 1 / exchange_rate_from_sqrt_price_x96(sqrt_price_x96)
        )
