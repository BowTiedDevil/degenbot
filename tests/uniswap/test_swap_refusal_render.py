"""The ONE swap-refusal render (ADR-037/t2): typed verdict -> exception tree.

``SwapRefused`` carries its verdict as the ``kind`` / ``word`` / ``amount``
attributes; ``swap_refusal_to_error`` is the single home that renders it as the
companions' historical exception. The request amount is the READ's own value,
so a refusal reports the side that was asked for and zero for the side that was
genuinely unconsumed — never a fabricated pair of zeros.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import SwapRefusalKind, SwapRefused
from degenbot.exceptions.pool import IncompleteSwap, MissingLiquidityData
from degenbot.uniswap import swap_refusal_to_error


def _refusal(
    kind: SwapRefusalKind,
    *,
    word: int | None = None,
    amount: int | None = None,
) -> SwapRefused:
    """A seam refusal carrying the typed attributes the Rust seam attaches.

    Returns:
        The refusal the render consumes.

    """
    exc = SwapRefused(kind, word)
    exc.kind = kind
    exc.word = word
    exc.amount = amount
    return exc


def test_exact_input_refusal_reports_the_requested_input() -> None:
    """A negative request is exact input: the input side is the unmet one."""
    rendered = swap_refusal_to_error(
        _refusal(SwapRefusalKind.NotComputable, amount=-1_000),
    )

    assert isinstance(rendered, IncompleteSwap)
    assert rendered.amount_in == 1_000
    assert rendered.amount_out == 0


def test_exact_output_refusal_reports_the_requested_output() -> None:
    """A positive request is exact output: the output side is the unmet one."""
    rendered = swap_refusal_to_error(
        _refusal(SwapRefusalKind.NotComputable, amount=2_500),
    )

    assert isinstance(rendered, IncompleteSwap)
    assert rendered.amount_in == 0
    assert rendered.amount_out == 2_500


def test_a_refusal_without_a_request_context_renders_the_empty_pair() -> None:
    """No amount ⇒ no claim about either side (the shape before the typing)."""
    rendered = swap_refusal_to_error(_refusal(SwapRefusalKind.NotComputable))

    assert isinstance(rendered, IncompleteSwap)
    assert rendered.amount_in == 0
    assert rendered.amount_out == 0


@pytest.mark.parametrize(
    "kind",
    [SwapRefusalKind.FetchFailed, SwapRefusalKind.FetchExhausted],
)
def test_fetch_arms_render_the_stalled_word(kind: SwapRefusalKind) -> None:
    """The fetch arms are an infrastructure failure, not a swap verdict."""
    rendered = swap_refusal_to_error(_refusal(kind, word=-7))

    assert isinstance(rendered, MissingLiquidityData)
    assert rendered.word == -7


def test_a_fetch_arm_without_a_word_reports_the_unknown_sentinel() -> None:
    """`None` is the no-word case; the render keeps the `-1` sentinel."""
    rendered = swap_refusal_to_error(_refusal(SwapRefusalKind.FetchExhausted))

    assert isinstance(rendered, MissingLiquidityData)
    assert rendered.word == -1
