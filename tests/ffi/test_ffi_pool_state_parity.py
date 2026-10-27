"""PyO3 FFI parity for pool state: registration in, views out, events in.

The Python edge over the Rust core is read-only for pool state. Values enter
Rust in exactly two shapes — a registration call's parameters, and a log driven
through ``Bot.dispatch_log`` (the pump's own door; the pool classes carry no
write method) — and leave through the read views on the pool handle and its
companion. These properties pin both directions at full integer width, so a
truncating or rounding seam (u256 sqrt ratio squeezed into a float, an i128 net
narrowed to i64, a u64 block wrapped) fails as a mismatch instead of passing as
a plausible-looking number.

The independent oracle is recomputed here from the contract's own tick-update
rules: gross is signed i128 arithmetic saturating at zero, net is i128
checked-add/checked-sub with an overflow reading as zero, a boundary tick whose
gross reaches zero is removed, and only an in-range event strictly after the
seed block moves the active-liquidity scalar (the rest of the time the price
clock stays untouched and the liquidity clock advances monotonically). For
swaps the ``concentrated_liquidity_math`` FFI functions carry the price truth:
the log's sqrt ratio is derived from its tick through
``get_sqrt_ratio_at_tick`` and checked back through ``get_tick_at_sqrt_ratio``,
so the price is verified against the tick math rather than against the log's
own echo.

Every pool here is registered ``sparse`` with the bitmap words its event ranges
touch. A sparse pool registers ``Live``, so a dispatched event applies directly
and the parity assertions are live; a ``tracked`` pool registers ``Quarantined``
and defers every event to its pump buffer (no assertion would ever fire), and a
sparse pool that has never checked a boundary word has its tick mutation refused
by the strict sparse-word gate. Registration parity covers both coverages, since
reads do not depend on the lifecycle.
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import asdict, dataclass, replace
from datetime import timedelta
from typing import TYPE_CHECKING

from hypothesis import example, given, settings
from hypothesis import strategies as st

from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.helpers.seam import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    Bot,
    PoolTickCoverage,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
    get_tick_word_and_bit_position,
    reinstall_config_holder_for_tests,
)
from tests.helpers.synthetic_logs import v3_burn, v3_mint, v3_swap, v4_modify_liquidity
from tests.suite_env import SUITE_ENV

if TYPE_CHECKING:
    from collections.abc import Generator, Mapping, Sequence

    from tests.helpers.seam import Pool

_CHAIN_ID = 1
_FEE = 3000
_TICK_SPACING = 60
_ONE_TO_ONE_SQRT_PRICE = 1 << 96

# The registration door's V3 fee bound (a V3 fee must stay below 0.1%).
_MAX_V3_FEE = 999_999
_MAX_UINT64 = 2**64 - 1
_MAX_UINT128 = 2**128 - 1
_MAX_INT128 = 2**127 - 1
_MIN_INT128 = -(2**127)
_MAX_INT256 = 2**255 - 1
_MIN_INT256 = -(2**255)

_TOKEN0 = "0x" + "a0" * 20
_TOKEN1 = "0x" + "b1" * 20
_FACTORY = "0x" + "c2" * 20
_V3_POOL = "0x" + "d3" * 20
_V4_POOL_MANAGER = "0x" + "e4" * 20
_V4_POOL_ID = "0x" + "f5" * 32

# Bounded so the whole module stays far inside the suite's pytest-timeout
# budget; the FFI seam is microsecond work, so a breach means a hung call.
_PER_EXAMPLE_DEADLINE = timedelta(seconds=10)

_SPARSE = "sparse"
_TRACKED = "tracked"
_COVERAGE = {_SPARSE: PoolTickCoverage.Sparse, _TRACKED: PoolTickCoverage.Tracked}


# ---------------------------------------------------------------------------
# Registration helpers (state enters Rust only here)
# ---------------------------------------------------------------------------


def _min_usable_tick(tick_spacing: int) -> int:
    """``TickMath.minUsableTick``: the lowest spacing multiple inside `MIN_TICK`."""
    return -((-MIN_TICK) // tick_spacing) * tick_spacing


def _max_usable_tick(tick_spacing: int) -> int:
    """``TickMath.maxUsableTick``: the highest spacing multiple inside `MAX_TICK`."""
    return (MAX_TICK // tick_spacing) * tick_spacing


def _new_bot() -> Bot:
    """A fresh single-chain ``Bot`` with both pool tokens registered."""
    bot = Bot(_CHAIN_ID)
    bot.register_token(_TOKEN0, "Token 0", "T0", 18, _CHAIN_ID)
    bot.register_token(_TOKEN1, "Token 1", "T1", 18, _CHAIN_ID)
    return bot


def _register_v3(
    bot: Bot,
    *,
    sqrt_price_x96: int,
    liquidity: int,
    tick: int,
    tick_data: Mapping[int, tuple[int, int, int]] | None,
    update_block: int = 0,
    coverage: str = _SPARSE,
    tick_spacing: int = _TICK_SPACING,
    fee: int = _FEE,
    known_bitmap_words: Sequence[int] | None = None,
) -> tuple[Pool, UniswapV3Pool]:
    """Register a V3 pool and wrap its handle in the production companion."""
    pool_id = bot.register_v3_pool(
        address=_V3_POOL,
        token0=_TOKEN0,
        token1=_TOKEN1,
        fee=fee,
        tick_spacing=tick_spacing,
        factory=_FACTORY,
        sqrt_price_x96=sqrt_price_x96,
        liquidity=liquidity,
        tick=tick,
        tick_data=dict(tick_data) if tick_data else None,
        update_block=update_block,
        coverage=coverage,
        known_bitmap_words=None if known_bitmap_words is None else list(known_bitmap_words),
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None, "register_v3_pool returned a pool_id with no handle"
    return handle, UniswapV3Pool.from_handle(handle)


def _register_v4(
    bot: Bot,
    *,
    sqrt_price_x96: int,
    liquidity: int,
    tick: int,
    block: int = 0,
    tick_data: Mapping[int, tuple[int, int, int]] | None = None,
    known_bitmap_words: Sequence[int] | None = None,
) -> Pool:
    """Register a sparse V4 pool (registered ``Live``, so events apply directly)."""
    pool_id = bot.register_v4_pool(
        pool_manager=_V4_POOL_MANAGER,
        pool_id_hex=_V4_POOL_ID,
        currency0=_TOKEN0,
        currency1=_TOKEN1,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_address=None,
        sqrt_price_x96=sqrt_price_x96,
        liquidity=liquidity,
        tick=tick,
        block=block,
        tick_data=dict(tick_data) if tick_data else None,
        coverage=_SPARSE,
        known_bitmap_words=None if known_bitmap_words is None else list(known_bitmap_words),
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None, "register_v4_pool returned a pool_id with no handle"
    return handle


# ---------------------------------------------------------------------------
# The independent oracle
# ---------------------------------------------------------------------------


@dataclass
class _ExpectedPoolState:
    """Python recomputation of the state the Rust event apply must land.

    Mirrors the contract's tick update (``Tick.update``): gross moves by the
    delta in the same direction at both boundary ticks and saturates at zero,
    net moves by ``+delta`` at the lower tick and ``-delta`` at the upper with
    i128 checked arithmetic (an overflow reads as zero, never as a wrap), and a
    boundary tick left at zero gross is removed from the map. Clocks follow the
    two-stamp rule: the liquidity clock advances monotonically for every
    applied tick mutation, the price clock only for an in-range event strictly
    after the seed block.
    """

    sqrt_price_x96: int
    liquidity: int
    tick: int
    update_block: int
    tick_data_block: int
    initial_state_block: int
    tick_data: dict[int, tuple[int, int, int]]

    def apply_liquidity(
        self,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
        block_number: int,
    ) -> None:
        """Apply one `Mint`/`Burn`/`ModifyLiquidity` delta on `[lower, upper)`."""
        in_range = tick_lower <= self.tick < tick_upper and block_number > self.initial_state_block
        for boundary, is_lower in ((tick_lower, True), (tick_upper, False)):
            gross, net, _ = self.tick_data.get(boundary, (0, 0, 0))
            gross_after = _as_signed_u128(gross) + liquidity_delta
            net_after = (
                _checked_i128(net + liquidity_delta)
                if is_lower
                else _checked_i128(net - liquidity_delta)
            )
            self.tick_data[boundary] = (max(gross_after, 0), net_after, block_number)
        for boundary in (tick_lower, tick_upper):
            if self.tick_data[boundary][0] == 0:
                del self.tick_data[boundary]
        if in_range:
            assert self.liquidity + liquidity_delta >= 0, (
                "generated delta exceeds the in-range active liquidity"
            )
            self.liquidity += liquidity_delta
        self.tick_data_block = max(self.tick_data_block, block_number)
        if in_range:
            self.update_block = max(self.update_block, block_number)

    def apply_swap(
        self,
        *,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block_number: int,
    ) -> None:
        """Apply one `Swap`: the log carries post-swap slot0, applied verbatim."""
        self.sqrt_price_x96 = sqrt_price_x96
        self.liquidity = liquidity
        self.tick = tick
        self.update_block = max(self.update_block, block_number)
        self.tick_data_block = max(self.tick_data_block, block_number)


def _as_signed_u128(value: int) -> int:
    """Rust ``u128::cast_signed``: reinterpret a gross as the i128 apply operand."""
    return value - (1 << 128) if value >= 1 << 127 else value


def _checked_i128(value: int) -> int:
    """``checked_add``/``checked_sub`` at the on-chain width: overflow reads as zero."""
    return value if _MIN_INT128 <= value <= _MAX_INT128 else 0


def _expected_after_registration(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    tick: int,
    update_block: int,
    tick_data: Mapping[int, tuple[int, int, int]] | None,
) -> _ExpectedPoolState:
    """The views a registration must expose: the liquidity clock seeds at the price clock."""
    return _ExpectedPoolState(
        sqrt_price_x96=sqrt_price_x96,
        liquidity=liquidity,
        tick=tick,
        update_block=update_block,
        tick_data_block=update_block,
        initial_state_block=update_block,
        tick_data=dict(tick_data or {}),
    )


# ---------------------------------------------------------------------------
# View assertions
# ---------------------------------------------------------------------------


def _rows(raw: Mapping[int, object]) -> dict[int, tuple[int, int, int]]:
    """Normalize a wire tick-data mapping onto ``{tick: (gross, net, block)}``."""
    return {int(tick): (int(row[0]), int(row[1]), int(row[2])) for tick, row in raw.items()}


def _snapshot(handle: Pool) -> tuple[object, ...]:
    """Every read view of a pool, as one comparable tuple."""
    view = handle.concentrated_liquidity()
    return (
        _rows(view.tick_data),
        view.liquidity,
        view.tick,
        view.sqrt_price_x96,
        view.update_block,
        view.tick_data_block,
        tuple(sorted(int(word) for word in view.tick_bitmap)),
    )


def _assert_handle_views(
    handle: Pool,
    expected: _ExpectedPoolState,
    *,
    coverage: PoolTickCoverage,
) -> None:
    """Each handle view equals the recomputation, with the integer width intact."""
    view = handle.concentrated_liquidity()
    assert view.coverage == coverage
    assert _rows(view.tick_data) == expected.tick_data
    assert isinstance(view.liquidity, int)
    assert view.liquidity == expected.liquidity
    assert isinstance(view.tick, int)
    assert view.tick == expected.tick
    assert isinstance(view.sqrt_price_x96, int)
    assert view.sqrt_price_x96 == expected.sqrt_price_x96
    assert view.update_block == expected.update_block
    assert view.tick_data_block == expected.tick_data_block


def _assert_companion_views(companion: UniswapV3Pool, expected: _ExpectedPoolState) -> None:
    """The companion reads (the Python driver's surface) equal the recomputation."""
    assert companion.liquidity == expected.liquidity
    assert companion.tick == expected.tick
    assert companion.sqrt_price_x96 == expected.sqrt_price_x96
    assert companion.update_block == expected.update_block
    assert companion.initial_state_block == expected.initial_state_block
    assert companion.tick_data == {
        tick: LiquidityAtTick(liquidity_net=net, liquidity_gross=gross, block=block)
        for tick, (gross, net, block) in expected.tick_data.items()
    }


# ---------------------------------------------------------------------------
# Event ops (state enters Rust only through the dispatcher)
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class _LiquidityOp:
    """A `Mint`/`Burn` log: a signed liquidity delta on ``[tick_lower, tick_upper)``."""

    tick_lower: int
    tick_upper: int
    amount: int
    mint: bool


@dataclass(frozen=True)
class _SwapOp:
    """A `Swap` log: post-swap slot0 scalars, price carried as a u160 sqrt ratio."""

    tick: int
    liquidity: int
    amount0: int
    amount1: int


type _Op = _LiquidityOp | _SwapOp


def _dispatch(bot: Bot, op: _Op, block_number: int) -> None:
    """Drive one synthetic log through the pump's door at ``block_number``."""
    if isinstance(op, _LiquidityOp):
        builder = v3_mint if op.mint else v3_burn
        bot.dispatch_log(
            address=_V3_POOL,
            block_number=block_number,
            **builder(tick_lower=op.tick_lower, tick_upper=op.tick_upper, amount=op.amount),
        )
        return
    bot.dispatch_log(
        address=_V3_POOL,
        block_number=block_number,
        **v3_swap(
            sqrt_price_x96=get_sqrt_ratio_at_tick(op.tick),
            liquidity=op.liquidity,
            tick=op.tick,
            amount0=op.amount0,
            amount1=op.amount1,
        ),
    )


def _apply_to_oracle(expected: _ExpectedPoolState, op: _Op, block_number: int) -> None:
    """Fold one op into the recomputation, mirroring the Rust apply it lands on."""
    if isinstance(op, _LiquidityOp):
        delta = op.amount if op.mint else -op.amount
        expected.apply_liquidity(op.tick_lower, op.tick_upper, delta, block_number)
        return
    expected.apply_swap(
        sqrt_price_x96=get_sqrt_ratio_at_tick(op.tick),
        liquidity=op.liquidity,
        tick=op.tick,
        block_number=block_number,
    )


def _assert_price_matches_tick_math(handle: Pool) -> None:
    """The read-back price and tick are each other's image under the tick math."""
    view = handle.concentrated_liquidity()
    assert get_sqrt_ratio_at_tick(view.tick) == view.sqrt_price_x96
    assert get_tick_at_sqrt_ratio(view.sqrt_price_x96) == view.tick


@contextmanager
def _decode_hard_fault_disabled() -> Generator[None, None, None]:
    """Install the suite environment with the decode-miss hard fault switched off.

    A forged (out-of-i128) ``ModifyLiquidity`` delta is a log a decoder must
    refuse, and the production default (``pump.ws_completeness``) turns a decode
    miss on a known pool topic into a process-fatal fault. The behaviour under
    test is the decoder's refusal, so the fault is switched off for the pool the
    test builds and the suite's declared environment reinstalled afterwards.
    """
    reinstall_config_holder_for_tests({**SUITE_ENV, "DEGENBOT_WS_COMPLETENESS": "false"})
    try:
        yield
    finally:
        reinstall_config_holder_for_tests(SUITE_ENV)


# ---------------------------------------------------------------------------
# Construction parity
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class _RegistrationCase:
    """A registrable V3 pool: contract-bounded scalars plus a tick-data book."""

    tick_spacing: int
    fee: int
    sqrt_price_x96: int
    liquidity: int
    tick: int
    update_block: int
    coverage: str
    tick_data: dict[int, tuple[int, int, int]]


_TICK_ROW = st.tuples(
    st.integers(min_value=0, max_value=_MAX_UINT128),
    st.integers(min_value=_MIN_INT128, max_value=_MAX_INT128),
    st.integers(min_value=0, max_value=_MAX_UINT64),
)


@st.composite
def _registration_case(draw: st.DrawFn) -> _RegistrationCase:
    """Draw a pool whose every field sits inside the value type the seam stores."""
    tick_spacing = draw(st.sampled_from([1, 10, 50, 60, 200, 1000]))
    spawn_tick = MAX_TICK // tick_spacing
    rows = draw(
        st.dictionaries(
            keys=st.integers(min_value=-spawn_tick, max_value=spawn_tick).map(
                lambda index: index * tick_spacing
            ),
            values=_TICK_ROW,
            max_size=6,
        )
    )
    return _RegistrationCase(
        tick_spacing=tick_spacing,
        fee=draw(st.integers(min_value=1, max_value=_MAX_V3_FEE)),
        sqrt_price_x96=draw(st.integers(min_value=MIN_SQRT_RATIO, max_value=MAX_SQRT_RATIO - 1)),
        liquidity=draw(st.integers(min_value=0, max_value=_MAX_UINT128)),
        tick=draw(st.integers(min_value=MIN_TICK, max_value=MAX_TICK)),
        update_block=draw(st.integers(min_value=0, max_value=_MAX_UINT64)),
        coverage=draw(st.sampled_from([_SPARSE, _TRACKED])),
        tick_data=rows,
    )


def _boundary_case(**overrides: object) -> _RegistrationCase:
    """A registration case with named boundary values over a plain base."""
    return _RegistrationCase(**{
        "tick_spacing": _TICK_SPACING,
        "fee": _FEE,
        "sqrt_price_x96": _ONE_TO_ONE_SQRT_PRICE,
        "liquidity": 0,
        "tick": 0,
        "update_block": 0,
        "coverage": _SPARSE,
        "tick_data": {},
        **overrides,
    })


@settings(max_examples=20, deadline=_PER_EXAMPLE_DEADLINE)
@given(case=_registration_case())
@example(case=_boundary_case(sqrt_price_x96=MIN_SQRT_RATIO, tick=MIN_TICK))
@example(
    case=_boundary_case(
        sqrt_price_x96=MAX_SQRT_RATIO - 1,
        liquidity=_MAX_UINT128,
        tick=MAX_TICK,
        coverage=_TRACKED,
    )
)
@example(
    case=_boundary_case(
        update_block=_MAX_UINT64,
        tick_data={
            -60: (_MAX_UINT128, _MAX_INT128, _MAX_UINT64),
            60: (0, _MIN_INT128, 0),
        },
    )
)
@example(
    case=_boundary_case(
        tick_spacing=1,
        tick_data={
            _min_usable_tick(1): (_MAX_UINT128, _MAX_INT128, 0),
            _max_usable_tick(1): (_MAX_UINT128, _MIN_INT128, _MAX_UINT64),
        },
    )
)
def test_construction_views_round_trip_every_registration_param(
    case: _RegistrationCase,
) -> None:
    """Registration in, views out: nothing is clamped, narrowed, or reordered."""
    bot = _new_bot()
    handle, companion = _register_v3(bot, **asdict(case))
    expected = _expected_after_registration(
        sqrt_price_x96=case.sqrt_price_x96,
        liquidity=case.liquidity,
        tick=case.tick,
        update_block=case.update_block,
        tick_data=case.tick_data,
    )

    _assert_handle_views(handle, expected, coverage=_COVERAGE[case.coverage])
    _assert_companion_views(companion, expected)

    view = handle.concentrated_liquidity()
    assert view.fee == case.fee
    assert view.tick_spacing == case.tick_spacing
    assert {int(word) for word in view.tick_bitmap} == {
        get_tick_word_and_bit_position(tick, case.tick_spacing)[0] for tick in case.tick_data
    }


# ---------------------------------------------------------------------------
# Event parity
# ---------------------------------------------------------------------------

_EVENT_SEED_BLOCK = 100
_EVENT_SEED_LIQUIDITY = 2**100
_EVENT_SEED_BOOK = {-60: (2**96, 2**96, 0), 60: (2**96, -(2**96), 0)}

# Ranges stay inside the compressed-tick words the seed book already checked
# (bit 0 of words -1 and 0), plus the ±max usable ticks registered as checked
# words — every drawn delta therefore reaches the apply instead of being
# refused by the sparse-word gate.
_RANGE_LOWER_INDEX = st.integers(min_value=-100, max_value=90)
_RANGE_WIDTH_INDEX = st.integers(min_value=1, max_value=10)
_DRAWN_AMOUNT = st.integers(min_value=0, max_value=2**64)
# Kept below ``u128::MAX / 2`` so a later in-range mint can never overflow the
# u128 the active-liquidity narrowing demands; the u128 boundary itself is an
# explicit example below.
_DRAWN_SWAP_LIQUIDITY = st.integers(min_value=0, max_value=_MAX_UINT128 // 2)


@st.composite
def _event_op(draw: st.DrawFn) -> _Op:
    """Draw one log: a Mint/Burn delta, or a Swap carrying post-swap slot0."""
    if draw(st.booleans()):
        lower_index = draw(_RANGE_LOWER_INDEX)
        return _LiquidityOp(
            tick_lower=lower_index * _TICK_SPACING,
            tick_upper=(lower_index + draw(_RANGE_WIDTH_INDEX)) * _TICK_SPACING,
            amount=draw(_DRAWN_AMOUNT),
            mint=draw(st.booleans()),
        )
    return _SwapOp(
        tick=draw(st.integers(min_value=-14787, max_value=14787)) * _TICK_SPACING,
        liquidity=draw(_DRAWN_SWAP_LIQUIDITY),
        amount0=draw(st.integers(min_value=_MIN_INT256, max_value=_MAX_INT256)),
        amount1=draw(st.integers(min_value=_MIN_INT256, max_value=_MAX_INT256)),
    )


@settings(max_examples=15, deadline=_PER_EXAMPLE_DEADLINE)
@given(ops=st.lists(_event_op(), min_size=1, max_size=5))
@example(
    ops=[
        _LiquidityOp(-60, 60, 4242, mint=True),
        _LiquidityOp(-60, 60, 4242, mint=False),
        _LiquidityOp(180, 240, 777, mint=True),
        _LiquidityOp(180, 240, 777, mint=False),
    ]
)
@example(
    ops=[
        _LiquidityOp(
            _min_usable_tick(_TICK_SPACING),
            _max_usable_tick(_TICK_SPACING),
            10**18,
            mint=True,
        )
    ]
)
@example(
    ops=[
        _LiquidityOp(
            _min_usable_tick(_TICK_SPACING),
            _max_usable_tick(_TICK_SPACING),
            10**18,
            mint=False,
        )
    ]
)
@example(
    ops=[
        _SwapOp(tick=_max_usable_tick(_TICK_SPACING), liquidity=_MAX_UINT128, amount0=1, amount1=-1)
    ]
)
def test_dispatched_events_land_the_recomputed_bookkeeping(ops: list[_Op]) -> None:
    """A dispatched log moves the Python views to the recomputed bookkeeping."""
    bot = _new_bot()
    handle, companion = _register_v3(
        bot,
        sqrt_price_x96=_ONE_TO_ONE_SQRT_PRICE,
        liquidity=_EVENT_SEED_LIQUIDITY,
        tick=0,
        tick_data=_EVENT_SEED_BOOK,
        update_block=_EVENT_SEED_BLOCK,
        known_bitmap_words=[
            get_tick_word_and_bit_position(_min_usable_tick(_TICK_SPACING), _TICK_SPACING)[0],
            get_tick_word_and_bit_position(_max_usable_tick(_TICK_SPACING), _TICK_SPACING)[0],
        ],
    )
    expected = _expected_after_registration(
        sqrt_price_x96=_ONE_TO_ONE_SQRT_PRICE,
        liquidity=_EVENT_SEED_LIQUIDITY,
        tick=0,
        update_block=_EVENT_SEED_BLOCK,
        tick_data=_EVENT_SEED_BOOK,
    )
    _assert_handle_views(handle, expected, coverage=PoolTickCoverage.Sparse)

    block_number = _EVENT_SEED_BLOCK
    for op in ops:
        block_number += 1
        if isinstance(op, _LiquidityOp) and not op.mint:
            # A Burn can only retire liquidity the book actually holds: the
            # boundary ticks' gross, plus the active in-range liquidity when
            # the range covers the current tick. The core refuses (panics)
            # beyond that, so the draw is clamped to the valid envelope
            # rather than discarded.
            gross_lower = expected.tick_data.get(op.tick_lower, (0, 0, 0))[0]
            gross_upper = expected.tick_data.get(op.tick_upper, (0, 0, 0))[0]
            cap = min(gross_lower, gross_upper)
            if op.tick_lower <= expected.tick < op.tick_upper:
                cap = min(cap, expected.liquidity)
            amount = min(op.amount, cap)
            if amount == 0:
                continue
            op = replace(op, amount=amount)
        _dispatch(bot, op, block_number)
        _apply_to_oracle(expected, op, block_number)
        _assert_handle_views(handle, expected, coverage=PoolTickCoverage.Sparse)
        _assert_companion_views(companion, expected)
        _assert_price_matches_tick_math(handle)


# ---------------------------------------------------------------------------
# Boundary cases: i128 extremes through the V4 wire
# ---------------------------------------------------------------------------

_V4_BOUNDARY_RANGE = (-600, 600)


@settings(max_examples=8, deadline=_PER_EXAMPLE_DEADLINE)
@given(
    liquidity_delta=st.integers(min_value=_MIN_INT128, max_value=_MAX_INT128),
)
@example(liquidity_delta=_MAX_INT128)
@example(liquidity_delta=_MIN_INT128)
@example(liquidity_delta=0)
@example(liquidity_delta=_MAX_INT128 - 1)
def test_v4_modify_liquidity_i128_boundaries_apply_at_full_width(
    liquidity_delta: int,
) -> None:
    """An i128 wire delta is accepted whole; gross/net are never narrowed."""
    bot = _new_bot()
    # Pool tick 1000 sits outside the delta's range, so the active-liquidity
    # scalar and the price clock stay put and only the tick map moves.
    handle = _register_v4(
        bot,
        sqrt_price_x96=get_sqrt_ratio_at_tick(1000),
        liquidity=1000,
        tick=1000,
        known_bitmap_words=[
            get_tick_word_and_bit_position(tick, _TICK_SPACING)[0] for tick in _V4_BOUNDARY_RANGE
        ],
    )
    expected = _expected_after_registration(
        sqrt_price_x96=get_sqrt_ratio_at_tick(1000),
        liquidity=1000,
        tick=1000,
        update_block=0,
        tick_data=None,
    )
    _assert_handle_views(handle, expected, coverage=PoolTickCoverage.Sparse)

    bot.dispatch_log(
        address=_V4_POOL_MANAGER,
        block_number=1,
        **v4_modify_liquidity(
            pool_id=_V4_POOL_ID,
            tick_lower=_V4_BOUNDARY_RANGE[0],
            tick_upper=_V4_BOUNDARY_RANGE[1],
            liquidity_delta=liquidity_delta,
        ),
    )
    expected.apply_liquidity(*_V4_BOUNDARY_RANGE, liquidity_delta, 1)

    _assert_handle_views(handle, expected, coverage=PoolTickCoverage.Sparse)
    # A zero-gross boundary tick is removed, so a non-positive delta leaves the
    # map empty, while a positive one appears at its full i128 width (net flips
    # sign at the upper boundary).
    read_rows = _rows(handle.concentrated_liquidity().tick_data)
    if liquidity_delta > 0:
        assert read_rows[_V4_BOUNDARY_RANGE[0]] == (liquidity_delta, liquidity_delta, 1)
        assert read_rows[_V4_BOUNDARY_RANGE[1]] == (liquidity_delta, -liquidity_delta, 1)
    else:
        assert read_rows == {}


# ---------------------------------------------------------------------------
# The anti-vacuous case: a log the decoder must refuse
# ---------------------------------------------------------------------------

_FORGED_DELTA = st.one_of(
    st.integers(min_value=_MAX_INT128 + 1, max_value=_MAX_INT256),
    st.integers(min_value=_MIN_INT256, max_value=_MIN_INT128 - 1),
)


@settings(max_examples=8, deadline=_PER_EXAMPLE_DEADLINE)
@given(liquidity_delta=_FORGED_DELTA)
@example(liquidity_delta=_MAX_INT128 + 1)
@example(liquidity_delta=_MIN_INT128 - 1)
@example(liquidity_delta=_MAX_INT256)
@example(liquidity_delta=_MIN_INT256)
def test_out_of_i128_delta_is_refused_and_leaves_state_unchanged(
    liquidity_delta: int,
) -> None:
    """An out-of-i128 `ModifyLiquidity` delta is refused before any state write.

    The control dispatch (``i128::MAX``, in bounds) proves the assertions are
    live: it lands, and the forged log one bit outside the wire bound then
    leaves the pool byte-identical.
    """
    assert not (_MIN_INT128 <= liquidity_delta <= _MAX_INT128)
    with _decode_hard_fault_disabled():
        bot = _new_bot()
        handle = _register_v4(
            bot,
            sqrt_price_x96=get_sqrt_ratio_at_tick(1000),
            liquidity=1000,
            tick=1000,
            known_bitmap_words=[
                get_tick_word_and_bit_position(tick, _TICK_SPACING)[0]
                for tick in _V4_BOUNDARY_RANGE
            ],
        )
        expected = _expected_after_registration(
            sqrt_price_x96=get_sqrt_ratio_at_tick(1000),
            liquidity=1000,
            tick=1000,
            update_block=0,
            tick_data=None,
        )
        at_registration = _snapshot(handle)

        control_delta = _MAX_INT128
        bot.dispatch_log(
            address=_V4_POOL_MANAGER,
            block_number=1,
            **v4_modify_liquidity(
                pool_id=_V4_POOL_ID,
                tick_lower=_V4_BOUNDARY_RANGE[0],
                tick_upper=_V4_BOUNDARY_RANGE[1],
                liquidity_delta=control_delta,
            ),
        )
        expected.apply_liquidity(*_V4_BOUNDARY_RANGE, control_delta, 1)
        _assert_handle_views(handle, expected, coverage=PoolTickCoverage.Sparse)
        after_control = _snapshot(handle)
        assert after_control != at_registration, "the in-bound control must move state"

        bot.dispatch_log(
            address=_V4_POOL_MANAGER,
            block_number=2,
            **v4_modify_liquidity(
                pool_id=_V4_POOL_ID,
                tick_lower=_V4_BOUNDARY_RANGE[0],
                tick_upper=_V4_BOUNDARY_RANGE[1],
                liquidity_delta=liquidity_delta,
            ),
        )
        assert _snapshot(handle) == after_control
        _assert_handle_views(handle, expected, coverage=PoolTickCoverage.Sparse)
