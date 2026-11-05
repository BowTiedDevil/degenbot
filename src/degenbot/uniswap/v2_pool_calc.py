"""Calculation mixin for Uniswap V2-style constant-product pools.

Provides read-only calculation methods that operate on state held by
V2PoolState (or a compatible state mixin). All methods are non-mutating.

The swap verbs (calculate_tokens_out_from_tokens_in /
calculate_tokens_in_from_tokens_out) delegate to the pool's `swap_quoter`; the
pricing verbs come from `PriceLadder`, and the one leaf that differs per family
— how the state encodes the exchange rate — is below.

Pools with different calculation paths (e.g., Aerodrome's stable mode,
Camelot's k invariant) answer through their own quoter instead of using this
one.

See: contract_reference/uniswap/V2/UniswapV2Factory.sol (UniswapV2Pair swap/mint/burn)
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, cast

from degenbot.types.price_ladder import PriceLadder

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.uniswap.v2_pool_quoter import UniswapV2SwapQuoter
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class UniswapV2PoolCalc(PriceLadder):
    """Calculation methods matching the Uniswap V2 contract.

    All methods operate on state held by the concrete class via V2PoolState.
    Subclasses override as needed for contract-specific differences.

    Class variables with Uniswap V2 defaults:
        FEE: Directional fee rate (default 0.3%)
    """

    # Attributes provided by V2PoolState in the MRO
    _token0: Erc20Token
    _token1: Erc20Token
    _fee_token0: Fraction
    _fee_token1: Fraction

    # Read-only views provided by the concrete pool class in the MRO. Declared
    # as properties because the concrete companions expose them read-only over
    # Rust-owned state; a writable annotation would make every concrete
    # read-only property an invalid override.
    @property
    def reserves_token0(self) -> int:
        """Current token0 reserve (concrete pool provides)."""
        raise NotImplementedError

    @property
    def reserves_token1(self) -> int:
        """Current token1 reserve (concrete pool provides)."""
        raise NotImplementedError

    @property
    def state(self) -> UniswapV2PoolState:
        """The pool's current state (concrete pool provides)."""
        raise NotImplementedError

    @property
    def update_block(self) -> int:
        """Block of the last state update (concrete pool provides)."""
        raise NotImplementedError

    @property
    def swap_quoter(self) -> UniswapV2SwapQuoter:
        """The pool's swap quoter (concrete pool provides)."""
        raise NotImplementedError

    # Pool handle (set by the concrete UniswapV2Pool companion in MRO). The
    # swap quoter reads the pool's constant-product state and math through it —
    # the registered reads by direct delegation, the override reads through the
    # swap-simulation door.
    _py_pool: Pool

    # These can be overridden by subclasses (e.g., PancakeSwap uses different fee)
    FEE: Fraction = Fraction(3, 1000)

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
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
        override_state: UniswapV2PoolState | None = None,
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
        """Return the raw reserve ratio — a V2 pool encodes the rate in its reserves.

        Returns:
            The absolute exchange rate as a Fraction.

        """
        reserves = cast("UniswapV2PoolState", state)
        return (
            Fraction(reserves.reserves_token1, reserves.reserves_token0)
            if token == self._token1
            else Fraction(reserves.reserves_token0, reserves.reserves_token1)
        )
