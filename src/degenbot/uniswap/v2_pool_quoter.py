"""The V2-family swap quoter: the reserve-pair round trip and Camelot's stable leaf.

Two species, one round trip. The volatile species answers a registered read when
no override state is given (Rust reads reserves + fee under one guard, no Python
state shadow to interleave with a pump update) and the Rust swap-simulation door
when one is; the Camelot solidly-stable species answers its own Solidly leaf,
because the door refuses a ``stable`` identity — its arm would need the
``10**decimals`` scales the V2 registration does not carry.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, NoReturn, Protocol, cast

from degenbot.aerodrome.math import (
    calc_exact_in_stable_camelot as _rs_calc_exact_in_stable_camelot,
)
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import InvalidSwapInputAmount, LiquidityPoolError
from degenbot.uniswap.swap_quote import (
    DoorSwapQuoter,
    SwapRequest,
    overdraw_message,
    overflow_message,
    refuse_as_not_computable,
)
from degenbot.uniswap.v2_types import UniswapV2PoolSimulationResult

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.uniswap import SwapRefused
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class UniswapV2QuoteHost(Protocol):
    """The companion reads the V2 quoter needs.

    The quoter reads its host's *public* surface only (the companion owns these
    over Rust-owned state), so a fake host is enough to test a refusal policy.
    """

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """The pool's token pair, token0 first."""
        ...

    @property
    def state(self) -> UniswapV2PoolState:
        """The pool's current state."""
        ...

    @property
    def update_block(self) -> int:
        """The block of the most recent state update."""
        ...

    @property
    def stable_swap(self) -> bool:
        """Whether this pool is a Camelot solidly-stable species."""
        ...


