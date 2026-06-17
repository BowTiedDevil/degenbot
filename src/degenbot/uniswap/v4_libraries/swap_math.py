"""Uniswap V4 SwapMath: swap step amounts and fees.

Rust-accelerated implementation used by default via wrapper that
preserves the keyword-argument API.

See: contract_reference/uniswap/V4/PoolManager.sol (SwapMath library)
"""

from degenbot.degenbot_rs import cl_compute_swap_step_v4 as _rs_compute_swap_step
from degenbot.exceptions.pool import EVMRevertError

MAX_SWAP_FEE = 1 * 10**6


def get_sqrt_price_target(
    *,
    zero_for_one: bool,
    sqrt_price_next_x96: int,
    sqrt_price_limit_x96: int,
) -> int:
    """Compute the price target for the next swap step.

    Returns:
        The target sqrt price for the next swap step.

    @dev This simplified implementation replicates the gas optimized Yul used by the Solidity
    contract.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/SwapMath.sol

    """
    return (
        max(sqrt_price_next_x96, sqrt_price_limit_x96)
        if zero_for_one
        else min(sqrt_price_next_x96, sqrt_price_limit_x96)
    )


def compute_swap_step(
    *,
    sqrt_ratio_x96_current: int,
    sqrt_ratio_x96_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute the result of swapping some amount in, or amount out.

    Delegates to Rust implementation. Preserves keyword-argument API
    for compatibility with V4 simulator.

    Returns:
        A tuple of (sqrt_price_next_x96, amount_in, amount_out, fee_amount).

    Raises:
        EVMRevertError: If the Rust computation fails.

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
