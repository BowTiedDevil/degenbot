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
    V4PoolKey,
)
from degenbot.types.pool_protocols import ConstantProductPool
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


def build_swap_amount(
    pool: object,
    swap_vector: SwapVector,
    amount_in: int,
    amount_out: int,
) -> AbstractSwapAmounts:
    zfo = swap_vector.zero_for_one

    # V4 — most specific (has pool_id, pool_key, hook_address)
    if isinstance(pool, UniswapV4Pool):
        limit = MIN_SQRT_RATIO + 1 if zfo else MAX_SQRT_RATIO - 1
        return UniswapV4PoolSwapAmounts(
            address=pool.address,
            id=pool.pool_id,
            pool_key=V4PoolKey(
                currency0=pool.token0.address,
                currency1=pool.token1.address,
                fee=pool.fee,
                tick_spacing=pool.tick_spacing,
                hooks=pool.hook_address,
            ),
            amount_in=amount_in,
            amount_out=amount_out,
            amount_specified=amount_in,
            zero_for_one=zfo,
            sqrt_price_limit_x96=limit,
        )

    # V3 — concentrated liquidity (has sqrt_price_x96, tick, etc.)
    if isinstance(pool, UniswapV3Pool):
        limit = MIN_SQRT_RATIO + 1 if zfo else MAX_SQRT_RATIO - 1
        return UniswapV3PoolSwapAmounts(
            pool=pool.address,
            amount_in=amount_in,
            amount_out=amount_out,
            amount_specified=amount_in,
            zero_for_one=zfo,
            sqrt_price_limit_x96=limit,
        )

    # V2 — constant product (has directional fees, reserves)
    # Use protocol check to catch all V2-style pools including test doubles
    # that satisfy the ConstantProductPool shape
    if isinstance(pool, UniswapV2Pool) or isinstance(pool, ConstantProductPool):
        return UniswapV2PoolSwapAmounts(
            pool=pool.address,
            amounts_in=(amount_in, 0) if zfo else (0, amount_in),
            amounts_out=(0, amount_out) if zfo else (amount_out, 0),
        )

    msg = f"No swap amount builder for pool type {type(pool).__name__}"
    raise TypeError(msg)
