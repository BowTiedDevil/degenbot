from fractions import Fraction

from degenbot.arbitrage.path.pool_adapter import register_pool_adapter
from degenbot.arbitrage.path.types import SwapVector
from degenbot.arbitrage.solver.types import ConcentratedLiquidityHopState, HopState, TickRangeState
from degenbot.arbitrage.types import (
    AbstractSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
)
from degenbot.types.abstract import AbstractConcentratedLiquidityPool
from degenbot.uniswap.v3_libraries.tick_bitmap import gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import UniswapV3PoolState
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import UniswapV4PoolState

Q96 = 2**96

CLPool = UniswapV3Pool | UniswapV4Pool
CLPoolState = UniswapV3PoolState | UniswapV4PoolState


def _v3_virtual_reserves(
    liquidity: int,
    sqrt_price_x96: int,
    *,
    zero_for_one: bool,
) -> tuple[int, int]:
    x_virtual = liquidity * Q96 * Q96 // sqrt_price_x96
    y_virtual = liquidity * sqrt_price_x96
    if zero_for_one:
        return x_virtual, y_virtual
    return y_virtual, x_virtual


_TICK_RANGE_CACHE: dict[tuple[str, int, bool], tuple[tuple[TickRangeState, ...], int] | None] = {}
_MAX_TICK_RANGE_CACHE_SIZE = 128


def _get_tick_ranges(
    pool: CLPool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[TickRangeState, ...], int] | None:
    cache_key = (str(pool.address), pool.tick, zero_for_one)

    if cache_key in _TICK_RANGE_CACHE:
        return _TICK_RANGE_CACHE[cache_key]

    result = _compute_tick_ranges(pool, zero_for_one, max_ranges)

    if len(_TICK_RANGE_CACHE) >= _MAX_TICK_RANGE_CACHE_SIZE:
        _TICK_RANGE_CACHE.clear()

    _TICK_RANGE_CACHE[cache_key] = result
    return result


def _compute_tick_ranges(
    pool: CLPool,
    *,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[TickRangeState, ...], int] | None:

    if getattr(pool, "sparse_liquidity_map", True):
        return None

    tick_data = getattr(pool, "tick_data", None)
    tick_bitmap = getattr(pool, "tick_bitmap", None)
    tick_spacing = getattr(pool, "tick_spacing", 0)

    if tick_data is None or tick_bitmap is None or tick_spacing == 0:
        return None

    current_tick = pool.tick
    less_than_or_equal = not zero_for_one

    try:
        ticks_along_path = gen_ticks(
            tick_data=tick_data,
            starting_tick=current_tick,
            tick_spacing=tick_spacing,
            less_than_or_equal=less_than_or_equal,
        )
    except Exception:
        return None

    initialized_ticks: list[int] = []
    try:
        for tick, is_initialized in ticks_along_path:
            clamped_tick = max(MIN_TICK, tick) if less_than_or_equal else min(MAX_TICK, tick)
            if clamped_tick != tick:
                break
            if len(initialized_ticks) >= max_ranges + 1:
                break
            if is_initialized or tick == current_tick:
                initialized_ticks.append(tick)
    except StopIteration:
        pass

    if len(initialized_ticks) < 2:
        return None

    ranges: list[TickRangeState] = []
    current_idx = 0

    for i in range(len(initialized_ticks) - 1):
        if zero_for_one:
            tick_lower = initialized_ticks[i + 1]
            tick_upper = initialized_ticks[i]
        else:
            tick_lower = initialized_ticks[i]
            tick_upper = initialized_ticks[i + 1]

        tick_info = tick_data.get(tick_lower if zero_for_one else tick_upper)
        liquidity = tick_info.liquidity_net if tick_info else pool.liquidity

        sqrt_price_lower = int(get_sqrt_ratio_at_tick(tick_lower))
        sqrt_price_upper = int(get_sqrt_ratio_at_tick(tick_upper))

        ranges.append(
            TickRangeState(
                tick_lower=tick_lower,
                tick_upper=tick_upper,
                liquidity=liquidity,
                sqrt_price_lower=sqrt_price_lower,
                sqrt_price_upper=sqrt_price_upper,
            )
        )

        if tick_lower <= current_tick < tick_upper:
            current_idx = i

    if len(ranges) < 1:
        return None

    return (tuple(ranges), current_idx)


class ConcentratedLiquidityAdapter:
    def extract_fee(
        self,
        pool: CLPool,
        *,
        zero_for_one: bool,
    ) -> Fraction:
        return Fraction(pool.fee, pool.FEE_DENOMINATOR)

    def to_hop_state(
        self,
        pool: CLPool,
        *,
        zero_for_one: bool,
        state_override: CLPoolState | None = None,
    ) -> HopState:
        state = state_override or pool.state
        fee = self.extract_fee(pool, zero_for_one=zero_for_one)

        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=state.liquidity,
            sqrt_price_x96=state.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )

        if state_override is None:
            tick_ranges = _get_tick_ranges(pool, zero_for_one)
            if tick_ranges is not None:
                ranges, current_idx = tick_ranges
                return ConcentratedLiquidityHopState(
                    reserve_in=reserve_in,
                    reserve_out=reserve_out,
                    fee=fee,
                    liquidity=state.liquidity,
                    sqrt_price=state.sqrt_price_x96,
                    tick_lower=state.tick,
                    tick_upper=state.tick,
                    tick_ranges=ranges,
                    current_range_index=current_idx,
                )

        return ConcentratedLiquidityHopState(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=state.tick,
            tick_upper=state.tick,
        )

    @staticmethod
    def build_swap_amount(
        pool: CLPool,
        swap_vector: SwapVector,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts:

        zfo = swap_vector.zero_for_one
        limit = MIN_SQRT_RATIO + 1 if zfo else MAX_SQRT_RATIO - 1

        if isinstance(pool, UniswapV4Pool):
            return UniswapV4PoolSwapAmounts(
                address=pool.address,
                id=pool.pool_id,
                amount_in=amount_in,
                amount_out=amount_out,
                amount_specified=amount_in,
                zero_for_one=zfo,
                sqrt_price_limit_x96=limit,
            )

        return UniswapV3PoolSwapAmounts(
            pool=pool.address,
            amount_in=amount_in,
            amount_out=amount_out,
            amount_specified=amount_in,
            zero_for_one=zfo,
            sqrt_price_limit_x96=limit,
        )


_ADAPTER = ConcentratedLiquidityAdapter()
register_pool_adapter(AbstractConcentratedLiquidityPool, _ADAPTER)
