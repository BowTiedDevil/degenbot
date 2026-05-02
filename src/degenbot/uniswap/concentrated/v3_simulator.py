"""Pure V3-style swap simulator.

Ported from ``UniswapV3Pool._calculate_swap``. Operates on a frozen
``LiquidityMapSnapshot`` and returns ``SwapResult`` with no side effects.
The caller is responsible for retrying when sparse maps need data fetches.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, cast

from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.concentrated.types import SwapResult
from degenbot.uniswap.v3_libraries.swap_math import compute_swap_step
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio

if TYPE_CHECKING:
    from degenbot.types.aliases import Tick
    from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, _HasLiquidityNet
    from degenbot.uniswap.v3_types import Liquidity, SqrtPriceX96


# Match V3 contract constants
_MIN_SQRT_RATIO = 4295128739
_MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342


def _get_sqrt_price_target(
    *,
    zero_for_one: bool,
    sqrt_price_next_x96: int,
    sqrt_price_limit_x96: int,
) -> int:
    """Mirror of ``_calculate_swap``'s price-target ternary."""
    if (zero_for_one and sqrt_price_next_x96 < sqrt_price_limit_x96) or (
        not zero_for_one and sqrt_price_next_x96 > sqrt_price_limit_x96
    ):
        return sqrt_price_limit_x96
    return sqrt_price_next_x96


@dataclasses.dataclass(slots=True, eq=False)
class _SwapState:
    amount_specified_remaining: int
    amount_calculated: int
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
    sqrt_price_limit_x96: int,
    fee: int,
    liquidity_start: Liquidity,
    sqrt_price_x96_start: SqrtPriceX96,
    tick_start: Tick,
) -> SwapResult:
    """Pure V3 swap calculation.

    Returns ``SwapResult`` containing the final amounts, price, liquidity and tick.

    Raises:
        EVMRevertError: If the swap amount is zero or price limits are violated.
        MissingLiquidityData: (via ``snapshot.next_initialized_tick``) if a
            required bitmap word is absent in a sparse mapping.
    """
    assert liquidity_start >= 0

    if amount_specified == 0:
        raise EVMRevertError(error="AS")

    exact_input = amount_specified > 0

    if zero_for_one and not (
        _MIN_SQRT_RATIO < sqrt_price_limit_x96 < sqrt_price_x96_start
    ):  # pragma: no cover
        raise EVMRevertError(error="SPL")

    if not zero_for_one and not (
        sqrt_price_x96_start < sqrt_price_limit_x96 < _MAX_SQRT_RATIO
    ):  # pragma: no cover
        raise EVMRevertError(error="SPL")

    swap_state = _SwapState(
        amount_specified_remaining=amount_specified,
        amount_calculated=0,
        sqrt_price_x96=sqrt_price_x96_start,
        tick=tick_start,
        liquidity=liquidity_start,
    )

    if not snapshot.sparse:
        ticks = snapshot.ticks_along_path(
            tick_start=tick_start,
            zero_for_one=zero_for_one,
        )

    step = _StepComputations()

    while (
        swap_state.amount_specified_remaining != 0
        and swap_state.sqrt_price_x96 != sqrt_price_limit_x96
    ):
        step.sqrt_price_start_x96 = swap_state.sqrt_price_x96

        if not snapshot.sparse:
            step.tick_next, step.initialized = next(ticks)
        else:
            step.tick_next, step.initialized = snapshot.next_initialized_tick(
                tick=swap_state.tick,
                zero_for_one=zero_for_one,
            )

        # Clamp to global min/max tick bounds
        step.tick_next = (
            max(-887272, step.tick_next) if zero_for_one else min(887272, step.tick_next)
        )

        step.sqrt_price_next_x96 = get_sqrt_ratio_at_tick(step.tick_next)

        # Determine current liquidity range boundaries
        if zero_for_one:
            tick_lower, tick_upper = step.tick_next, step.tick_next + snapshot.tick_spacing
        else:
            tick_lower, tick_upper = step.tick_next - snapshot.tick_spacing, step.tick_next

        assert tick_lower < tick_upper, f"{tick_lower} should be < {tick_upper}"

        swap_state.sqrt_price_x96, step.amount_in, step.amount_out, step.fee_amount = (
            compute_swap_step(
                sqrt_ratio_x96_current=swap_state.sqrt_price_x96,
                sqrt_ratio_x96_target=_get_sqrt_price_target(
                    zero_for_one=zero_for_one,
                    sqrt_price_next_x96=step.sqrt_price_next_x96,
                    sqrt_price_limit_x96=sqrt_price_limit_x96,
                ),
                liquidity=swap_state.liquidity,
                amount_remaining=swap_state.amount_specified_remaining,
                fee_pips=fee,
            )
        )

        if exact_input:
            swap_state.amount_specified_remaining -= step.amount_in + step.fee_amount
            swap_state.amount_calculated -= step.amount_out
        else:
            swap_state.amount_specified_remaining += step.amount_out
            swap_state.amount_calculated += step.amount_in + step.fee_amount

        if swap_state.sqrt_price_x96 == step.sqrt_price_next_x96:
            if step.initialized:
                tick_info = snapshot.tick_data[step.tick_next]
                liquidity_net = cast("_HasLiquidityNet", tick_info).liquidity_net
                swap_state = dataclasses.replace(
                    swap_state,
                    liquidity=swap_state.liquidity
                    + (-liquidity_net if zero_for_one else liquidity_net),
                )
            swap_state = dataclasses.replace(
                swap_state,
                tick=step.tick_next - 1 if zero_for_one else step.tick_next,
            )

        elif swap_state.sqrt_price_x96 != step.sqrt_price_start_x96:
            swap_state = dataclasses.replace(
                swap_state,
                tick=get_tick_at_sqrt_ratio(swap_state.sqrt_price_x96),
            )

    amount0, amount1 = (
        (
            amount_specified - swap_state.amount_specified_remaining,
            swap_state.amount_calculated,
        )
        if zero_for_one == exact_input
        else (
            swap_state.amount_calculated,
            amount_specified - swap_state.amount_specified_remaining,
        )
    )

    return SwapResult(
        amount0=amount0,
        amount1=amount1,
        sqrt_price_x96=swap_state.sqrt_price_x96,
        liquidity=swap_state.liquidity,
        tick=swap_state.tick,
    )
