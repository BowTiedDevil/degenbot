"""The V4 swap quoter: the hooked-pool round trip over the door.

Every V4 read is a door read, so the arm choice is the override state alone.
What V4 adds on top of the V3 round trip is its own verdict set: a swap-modifying
hook makes a hypothetical (override) read an approximation the pool refuses to
present as an answer, and the seam's ``specified_met`` is the family's verdict on
whether the request was met.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, NoReturn, Protocol

from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import HookedPoolResult, IncompleteSwap
from degenbot.uniswap.swap_quote import (
    DoorSwapQuoter,
    SwapRequest,
    refuse_as_not_computable,
    signed_amounts,
)

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.uniswap import SwapRefused
    from degenbot.uniswap.v4_liquidity_pool import Hooks
    from degenbot.uniswap.v4_types import UniswapV4PoolState


class UniswapV4QuoteHost(Protocol):
    """The companion reads the V4 quoter needs.

    The quoter reads its host's *public* surface only (the companion owns these
    over Rust-owned state), so a fake host is enough to test a refusal policy.
    """

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        """The pool's token pair, currency0 first."""
        ...

    @property
    def address(self) -> ChecksummedAddress:
        """The PoolManager address the pool lives in."""
        ...


class UniswapV4SwapQuoter(DoorSwapQuoter):
    """The V4 family's swap read: one door call plus the hooked-pool verdicts."""

    def __init__(
        self,
        host: UniswapV4QuoteHost,
        handle: Pool,
        *,
        swap_hooks: frozenset[Hooks],
    ) -> None:
        """Bind the companion reads, the handle, and the pool's swap-modifying hooks."""
        super().__init__(handle)
        self._host = host
        self._swap_hooks = swap_hooks

    def exact_in(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV4PoolState | None = None,
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
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=-quantity,
                override_state=override_state,
            ),
        )
        amount_in, amount_out = signed_amounts(read, zero_for_one=zero_for_one)
        self._hooks_verdict(amount_in, amount_out, override_state=override_state)
        # The verdict is the seam's (`specified_met`); the payload amounts are
        # the read's own end amounts, never a re-derivation from the deltas.
        if not read.specified_met:
            raise IncompleteSwap(amount_in=amount_in, amount_out=amount_out)
        return amount_out

    def exact_out(
        self,
        token: Erc20Token,
        quantity: int,
        override_state: UniswapV4PoolState | None = None,
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
        zero_for_one = token == self._host.tokens[1]
        read = self.read(
            SwapRequest(
                zero_for_one=zero_for_one,
                amount_specified=quantity,
                override_state=override_state,
            ),
        )
        amount_in, amount_out = signed_amounts(read, zero_for_one=zero_for_one)
        self._hooks_verdict(amount_in, amount_out, override_state=override_state)
        if not read.specified_met:
            raise IncompleteSwap(amount_in=amount_in, amount_out=amount_out)
        return amount_in

    def refuse(self, exc: SwapRefused, request: SwapRequest) -> NoReturn:
        """Raise the hooked pool's verdict for a refusal.

        A not-computable V4 refusal is the pool's own "the execution could not
        be computed" — a revert-analog that does not claim an overdraw.

        """
        refuse_as_not_computable(exc, message=self._could_not_compute(request))

    def _could_not_compute(self, request: SwapRequest) -> str:
        """Name the request a not-computable read could not answer.

        Returns:
            The verdict message.

        """
        side = "amount_in" if request.exact_input else "amount_out"
        return (
            f"Simulated execution could not compute. pool={self._host.address} "
            f"zfo={request.zero_for_one} {side}={request.quantity}"
        )

    def _hooks_verdict(
        self,
        amount_in: int,
        amount_out: int,
        *,
        override_state: UniswapV4PoolState | None,
    ) -> None:
        """Refuse a hypothetical read whose swap hooks a simulation cannot model.

        A live read is the pool's own state; a hypothetical one is only an
        approximation when nothing runs between the read and the state, so an
        active swap-modifying hook is the one case the quoter must refuse.

        Raises:
            HookedPoolResult: If the pool's swap hooks are active on the
                hypothetical arm.

        """
        if override_state is not None and self._swap_hooks:
            raise HookedPoolResult(
                amount_in=amount_in,
                amount_out=amount_out,
                hooks=set(self._swap_hooks),
            )
