"""The door families' refusal verdicts: one table over kinds x arms x family.

A refusal from the swap-simulation door is a *kind*, and what a kind means is the
family's verdict policy — never a second copy of the kind mapping. This table
pins that policy for every door arm: the two idioms (a reserve pair's overdraw /
overflow error, a concentrated-liquidity pair's incomplete-swap verdict) and the
one rule every family shares (a fetch-arm refusal was never about swap geometry,
so it renders as ``MissingLiquidityData`` through the single kind-mapping home).

No pool fixture: the door handle is a fake whose only behaviour is to refuse, and
the host supplies the ordinary reads a quota needs, so the table costs one row
per shape rather than a pool build per class.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.aerodrome.v2_pool_quoter import AerodromeV2SwapQuoter
from degenbot.exceptions.pool import (
    IncompleteSwap,
    LiquidityPoolError,
    MissingLiquidityData,
)
from degenbot.uniswap.v2_pool_quoter import UniswapV2SwapQuoter
from degenbot.uniswap.v3_pool_quoter import UniswapV3SwapQuoter
from degenbot.uniswap.v4_pool_quoter import UniswapV4SwapQuoter
from tests.helpers.seam import SwapRefusalKind, SwapRefused


class FakeToken:
    """The token identity a quoter compares, and nothing else."""

    def __init__(self, address: str, decimals: int = 18) -> None:
        self.address = address
        self.decimals = decimals
        self.symbol = address[-4:]

    def __repr__(self) -> str:  # pragma: no cover - diagnostics only
        return f"FakeToken({self.address})"


class FakeState:
    """The pool-state reads a door quoter touches before the read refuses."""

    def __init__(self, *, reserves: int = 10**24) -> None:
        self.reserves_token0 = reserves
        self.reserves_token1 = reserves
        self.liquidity = 10**24
        self.sqrt_price_x96 = 1 << 96
        self.tick = 0
        self.block = 0


class FakeHost:
    """The companion reads a door quoter needs (no handle, no registration)."""

    def __init__(self, *, stable_swap: bool = False) -> None:
        self.tokens = (FakeToken("0x" + "11" * 20), FakeToken("0x" + "22" * 20))
        self.state = FakeState()
        self.update_block = 0
        self.stable_swap = stable_swap
        self.hook_address = "0x" + "00" * 20
        self.address = "0x" + "33" * 20
        self.active_hooks: frozenset[object] = frozenset()


class FakeDoor:
    """A swap-simulation handle whose only answer is a preselected refusal."""

    def __init__(self, refusal: SwapRefused) -> None:
        self._refusal = refusal

    def simulate(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        override_state: object = None,
        sqrt_price_limit_x96: int | None = None,
    ) -> None:
        raise self._refusal


def _refusal(kind: SwapRefusalKind, *, amount: int | None = -1_000) -> SwapRefused:
    """A seam refusal carrying the typed attributes the Rust seam attaches.

    Returns:
        The refusal under test.

    """
    exc = SwapRefused(kind, None)
    exc.kind = kind
    exc.word = -7
    exc.amount = amount
    return exc


def _v2_quoter(host: FakeHost, handle: FakeDoor) -> UniswapV2SwapQuoter:
    return UniswapV2SwapQuoter(
        host,  # ty: ignore[invalid-argument-type]
        handle,  # ty: ignore[invalid-argument-type]
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
    )


def _v3_quoter(host: FakeHost, handle: FakeDoor) -> UniswapV3SwapQuoter:
    return UniswapV3SwapQuoter(host, handle)  # ty: ignore[invalid-argument-type]


def _v4_quoter(host: FakeHost, handle: FakeDoor) -> UniswapV4SwapQuoter:
    return UniswapV4SwapQuoter(host, handle, swap_hooks=frozenset())  # ty: ignore[invalid-argument-type]


def _aerodrome_quoter(host: FakeHost, handle: FakeDoor) -> AerodromeV2SwapQuoter:
    return AerodromeV2SwapQuoter(
        host,  # ty: ignore[invalid-argument-type]
        handle,  # ty: ignore[invalid-argument-type]
        stable=False,
        fee=Fraction(3, 1000),
    )


# (id, quoter factory, the arm, the verdict a not-computable refusal means)
DOOR_ARMS = [
    (
        "v2-exact-in",
        _v2_quoter,
        lambda quoter, host: quoter.exact_in(host.tokens[0], 1_000, override_state=FakeState()),
        LiquidityPoolError,
    ),
    (
        "v2-exact-out",
        _v2_quoter,
        lambda quoter, host: quoter.exact_out(host.tokens[1], 1_000, override_state=FakeState()),
        LiquidityPoolError,
    ),
    (
        "v2-simulate-in",
        _v2_quoter,
        lambda quoter, host: quoter.simulate_exact_in(host.tokens[0], 1_000),
        LiquidityPoolError,
    ),
    (
        "v2-simulate-out",
        _v2_quoter,
        lambda quoter, host: quoter.simulate_exact_out(host.tokens[1], 1_000),
        LiquidityPoolError,
    ),
    (
        "v3-exact-in",
        _v3_quoter,
        lambda quoter, host: quoter.exact_in(host.tokens[0], 1_000),
        IncompleteSwap,
    ),
    (
        "v3-exact-out",
        _v3_quoter,
        lambda quoter, host: quoter.exact_out(host.tokens[1], 1_000),
        IncompleteSwap,
    ),
    (
        "v3-simulate-in",
        _v3_quoter,
        lambda quoter, host: quoter.simulate_exact_in(host.tokens[0], 1_000),
        IncompleteSwap,
    ),
    (
        "v3-simulate-out",
        _v3_quoter,
        lambda quoter, host: quoter.simulate_exact_out(host.tokens[1], 1_000),
        IncompleteSwap,
    ),
    (
        "v4-exact-in",
        _v4_quoter,
        lambda quoter, host: quoter.exact_in(host.tokens[0], 1_000),
        LiquidityPoolError,
    ),
    (
        "v4-exact-out",
        _v4_quoter,
        lambda quoter, host: quoter.exact_out(host.tokens[1], 1_000),
        LiquidityPoolError,
    ),
    (
        "aerodrome-exact-in",
        _aerodrome_quoter,
        lambda quoter, host: quoter.exact_in(host.tokens[0], 1_000, override_state=FakeState()),
        LiquidityPoolError,
    ),
]


@pytest.mark.parametrize(
    ("quoter_factory", "arm", "not_computable_verdict"),
    [case[1:] for case in DOOR_ARMS],
    ids=[case[0] for case in DOOR_ARMS],
)
def test_not_computable_refusal_means_the_familys_verdict(
    quoter_factory,
    arm,
    not_computable_verdict,
) -> None:
    """A not-computable refusal is the family's own verdict, never a generic error."""
    host = FakeHost()
    quoter = quoter_factory(host, FakeDoor(_refusal(SwapRefusalKind.NotComputable)))

    with pytest.raises(not_computable_verdict) as excinfo:
        arm(quoter, host)

    assert type(excinfo.value) is not_computable_verdict


