"""The V3 swap quoter: the concentrated-liquidity round trip over the door.

``simulate_*`` is the one verb behind both public entry points (exact-in passes
a negative ``amount_specified``, exact-out a positive one, and a non-``None``
override state selects the hypothetical arm); ``exact_*`` is the same read with
the pair's verdict applied. The seam owns the sign mapping (deposited ``+``,
sent ``-``) and the end state; the quoter only assembles the result value.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Protocol

from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import IncompleteSwap, InvalidSwapInputAmount
from degenbot.uniswap.swap_quote import DoorSwapQuoter, SwapRequest
from degenbot.uniswap.v3_types import UniswapV3PoolSimulationResult

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.uniswap.v3_types import UniswapV3PoolState


class UniswapV3QuoteHost(Protocol):
    """The companion reads the V3 quoter needs.

    The quoter reads its host's *public* surface only (the companion owns these
    over Rust-owned state), so a fake host is enough to test a refusal policy.
    """

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """The pool's token pair, token0 first."""
        ...

    @property
    def state(self) -> UniswapV3PoolState:
        """The pool's current state."""
        ...

    @property
    def update_block(self) -> int:
        """The block of the most recent state update."""
        ...


class UniswapV3SwapQuoter(DoorSwapQuoter):
    """The V3 family's swap read: one door call, the pair's verdict on top.

    Every V3 read is a door read (the pool's registered read IS the door), so
    the arm choice collapses: an override state selects the hypothetical arm,
    ``None`` the live one. The refusal policy is the door shape's default: a
    concentrated-liquidity refusal IS the pair's verdict (the walk exhausted the
    tick range with input remaining — the real pool's revert-analog), so nothing
    here renames it a reserve-pair overdraw.
    """

    def __init__(self, host: UniswapV3QuoteHost, handle: Pool) -> None:
        """Bind the companion reads and the handle the door reads run through."""
        super().__init__(handle)
        self._host = host

    def simulate_exact_in(
        self,
        token_in: Erc20Token,
        quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: UniswapV3PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Simulate an exact-input swap.

        Returns:
            The simulation result with signed deltas and the end state.

        Raises:
            DegenbotValueError: If ``token_in`` is not held by this pool.

        """
        if token_in not in self._host.tokens:  # pragma: no cover
            msg = f"Unknown token {token_in}"
            raise DegenbotValueError(message=msg)
        return self.simulate(
            zero_for_one=token_in == self._host.tokens[0],
            amount_specified=-quantity,
            override_state=override_state,
            sqrt_price_limit_x96=sqrt_price_limit_x96,
        )

    def simulate_exact_out(
        self,
        token_out: Erc20Token,
        quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: UniswapV3PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Simulate an exact-output swap.

        Returns:
            The simulation result with signed deltas and the end state.

        Raises:
            DegenbotValueError: If ``token_out`` is not held by this pool.

        """
        if token_out not in self._host.tokens:  # pragma: no cover
            msg = f"Unknown token {token_out}"
            raise DegenbotValueError(message=msg)
        return self.simulate(
            zero_for_one=token_out == self._host.tokens[1],
            amount_specified=quantity,
            override_state=override_state,
            sqrt_price_limit_x96=sqrt_price_limit_x96,
        )

    def simulate(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        override_state: UniswapV3PoolState | None,
        sqrt_price_limit_x96: int | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Run one simulation through the Rust swap-simulation seam.

        The one verb behind both public entry points: exact-in passes a
        negative ``amount_specified``, exact-out a positive one, and a
        non-``None`` ``override_state`` selects the hypothetical path. The
        seam owns the sign mapping (deposited +, sent -) and the end state;
        this shell only assembles the result value.

        Returns:
            The simulation result with signed deltas and the end state.

        Raises:
            InvalidSwapInputAmount: On a zero-amount swap.

        """
        initial_state = override_state if override_state is not None else self._host.state
        if amount_specified == 0:
            # The real pool reverts a zero-amount swap; mirror that honestly
            # instead of flattening it into a generic compute failure.
            raise InvalidSwapInputAmount
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=amount_specified,
                override_state=override_state,
            ),
            sqrt_price_limit_x96=sqrt_price_limit_x96,
        )
        assert read.liquidity is not None  # pragma: no cover — concentrated-liquidity arm asserted
        assert read.sqrt_price_x96 is not None  # pragma: no cover — CL arm asserted
        assert read.tick is not None  # pragma: no cover — CL arm asserted
        return UniswapV3PoolSimulationResult(
            amount0_delta=int(read.amount0_delta),
            amount1_delta=int(read.amount1_delta),
            specified_met=read.specified_met,
            initial_state=initial_state,
            final_state=dataclasses.replace(
                initial_state,
                liquidity=read.liquidity,
                sqrt_price_x96=read.sqrt_price_x96,
                tick=read.tick,
                block=self._host.update_block if override_state is None else initial_state.block,
            ),
        )

    def exact_in(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Calculate the expected token output for an exact input.

        Returns:
            The expected output token amount.

        Raises:
            DegenbotValueError: If ``token`` is not held by this pool.
            IncompleteSwap: If the swap cannot fulfill the full input amount.

        """
        if token not in self._host.tokens:  # pragma: no cover
            msg = "token_in not found!"
            raise DegenbotValueError(message=msg)
        zero_for_one = token == self._host.tokens[0]
        result = self.simulate_exact_in(
            token_in=token,
            quantity=quantity,
            override_state=override_state,
        )
        amount0_delta = result.amount0_delta
        amount1_delta = result.amount1_delta
        # The verdict is the seam's (`specified_met`); the payload amounts are
        # the read's own end amounts, never a re-derivation from the deltas.
        if not result.specified_met:
            raise IncompleteSwap(
                amount_in=amount0_delta if zero_for_one else amount1_delta,
                amount_out=-amount1_delta if zero_for_one else -amount0_delta,
            )
        return -amount1_delta if zero_for_one else -amount0_delta

    def exact_out(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Calculate the required token input for an exact output.

        Returns:
            The required input token amount.

        Raises:
            DegenbotValueError: If ``token`` is not held by this pool.
            IncompleteSwap: If the swap cannot fulfill the full output amount.

        """
        if token not in self._host.tokens:  # pragma: no cover
            msg = "token_out not found!"
            raise DegenbotValueError(message=msg)
        is_zero_for_one = token == self._host.tokens[1]
        result = self.simulate_exact_out(
            token_out=token,
            quantity=quantity,
            override_state=override_state,
        )
        amount0_delta = result.amount0_delta
        amount1_delta = result.amount1_delta
        if not result.specified_met:
            raise IncompleteSwap(
                amount_in=amount0_delta if is_zero_for_one else amount1_delta,
                amount_out=-amount1_delta if is_zero_for_one else -amount0_delta,
            )
        return amount0_delta if is_zero_for_one else amount1_delta
