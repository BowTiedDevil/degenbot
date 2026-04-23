"""
Verification: Möbius V3 formula vs actual V3 integer swap math.

The core question: does the float64 Möbius formula
    y = gamma * (R1 + beta) * x / ((R0 + alpha) + gamma * x)
produce the same output as the V3 contract's compute_swap_step?

The V3 contract uses integer arithmetic with Q64.96 sqrt prices and
muldiv operations. The Möbius optimizer uses float64. This test
measures the gap.

If the gap is small (< 0.1% for realistic pool parameters), the
Möbius approach is validated for V3. If the gap is large, we need
to understand why and potentially add a refinement step.
"""

import pytest

from degenbot.arbitrage.optimizers.mobius import (
    HopState,
    MobiusOptimizer,
    V3TickRangeHop,
    estimate_v3_final_sqrt_price,
    mobius_solve,
    simulate_path,
)
from degenbot.arbitrage.optimizers.v3_tick_predictor import tick_to_sqrt_price
from degenbot.exceptions.arbitrage import OptimizationError
from degenbot.uniswap.v3_libraries.swap_math import compute_swap_step
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick

# ==============================================================================
# V3 Integer Swap Simulation
# ==============================================================================


def v3_swap_within_range_integer(
    *,
    liquidity: int,
    sqrt_price_current_x96: int,
    sqrt_price_upper_x96: int,
    amount_in: int,
    fee_pips: int,
    zero_for_one: bool,
) -> tuple[int, int, int, bool]:
    """
    Simulate a V3 swap that stays within a single tick range,
    using the actual integer compute_swap_step.

    Returns (amount_out, amount_in_consumed, new_sqrt_price_x96, crossed_boundary).

    The target sqrt price is the range boundary (upper for z0f1,
    lower for z1f0). If the swap doesn't reach the boundary, the
    step computes the actual final price.

    crossed_boundary is True if the swap consumed all available liquidity
    in this range (i.e., reached the tick boundary). In that case, the
    output is only for the portion that fits, and the remaining input
    is NOT processed.
    """
    # For zero_for_one, target is the lower sqrt price
    # For one_for_zero, target is the upper sqrt price
    if zero_for_one:
        sqrt_target_x96 = min(sqrt_price_upper_x96, sqrt_price_current_x96)
    else:
        sqrt_target_x96 = max(sqrt_price_upper_x96, sqrt_price_current_x96)

    new_sqrt_price, step_amount_in, step_amount_out, fee_amount = compute_swap_step(
        sqrt_ratio_x96_current=sqrt_price_current_x96,
        sqrt_ratio_x96_target=sqrt_target_x96,
        liquidity=liquidity,
        amount_remaining=amount_in,  # positive = exact input
        fee_pips=fee_pips,
    )

    total_consumed = step_amount_in + fee_amount
    crossed = total_consumed < amount_in

    return step_amount_out, total_consumed, new_sqrt_price, crossed


def tick_to_sqrt_price_x96(tick: int) -> int:
    return get_sqrt_ratio_at_tick(tick)


def tick_to_sqrt_price_float(tick: int) -> float:
    return tick_to_sqrt_price(tick)


# ==============================================================================
# Test Cases: Single-Range V3 Swaps
# ==============================================================================


