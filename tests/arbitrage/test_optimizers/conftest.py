from fractions import Fraction

from scipy.optimize import minimize_scalar

from degenbot._rs import mobius as rs_mobius
from degenbot.arbitrage.optimizers.mobius import HopState, simulate_path

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


def brent_solve_hops(
    hops: list[HopState],
    xatol: float = 1.0,
) -> tuple[float, float, int]:
    """Solve optimal arbitrage using scipy Brent method."""
    first = hops[0]
    upper = min(first.reserve_in / first.gamma, first.reserve_in * 0.5)

    result = minimize_scalar(
        lambda x: -(simulate_path(x, hops) - x),
        bounds=(1.0, upper),
        method="bounded",
        options={"xatol": xatol},
    )

    x_opt = result.x
    output = simulate_path(x_opt, hops)
    return x_opt, output - x_opt, getattr(result, "nit", 0)


def make_rust_v3_hop(
    liquidity: float,
    sqrt_price: float,
    sqrt_lower: float,
    sqrt_upper: float,
    fee: float,
    *,
    zero_for_one: bool,
) -> rs_mobius.RustV3TickRangeHop:
    """Create a Rust V3TickRangeHop."""
    return rs_mobius.RustV3TickRangeHop(
        liquidity=liquidity,
        sqrt_price_current=sqrt_price,
        sqrt_price_lower=sqrt_lower,
        sqrt_price_upper=sqrt_upper,
        fee=fee,
        zero_for_one=zero_for_one,
    )
