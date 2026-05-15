"""
Standalone swap amount construction.

Delegates to pool.build_swap_amount() for protocol-based dispatch.
Kept for backward compatibility; prefer calling pool.build_swap_amount() directly.
"""

from degenbot.arbitrage.path.types import SwapVector
from degenbot.arbitrage.types import AbstractSwapAmounts


def build_swap_amount(
    pool: object,
    swap_vector: SwapVector,
    amount_in: int,
    amount_out: int,
) -> AbstractSwapAmounts:
    return pool.build_swap_amount(swap_vector.zero_for_one, amount_in, amount_out)