@pytest.mark.parametrize(
    ("quoter_factory", "arm", "verdict"),
    [case[1:] for case in DOOR_ARMS],
    ids=[case[0] for case in DOOR_ARMS],
)
@pytest.mark.parametrize(
    "kind",
    [SwapRefusalKind.FetchFailed, SwapRefusalKind.FetchExhausted],
    ids=["fetch-failed", "fetch-exhausted"],
)
def test_fetch_refusal_renders_the_stalled_word_everywhere(
    quoter_factory,
    arm,
    verdict,
    kind,
) -> None:
    """A fetch-arm refusal is infrastructure, not swap geometry, in every family.

    The one kind-mapping home renders it as ``MissingLiquidityData`` naming the
    word the walk stalled on — no family renames it an overdraw or an
    incomplete swap.
    """
    host = FakeHost()
    quoter = quoter_factory(host, FakeDoor(_refusal(kind, amount=None)))

    with pytest.raises(MissingLiquidityData) as excinfo:
        arm(quoter, host)

    assert excinfo.value.word == -7


def test_overdraw_verdict_names_the_hypothetical_reserve() -> None:
    """The reserve-pair overdraw message names the reserve the read compared against."""
    host = FakeHost()
    quoter = _v2_quoter(host, FakeDoor(_refusal(SwapRefusalKind.NotComputable, amount=5_000)))
    state = FakeState(reserves=4_000)

    with pytest.raises(LiquidityPoolError) as excinfo:
        quoter.exact_out(host.tokens[1], 5_000, override_state=state)

    assert "(4000)" in (excinfo.value.message or "")


def test_the_refusal_policy_sees_the_requests_signed_side() -> None:
    """Exact input and exact output read one kind as two different verdicts."""
    host = FakeHost()
    handle = FakeDoor(_refusal(SwapRefusalKind.NotComputable, amount=-1_000))
    quoter = _v2_quoter(host, handle)

    with pytest.raises(LiquidityPoolError) as excinfo:
        quoter.exact_in(host.tokens[0], 1_000, override_state=FakeState())

    assert "overflows uint256 swap math" in (excinfo.value.message or "")