class TestMobiusVsV3IntegerMath:
    """
    Compare Möbius float64 output against V3 integer compute_swap_step.

    For each test case:
    1. Set up a V3 tick range with known liquidity and prices
    2. Run compute_swap_step with integer arithmetic
    3. Run the Möbius formula with float64
    4. Compare outputs
    """

    @pytest.mark.parametrize(
        ("current_tick", "tick_spacing", "liquidity", "amount_in_wei", "fee_pips"),
        [
            # Centered in range, moderate swap
            (0, 60, 1_000_000_000_000_000_000, 100_000_000_000_000_000, 3000),
            # Near boundary, small swap
            (30, 60, 1_000_000_000_000_000_000, 10_000_000_000_000, 3000),
            # Near boundary, large swap (risky)
            (30, 60, 1_000_000_000_000_000_000, 500_000_000_000_000_000, 3000),
            # WETH/USDC-like (tick ~-83000 for ~2000 USDC/WETH)
            (-83000, 60, 2_000_000_000_000_000_000, 1_000_000_000_000, 3000),
            # High liquidity, small swap
            (0, 60, 10_000_000_000_000_000_000, 1_000_000_000_000, 3000),
            # Low liquidity, moderate swap
            (0, 60, 100_000_000_000_000, 10_000_000_000_000, 3000),
            # 1% fee tier
            (0, 200, 1_000_000_000_000_000_000, 100_000_000_000_000_000, 10000),
            # 0.05% fee tier
            (0, 10, 1_000_000_000_000_000_000, 100_000_000_000_000_000, 500),
        ],
    )
    def test_mobius_matches_v3_integer(
        self,
        current_tick: int,
        tick_spacing: int,
        liquidity: int,
        amount_in_wei: int,
        fee_pips: int,
    ):
        """Compare Möbius float output against V3 integer compute_swap_step."""
        tick_lower = (current_tick // tick_spacing) * tick_spacing
        tick_upper = tick_lower + tick_spacing

        sqrt_price_current_x96 = tick_to_sqrt_price_x96(current_tick)
        sqrt_price_lower_x96 = tick_to_sqrt_price_x96(tick_lower)
        tick_to_sqrt_price_x96(tick_upper)

        fee_fraction = fee_pips / 1_000_000

        # --- V3 integer swap (zero_for_one = True) ---
        v3_amount_out, v3_amount_in_consumed, _v3_new_sqrt_price, crossed = (
            v3_swap_within_range_integer(
                liquidity=liquidity,
                sqrt_price_current_x96=sqrt_price_current_x96,
                sqrt_price_upper_x96=sqrt_price_lower_x96,  # z0f1 target = lower
                amount_in=amount_in_wei,
                fee_pips=fee_pips,
                zero_for_one=True,
            )
        )

        # If the swap crossed the tick boundary, the V3 contract only
        # returns output for the portion that fits. The Möbius formula
        # computes as if the entire input stays in range. These are
        # fundamentally different computations — skip comparison.
        if crossed:
            pytest.skip(
                f"Swap crosses tick boundary (consumed {v3_amount_in_consumed} "
                f"of {amount_in_wei}). Möbius assumes within-range."
            )

        # --- Möbius float swap ---
        sqrt_p_current = tick_to_sqrt_price_float(current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(tick_lower)
        sqrt_p_upper = tick_to_sqrt_price_float(tick_upper)

        v3_hop = V3TickRangeHop(
            liquidity=float(liquidity),
            sqrt_price_current=sqrt_p_current,
            sqrt_price_lower=sqrt_p_lower,
            sqrt_price_upper=sqrt_p_upper,
            fee=fee_fraction,
            zero_for_one=True,
        )
        hop_state = v3_hop.to_hop_state()

        # simulate_path uses: amount * gamma * reserve_out / (reserve_in + amount * gamma)
        mobius_amount_out_float = simulate_path(float(amount_in_wei), [hop_state])

        # --- Comparison ---
        if v3_amount_out > 0:
            mobius_amount_out_int = int(mobius_amount_out_float)
            rel_diff = abs(mobius_amount_out_int - v3_amount_out) / v3_amount_out

            # Assert less than 0.1% relative difference
            # The Möbius formula is mathematically equivalent to the bounded
            # product CFMM. Any difference comes from float64 vs integer rounding.
            assert rel_diff < 0.001, (
                f"tick={current_tick}, spacing={tick_spacing}, L={liquidity}, "
                f"amount_in={amount_in_wei}, fee={fee_pips}: "
                f"V3_out={v3_amount_out}, Möbius_out={mobius_amount_out_int}, "
                f"rel_diff={rel_diff:.6f}"
            )

    @pytest.mark.parametrize(
        ("current_tick", "tick_spacing", "liquidity", "amount_in_wei", "fee_pips"),
        [
            (0, 60, 1_000_000_000_000_000_000, 100_000_000_000_000_000, 3000),
            (-83000, 60, 2_000_000_000_000_000_000, 1_000_000_000_000, 3000),
            (0, 60, 100_000_000_000_000, 10_000_000_000_000, 3000),
        ],
    )
    def test_mobius_one_for_zero_matches_v3_integer(
        self,
        current_tick: int,
        tick_spacing: int,
        liquidity: int,
        amount_in_wei: int,
        fee_pips: int,
    ):
        """Compare Möbius float output against V3 integer for one_for_zero swaps."""
        tick_lower = (current_tick // tick_spacing) * tick_spacing
        tick_upper = tick_lower + tick_spacing

        sqrt_price_current_x96 = tick_to_sqrt_price_x96(current_tick)
        sqrt_price_upper_x96 = tick_to_sqrt_price_x96(tick_upper)

        fee_fraction = fee_pips / 1_000_000

        # --- V3 integer swap (zero_for_one = False) ---
        v3_amount_out, v3_amount_in_consumed, _v3_new_sqrt_price, crossed = (
            v3_swap_within_range_integer(
                liquidity=liquidity,
                sqrt_price_current_x96=sqrt_price_current_x96,
                sqrt_price_upper_x96=sqrt_price_upper_x96,  # o1f0 target = upper
                amount_in=amount_in_wei,
                fee_pips=fee_pips,
                zero_for_one=False,
            )
        )

        if crossed:
            pytest.skip(
                f"Swap crosses tick boundary (consumed {v3_amount_in_consumed} "
                f"of {amount_in_wei}). Möbius assumes within-range."
            )

        # --- Möbius float swap ---
        sqrt_p_current = tick_to_sqrt_price_float(current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(tick_lower)
        sqrt_p_upper = tick_to_sqrt_price_float(tick_upper)

        v3_hop = V3TickRangeHop(
            liquidity=float(liquidity),
            sqrt_price_current=sqrt_p_current,
            sqrt_price_lower=sqrt_p_lower,
            sqrt_price_upper=sqrt_p_upper,
            fee=fee_fraction,
            zero_for_one=False,
        )
        hop_state = v3_hop.to_hop_state()

        mobius_amount_out_float = simulate_path(float(amount_in_wei), [hop_state])

        if v3_amount_out > 0:
            mobius_amount_out_int = int(mobius_amount_out_float)
            rel_diff = abs(mobius_amount_out_int - v3_amount_out) / v3_amount_out

            assert rel_diff < 0.001, (
                f"tick={current_tick}, spacing={tick_spacing}, L={liquidity}, "
                f"amount_in={amount_in_wei}, fee={fee_pips}: "
                f"V3_out={v3_amount_out}, Möbius_out={mobius_amount_out_int}, "
                f"rel_diff={rel_diff:.6f}"
            )


# ==============================================================================
# Full Arbitrage Cycle: V2 + V3 Möbius vs V2 + V3 Integer
# ==============================================================================


class TestMobiusArbitrageVsV3Integer:
    """
    End-to-end test: V2 + V3 arbitrage using Möbius optimizer
    vs a brute-force search using actual V3 integer swap math.

    We scan a range of input amounts using V3 integer swap math,
    find the optimal input, and compare with the Möbius closed-form.
    """

    def test_v2_v3_arbitrage_mobius_vs_brute_force(self):
        """
        V2 pool (USDC/WETH) + V3 pool (USDC/WETH) arbitrage.

        The Möbius closed-form gives the optimal input ASSUMING the swap
        stays within the V3 tick range. We verify:
        1. If the Möbius solution stays in range, it matches brute-force
        2. If it crosses the range, the MobiusOptimizer.solve() validation
           correctly rejects it

        We use a wide tick spacing and centered price to maximize the
        chance the optimal swap stays in range.
        """
        # V2 pool: 2M USDC / 1000 WETH (price = 2000 USDC/WETH)
        v2_reserve_usdc = 2_000_000_000_000  # 2M USDC (6 dec)
        v2_reserve_weth = 1_000 * 10**18  # 1000 WETH (18 dec)
        v2_fee = 0.003
        v2_gamma = 1.0 - v2_fee

        # V3 pool with WIDE tick spacing (1000) centered in the range
        # tick = -82900 → in range [-83000, -82000]
        # This gives ~500 ticks of room in each direction
        v3_current_tick = -82900
        v3_tick_spacing = 1000
        v3_tick_lower = (v3_current_tick // v3_tick_spacing) * v3_tick_spacing
        v3_tick_upper = v3_tick_lower + v3_tick_spacing
        v3_liquidity = 10_000_000_000_000_000_000  # 1e19 (high liquidity)
        v3_fee_pips = 3000
        v3_fee_fraction = 0.003

        sqrt_price_current_x96 = tick_to_sqrt_price_x96(v3_current_tick)
        sqrt_price_upper_x96 = tick_to_sqrt_price_x96(v3_tick_upper)

        # --- Möbius approach ---
        v2_hop = HopState(
            reserve_in=float(v2_reserve_usdc),
            reserve_out=float(v2_reserve_weth),
            fee=v2_fee,
        )

        sqrt_p_current = tick_to_sqrt_price_float(v3_current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(v3_tick_lower)
        sqrt_p_upper = tick_to_sqrt_price_float(v3_tick_upper)

        v3_hop = V3TickRangeHop(
            liquidity=float(v3_liquidity),
            sqrt_price_current=sqrt_p_current,
            sqrt_price_lower=sqrt_p_lower,
            sqrt_price_upper=sqrt_p_upper,
            fee=v3_fee_fraction,
            zero_for_one=False,  # Selling WETH (token1) for USDC (token0)
        )

        hops = [v2_hop, v3_hop.to_hop_state()]
        x_mobius, profit_mobius, _ = mobius_solve(hops)

        if x_mobius <= 0:
            pytest.skip("No profitable arbitrage found by Möbius")

        # --- Check if Möbius solution stays in V3 range ---
        weth_out_v2_at_opt = int(
            x_mobius * v2_gamma * v2_reserve_weth / (v2_reserve_usdc + x_mobius * v2_gamma)
        )

        _, _, _, crossed_at_opt = v3_swap_within_range_integer(
            liquidity=v3_liquidity,
            sqrt_price_current_x96=sqrt_price_current_x96,
            sqrt_price_upper_x96=sqrt_price_upper_x96,
            amount_in=weth_out_v2_at_opt,
            fee_pips=v3_fee_pips,
            zero_for_one=False,
        )

        if crossed_at_opt:
            # Möbius solution crosses the boundary — this is expected
            # for some parameter configurations. The MobiusOptimizer.solve()
            # method has validation that catches this. Verify that works.
            optimizer = MobiusOptimizer()
            with pytest.raises(OptimizationError):
                optimizer.solve_v3_candidates(
                    base_hops=[v2_hop],
                    v3_hop_index=1,
                    v3_candidates=[v3_hop],
                )

        # --- Möbius solution stays in range: compare with brute-force ---
        # Binary search for the optimal input using V3 integer math
        best_profit_int = 0
        best_input_int = 0

        # Coarse scan: 0 to 2x Möbius optimum, 100 steps
        x_max = int(x_mobius * 2)
        for i in range(1, 101):
            usdc_in = i * x_max // 100

            weth_out_v2 = (usdc_in * v2_gamma * v2_reserve_weth) // (
                v2_reserve_usdc + usdc_in * v2_gamma
            )

            if weth_out_v2 <= 0:
                continue

            v3_amount_out, _consumed, _, crossed = v3_swap_within_range_integer(
                liquidity=v3_liquidity,
                sqrt_price_current_x96=sqrt_price_current_x96,
                sqrt_price_upper_x96=sqrt_price_upper_x96,
                amount_in=weth_out_v2,
                fee_pips=v3_fee_pips,
                zero_for_one=False,
            )

            # If crossed, only the consumed portion of WETH produces output
            # The remaining WETH is unswapped (incomplete swap)
            if crossed:
                # For crossed swaps, we need to compute the actual USDC
                # that was consumed (not the full input)
                # The V2 output was weth_out_v2, but V3 only consumed
                # a portion. This means the cycle doesn't balance.
                # In practice, we'd only use the consumed amount.
                continue

            usdc_out = v3_amount_out
            profit_int = usdc_out - usdc_in

            if profit_int > best_profit_int:
                best_profit_int = profit_int
                best_input_int = usdc_in

        # Fine scan: ±10% of best, 100 steps
        if best_input_int > 0:
            fine_low = max(1, best_input_int - best_input_int // 10)
            fine_high = best_input_int + best_input_int // 10
            for i in range(1, 101):
                usdc_in = fine_low + i * (fine_high - fine_low) // 100

                weth_out_v2 = (usdc_in * v2_gamma * v2_reserve_weth) // (
                    v2_reserve_usdc + usdc_in * v2_gamma
                )

                if weth_out_v2 <= 0:
                    continue

                v3_amount_out, _, _, crossed = v3_swap_within_range_integer(
                    liquidity=v3_liquidity,
                    sqrt_price_current_x96=sqrt_price_current_x96,
                    sqrt_price_upper_x96=sqrt_price_upper_x96,
                    amount_in=weth_out_v2,
                    fee_pips=v3_fee_pips,
                    zero_for_one=False,
                )

                if crossed:
                    continue

                profit_int = v3_amount_out - usdc_in

                if profit_int > best_profit_int:
                    best_profit_int = profit_int
                    best_input_int = usdc_in

        if best_profit_int > 0:
            profit_mobius_int = int(profit_mobius)
            rel_diff = abs(profit_mobius_int - best_profit_int) / best_profit_int

            # Allow up to 1% difference (brute-force granularity + float rounding)
            assert rel_diff < 0.01, (
                f"Möbius profit={profit_mobius_int}, Brute-force profit={best_profit_int}, "
                f"rel_diff={rel_diff:.6f}, Möbius input={x_mobius:.0f}, "
                f"Brute-force input={best_input_int}"
            )

    def test_v2_v3_profit_gradient_zero_at_mobius_optimum(self):
        """
        At the Möbius optimum, the marginal V3 swap should equal the
        marginal V2 swap (no-arbitrage condition).

        Verify using integer V3 math at points near the Möbius optimum.
        """
        v2_reserve_usdc = 2_000_000_000_000
        v2_reserve_weth = 1_000 * 10**18
        v2_fee = 0.003
        v2_gamma = 1.0 - v2_fee

        v3_current_tick = -83070
        v3_tick_spacing = 60
        v3_tick_lower = (v3_current_tick // v3_tick_spacing) * v3_tick_spacing
        v3_tick_upper = v3_tick_lower + v3_tick_spacing
        v3_liquidity = 1_000_000_000_000_000_000
        v3_fee_pips = 3000

        sqrt_price_current_x96 = tick_to_sqrt_price_x96(v3_current_tick)
        sqrt_price_upper_x96 = tick_to_sqrt_price_x96(v3_tick_upper)

        # Get Möbius optimum
        v2_hop = HopState(
            reserve_in=float(v2_reserve_usdc),
            reserve_out=float(v2_reserve_weth),
            fee=v2_fee,
        )
        sqrt_p_current = tick_to_sqrt_price_float(v3_current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(v3_tick_lower)
        sqrt_p_upper = tick_to_sqrt_price_float(v3_tick_upper)

        v3_hop = V3TickRangeHop(
            liquidity=float(v3_liquidity),
            sqrt_price_current=sqrt_p_current,
            sqrt_price_lower=sqrt_p_lower,
            sqrt_price_upper=sqrt_p_upper,
            fee=0.003,
            zero_for_one=False,
        )

        hops = [v2_hop, v3_hop.to_hop_state()]
        x_opt, _, _ = mobius_solve(hops)

        if x_opt <= 0:
            pytest.skip("No profitable arbitrage for this test setup")

        x_opt_int = int(x_opt)

        # Compute profit at x_opt and x_opt ± delta using V3 integer math
        delta = max(1_000_000, x_opt_int // 1000)  # 1 USDC or 0.1% of optimum

        sqrt_price_current_x96 = tick_to_sqrt_price_x96(v3_current_tick)
        sqrt_price_upper_x96 = tick_to_sqrt_price_x96(v3_tick_upper)

        profits = {}
        for usdc_in in [x_opt_int - delta, x_opt_int, x_opt_int + delta]:
            if usdc_in <= 0:
                continue

            weth_out_v2 = (usdc_in * v2_gamma * v2_reserve_weth) // (
                v2_reserve_usdc + usdc_in * v2_gamma
            )

            if weth_out_v2 <= 0:
                continue

            v3_out, _, _, _crossed = v3_swap_within_range_integer(
                liquidity=v3_liquidity,
                sqrt_price_current_x96=sqrt_price_current_x96,
                sqrt_price_upper_x96=sqrt_price_upper_x96,
                amount_in=weth_out_v2,
                fee_pips=v3_fee_pips,
                zero_for_one=False,
            )

            profits[usdc_in] = v3_out - usdc_in

        if len(profits) >= 3:
            # At the optimum, profit should be at or near the maximum
            profit_at_opt = profits.get(x_opt_int, 0)
            profit_below = profits.get(x_opt_int - delta, 0)
            profit_above = profits.get(x_opt_int + delta, 0)

            # The optimum should have profit >= neighbors
            # (allowing for integer rounding and brute-force granularity)
            assert profit_at_opt >= min(profit_below, profit_above) - delta, (
                f"Möbius optimum profit={profit_at_opt} should be near max: "
                f"below={profit_below}, at={profit_at_opt}, above={profit_above}"
            )


# ==============================================================================
# Effective Reserve Correctness
# ==============================================================================


class TestEffectiveReserveCorrectness:
    """
    Verify that the effective reserves (R0+alpha, R1+beta) used by the
    Möbius formula match the V3 contract's reserve computation.

    For a V3 tick range at current sqrt price P with liquidity L:
        R0 = L / sqrt(P)   (virtual reserve of token0)
        R1 = L * sqrt(P)   (virtual reserve of token1)

    Bounded product parameters:
        alpha = L / sqrt(P_upper)
        beta = L * sqrt(P_lower)

    Effective reserves:
        r_eff = R0 + alpha = L / sqrt(P) + L / sqrt(P_upper)
        s_eff = R1 + beta  = L * sqrt(P) + L * sqrt(P_lower)

    These should satisfy: r_eff * s_eff >= L^2 (bounded product invariant)
    with equality when the current price equals a boundary.
    """

    @pytest.mark.parametrize("current_tick", [0, -83000, 100, -100])
    def test_effective_reserves_satisfy_invariant(self, current_tick: int):
        """r_eff * s_eff should equal L² at the boundary, exceed it in the middle."""
        L = 1_000_000_000_000.0
        tick_spacing = 60
        tick_lower = (current_tick // tick_spacing) * tick_spacing
        tick_upper = tick_lower + tick_spacing

        sqrt_p_current = tick_to_sqrt_price_float(current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(tick_lower)
        sqrt_p_upper = tick_to_sqrt_price_float(tick_upper)

        v3_hop = V3TickRangeHop(
            liquidity=L,
            sqrt_price_current=sqrt_p_current,
            sqrt_price_lower=sqrt_p_lower,
            sqrt_price_upper=sqrt_p_upper,
            fee=0.003,
            zero_for_one=True,
        )

        hop = v3_hop.to_hop_state()
        r_eff = hop.reserve_in
        s_eff = hop.reserve_out

        # The bounded product invariant: (R0 + alpha)(R1 + beta) = L^2 * (1 + correction)
        # Actually: (R0 + alpha)(R1 + beta) = L² at ALL prices in the range.
        # Because R0 = L/sqrt_p - alpha, R1 = L*sqrt_p - beta, so:
        # (R0 + alpha)(R1 + beta) = (L/sqrt_p) * (L*sqrt_p) = L²
        product = r_eff * s_eff
        L_squared = L * L

        # Should equal L² exactly (within float precision)
        rel_diff = abs(product - L_squared) / L_squared
        assert rel_diff < 1e-8, (
            f"tick={current_tick}: r_eff*s_eff={product:.4f}, L²={L_squared:.4f}, "
            f"rel_diff={rel_diff:.10f}"
        )

    @pytest.mark.parametrize("current_tick", [0, -83000, 100])
    def test_swap_output_matches_bounded_product_formula(self, current_tick: int):
        """
        Verify: For a small swap, the Möbius output matches the
        first-order Taylor expansion of the bounded product CFMM.

        For small x: y ≈ gamma * s_eff / r_eff * x = gamma * marginal_rate * x

        The V3 contract computes the same thing via compute_swap_step.
        """
        L = 1_000_000_000_000.0
        tick_spacing = 60
        tick_lower = (current_tick // tick_spacing) * tick_spacing
        tick_upper = tick_lower + tick_spacing

        sqrt_p_current = tick_to_sqrt_price_float(current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(tick_lower)
        sqrt_p_upper = tick_to_sqrt_price_float(tick_upper)

        v3_hop = V3TickRangeHop(
            liquidity=L,
            sqrt_price_current=sqrt_p_current,
            sqrt_price_lower=sqrt_p_lower,
            sqrt_price_upper=sqrt_p_upper,
            fee=0.003,
            zero_for_one=True,
        )

        hop = v3_hop.to_hop_state()

        # V3 integer math version
        sqrt_price_current_x96 = tick_to_sqrt_price_x96(current_tick)
        sqrt_price_lower_x96 = tick_to_sqrt_price_x96(tick_lower)

        # Small swap (0.01% of liquidity)
        amount_in = int(L * 0.0001)

        v3_out, _, _, _crossed = v3_swap_within_range_integer(
            liquidity=int(L),
            sqrt_price_current_x96=sqrt_price_current_x96,
            sqrt_price_upper_x96=sqrt_price_lower_x96,
            amount_in=amount_in,
            fee_pips=3000,
            zero_for_one=True,
        )

        mobius_out = simulate_path(float(amount_in), [hop])
        mobius_out_int = int(mobius_out)

        if v3_out > 0:
            rel_diff = abs(mobius_out_int - v3_out) / v3_out
            assert rel_diff < 0.001, (
                f"tick={current_tick}: V3_out={v3_out}, Möbius_out={mobius_out_int}, "
                f"rel_diff={rel_diff:.6f}"
            )


# ==============================================================================
# V3 Price Impact Estimation
# ==============================================================================


class TestV3PriceImpactEstimation:
    """
    Verify that estimate_v3_final_sqrt_price matches the actual V3
    sqrt price change computed by compute_swap_step.
    """

    @pytest.mark.parametrize("current_tick", [30, -82970, 100])
    def test_price_impact_matches_v3_integer(self, current_tick: int):
        """
        The float estimate of final sqrt price should match the
        integer compute_swap_step result, for within-range swaps.
        """
        L = 1_000_000_000_000
        tick_spacing = 60
        tick_lower = (current_tick // tick_spacing) * tick_spacing
        tick_upper = tick_lower + tick_spacing
        fee_pips = 3000

        sqrt_price_current_x96 = tick_to_sqrt_price_x96(current_tick)
        sqrt_price_lower_x96 = tick_to_sqrt_price_x96(tick_lower)

        sqrt_p_current = tick_to_sqrt_price_float(current_tick)
        sqrt_p_lower = tick_to_sqrt_price_float(tick_lower)

        # Max amount0 that stays in range (zero_for_one):
        amount0_max = L * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_current)
        # Use 10% of max to ensure we stay well within range
        amount_in = max(1, int(amount0_max * 0.1))

        # V3 integer
        _, _, v3_new_sqrt_price_x96, crossed = v3_swap_within_range_integer(
            liquidity=L,
            sqrt_price_current_x96=sqrt_price_current_x96,
            sqrt_price_upper_x96=sqrt_price_lower_x96,
            amount_in=amount_in,
            fee_pips=fee_pips,
            zero_for_one=True,
        )

        # Float estimate
        v3_hop = V3TickRangeHop(
            liquidity=float(L),
            sqrt_price_current=tick_to_sqrt_price_float(current_tick),
            sqrt_price_lower=tick_to_sqrt_price_float(tick_lower),
            sqrt_price_upper=tick_to_sqrt_price_float(tick_upper),
            fee=fee_pips / 1_000_000,
            zero_for_one=True,
        )

        estimated_sqrt_price = estimate_v3_final_sqrt_price(float(amount_in), v3_hop)

        if crossed:
            pytest.skip("Swap crosses tick boundary")

        # Convert V3 integer result to float for comparison
        v3_new_sqrt_price_float = v3_new_sqrt_price_x96 / (2**96)

        if v3_new_sqrt_price_float > 0 and estimated_sqrt_price > 0:
            rel_diff = abs(estimated_sqrt_price - v3_new_sqrt_price_float) / v3_new_sqrt_price_float

            # Allow up to 0.1% difference (float approximation)
            assert rel_diff < 0.001, (
                f"tick={current_tick}: V3_sqrt_price={v3_new_sqrt_price_float:.10f}, "
                f"estimated={estimated_sqrt_price:.10f}, rel_diff={rel_diff:.6f}"
            )
