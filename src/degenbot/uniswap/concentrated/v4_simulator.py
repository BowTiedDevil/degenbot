"""Pure V4-style swap simulator.

Ported from ``UniswapV4Pool._calculate_swap``. Operates on a frozen
``LiquidityMapSnapshot`` and returns ``SwapResult`` with no side effects.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, cast

from degenbot.constants import MAX_INT256, MIN_INT256
from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.concentrated.types import SwapResult
from degenbot.uniswap.v4_libraries.swap_math import compute_swap_step, get_sqrt_price_target
from degenbot.uniswap.v4_libraries.tick_math import get_sqrt_price_at_tick, get_tick_at_sqrt_price

if TYPE_CHECKING:
    from degenbot.types.aliases import Tick
    from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, _HasLiquidityNet
    from degenbot.uniswap.v3_types import Liquidity, SqrtPriceX96


_MIN_SQRT_PRICE = 4295128739
_MAX_SQRT_PRICE = 1461446703485210103287273052203988822378723970342
_FEE_DENOMINATOR = 1_000_000


@dataclasses.dataclass(slots=True, eq=False)
class _SwapResult:
    """Mutable accumulator used only inside the V4 simulator loop."""

    sqrt_price_x96: int
    tick: int
    liquidity: int


@dataclasses.dataclass(slots=True, eq=False)
class _StepComputations:
    sqrt_price_start_x96: int = 0
    sqrt_price_next_x96: int = 0
    tick_next: int = 0
    initialized: bool = False
    amount_in: int = 0
    amount_out: int = 0
    fee_amount: int = 0


def calculate_swap(
    *,
    snapshot: LiquidityMapSnapshot,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_x96_limit: int,
    lp_fee: int,
    protocol_fee: int,
    liquidity_start: Liquidity,
    sqrt_price_x96_start: SqrtPriceX96,
    tick_start: Tick,
) -> SwapResult:
    """Pure V4 swap calculation.

    V4 sign convention:
    - amount_specified < 0 : exact input (deposit this amount)
    - amount_specified > 0 : exact output (withdraw this amount)

    Raises:
        EVMRevertError: If swap fee is >= max or price limits are violated.
        MissingLiquidityData: If a required bitmap word is absent in a sparse map.
    """
    assert liquidity_start >= 0

    swap_fee = lp_fee if protocol_fee == 0 else _calculate_swap_fee(protocol_fee, lp_fee)

    if swap_fee >= _FEE_DENOMINATOR and amount_specified > 0:  # pragma: no cover
        raise EVMRevertError(error="InvalidFeeForExactOut")

    if amount_specified == 0:
        return SwapResult(
            amount0=0,
            amount1=0,
            sqrt_price_x96=sqrt_price_x96_start,
            liquidity=liquidity_start,
            tick=tick_start,
        )

    if zero_for_one:
        if sqrt_price_x96_limit >= sqrt_price_x96_start:
            raise EVMRevertError(error="PriceLimitAlreadyExceeded")
        if sqrt_price_x96_limit <= _MIN_SQRT_PRICE:
            raise EVMRevertError(error="PriceLimitOutOfBounds")
    else:
        if sqrt_price_x96_limit <= sqrt_price_x96_start:
            raise EVMRevertError(error="PriceLimitAlreadyExceeded")
        if sqrt_price_x96_limit >= _MAX_SQRT_PRICE:
            raise EVMRevertError(error="PriceLimitOutOfBounds")

    amount_specified_remaining = amount_specified
    amount_calculated = 0

    result = _SwapResult(
        sqrt_price_x96=sqrt_price_x96_start,
        tick=tick_start,
        liquidity=liquidity_start,
    )

    step = _StepComputations()

    if not snapshot.sparse:
        ticks = snapshot.ticks_along_path(
            tick_start=tick_start,
            zero_for_one=zero_for_one,
        )

    while not (amount_specified_remaining == 0 or result.sqrt_price_x96 == sqrt_price_x96_limit):
        step.sqrt_price_start_x96 = result.sqrt_price_x96

        if not snapshot.sparse:
            step.tick_next, step.initialized = next(ticks)
        else:
            step.tick_next, step.initialized = snapshot.next_initialized_tick(
                tick=result.tick,
                zero_for_one=zero_for_one,
            )

        step.tick_next = (
            max(-887272, step.tick_next) if zero_for_one else min(887272, step.tick_next)
        )
        step.sqrt_price_next_x96 = get_sqrt_price_at_tick(step.tick_next)

        if zero_for_one:
            tick_lower, tick_upper = step.tick_next, step.tick_next + snapshot.tick_spacing
        else:
            tick_lower, tick_upper = step.tick_next - snapshot.tick_spacing, step.tick_next
        assert tick_lower < tick_upper

        exact_input = amount_specified < 0

        result.sqrt_price_x96, step.amount_in, step.amount_out, step.fee_amount = compute_swap_step(
            sqrt_ratio_x96_current=result.sqrt_price_x96,
            sqrt_ratio_x96_target=get_sqrt_price_target(
                zero_for_one=zero_for_one,
                sqrt_price_next_x96=step.sqrt_price_next_x96,
                sqrt_price_limit_x96=sqrt_price_x96_limit,
            ),
            liquidity=result.liquidity,
            amount_remaining=amount_specified_remaining,
            fee_pips=swap_fee,
        )

        if exact_input:
            total = step.amount_in + step.fee_amount
            if not (MIN_INT256 <= total <= MAX_INT256):  # pragma: no cover
                raise EVMRevertError(error="SafeCastOverflow")
            if not (MIN_INT256 <= step.amount_out <= MAX_INT256):  # pragma: no cover
                raise EVMRevertError(error="SafeCastOverflow")
            amount_specified_remaining += total
            amount_calculated += step.amount_out
        else:
            if not (MIN_INT256 <= step.amount_out <= MAX_INT256):  # pragma: no cover
                raise EVMRevertError(error="SafeCastOverflow")
            total = step.amount_in + step.fee_amount
            if not (MIN_INT256 <= total <= MAX_INT256):  # pragma: no cover
                raise EVMRevertError(error="SafeCastOverflow")
            amount_specified_remaining -= step.amount_out
            amount_calculated -= total

        if protocol_fee > 0:
            delta = (
                step.fee_amount
                if swap_fee == protocol_fee
                else (step.amount_in + step.fee_amount) * protocol_fee // 1_000_000
            )
            step.fee_amount -= delta

        if result.sqrt_price_x96 == step.sqrt_price_next_x96:
            if step.initialized:
                tick_info = snapshot.tick_data[step.tick_next]
                liquidity_net = cast("_HasLiquidityNet", tick_info).liquidity_net
                result.liquidity += -liquidity_net if zero_for_one else liquidity_net
            result.tick = step.tick_next - 1 if zero_for_one else step.tick_next
        elif result.sqrt_price_x96 != step.sqrt_price_start_x96:
            result.tick = get_tick_at_sqrt_price(result.sqrt_price_x96)

        assert result.liquidity >= 0

    if zero_for_one != (amount_specified < 0):
        amount0 = amount_calculated
        amount1 = amount_specified - amount_specified_remaining
    else:
        amount0 = amount_specified - amount_specified_remaining
        amount1 = amount_calculated

    return SwapResult(
        amount0=amount0,
        amount1=amount1,
        sqrt_price_x96=result.sqrt_price_x96,
        liquidity=result.liquidity,
        tick=result.tick,
    )


def _calculate_swap_fee(protocol_fee: int, lp_fee: int) -> int:
    """Compute total swap fee from protocol + LP fee portions (V4).

    Matches V4Pool._calculate_swap_fee exactly.
    """
    lp_fee &= 0xFFFFFF
    return (protocol_fee + lp_fee) - (protocol_fee * lp_fee // _FEE_DENOMINATOR)
