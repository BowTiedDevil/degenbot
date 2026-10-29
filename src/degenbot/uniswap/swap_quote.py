"""The swap-quote shapes: how a companion answers a swap read.

A companion's swap verbs (``calculate_tokens_out_from_tokens_in`` and friends)
are one round trip in every family — a companion-level request in, the family's
verdict out — and the round trip has exactly two shapes:

- **Door shape**: the family's read runs through the Rust swap-simulation door
  (``Pool.simulate``). The round trip is then the whole way: companion concepts
  to seam vocabulary (``zero_for_one``, signed ``amount_specified``), the arm
  choice between a registered read and the door over an override state, the
  refusal fork, delta decoding, and the post-state rebuild.
- **Leaf shape**: the family's read is a pure Rust math leaf driven from Python
  facts (Balancer's pair reads, Curve's ``dy``, Camelot's solidly-stable leaf).
  Nothing is rebuilt and no door refusal exists: the quoter translates
  arguments into the leaf's terms and maps the leaf's verdict.

:class:`DoorSwapQuoter` is the door shape's one home for the seam call and the
refusal fork. Everything else about a family's round trip — its arm choice, its
verdicts, its state rebuild — belongs to that family's quoter, because the arm
choices genuinely differ: V2 answers a registered read when no override state is
given and the door when one is, V3/V4 always answer the door, and Aerodrome's
volatile species answers a Solidly leaf until an override state arrives.

The kind-mapping home stays :func:`degenbot.uniswap.swap_refusal_to_error`: a
quoter owns its *verdict policy* over that mapping, never a second copy of it.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, NoReturn

from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.uniswap import SwapRefusalKind, SwapRefused, swap_refusal_to_error

if TYPE_CHECKING:
    from degenbot.types import Pool
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.uniswap import SwapRead


@dataclasses.dataclass(slots=True, frozen=True)
class SwapRequest:
    """One swap read stated in the seam's vocabulary.

    ``amount_specified`` is signed from the user's perspective — negative is
    exact input, positive is exact output — and ``override_state`` is the
    hypothetical state to read against (``None`` reads the pool's own state).
    The sign convention is the seam's, so a quoter never re-derives it.
    """

    zero_for_one: bool
    amount_specified: int
    override_state: AbstractPoolState | None = None

    @property
    def exact_input(self) -> bool:
        """Whether the request names its input side.

        Returns:
            True for an exact-input request, False for exact-output.

        """
        return self.amount_specified < 0

    @property
    def quantity(self) -> int:
        """The requested amount, unsigned.

        Returns:
            The magnitude of the request.

        """
        return abs(self.amount_specified)


def signed_amounts(read: SwapRead, *, zero_for_one: bool) -> tuple[int, int]:
    """Split a computed read's signed deltas into ``(amount_in, amount_out)``.

    The seam returns deposited-``+``/sent-``-`` per-token deltas; the input side
    of the swap reads positive and the output side negative.

    Returns:
        The ``(amount_in, amount_out)`` unsigned magnitudes.

    """
    amount0_delta = int(read.amount0_delta)
    amount1_delta = int(read.amount1_delta)
    if zero_for_one:
        return amount0_delta, -amount1_delta
    return amount1_delta, -amount0_delta


def overflow_message(quantity: int) -> str:
    """Render the constant-product overflow verdict's one wording.

    The Rust leaf mirrors on-chain ``getAmountOut`` SafeMath and reverts on a
    ``uint256`` overflow; a reserve-pair pool reports that to its callers as a
    ``LiquidityPoolError`` naming the request.

    Returns:
        The verdict message.

    """
    return f"Input amount ({quantity}) overflows uint256 swap math (on-chain revert)"


def overdraw_message(quantity: int, *, reserves_out: int | None = None) -> str:
    """Render the reserve-pair overdraw verdict's one wording.

    A registered read is one read guard with no Python state shadow, so it
    never learns the reserve it compared against; an override arm has the
    hypothetical state in hand and names it.

    Returns:
        The verdict message.

    """
    named_reserve = "" if reserves_out is None else f" ({reserves_out})"
    return f"Requested amount out ({quantity}) >= pool reserves{named_reserve}"


def refuse_as_not_computable(exc: SwapRefused, *, message: str) -> NoReturn:
    """Raise a family's not-computable verdict for a refusal from the door.

    The policy a family owns is only *which verdict* a not-computable refusal
    means: the reserve-pair families read it as the historical overdraw /
    overflow ``LiquidityPoolError``, where a family whose pool reverts the
    analog (V3) renders the seam's ``IncompleteSwap`` instead. A fetch-arm
    refusal never was about swap geometry — it renders through the one
    kind-mapping home either way.

    Raises:
        LiquidityPoolError: On a not-computable refusal, with ``message``.
        swap_refusal_to_error: Renders a fetch-arm refusal (a bitmap word the
            walk could not fetch) as ``MissingLiquidityData``.

    """
    if exc.kind != SwapRefusalKind.NotComputable:  # ty: ignore[unresolved-attribute]
        raise swap_refusal_to_error(exc) from exc
    raise LiquidityPoolError(message=message) from exc


class DoorSwapQuoter:
    """One family's swap read over the Rust swap-simulation door.

    Holds the pool handle the read runs through and nothing else. The family
    supplies the arm choice, the verdict policy (:meth:`refuse`), the delta
    decode, and the post-state rebuild.
    """

    def __init__(self, handle: Pool) -> None:
        """Bind the pool handle a family's door reads run through."""
        self._handle = handle

    def read(
        self,
        request: SwapRequest,
        *,
        sqrt_price_limit_x96: int | None = None,
    ) -> SwapRead:
        """Read one swap over the door, rendering a refusal as the family's verdict.

        A refused read raises this family's verdict (:meth:`refuse`) — the
        policy is the family's, the kind mapping is not.

        Returns:
            The computed read (signed deltas, end state, ``specified_met``).

        """
        try:
            return self._handle.simulate(
                zero_for_one=request.zero_for_one,
                amount_specified=request.amount_specified,
                override_state=request.override_state,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
        except SwapRefused as exc:
            self.refuse(exc, request)

    def refuse(  # ruff:ignore[no-self-use]
        self,
        exc: SwapRefused,
        request: SwapRequest,  # ruff:ignore[unused-method-argument]
    ) -> NoReturn:
        """Raise the verdict a refusal from the door means for this family.

        The default policy is the plain render: a concentrated-liquidity pool's
        refusal IS the pair's verdict (the walk exhausted the tick range with
        input remaining — the real pool's revert-analog). A family that reads a
        not-computable refusal as its own error — a reserve pair's overdraw or
        ``uint256`` overflow — overrides this and reads the request to name it;
        the default needs nothing from the request.

        Raises:
            swap_refusal_to_error: Renders the refusal as ``IncompleteSwap`` (the
                revert-analog) or ``MissingLiquidityData`` (an unfetchable word).

        """
        raise swap_refusal_to_error(exc) from exc
