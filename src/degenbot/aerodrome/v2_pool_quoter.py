"""The Aerodrome V2 swap quoter: the volatile door arm and the Solidly leaves.

Aerodrome's two species answer different reads. The volatile species' hypothetical
arm runs core-side through the swap-simulation door (the door borrows the fee
from the registration, never re-sent); everything else — the registered volatile
read and both stable reads — is a Solidly leaf driven from the reserves,
decimals, and fee this companion already holds. The exact-output read has no
door arm at all: the family's design answers exact-input over the door, so
exact-output resolves its reserves and stays on the leaf.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Literal, NoReturn, Protocol, cast

from degenbot.aerodrome.math import (
    calc_exact_in_stable_solidly as _rs_calc_exact_in_stable_solidly,
)
from degenbot.aerodrome.math import (
    calc_exact_in_volatile as _rs_calc_exact_in_volatile,
)
from degenbot.aerodrome.math import (
    calc_exact_out_stable_solidly as _rs_calc_exact_out_stable_solidly,
)
from degenbot.aerodrome.math import calc_exact_out_v2 as _rs_calc_exact_out_volatile
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import InvalidSwapInputAmount, LiquidityPoolError
from degenbot.uniswap.swap_quote import (
    DoorSwapQuoter,
    SwapRequest,
    overdraw_message,
    overflow_message,
    refuse_as_not_computable,
)

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.aerodrome.types import AerodromeV2PoolState
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.uniswap import SwapRefused


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class _ExactInRequest:
    """Resolved context shared by the volatile and stable exact-in leaves.

    The volatile leaf reads only the constant-product fields; the stable-only
    context travels with the request so the wired leaf has one uniform shape.
    """

    amount_in: int
    token_in: Literal[0, 1]
    reserves0: int
    reserves1: int
    decimals0: int
    decimals1: int
    fee: Fraction


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class _ExactOutRequest:
    """Resolved context shared by the volatile and stable exact-out leaves.

    The volatile leaf reads only the constant-product fields; the stable-only
    context travels with the request so the wired leaf has one uniform shape.
    """

    token_out_quantity: int
    reserves_in: int
    reserves_out: int
    fee: Fraction
    reserves_0: int
    reserves_1: int
    decimals_0: int
    decimals_1: int
    token_in: Literal[0, 1]


class AerodromeV2QuoteHost(Protocol):
    """The companion reads the Aerodrome quoter needs."""

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """The pool's token pair, token0 first."""
        ...

    @property
    def state(self) -> AerodromeV2PoolState:
        """The pool's current state."""
        ...


