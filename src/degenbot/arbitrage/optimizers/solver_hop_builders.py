"""Convert pool objects into solver-compatible Hop representations."""

from fractions import Fraction
from typing import Any

from degenbot.aerodrome.functions import calc_exact_in_stable as _aerodrome_stable_calc
from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.optimizers._v3_utils import _get_cached_tick_ranges, _v3_virtual_reserves
from degenbot.arbitrage.optimizers.hop_types import SolveInput
from degenbot.camelot.functions import get_y_camelot, k_camelot
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.solidly.solidly_functions import general_calc_exact_in_stable
from degenbot.types.hop_types import (
    BoundedProductHop,
    ConstantProductHop,
    HopType,
    SolidlyStableHop,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


def pool_to_hop(
    pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool | CamelotLiquidityPool,
    input_token: Erc20Token,
) -> HopType:
    """
    Convert a pool object to a Hop for the solver.

    For V2/Aerodrome volatile pools: returns ConstantProductHop with actual reserves.
    For Aerodrome stable pools: returns SolidlyStableHop with decimals.
    For Camelot volatile pools: returns ConstantProductHop with asymmetric fees.
    For Camelot stable pools: returns SolidlyStableHop with decimals.
    For V3/V4 pools: returns BoundedProductHop with virtual reserves.

    Parameters
    ----------
    pool
        A UniswapV2Pool, AerodromeV2Pool, UniswapV3Pool, or UniswapV4Pool.
    input_token
        The token being deposited into this pool.

    Returns
    -------
    Hop
        A Hop with reserves oriented for the swap direction.
    """
    zero_for_one = input_token == pool.token0

    # Camelot stable pool — Solidly invariant
    if isinstance(pool, CamelotLiquidityPool) and getattr(pool, "stable_swap", False):
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals

        # Build swap_fn using Camelot's get_y
        reserves0 = pool.state.reserves_token0
        reserves1 = pool.state.reserves_token1
        decimals0 = 10**pool.token0.decimals
        decimals1 = 10**pool.token1.decimals
        fee = pool.fee
        token_in = 0 if zero_for_one else 1

        def _camelot_stable_swap_fn(
            amount_in: int,
            __reserves0: int = reserves0,
            __reserves1: int = reserves1,
            __decimals0: int = decimals0,
            __decimals1: int = decimals1,
            __fee: Fraction = fee,
            __token_in: int = token_in,
        ) -> int:
            return general_calc_exact_in_stable(
                amount_in=amount_in,
                token_in=__token_in,  # type: ignore[arg-type]
                reserves0=__reserves0,
                reserves1=__reserves1,
                decimals0=__decimals0,
                decimals1=__decimals1,
                fee=__fee,
                k_func=k_camelot,
                get_y_func=get_y_camelot,
            )

        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
            swap_fn=_camelot_stable_swap_fn,
        )

    # Camelot volatile pool — constant product with asymmetric fees
    if isinstance(pool, CamelotLiquidityPool):
        # Camelot stores fee as tuple: (Fraction(fee_token0, denom), Fraction(fee_token1, denom))
        # pool.fee is the tuple from super().__init__
        fee_tuple = pool.fee
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            fee_in = fee_tuple[0]  # fee for token0 → token1
            fee_out = fee_tuple[1]  # fee for token1 → token0
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            fee_in = fee_tuple[1]  # fee for token1 → token0
            fee_out = fee_tuple[0]  # fee for token0 → token1
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_in,
            fee_out=fee_out,
        )

    # Aerodrome stable pool — Solidly invariant
    if isinstance(pool, AerodromeV2Pool) and getattr(pool, "stable", False):
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals

        # Build swap_fn using Aerodrome's calc_exact_in_stable
        reserves0 = pool.state.reserves_token0
        reserves1 = pool.state.reserves_token1
        decimals0 = 10**pool.token0.decimals
        decimals1 = 10**pool.token1.decimals
        fee = pool.fee
        token_in = 0 if zero_for_one else 1

        def _aerodrome_stable_swap_fn(
            amount_in: int,
            __reserves0: int = reserves0,
            __reserves1: int = reserves1,
            __decimals0: int = decimals0,
            __decimals1: int = decimals1,
            __fee: Fraction = fee,
            __token_in: int = token_in,
        ) -> int:
            return _aerodrome_stable_calc(
                amount_in=amount_in,
                token_in=__token_in,  # type: ignore[arg-type]
                reserves0=__reserves0,
                reserves1=__reserves1,
                decimals0=__decimals0,
                decimals1=__decimals1,
                fee=__fee,
            )

        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
            swap_fn=_aerodrome_stable_swap_fn,
        )

    if isinstance(pool, UniswapV3Pool):
        # V3: virtual reserves from L and sqrt_price_x96
        # Fee is stored as int pip (e.g. 3000 = 0.3%), denominator 1_000_000
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=pool.liquidity,
            sqrt_price_x96=pool.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
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
            fee=fee_fraction,
            liquidity=pool.liquidity,
            sqrt_price=pool.sqrt_price_x96,
            tick_lower=pool.tick,
            tick_upper=pool.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
            zero_for_one=zero_for_one,
        )

    if isinstance(pool, UniswapV4Pool):
        # V4: same structure as V3 (concentrated liquidity)
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=pool.liquidity,
            sqrt_price_x96=pool.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
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
            fee=fee_fraction,
            liquidity=pool.liquidity,
            sqrt_price=pool.sqrt_price_x96,
            tick_lower=pool.tick,
            tick_upper=pool.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
            zero_for_one=zero_for_one,
        )

    # V2 pool (UniswapV2Pool or AerodromeV2Pool volatile) — actual reserves
    if zero_for_one:
        reserve_in = pool.state.reserves_token0
        reserve_out = pool.state.reserves_token1
    else:
        reserve_in = pool.state.reserves_token1
        reserve_out = pool.state.reserves_token0

    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=pool.fee,
    )


