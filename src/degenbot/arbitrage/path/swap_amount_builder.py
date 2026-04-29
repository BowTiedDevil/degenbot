"""
Standalone swap amount construction.

Replaces the adapter-based build_swap_amount with a function that
dispatches on pool type.
"""

from degenbot.arbitrage.path.types import SwapVector
from degenbot.arbitrage.types import (
    AbstractSwapAmounts,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
)
from degenbot.types.abstract import (
    AbstractAerodromeV2Pool,
    AbstractConcentratedLiquidityPool,
    AbstractUniswapV2Pool,
)
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


def build_swap_amount(
    pool: object,
    swap_vector: SwapVector,
    amount_in: int,
    amount_out: int,
) -> AbstractSwapAmounts:
    zfo = swap_vector.zero_for_one

    if isinstance(pool, AbstractUniswapV2Pool):
        return UniswapV2PoolSwapAmounts(
            pool=pool.address,
            amounts_in=(amount_in, 0) if zfo else (0, amount_in),
            amounts_out=(0, amount_out) if zfo else (amount_out, 0),
        )

    if isinstance(pool, AbstractConcentratedLiquidityPool):
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

    if isinstance(pool, AbstractAerodromeV2Pool):
        msg = "Aerodrome swap amount construction not yet supported"
        raise NotImplementedError(msg)

    msg = f"No swap amount builder for pool type {type(pool).__name__}"
    raise TypeError(msg)