class AerodromeV2SwapQuoter(DoorSwapQuoter):
    """The Aerodrome V2 family's swap read: one wired leaf per species, plus the door arm."""

    def __init__(
        self,
        host: AerodromeV2QuoteHost,
        handle: Pool,
        *,
        stable: bool,
        fee: Fraction,
    ) -> None:
        """Bind the companion reads, the handle, the species, and the fee.

        The species decides which pair of leaves this quoter drives — wired here,
        once, so no read re-decides it.
        """
        super().__init__(handle)
        self._host = host
        self._stable = stable
        self._fee = fee
        if stable:
            self._tokens_out_leaf = _leaf_tokens_out_stable
            self._tokens_in_leaf = _leaf_tokens_in_stable
        else:
            self._tokens_out_leaf = _leaf_tokens_out_volatile
            self._tokens_in_leaf = _leaf_tokens_in_volatile

    def exact_in(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token output for an exact input.

        Returns:
            The expected output token amount.

        Raises:
            DegenbotValueError: If ``token`` is not held by this pool.
            InvalidSwapInputAmount: If ``quantity`` is not positive.

        """
        if token not in self._host.tokens:  # pragma: no cover
            msg = "token_in not recognized."
            raise DegenbotValueError(message=msg)
        if quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount
        token_in_dir: Literal[0, 1] = 0 if token == self._host.tokens[0] else 1

        if override_state is not None and not self._stable:
            # The volatile hypothetical read: constant product over the override
            # reserves, run core-side. The stable species keeps its Solidly leaf
            # — the door refuses a `stable` identity, whose read arm owns that
            # invariant.
            read = self.read(
                SwapRequest(
                    zero_for_one=token_in_dir == 0,
                    amount_specified=-quantity,
                    override_state=override_state,
                ),
            )
            return -int(read.amount1_delta if token_in_dir == 0 else read.amount0_delta)

        reserves_0, reserves_1 = self._reserves(override_state)
        return self._tokens_out_leaf(
            ctx=_ExactInRequest(
                amount_in=quantity,
                token_in=token_in_dir,
                reserves0=reserves_0,
                reserves1=reserves_1,
                decimals0=10 ** self._host.tokens[0].decimals,
                decimals1=10 ** self._host.tokens[1].decimals,
                fee=self._fee,
            ),
        )

    def exact_out(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: AerodromeV2PoolState | None = None,
    ) -> int:
        """Calculate the required token input for an exact output.

        There is no door arm on this side: the family's reserve-pair design
        answers exact-input over the door, so exact-output resolves its reserves
        and calls the wired leaf.

        Returns:
            The required input token amount.

        Raises:
            DegenbotValueError: If ``token`` is not held by this pool.
            InvalidSwapInputAmount: If ``quantity`` is not positive.
            LiquidityPoolError: If the requested output exceeds the reserves.

        """
        if quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount
        token0, token1 = self._host.tokens
        if token == token1:
            reserves_0, reserves_1 = self._reserves(override_state)
            reserves_in, reserves_out = reserves_0, reserves_1
        elif token == token0:
            reserves_0, reserves_1 = self._reserves(override_state)
            reserves_in, reserves_out = reserves_1, reserves_0
        else:  # pragma: no cover
            msg = f"Could not identify token_out: {token}! This pool holds: {token0} {token1}"
            raise DegenbotValueError(message=msg)

        # The reserve pair reverts an output at (or past) its own reserve; the
        # Python side owns that check for this family (the leaves do not).
        if quantity > reserves_out - 1:
            raise LiquidityPoolError(message=overdraw_message(quantity, reserves_out=reserves_out))

        # Canonical (token0, token1) reserves + decimals for the stable leaf (the
        # volatile leaf reads only the swap-direction pair).
        token_in: Literal[0, 1] = 0 if token == token1 else 1
        return self._tokens_in_leaf(
            ctx=_ExactOutRequest(
                token_out_quantity=quantity,
                reserves_in=reserves_in,
                reserves_out=reserves_out,
                fee=self._fee,
                reserves_0=reserves_0,
                reserves_1=reserves_1,
                decimals_0=10**token0.decimals,
                decimals_1=10**token1.decimals,
                token_in=token_in,
            ),
        )

    def refuse(self, exc: SwapRefused, request: SwapRequest) -> NoReturn:  # ruff:ignore[no-self-use]
        """Raise the reserve pair's verdict for a refusal from the volatile door arm.

        The only door read this family takes is an exact-input one, so a
        not-computable refusal is the ``uint256`` overflow the on-chain pair
        reverts with.

        """
        refuse_as_not_computable(exc, message=overflow_message(request.quantity))

    def _reserves(
        self,
        override_state: AerodromeV2PoolState | None,
    ) -> tuple[int, int]:
        """Resolve the (token0, token1) reserves this read runs against.

        Returns:
            The reserve pair — the hypothetical one when given, else the pool's.

        """
        state = override_state if override_state is not None else self._host.state
        return state.reserves_token0, state.reserves_token1


def _leaf_tokens_out_volatile(*, ctx: _ExactInRequest) -> int:
    """Volatile (constant-product) exact-in calculation.

    Returns:
        The computed output amount.

    """
    return cast(
        "int",
        _rs_calc_exact_in_volatile(
            ctx.amount_in,
            ctx.token_in,
            ctx.reserves0,
            ctx.reserves1,
            ctx.fee.numerator,
            ctx.fee.denominator,
        ),
    )


def _leaf_tokens_out_stable(*, ctx: _ExactInRequest) -> int:
    """Stable (Solidly invariant) exact-in calculation.

    Returns:
        The computed output amount.

    """
    return cast(
        "int",
        _rs_calc_exact_in_stable_solidly(
            ctx.amount_in,
            ctx.token_in,
            ctx.reserves0,
            ctx.reserves1,
            ctx.decimals0,
            ctx.decimals1,
            ctx.fee.numerator,
            ctx.fee.denominator,
        ),
    )


def _leaf_tokens_in_volatile(*, ctx: _ExactOutRequest) -> int:
    """Volatile (constant-product) exact-out calculation.

    Returns:
        The computed input amount.

    """
    return cast(
        "int",
        _rs_calc_exact_out_volatile(
            ctx.reserves_in,
            ctx.reserves_out,
            ctx.token_out_quantity,
            ctx.fee.numerator,
            ctx.fee.denominator,
        ),
    )


def _leaf_tokens_in_stable(*, ctx: _ExactOutRequest) -> int:
    """Stable (Solidly invariant) exact-out calculation.

    Delegates to the Rust ``calc_exact_out_stable_solidly`` leaf — the inverse
    of ``calc_exact_in_stable_solidly`` over the Solidly/Aerodrome invariant
    ``f(x0, y) = x0*y*(x0**2 + y**2)``. The post-fee input is recovered by
    inverting the invariant via the solver's symmetry and then ceiling-dividing
    the fee (getAmountIn rounding convention).

    Returns:
        The computed input amount.

    """
    return cast(
        "int",
        _rs_calc_exact_out_stable_solidly(
            ctx.token_out_quantity,
            ctx.token_in,
            ctx.reserves_0,
            ctx.reserves_1,
            ctx.decimals_0,
            ctx.decimals_1,
            ctx.fee.numerator,
            ctx.fee.denominator,
        ),
    )
