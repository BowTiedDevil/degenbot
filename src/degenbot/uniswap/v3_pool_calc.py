"""Calculation mixin for Uniswap V3 concentrated-liquidity pools.

Provides calculation methods that operate on state held by V3PoolState.
All methods are read-only with respect to pool state — they compute
results based on current state but don't modify it.

The swap verbs (calculate_tokens_out_from_tokens_in /
calculate_tokens_in_from_tokens_out) delegate to the pool's `swap_quoter`; the
pricing verbs come from `PriceLadder`, and the one leaf that differs per family
— how the state encodes the exchange rate — is below.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol
(UniswapV3Pool swap, Oracle, Tick, TickBitmap, SqrtPriceMath, SwapMath)
"""

from __future__ import annotations

from abc import abstractmethod
from typing import TYPE_CHECKING, cast

from degenbot.types.price_ladder import PriceLadder
from degenbot.uniswap.v3_functions import exchange_rate_from_sqrt_price_x96

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20 import Erc20Token, NativeToken
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.uniswap.v3_pool_quoter import UniswapV3SwapQuoter
    from degenbot.uniswap.v3_types import UniswapV3PoolSimulationResult, UniswapV3PoolState


class UniswapV3PoolCalc(PriceLadder):
    """Calculation methods matching the Uniswap V3 contract.

    All methods operate on state held by the concrete class via V3PoolState.

    Class variables that V3 subclasses may override:
    - FEE_DENOMINATOR
    - SLOT0_STRUCT_TYPES
    """

    # Attributes provided by sibling/child mixins in the MRO.
    _token0: Erc20Token | NativeToken
    _token1: Erc20Token | NativeToken
    _fee: int

    @property
    @abstractmethod
    def tokens(self) -> tuple[Erc20Token | NativeToken, Erc20Token | NativeToken]:
        """Tokens.

        Returns:
            The token pair held by this pool.

        """
        ...

    @property
    @abstractmethod
    def state(self) -> UniswapV3PoolState:
        """State.

        Returns:
            The current pool state.

        """
        ...

    @abstractmethod
    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: UniswapV3PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Exact-input swap simulation (routed to the Rust seam)."""
        ...

    @abstractmethod
    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: UniswapV3PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Exact-output swap simulation (routed to the Rust seam)."""
        ...

    @property
    def swap_quoter(self) -> UniswapV3SwapQuoter:
        """The pool's swap quoter (concrete pool provides)."""
        raise NotImplementedError

    FEE_DENOMINATOR = 1_000_000

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Calculate expected token OUTPUT for a given INPUT at current pool state.

        Returns:
            The expected output token amount.

        """
        return self.swap_quoter.exact_in(
            token_in,
            token_in_quantity,
            override_state=override_state,
        )

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Calculate required token INPUT for a target OUTPUT at current pool state.

        Returns:
            The required input token amount.

        """
        return self.swap_quoter.exact_out(
            token_out,
            token_out_quantity,
            override_state=override_state,
        )

    def _absolute_rate(self, token: Erc20Token, state: AbstractPoolState) -> Fraction:
        """Read the rate out of `sqrt_price_x96` — the concentrated-liquidity encoding.

        Returns:
            The absolute exchange rate as a Fraction.

        """
        sqrt_price_x96 = cast("UniswapV3PoolState", state).sqrt_price_x96
        return (
            exchange_rate_from_sqrt_price_x96(sqrt_price_x96)
            if token == self._token1
            else 1 / exchange_rate_from_sqrt_price_x96(sqrt_price_x96)
        )
