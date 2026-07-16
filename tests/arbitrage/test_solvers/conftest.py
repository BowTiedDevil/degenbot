from fractions import Fraction

from degenbot.types.hop_types import ConstantProductHop
from degenbot.uniswap.v3_libraries import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_libraries.constants import Q96

# ACDWOC retire: the f64 Möbius-math conftest helpers went with the deleted
# solver stack (the `brent_solve_hops` / `make_v3_tick_range` helpers used
# `_mobius_math.simulate_path` + `MobiusFloatHop` / `V3TickRangeHop` types).
# The realistic-decimal reserve + fee constants below stay — they're shared by
# the kept tag-union tests.

# ==============================================================================
# Shared constants — realistic reserve magnitudes with correct decimals
# ==============================================================================

USDC_DECIMALS = 6
WETH_DECIMALS = 18

USDC_2M = 2_000_000 * 10**USDC_DECIMALS
USDC_1_5M = 1_500_000 * 10**USDC_DECIMALS
WETH_1000 = 1_000 * 10**WETH_DECIMALS
WETH_800 = 800 * 10**WETH_DECIMALS

FEE_0_3_PCT = Fraction(3, 1000)  # 0.3% (Uniswap fee_pips=3000)
FEE_0_05_PCT = Fraction(5, 10000)  # 0.05% (Uniswap fee_pips=500)
FEE_0_5_PCT = Fraction(5, 1000)  # 0.5% (non-standard, high-fee tests)
FEE_1_PCT = Fraction(1, 100)  # 1% (Uniswap fee_pips=10000)


# ==============================================================================
# Shared helpers
# ==============================================================================


def _tick_to_float_sqrt_price(tick: int) -> float:
    """Convert tick to float sqrt price via the canonical integer Q64.96 conversion."""
    return get_sqrt_ratio_at_tick(tick) / Q96


# re-export so existing imports keep working
__all__ = [
    "FEE_0_05_PCT",
    "FEE_0_3_PCT",
    "FEE_0_5_PCT",
    "FEE_1_PCT",
    "USDC_1_5M",
    "USDC_2M",
    "USDC_DECIMALS",
    "WETH_800",
    "WETH_1000",
    "WETH_DECIMALS",
    "ConstantProductHop",
    "_tick_to_float_sqrt_price",
]
