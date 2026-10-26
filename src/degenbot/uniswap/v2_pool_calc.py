"""Calculation mixin for Uniswap V2-style constant-product pools.

Provides read-only calculation methods that operate on state held by
V2PoolState (or a compatible state mixin). All methods are non-mutating
and delegate to standalone calculation functions where possible.

Pools with different calculation paths (e.g., Aerodrome's stable mode,
Camelot's k invariant) define their own calc mixin instead of using this one.

See: contract_reference/uniswap/V2/UniswapV2Factory.sol (UniswapV2Pair swap/mint/burn)
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, cast

from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    InvalidSwapInputAmount,
    LiquidityPoolError,
)
from degenbot.uniswap import SwapRefused

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class UniswapV2PoolCalc:
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

    tokens: tuple[Erc20Token, Erc20Token]

    @property
    def update_block(self) -> int:
        """Block of the last state update (concrete pool provides)."""
        raise NotImplementedError

    # Pool handle (set by the concrete UniswapV2Pool companion in MRO).
    # Calc delegation routes the constant-product math through it — the
    # registered reads by direct delegation, the override reads through the
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

        Raises:
            InvalidSwapInputAmount: If token_out_quantity is not positive.
            DegenbotValueError: If token_out is not held by this pool.
            LiquidityPoolError: If the requested output exceeds available reserves.

        """
        if token_out_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if token_out == self._token1:
            zero_for_one = True
        elif token_out == self._token0:
            zero_for_one = False
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_out: {token_out}! This pool holds: {self._token0} {self._token1}",  # ruff:ignore[line-too-long]
            )

        if override_state is None:
            # Delegate the constant-product math to the Rust pool handle. Rust
            # reads reserves + fee under a single read guard (no separate Python
            # state read beforehand — no pump-interleave risk).
            # Rust returns 0 on overdraw (amount_out >= reserve_out); convert to
            # the original Python contract's LiquidityPoolError.
            result: int = self._py_pool.calculate_tokens_in(
                zero_for_one=zero_for_one,
                amount_out=token_out_quantity,
            )
            if result == 0:
                raise LiquidityPoolError(
                    message=f"Requested amount out ({token_out_quantity}) >= pool reserves",
                )
            return result

        # Override path: the exact-output read runs core-side through the
        # swap-simulation door over the override reserves (the door borrows the
        # fee fractions from the registration), so the companion holds no swap
        # math. A refusal is the historical overdraw verdict — the requested
        # output is not available against those reserves.
        # The typed read (ADR-037/t2): a refusal is the overdraw verdict.
        try:
            read = self._py_pool.simulate(
                zero_for_one=zero_for_one,
                amount_specified=token_out_quantity,
                block=self.update_block,
                override_state=override_state,
            )
        except SwapRefused as exc:
            reserves_out = (
                override_state.reserves_token1 if zero_for_one else override_state.reserves_token0
            )
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})",  # ruff:ignore[line-too-long]
            ) from exc
        return int(read.amount0_delta if zero_for_one else read.amount1_delta)

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current pool reserves.

        Returns:
            The expected output token amount.

        Raises:
            InvalidSwapInputAmount: If token_in_quantity is not positive.
            DegenbotValueError: If token_in is not held by this pool.
            LiquidityPoolError: If `uint256` swap math overflows (on-chain revert).

        """
        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if token_in == self._token0:
            zero_for_one = True
        elif token_in == self._token1:
            zero_for_one = False
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_in: {token_in}! Pool holds: {self._token0} {self._token1}",  # ruff:ignore[line-too-long]
            )

        if override_state is None:
            # Delegate the constant-product math to the Rust pool handle. Rust
            # reads reserves + fee under a single read guard (no separate Python
            # state read beforehand — no pump-interleave).
            #
            # cdbc03bb: Rust mirrors on-chain `getAmountOut` SafeMath and
            # reverts on `uint256` overflow (e.g. `amount_in = 2**256 - 1`
            # overflows `amount_in * gamma_numer`); the FFI surfaces that
            # revert as a `ValueError`. Translate it to the original Python
            # contract's `LiquidityPoolError` (mirrors
            # `calculate_tokens_in_from_tokens_out`'s overdraw-sentinel
            # translation below).
            try:
                return cast(
                    "int",
                    self._py_pool.calculate_tokens_out(
                        zero_for_one=zero_for_one,
                        amount_in=token_in_quantity,
                    ),
                )
            except ValueError as e:
                raise LiquidityPoolError(
                    message=(
                        f"Input amount ({token_in_quantity}) overflows "
                        "uint256 swap math (on-chain revert)"
                    ),
                ) from e

        # Override path: the exact-input read runs core-side through the
        # swap-simulation door over the override reserves (the door borrows the
        # fee fractions from the registration), so the companion holds no swap
        # math. The read's signed deltas ARE the answer: the output side is the
        # negative of the token that left the pool.
        # The typed read (ADR-037/t2): a refusal is the overflow verdict.
        try:
            read = self._py_pool.simulate(
                zero_for_one=zero_for_one,
                amount_specified=-token_in_quantity,
                block=self.update_block,
                override_state=override_state,
            )
        except SwapRefused as exc:
            raise LiquidityPoolError(
                message=(
                    f"Input amount ({token_in_quantity}) overflows "
                    "uint256 swap math (on-chain revert)"
                ),
            ) from exc
        return -int(read.amount1_delta if zero_for_one else read.amount0_delta)

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the absolute price for the given token, expressed in units of the other.

        Returns:
            The absolute price as a Fraction.

        """
        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the absolute exchange rate for the given token.

        Returns:
            The absolute exchange rate as a Fraction.

        Raises:
            DegenbotValueError: If the token is not held by this pool.

        """
        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            Fraction(state.reserves_token1, state.reserves_token0)
            if token == self._token1
            else Fraction(state.reserves_token0, state.reserves_token1)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the nominal price for the given token, corrected for decimals.

        Returns:
            The nominal price as a Fraction.

        """
        return 1 / self.get_nominal_exchange_rate(token=token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """Get the nominal exchange rate, corrected for decimal place values.

        Returns:
            The nominal exchange rate as a Fraction.

        """
        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
        )