class CamelotStableSwapQuoter:
    """Camelot's solidly-stable exact-in leaf over the ``degenbot-solidly-math`` core.

    A leaf shape: the arguments are translated into the leaf's terms (token
    index, reserve pair, decimal scales, fee) and the leaf's answer is the
    verdict. There is no door arm and no end-state rebuild on this path — the
    caller owns the state advance.
    """

    def __init__(
        self,
        host: UniswapV2QuoteHost,
        *,
        fee_token0: Fraction,
        fee_token1: Fraction,
    ) -> None:
        """Bind the companion reads and the directional fee fractions."""
        self._host = host
        self._fee_token0 = fee_token0
        self._fee_token1 = fee_token1

    def exact_in(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the output of an exact input over Camelot's ``k_camelot``/``get_y_camelot``.

        Returns:
            The computed output amount.

        """
        token0, token1 = self._host.tokens
        state = override_state if override_state is not None else self._host.state
        fee = self._fee_token0 if token == token0 else self._fee_token1
        return cast(
            "int",
            _rs_calc_exact_in_stable_camelot(
                quantity,
                0 if token == token0 else 1,
                state.reserves_token0,
                state.reserves_token1,
                10**token0.decimals,
                10**token1.decimals,
                fee.numerator,
                fee.denominator,
            ),
        )


class UniswapV2SwapQuoter(DoorSwapQuoter):
    """The V2 family's swap read: a registered read, or the door over an override state."""

    def __init__(
        self,
        host: UniswapV2QuoteHost,
        handle: Pool,
        *,
        fee_token0: Fraction,
        fee_token1: Fraction,
    ) -> None:
        """Bind the companion reads, the handle, and the Camelot leaf's fee fractions."""
        super().__init__(handle)
        self._host = host
        self._camelot = CamelotStableSwapQuoter(
            host,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
        )

    def exact_in(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token output for an exact input.

        Returns:
            The expected output token amount.

        Raises:
            InvalidSwapInputAmount: If ``quantity`` is not positive (volatile
                species only — the solidly-stable leaf takes any amount).
            LiquidityPoolError: If the swap math overflows ``uint256`` (the
                on-chain revert).

        """
        if self._host.stable_swap:
            return self._camelot.exact_in(token, quantity, override_state=override_state)
        if quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount
        zero_for_one = self._direction_in(token)
        if override_state is None:
            # The registered read: Rust reads reserves + fee under a single
            # read guard (no separate Python state read beforehand — no
            # pump-interleave risk). A `uint256` overflow surfaces as the
            # leaf's revert, which the pool reports as its overflow verdict.
            try:
                return cast(
                    "int",
                    self._handle.calculate_tokens_out(
                        zero_for_one=zero_for_one,
                        amount_in=quantity,
                    ),
                )
            except ValueError as e:
                raise LiquidityPoolError(message=overflow_message(quantity)) from e
        # The door read's signed deltas ARE the answer: the output side is the
        # negative of the token that left the pool.
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=-quantity,
                override_state=override_state,
            ),
        )
        return -int(read.amount1_delta if zero_for_one else read.amount0_delta)

    def exact_out(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the required token input for an exact output.

        Returns:
            The required input token amount.

        Raises:
            InvalidSwapInputAmount: If ``quantity`` is not positive.
            LiquidityPoolError: If the requested output exceeds the reserves.

        """
        if quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount
        zero_for_one = self._direction_out(token)
        if override_state is None:
            # The registered read answers 0 on an overdraw (amount_out >=
            # reserve_out); that sentinel is the pool's overdraw verdict.
            result: int = self._handle.calculate_tokens_in(
                zero_for_one=zero_for_one,
                amount_out=quantity,
            )
            if result == 0:
                raise LiquidityPoolError(message=overdraw_message(quantity))
            return result
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=quantity,
                override_state=override_state,
            ),
        )
        return int(read.amount0_delta if zero_for_one else read.amount1_delta)

    def simulate_exact_in(
        self,
        token_in: Erc20Token,
        quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """Simulate an exact-input swap.

        Returns:
            The simulation result with signed deltas and the end reserves.

        Raises:
            DegenbotValueError: If ``token_in`` is not held by this pool.
            InvalidSwapInputAmount: If ``quantity`` is not positive.

        """
        if token_in not in self._host.tokens:
            msg = "token_in is unknown."
            raise DegenbotValueError(message=msg)
        if quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount
        # One atomic snapshot drives the whole simulation: the read's deltas
        # and its end reserves both come from one door read over `state`, so a
        # Rust-side sync_reserves (pump) cannot interleave.
        state = override_state if override_state is not None else self._host.state
        zero_for_one = token_in == self._host.tokens[0]
        if self._host.stable_swap:
            token_out_quantity = self._camelot.exact_in(token_in, quantity, override_state=state)
            token0_delta = -token_out_quantity if zero_for_one is False else quantity
            token1_delta = -token_out_quantity if zero_for_one is True else quantity
            return UniswapV2PoolSimulationResult(
                amount0_delta=token0_delta,
                amount1_delta=token1_delta,
                initial_state=state,
                final_state=dataclasses.replace(
                    state,
                    reserves_token0=state.reserves_token0 + token0_delta,
                    reserves_token1=state.reserves_token1 + token1_delta,
                    block=self._host.update_block if override_state is not None else None,
                ),
            )
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=-quantity,
                override_state=state,
            ),
        )
        assert read.reserves_token0 is not None  # pragma: no cover — reserve-pair arm asserted
        assert read.reserves_token1 is not None  # pragma: no cover — reserve-pair arm asserted
        return UniswapV2PoolSimulationResult(
            amount0_delta=int(read.amount0_delta),
            amount1_delta=int(read.amount1_delta),
            initial_state=state,
            final_state=dataclasses.replace(
                state,
                reserves_token0=read.reserves_token0,
                reserves_token1=read.reserves_token1,
                block=self._host.update_block if override_state is not None else None,
            ),
        )

    def simulate_exact_out(
        self,
        token_out: Erc20Token,
        quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """Simulate an exact-output swap.

        Returns:
            The simulation result with signed deltas and the end reserves.

        Raises:
            DegenbotValueError: If ``token_out`` is not held by this pool.
            InvalidSwapInputAmount: If ``quantity`` is not positive.

        """
        if token_out not in self._host.tokens:
            msg = "token_out is unknown."
            raise DegenbotValueError(message=msg)
        if quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount
        state = override_state if override_state is not None else self._host.state
        zero_for_one = token_out == self._host.tokens[1]
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=quantity,
                override_state=state,
            ),
        )
        assert read.reserves_token0 is not None  # pragma: no cover — reserve-pair arm asserted
        assert read.reserves_token1 is not None  # pragma: no cover — reserve-pair arm asserted
        return UniswapV2PoolSimulationResult(
            amount0_delta=int(read.amount0_delta),
            amount1_delta=int(read.amount1_delta),
            initial_state=state,
            final_state=dataclasses.replace(
                state,
                reserves_token0=read.reserves_token0,
                reserves_token1=read.reserves_token1,
                block=self._host.update_block if override_state is not None else None,
            ),
        )

    def refuse(self, exc: SwapRefused, request: SwapRequest) -> NoReturn:
        """Raise the reserve pair's verdict for a refusal.

        Exact input reads a not-computable refusal as the ``uint256`` overflow
        the on-chain pair reverts with; exact output as an overdraw of the
        output reserve (the same verdict the registered read's ``0`` sentinel
        carries).

        """
        if request.exact_input:
            refuse_as_not_computable(exc, message=overflow_message(request.quantity))
        state = cast(
            "UniswapV2PoolState",
            request.override_state if request.override_state is not None else self._host.state,
        )
        reserves_out = state.reserves_token1 if request.zero_for_one else state.reserves_token0
        refuse_as_not_computable(
            exc,
            message=overdraw_message(request.quantity, reserves_out=reserves_out),
        )

    def _direction_in(self, token: Erc20Token) -> bool:
        """Resolve a token entering the pool to the read's direction.

        Returns:
            True for a token0-in swap.

        Raises:
            DegenbotValueError: If ``token`` is not held by this pool.

        """
        token0, token1 = self._host.tokens
        if token == token0:
            return True
        if token == token1:
            return False
        msg = f"Could not identify token_in: {token}! Pool holds: {token0} {token1}"
        raise DegenbotValueError(message=msg)  # pragma: no cover

    def _direction_out(self, token: Erc20Token) -> bool:
        """Resolve a token leaving the pool to the read's direction.

        Returns:
            True for a token1-out (token0-in) swap.

        Raises:
            DegenbotValueError: If ``token`` is not held by this pool.

        """
        token0, token1 = self._host.tokens
        if token == token1:
            return True
        if token == token0:
            return False
        msg = f"Could not identify token_out: {token}! This pool holds: {token0} {token1}"
        raise DegenbotValueError(message=msg)  # pragma: no cover
