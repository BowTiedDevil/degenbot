"""Uniswap V3 SwapMath: swap step amounts and fees.

Rust-accelerated implementation used by default via wrapper that
preserves the keyword-argument API.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (SwapMath library)
"""

from degenbot.degenbot_rs import cl_compute_swap_step_v3 as _rs_compute_swap_step
from degenbot.exceptions.pool import EVMRevertError

from degenbot.uniswap.v3_types import SqrtPriceX96

type AmountIn = int
type AmountOut = int
type FeeTaken = int


def compute_swap_step(
    sqrt_ratio_x96_current: int,
    sqrt_ratio_x96_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[SqrtPriceX96, AmountIn, AmountOut, FeeTaken]:
    """Compute swap step.

    Delegates to Rust implementation.

    Returns:
        A tuple of (sqrt_ratio_x96_next, amount_in, amount_out, fee_taken).

    """
    try:
        return _rs_compute_swap_step(
            sqrt_ratio_x96_current,
            sqrt_ratio_x96_target,
            liquidity,
            amount_remaining,
            fee_pips,
        )
    except (ValueError, OverflowError) as e:
        raise EVMRevertError(error=str(e)) from e
