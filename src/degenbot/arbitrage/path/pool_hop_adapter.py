"""Extract ``to_hop_state`` and ``extract_fee`` from pool classes.

Pool classes should not need to know about solver-internal ``HopType``
representations. This module provides pure-function adapters that translate
any supported pool into its solver-compatible hop state.

Duck-typing is preferred: if a pool object already exposes ``to_hop_state``
or ``extract_fee``, those methods are called directly. Named dispatch is used
as a fallback for concrete classes that do not (yet) carry the methods.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

from degenbot.arbitrage.optimizers._v3_utils import _get_cached_tick_ranges, _v3_virtual_reserves
from degenbot.types.hop_types import (
    BoundedProductHop,
    ConstantProductHop,
    HopType,
    SolidlyStableHop,
)

if TYPE_CHECKING:
    from collections.abc import Callable


@runtime_checkable
class _FeeExtractor(Protocol):
    def extract_fee(self, zero_for_one: bool) -> Fraction: ...


@runtime_checkable
class _HopStateExtractor(Protocol):
    def to_hop_state(
        self,
        zero_for_one: bool,
        state_override: Any = None,
    ) -> HopType: ...


def extract_fee(pool: Any, *, zero_for_one: bool) -> Fraction:
    """Extract the trading fee for a swap direction from any supported pool."""
    # Prefer duck-typing — allows test fakes and custom pools to work
    if isinstance(pool, _FeeExtractor):
        return pool.extract_fee(zero_for_one=zero_for_one)

    if (cls_name := type(pool).__name__) == "UniswapV2Pool":
        return Fraction(pool.fee_token0 if zero_for_one else pool.fee_token1)

    if cls_name in {"UniswapV3Pool", "UniswapV4Pool", "AerodromeV2Pool"}:
        return Fraction(pool.fee)

    if cls_name == "CurveStableswapPool":
        return Fraction(pool.fee, pool.FEE_DENOMINATOR)

    if cls_name == "BalancerV2Pool":
        msg = (
            "BalancerV2Pool.extract_fee is not yet implemented. "
            "See architecture candidate #5 for future implementation."
        )
        raise NotImplementedError(
            msg
        )

    if cls_name == "CamelotLiquidityPool":
        fee_tuple = pool.fee  # (fee_0to1, fee_1to0)
        return Fraction(fee_tuple[0] if zero_for_one else fee_tuple[1])

    if hasattr(pool, "fee"):
        return Fraction(pool.fee)

    msg = f"Unsupported pool type for fee extraction: {type(pool).__name__}"
    raise TypeError(msg)


def _v2_like_hop(
    pool: Any,
    *,
    zero_for_one: bool,
    state: Any,
) -> ConstantProductHop:
    fee = extract_fee(pool, zero_for_one=zero_for_one)
    if zero_for_one:
        reserve_in = state.reserves_token0
        reserve_out = state.reserves_token1
    else:
        reserve_in = state.reserves_token1
        reserve_out = state.reserves_token0
    return ConstantProductHop(reserve_in=reserve_in, reserve_out=reserve_out, fee=fee)


def _solidly_stable_hop(
    pool: Any,
    *,
    zero_for_one: bool,
    state: Any,
) -> SolidlyStableHop:
    fee = extract_fee(pool, zero_for_one=zero_for_one)
    if zero_for_one:
        reserve_in = state.reserves_token0
        reserve_out = state.reserves_token1
        decimals_in = pool.token0.decimals
        decimals_out = pool.token1.decimals
    else:
        reserve_in = state.reserves_token1
        reserve_out = state.reserves_token0
        decimals_in = pool.token1.decimals
        decimals_out = pool.token0.decimals
    return SolidlyStableHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=fee,
        decimals_in=decimals_in,
        decimals_out=decimals_out,
    )


def _v3_like_hop(
    pool: Any,
    *,
    zero_for_one: bool,
    state: Any,
) -> BoundedProductHop:
    fee = extract_fee(pool, zero_for_one=zero_for_one)
    reserve_in, reserve_out = _v3_virtual_reserves(
        liquidity=state.liquidity,
        sqrt_price_x96=state.sqrt_price_x96,
        zero_for_one=zero_for_one,
    )
    tick_ranges_result = _get_cached_tick_ranges(
        pool=pool,
        zero_for_one=zero_for_one,
        max_ranges=3,
    )
    if tick_ranges_result is not None:
        tick_ranges, current_range_index = tick_ranges_result
    else:
        tick_ranges = None
        current_range_index = 0

    return BoundedProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=fee,
        liquidity=state.liquidity,
        sqrt_price=state.sqrt_price_x96,
        tick_lower=state.tick,
        tick_upper=state.tick,
        tick_ranges=tick_ranges,
        current_range_index=current_range_index,
        zero_for_one=zero_for_one,
    )


def to_hop_state(
    pool: Any,
    *,
    zero_for_one: bool,
    state_override: Any = None,
) -> HopType:
    """Convert any supported pool to its solver-compatible ``HopType``."""
    # Prefer duck-typing — allows test fakes and custom pools to work
    if isinstance(pool, _HopStateExtractor):
        return pool.to_hop_state(zero_for_one=zero_for_one, state_override=state_override)

    state = state_override or pool.state
    cls_name = type(pool).__name__

    if cls_name in {"UniswapV2Pool", "AerodromeV2Pool"}:
        if cls_name == "AerodromeV2Pool" and getattr(pool, "stable", False):
            return _solidly_stable_hop(pool, zero_for_one=zero_for_one, state=state)
        return _v2_like_hop(pool, zero_for_one=zero_for_one, state=state)

    if cls_name == "CamelotLiquidityPool":
        if getattr(pool, "stable_swap", False):
            return _solidly_stable_hop(pool, zero_for_one=zero_for_one, state=state)
        return _v2_like_hop(pool, zero_for_one=zero_for_one, state=state)

    if cls_name in {"UniswapV3Pool", "UniswapV4Pool"}:
        return _v3_like_hop(pool, zero_for_one=zero_for_one, state=state)

    if cls_name == "CurveStableswapPool":
        balances = state.balances
        if zero_for_one:
            i, j = 0, 1
        else:
            i, j = 1, 0
        if i >= len(balances) or j >= len(balances):
            from degenbot.exceptions import DegenbotValueError

            raise DegenbotValueError(
                message=f"Invalid swap indices ({i}, {j}) for pool with {len(balances)} tokens"
            )
        reserve_in = balances[i]
        reserve_out = balances[j]
        from degenbot.types.hop_types import CurveStableswapHop

        swap_fn: Callable[[int], int] | None = None
        get_dy = getattr(pool, "get_dy", None)
        if callable(get_dy):
            def swap_fn(dx: int) -> int:
                return int(get_dy(i=i, j=j, dx=dx, override_state=state_override))

        return CurveStableswapHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=extract_fee(pool, zero_for_one=zero_for_one),
            curve_a=int(pool.a_coefficient),
            curve_n_coins=len(pool.tokens),
            curve_d=0,
            token_index_in=i,
            token_index_out=j,
            precisions=tuple(pool.precision_multipliers),
            swap_fn=swap_fn,
        )

    if cls_name == "BalancerV2Pool":
        msg = (
            "BalancerV2Pool.to_hop_state is not yet implemented. "
            "See architecture candidate #5 for future implementation."
        )
        raise NotImplementedError(
            msg
        )

    msg = f"Unsupported pool type for hop state extraction: {type(pool).__name__}"
    raise TypeError(msg)