def pool_state_to_hop(
    pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool | CamelotLiquidityPool,
    input_token: Erc20Token,
    state_override: Any = None,
) -> HopType:
    """
    Convert a pool object to a Hop, with optional state override.

    Like pool_to_hop() but accepts a PoolState override for the pool's
    current state (used when simulating a different reserve configuration).

    Parameters
    ----------
    pool
        A UniswapV2Pool, AerodromeV2Pool, UniswapV3Pool, or UniswapV4Pool.
    input_token
        The token being deposited into this pool.
    state_override
        Optional PoolState to use instead of pool.state.

    Returns
    -------
    Hop
        A Hop with reserves oriented for the swap direction.
    """
    state = state_override or pool.state
    zero_for_one = input_token == pool.token0

    # Aerodrome stable pool — Solidly invariant
    if isinstance(pool, AerodromeV2Pool) and getattr(pool, "stable", False):
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
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
        )

    if isinstance(pool, UniswapV3Pool | UniswapV4Pool):
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=state.liquidity,
            sqrt_price_x96=state.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
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
            fee=fee_fraction,
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=state.tick,
            tick_upper=state.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
            zero_for_one=zero_for_one,
        )

    # V2 pool (UniswapV2Pool or AerodromeV2Pool volatile)
    if zero_for_one:
        reserve_in = state.reserves_token0
        reserve_out = state.reserves_token1
    else:
        reserve_in = state.reserves_token1
        reserve_out = state.reserves_token0

    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=pool.fee,
    )


def pools_to_solve_input(
    pools: list,
    input_token: Erc20Token,
    max_input: int | None = None,
) -> SolveInput:
    """
    Convert a list of pool objects to a SolveInput.

    Parameters
    ----------
    pools
        Ordered list of pools in the arbitrage path.
    input_token
        The input (profit) token.
    max_input
        Optional maximum input constraint.

    Returns
    -------
    SolveInput
        Solver input with Hop for each pool.
    """
    hops: list[HopType] = []
    current_token = input_token

    for pool in pools:
        hop = pool_to_hop(pool, current_token)
        hops.append(hop)
        # Advance the current token
        current_token = pool.token1 if current_token == pool.token0 else pool.token0

    return SolveInput(hops=tuple(hops), max_input=max_input)
