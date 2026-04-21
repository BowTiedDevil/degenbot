from fractions import Fraction

from scipy.optimize import minimize_scalar

from degenbot.arbitrage.optimizers.mobius import HopState, V3TickRangeHop, simulate_path
from degenbot.arbitrage.optimizers.solver import Hop, SolveInput
from degenbot.arbitrage.optimizers.v3_tick_predictor import tick_to_sqrt_price
from degenbot.degenbot_rs import mobius as rs_mobius

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


def make_2hop_v2_input(
    reserve_in_buy=USDC_1_5M,
    reserve_out_buy=WETH_800,
    reserve_in_sell=WETH_1000,
    reserve_out_sell=USDC_2M,
    fee=FEE_0_3_PCT,
) -> SolveInput:
    """Create a standard 2-hop V2-V2 arbitrage input.

    Pool 1 (buy): buy WETH where it's cheap (lower USDC/WETH price)
    Pool 2 (sell): sell WETH where it's expensive (higher USDC/WETH price)

    Default: Pool 1 = 1.5M USDC / 800 WETH ($1875/WETH)
             Pool 2 = 2M USDC / 1000 WETH ($2000/WETH)
    """
    return SolveInput(
        hops=(
            Hop(reserve_in=reserve_in_buy, reserve_out=reserve_out_buy, fee=fee),
            Hop(reserve_in=reserve_in_sell, reserve_out=reserve_out_sell, fee=fee),
        )
    )


def make_v3_tick_range(
    liquidity: float,
    current_tick: int,
    tick_spacing: int = 60,
    fee: float = 0.003,
    *,
    zero_for_one: bool = True,
) -> V3TickRangeHop:
    """Create a V3TickRangeHop centered at current_tick with given tick_spacing."""
    tick_lower = (current_tick // tick_spacing) * tick_spacing
    tick_upper = tick_lower + tick_spacing

    sqrt_price_current = tick_to_sqrt_price(current_tick)
    sqrt_price_lower = tick_to_sqrt_price(tick_lower)
    sqrt_price_upper = tick_to_sqrt_price(tick_upper)

    return V3TickRangeHop(
        liquidity=liquidity,
        sqrt_price_current=sqrt_price_current,
        sqrt_price_lower=sqrt_price_lower,
        sqrt_price_upper=sqrt_price_upper,
        fee=fee,
        zero_for_one=zero_for_one,
    )
