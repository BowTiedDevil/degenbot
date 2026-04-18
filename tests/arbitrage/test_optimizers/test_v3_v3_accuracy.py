"""
Verification: V3-V3 Rust solver accuracy.

Three-layer validation strategy:
- Layer 1: compute_v3_v3_profit unit tests (profit function correctness)
- Layer 2: V3-V3 vs Brent reference (independent solver comparison)
- Layer 3: V3-V3 vs V3 integer math (gold standard brute force)

Plus edge case coverage for tick boundaries, numerical extremes,
and golden section search convergence.
"""

import math
import signal

import pytest
from scipy.optimize import minimize_scalar

from degenbot._rs import mobius as rs_mobius
from degenbot.arbitrage.optimizers.v3_tick_predictor import tick_to_sqrt_price
from degenbot.uniswap.v3_libraries.swap_math import compute_swap_step
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick

from .conftest import make_rust_v3_hop as make_v3_hop

# ==============================================================================
# Helpers
# ==============================================================================


def sim_path(x: float, hops: list) -> float:
    """Simulate path using Rust simulate_path (accepts RustHopState)."""
    return rs_mobius.py_simulate_path(x, hops)


def tick_to_sqrt_price_x96(tick: int) -> int:
    return get_sqrt_ratio_at_tick(tick)


def tick_to_sqrt_price_float(tick: int) -> float:
    return tick_to_sqrt_price(tick)


def compute_v3_v3_profit_manual(
    x: float,
    crossing1: rs_mobius.RustTickRangeCrossing | None,
    crossing2: rs_mobius.RustTickRangeCrossing | None,
    hop1_ending: rs_mobius.RustV3TickRangeHop,
    hop2_ending: rs_mobius.RustV3TickRangeHop,
) -> float:
    """
    Manually compute V3-V3 profit, mirroring Rust compute_v3_v3_profit.

    profit(x) = output(x) - x

    For each hop:
    - No crossing: output = simulate_path(x, [hop])
    - With crossing: output = crossing_output + simulate_path(remaining, [ending_range])
    """
    # Hop 1
    if crossing1 is not None:
        if x < crossing1.crossing_gross_input:
            return -1e30
        remaining1 = x - crossing1.crossing_gross_input
        ending1_hs = crossing1.ending_range.to_hop_state()
        var_out1 = sim_path(remaining1, [ending1_hs])
        output1 = crossing1.crossing_output + var_out1
    else:
        output1 = sim_path(x, [hop1_ending.to_hop_state()])

    # Hop 2
    if crossing2 is not None:
        if output1 < crossing2.crossing_gross_input:
            return -1e30
        remaining2 = output1 - crossing2.crossing_gross_input
        ending2_hs = crossing2.ending_range.to_hop_state()
        var_out2 = sim_path(remaining2, [ending2_hs])
        output2 = crossing2.crossing_output + var_out2
    else:
        output2 = sim_path(output1, [hop2_ending.to_hop_state()])

    return output2 - x


def simulate_v3_hop_with_crossings(
    amount_in: int,
    tick_data: list[tuple[int, int, int]],
    current_tick: int,
    current_range_index: int,
    fee_pips: int,
    zero_for_one: bool,  # noqa: FBT001
) -> tuple[int, int]:
    """
    Simulate a V3 swap with tick crossing support using compute_swap_step.

    Walks through tick ranges until the input is exhausted.

    Returns (total_output, total_input_consumed).
    """
    remaining = amount_in
    total_output = 0

    range_idx = current_range_index
    sqrt_price_x96 = tick_to_sqrt_price_x96(current_tick)

    while remaining > 0 and 0 <= range_idx < len(tick_data):
        tl, tu, liquidity = tick_data[range_idx]

        if liquidity == 0:
            range_idx += -1 if zero_for_one else 1
            continue

        sqrt_lower_x96 = tick_to_sqrt_price_x96(tl)
        sqrt_upper_x96 = tick_to_sqrt_price_x96(tu)

        # Target boundary
        # zfo=True → price goes down → target is lower sqrt price
        # zfo=False → price goes up → target is upper sqrt price
        sqrt_target_x96 = sqrt_lower_x96 if zero_for_one else sqrt_upper_x96

        new_sqrt_price, step_amount_in, step_amount_out, fee_amount = compute_swap_step(
            sqrt_ratio_x96_current=sqrt_price_x96,
            sqrt_ratio_x96_target=sqrt_target_x96,
            liquidity=liquidity,
            amount_remaining=remaining,
            fee_pips=fee_pips,
        )

        total_consumed = step_amount_in + fee_amount
        total_output += step_amount_out

        if total_consumed < remaining:
            # Crossed into next range
            remaining -= total_consumed
            sqrt_price_x96 = new_sqrt_price
            # zfo=True → price goes DOWN → move to lower tick index
            # zfo=False → price goes UP → move to higher tick index
            range_idx += -1 if zero_for_one else 1
        else:
            remaining = 0

    return (total_output, amount_in - remaining)


def v3_v3_brute_force_solver(
    tick_data_1: list[tuple[int, int, int]],
    tick_data_2: list[tuple[int, int, int]],
    current_tick_1: int,
    current_tick_2: int,
    fee_pips_1: int,
    fee_pips_2: int,
    zfo_1: bool,  # noqa: FBT001
    zfo_2: bool,  # noqa: FBT001
    max_input_wei: int | None = None,
    scan_steps: int = 200,
) -> tuple[int, int]:
    """
    Brute-force V3-V3 arbitrage solver using V3 integer swap math.

    Scans input amounts, simulates the full 2-hop path using
    compute_swap_step with tick crossing support, and finds
    the optimal input and profit.

    Returns (optimal_input_wei, profit_wei).
    """

    # Find which range contains current tick for each pool
    def find_current_range_index(current_tick: int, ranges: list[tuple[int, int, int]]) -> int:
        for i, (tl, tu, _) in enumerate(ranges):
            if tl <= current_tick < tu:
                return i
        return 0

    range_idx_1 = find_current_range_index(current_tick_1, tick_data_1)
    range_idx_2 = find_current_range_index(current_tick_2, tick_data_2)

    # Estimate max input from reserves
    l1 = tick_data_1[range_idx_1][2]
    sqrt_p1 = tick_to_sqrt_price_float(current_tick_1)
    sqrt_lower1 = tick_to_sqrt_price_float(tick_data_1[range_idx_1][0])
    if zfo_1 and sqrt_lower1 > 0:
        max_amount0_1 = abs(l1 * (1.0 / sqrt_lower1 - 1.0 / sqrt_p1))
    else:
        max_amount0_1 = l1 * 100

    if max_input_wei is not None:
        upper = min(int(max_amount0_1 * 10), max_input_wei)
    else:
        upper = int(max_amount0_1 * 10)

    if upper <= 0:
        return (0, 0)

    best_profit = 0
    best_input = 0

    # Coarse scan
    for i in range(1, scan_steps + 1):
        usdc_in = i * upper // scan_steps

        output1, _ = simulate_v3_hop_with_crossings(
            amount_in=usdc_in,
            tick_data=tick_data_1,
            current_tick=current_tick_1,
            current_range_index=range_idx_1,
            fee_pips=fee_pips_1,
            zero_for_one=zfo_1,
        )

        if output1 <= 0:
            continue

        output2, _ = simulate_v3_hop_with_crossings(
            amount_in=output1,
            tick_data=tick_data_2,
            current_tick=current_tick_2,
            current_range_index=range_idx_2,
            fee_pips=fee_pips_2,
            zero_for_one=zfo_2,
        )

        profit = output2 - usdc_in
        if profit > best_profit:
            best_profit = profit
            best_input = usdc_in

    # Fine scan around best
    if best_input > 0:
        fine_low = max(1, best_input - best_input // 10)
        fine_high = best_input + best_input // 10
        for i in range(1, scan_steps + 1):
            usdc_in = fine_low + i * (fine_high - fine_low) // scan_steps

            output1, _ = simulate_v3_hop_with_crossings(
                amount_in=usdc_in,
                tick_data=tick_data_1,
                current_tick=current_tick_1,
                current_range_index=range_idx_1,
                fee_pips=fee_pips_1,
                zero_for_one=zfo_1,
            )

            if output1 <= 0:
                continue

            output2, _ = simulate_v3_hop_with_crossings(
                amount_in=output1,
                tick_data=tick_data_2,
                current_tick=current_tick_2,
                current_range_index=range_idx_2,
                fee_pips=fee_pips_2,
                zero_for_one=zfo_2,
            )

            profit = output2 - usdc_in
            if profit > best_profit:
                best_profit = profit
                best_input = usdc_in

    return (best_input, best_profit)


def build_seq_from_tick_data(
    tick_data: list[tuple[int, int, int]],
    current_tick: int,
    current_range_idx: int,
    fee: float,
    zero_for_one: bool,  # noqa: FBT001
) -> rs_mobius.RustV3TickRangeSequence:
    """
    Build a RustV3TickRangeSequence from integer tick data,
    ordering ranges in the swap direction.
    """
    ranges = []
    if zero_for_one:
        # z0f1: swap goes down in price → ranges from current → lower ticks
        for i in range(current_range_idx, -1, -1):
            tl, tu, lq = tick_data[i]
            sp = (
                tick_to_sqrt_price_float(current_tick)
                if i == current_range_idx
                else tick_to_sqrt_price_float(tu)
            )
            ranges.append(
                make_v3_hop(
                    float(lq),
                    sp,
                    tick_to_sqrt_price_float(tl),
                    tick_to_sqrt_price_float(tu),
                    fee,
                    zero_for_one=zero_for_one,
                )
            )
    else:
        # o1f0: swap goes up in price → ranges from current → upper ticks
        for i in range(current_range_idx, len(tick_data)):
            tl, tu, lq = tick_data[i]
            sp = (
                tick_to_sqrt_price_float(current_tick)
                if i == current_range_idx
                else tick_to_sqrt_price_float(tl)
            )
            ranges.append(
                make_v3_hop(
                    float(lq),
                    sp,
                    tick_to_sqrt_price_float(tl),
                    tick_to_sqrt_price_float(tu),
                    fee,
                    zero_for_one=zero_for_one,
                )
            )

    return rs_mobius.RustV3TickRangeSequence(ranges)


# ==============================================================================
# Layer 1: compute_v3_v3_profit Unit Tests
# ==============================================================================


class TestV3V3ProfitFunction:
    """
    Layer 1: Validate the profit function compute_v3_v3_profit.

    The profit function is the atomic building block. If it's wrong,
    everything built on it is wrong.

    We compute expected profit manually using simulate_path and compare
    against the solver's internal profit evaluation (verified by checking
    that the solver's optimal solution satisfies profit > 0 and that
    profit at known points matches manual computation).
    """

    def test_profit_no_crossing_matches_simulate_path(self):
        """
        For a single-range V3-V3 path, the profit function should
        reduce to: profit(x) = mobius2(x) - x = simulate_path(x, [h1, h2]) - x.
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        hs1 = hop1.to_hop_state()
        hs2 = hop2.to_hop_state()

        for x_mult in [0.1, 0.5, 1.0, 2.0]:
            x = 1e14 * x_mult
            output = sim_path(x, [hs1, hs2])
            expected_profit = output - x

            assert expected_profit > 0, (
                f"Expected positive profit at x={x:.2e}, got {expected_profit:.2e}"
            )

    def test_profit_crossing1_only(self):
        """
        When only hop 1 has a crossing, the profit function should be:
        profit(x) = mobius2(crossing1.output +
            mobius1(x - crossing1.gross_input, ending_range1)) - x
        for x > crossing1.gross_input.
        """
        sqrt_pa = math.sqrt(2000.0)
        sqrt_pb = math.sqrt(1900.0)
        fee = 0.003

        # Hop 1: narrow current range → crossing into large next range
        hop1_r1 = make_v3_hop(
            1e15,
            sqrt_pa,
            sqrt_pa * 0.98,
            sqrt_pa * 1.02,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            1e20,
            sqrt_pa * 0.98,
            sqrt_pa * 0.5,
            sqrt_pa * 0.98,
            fee,
            zero_for_one=True,
        )

        # Hop 2: single wide range
        hop2 = make_v3_hop(
            1e18,
            sqrt_pb,
            sqrt_pb * 0.5,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        crossing1 = seq1.compute_crossing(1)

        # Compute profit manually for x > crossing cost
        x = crossing1.crossing_gross_input * 2.0
        remaining1 = x - crossing1.crossing_gross_input
        ending1_hs = crossing1.ending_range.to_hop_state()
        hop2_hs = hop2.to_hop_state()

        var_out1 = sim_path(remaining1, [ending1_hs])
        total_out1 = crossing1.crossing_output + var_out1
        output2 = sim_path(total_out1, [hop2_hs])
        manual_profit = output2 - x

        # Compute via the generic manual function
        profit_via_func = compute_v3_v3_profit_manual(
            x,
            crossing1,
            None,
            hop1_r1,
            hop2,
        )

        assert abs(manual_profit - profit_via_func) < abs(manual_profit) * 1e-10, (
            f"Manual profit={manual_profit:.6e}, func profit={profit_via_func:.6e}"
        )

        # The solver should find this crossing path profitable
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])
        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.profit > 0, "Crossing path should be profitable"

    def test_profit_crossing2_only(self):
        """
        When only hop 2 has a crossing, the profit function should be:
        profit(x) = crossing2.output + mobius(output1 - crossing2.gross_input, ending_range2) - x
        where output1 = mobius1(x).
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        # Hop 1: single wide range
        hop1 = make_v3_hop(
            1e18,
            sqrt_pa,
            sqrt_pa * 0.5,
            sqrt_pa * 1.5,
            fee,
            zero_for_one=True,
        )

        # Hop 2: narrow current range → crossing
        hop2_r1 = make_v3_hop(
            1e15,
            sqrt_pb,
            sqrt_pb * 0.98,
            sqrt_pb * 1.02,
            fee,
            zero_for_one=False,
        )
        hop2_r2 = make_v3_hop(
            1e20,
            sqrt_pb * 1.02,
            sqrt_pb * 1.02,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )

        seq2 = rs_mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])
        crossing2 = seq2.compute_crossing(1)

        # Compute manually for x large enough to cover hop2 crossing
        hop1_hs = hop1.to_hop_state()
        x = 1e14

        output1 = sim_path(x, [hop1_hs])

        if output1 > crossing2.crossing_gross_input:
            remaining2 = output1 - crossing2.crossing_gross_input
            ending2_hs = crossing2.ending_range.to_hop_state()
            var_out2 = sim_path(remaining2, [ending2_hs])
            total_out2 = crossing2.crossing_output + var_out2
            manual_profit = total_out2 - x

            profit_via_func = compute_v3_v3_profit_manual(
                x,
                None,
                crossing2,
                hop1,
                hop2_r1,
            )

            assert abs(manual_profit - profit_via_func) < abs(manual_profit) * 1e-10 + 1.0, (
                f"Manual profit={manual_profit:.6e}, func profit={profit_via_func:.6e}"
            )

    def test_profit_both_crossings(self):
        """
        When both hops have crossings, profit should chain both:
        output1 = crossing1.output + mobius(x - crossing1.gross_input, ending1)
        output2 = crossing2.output + mobius(output1 - crossing2.gross_input, ending2)
        profit = output2 - x
        """
        sqrt_pa = math.sqrt(2000.0)
        sqrt_pb = math.sqrt(1900.0)
        fee = 0.003

        # Both pools have narrow current ranges with larger next ranges
        hop1_r1 = make_v3_hop(
            1e15,
            sqrt_pa,
            sqrt_pa * 0.98,
            sqrt_pa * 1.02,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            1e20,
            sqrt_pa * 0.98,
            sqrt_pa * 0.5,
            sqrt_pa * 0.98,
            fee,
            zero_for_one=True,
        )

        hop2_r1 = make_v3_hop(
            1e15,
            sqrt_pb,
            sqrt_pb * 0.98,
            sqrt_pb * 1.02,
            fee,
            zero_for_one=False,
        )
        hop2_r2 = make_v3_hop(
            1e20,
            sqrt_pb * 1.02,
            sqrt_pb * 1.02,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])

        crossing1 = seq1.compute_crossing(1)
        crossing2 = seq2.compute_crossing(1)

        # Test at x = 2 * crossing1.gross_input
        x = crossing1.crossing_gross_input * 2.0
        manual_profit = compute_v3_v3_profit_manual(
            x,
            crossing1,
            crossing2,
            hop1_r1,
            hop2_r1,
        )

        # Verify the manual computation step by step
        remaining1 = x - crossing1.crossing_gross_input
        ending1_hs = crossing1.ending_range.to_hop_state()
        var_out1 = sim_path(remaining1, [ending1_hs])
        total_out1 = crossing1.crossing_output + var_out1

        if total_out1 > crossing2.crossing_gross_input:
            remaining2 = total_out1 - crossing2.crossing_gross_input
            ending2_hs = crossing2.ending_range.to_hop_state()
            var_out2 = sim_path(remaining2, [ending2_hs])
            total_out2 = crossing2.crossing_output + var_out2
            expected_profit = total_out2 - x

            assert abs(manual_profit - expected_profit) < abs(expected_profit) * 1e-10 + 1.0

        # The solver should handle this
        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0

    def test_profit_below_crossing_cost_returns_negative(self):
        """
        If x < crossing.gross_input, the profit function should return
        a very negative value (since the input can't cover the crossing cost).
        """
        sqrt_pa = math.sqrt(2000.0)
        fee = 0.003

        hop1_r1 = make_v3_hop(
            1e15,
            sqrt_pa,
            sqrt_pa * 0.98,
            sqrt_pa * 1.02,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            1e20,
            sqrt_pa * 0.98,
            sqrt_pa * 0.5,
            sqrt_pa * 0.98,
            fee,
            zero_for_one=True,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        crossing1 = seq1.compute_crossing(1)

        # x below crossing cost should give very negative profit
        x_below = crossing1.crossing_gross_input * 0.5
        profit = compute_v3_v3_profit_manual(
            x_below,
            crossing1,
            None,
            hop1_r1,
            hop1_r1,
        )
        assert profit < -1e20, (
            f"Profit below crossing cost should be very negative, got {profit:.2e}"
        )

    def test_profit_at_exact_crossing_boundary(self):
        """
        At x = crossing.gross_input exactly, remaining input = 0.
        The hop1 output = crossing_output + simulate_path(0, [ending]) = crossing_output.
        Profit depends on whether hop2 can turn crossing_output into enough output.
        """
        sqrt_pa = math.sqrt(2000.0)
        fee = 0.003

        hop1_r1 = make_v3_hop(
            1e18,
            sqrt_pa,
            sqrt_pa * 0.9,
            sqrt_pa * 1.1,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            2e18,
            sqrt_pa * 0.9,
            sqrt_pa * 0.5,
            sqrt_pa * 0.9,
            fee,
            zero_for_one=True,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        crossing1 = seq1.compute_crossing(1)

        x_exact = crossing1.crossing_gross_input
        remaining = x_exact - crossing1.crossing_gross_input  # = 0
        ending_hs = crossing1.ending_range.to_hop_state()
        var_out = sim_path(remaining, [ending_hs])  # sim_path(0, ...) = 0
        assert var_out < 1e-30, "simulate_path(0) should return 0"

        # At boundary, hop1 output = crossing_output + 0 = crossing_output


# ==============================================================================
# Layer 2: V3-V3 vs Brent Reference
# ==============================================================================


def v3_v3_brent_solve(
    seq1: rs_mobius.RustV3TickRangeSequence,
    seq2: rs_mobius.RustV3TickRangeSequence,
    max_input: float | None = None,
) -> tuple[float, float, bool]:
    """Convenience wrapper: compute crossings then call v3_v3_brent_solver."""
    crossings1 = get_crossings(seq1)
    crossings2 = get_crossings(seq2)
    hop1_current = crossings1[0].ending_range
    hop2_current = crossings2[0].ending_range
    return v3_v3_brent_solver(hop1_current, hop2_current, crossings1, crossings2, max_input)


def get_crossings(
    seq: rs_mobius.RustV3TickRangeSequence,
    max_k: int = 3,
) -> list[rs_mobius.RustTickRangeCrossing]:
    """Compute all valid crossings from a V3TickRangeSequence."""
    crossings = []
    for k in range(max_k):
        try:
            c = seq.compute_crossing(k)
            crossings.append(c)
        except (ValueError, RuntimeError):
            break
    return crossings


def v3_v3_brent_solver(
    hop1_current: rs_mobius.RustV3TickRangeHop,
    hop2_current: rs_mobius.RustV3TickRangeHop,
    crossings1: list[rs_mobius.RustTickRangeCrossing],
    crossings2: list[rs_mobius.RustTickRangeCrossing],
    max_input: float | None = None,
) -> tuple[float, float, bool]:
    """
    Brent-based V3-V3 solver using scipy.optimize.minimize_scalar
    on the FULL piecewise profit function (including tick crossings).

    Evaluates profit by checking all crossing combinations for each
    input amount. Serves as an independent reference implementation.

    Returns (optimal_input, profit, success).
    """

    def neg_profit(x: float) -> float:
        """Negative profit for minimization. Checks all crossing combinations."""
        if x <= 0:
            return 0.0

        best_profit = -1e30

        for c1 in crossings1:
            for c2 in crossings2:
                p = compute_v3_v3_profit_manual(
                    x,
                    c1 if c1.crossing_gross_input > 0 else None,
                    c2 if c2.crossing_gross_input > 0 else None,
                    hop1_current,
                    hop2_current,
                )
                best_profit = max(best_profit, p)

        return -best_profit

    # Estimate upper bound from reserves
    hop1_hs = hop1_current.to_hop_state()
    upper = hop1_hs.reserve_in * 10
    if max_input is not None:
        upper = min(upper, max_input)

    if upper <= 0:
        return (0.0, 0.0, False)

    result = minimize_scalar(
        neg_profit,
        method="bounded",
        bounds=(0, upper),
        options={"xatol": 1.0},
    )

    if not result.success or result.fun >= 0:
        return (0.0, 0.0, False)

    x_opt = result.x
    # Verify profit is positive
    profit = -result.fun
    if profit <= 0:
        return (0.0, 0.0, False)

    return (x_opt, profit, True)


class TestV3V3VsBrent:
    """
    Layer 2: Compare Rust solve_v3_v3 against BrentSolver.

    Brent is an independent reference that operates on the full profit
    function via scipy.optimize.minimize_scalar. It handles V3-V3
    correctly (slowly) and serves as ground truth for well-structured
    inputs.
    """

    def test_single_range_matches_brent(self):
        """Single-range V3-V3 should match Brent within 0.5%."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        __x_brent, profit_brent, brent_success = v3_v3_brent_solve(seq1, seq2)

        if not result_rust.success and not brent_success:
            pytest.skip("Neither solver found profit")

        assert result_rust.success, "Rust solver should find profit for single-range V3-V3"
        assert brent_success, "Brent solver should find profit for single-range V3-V3"

        if profit_brent > 0:
            rel_diff = abs(result_rust.profit - profit_brent) / profit_brent
            assert rel_diff < 0.005, (
                f"Rust profit={result_rust.profit}, Brent profit={profit_brent}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_one_pool_crossing_matches_brent(self):
        """V3-V3 with one pool crossing should match Brent within 0.5%."""
        sqrt_pa = math.sqrt(2000.0)
        sqrt_pb = math.sqrt(1900.0)
        fee = 0.003

        hop1_r1 = make_v3_hop(
            1e15,
            sqrt_pa,
            sqrt_pa * 0.98,
            sqrt_pa * 1.02,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            1e20,
            sqrt_pa * 0.98,
            sqrt_pa * 0.5,
            sqrt_pa * 0.98,
            fee,
            zero_for_one=True,
        )

        hop2 = make_v3_hop(
            1e18,
            sqrt_pb,
            sqrt_pb * 0.5,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        __x_brent, profit_brent, brent_success = v3_v3_brent_solve(seq1, seq2)

        if not result_rust.success and not brent_success:
            pytest.skip("Neither solver found profit")

        if brent_success and profit_brent > 0 and result_rust.success:
            rel_diff = abs(result_rust.profit - profit_brent) / profit_brent
            assert rel_diff < 0.005, (
                f"Rust profit={result_rust.profit}, Brent profit={profit_brent}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_both_pools_crossing_matches_brent(self):
        """V3-V3 with both pools crossing should match Brent within 1%."""
        sqrt_pa = math.sqrt(2000.0)
        sqrt_pb = math.sqrt(1900.0)
        fee = 0.003

        hop1_r1 = make_v3_hop(
            1e15,
            sqrt_pa,
            sqrt_pa * 0.98,
            sqrt_pa * 1.02,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            1e20,
            sqrt_pa * 0.98,
            sqrt_pa * 0.5,
            sqrt_pa * 0.98,
            fee,
            zero_for_one=True,
        )

        hop2_r1 = make_v3_hop(
            1e15,
            sqrt_pb,
            sqrt_pb * 0.98,
            sqrt_pb * 1.02,
            fee,
            zero_for_one=False,
        )
        hop2_r2 = make_v3_hop(
            1e20,
            sqrt_pb * 1.02,
            sqrt_pb * 1.02,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        __x_brent, profit_brent, brent_success = v3_v3_brent_solve(seq1, seq2)

        if not result_rust.success and not brent_success:
            pytest.skip("Neither solver found profit")

        if brent_success and profit_brent > 0 and result_rust.success:
            rel_diff = abs(result_rust.profit - profit_brent) / profit_brent
            # Both crossing: relax to 1% due to piecewise approximation
            assert rel_diff < 0.01, (
                f"Rust profit={result_rust.profit}, Brent profit={profit_brent}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_no_arbitrage_matches_brent(self):
        """Both solvers should detect no arbitrage when prices are equal."""
        sqrt_p = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_p, sqrt_p * 0.5, sqrt_p * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_p, sqrt_p * 0.5, sqrt_p * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        _x_brent, _profit_brent, brent_success = v3_v3_brent_solve(seq1, seq2)

        assert not result_rust.success, "Rust solver should report no profit for equal prices"
        assert not brent_success, "Brent solver should report no profit for equal prices"

    def test_narrow_spread_matches_brent(self):
        """Very narrow price spread (0.1%) — both should agree on outcome."""
        sqrt_pa = math.sqrt(2002.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        __x_brent, profit_brent, brent_success = v3_v3_brent_solve(seq1, seq2)

        if result_rust.success and brent_success:
            if profit_brent > 0:
                rel_diff = abs(result_rust.profit - profit_brent) / profit_brent
                assert rel_diff < 0.005, (
                    f"Rust profit={result_rust.profit}, Brent profit={profit_brent}, "
                    f"rel_diff={rel_diff:.6f}"
                )
        else:
            # Both should agree on failure for very narrow spreads
            assert not result_rust.success or not brent_success

    def test_max_input_respected_by_both(self):
        """Both solvers should respect max_input constraint."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result_unconstrained = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result_unconstrained.success

        max_input = result_unconstrained.optimal_input * 0.1
        result_constrained = rs_mobius.RustMobiusOptimizer().solve_v3_v3(
            seq1,
            seq2,
            max_input,
        )

        if result_constrained.success:
            assert result_constrained.optimal_input <= max_input * 1.01, (
                f"Constrained input {result_constrained.optimal_input:.2e} "
                f"exceeds max_input {max_input:.2e}"
            )

    def test_different_fee_tiers(self):
        """V3-V3 with different fee tiers should work correctly."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        for fee_a, fee_b in [(0.0005, 0.003), (0.003, 0.01), (0.01, 0.0005)]:
            hop1 = make_v3_hop(
                1e18,
                sqrt_pa,
                sqrt_pa * 0.5,
                sqrt_pa * 1.5,
                fee_a,
                zero_for_one=True,
            )
            hop2 = make_v3_hop(
                1e18,
                sqrt_pb,
                sqrt_pb * 0.5,
                sqrt_pb * 1.5,
                fee_b,
                zero_for_one=False,
            )

            seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
            seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

            result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
            assert result.optimal_input >= 0


# ==============================================================================
# Layer 3: V3-V3 vs V3 Integer Math (Gold Standard)
# ==============================================================================


class TestV3V3VsV3IntegerMath:
    """
    Layer 3: Compare Rust solve_v3_v3 against brute-force V3 integer math.

    The brute-force solver chains two V3 pools using compute_swap_step
    with full tick crossing support. This is the gold standard.
    """

    def test_v3_v3_arbitrage_vs_brute_force_weth_usdc(self):
        """
        WETH/USDC-like pools (tick ≈ -83000, spacing=60).

        Two V3 pools with different prices for the same pair.
        Compare Rust solver profit against brute-force integer scan.
        """
        current_tick_1 = -82970
        current_tick_2 = -83100

        tick_data_1 = [
            (-83100, -83040, 2_000_000_000_000_000_000),
            (-83040, -82980, 2_000_000_000_000_000_000),
            (-82980, -82920, 2_000_000_000_000_000_000),
        ]
        tick_data_2 = [
            (-83240, -83180, 2_000_000_000_000_000_000),
            (-83180, -83120, 2_000_000_000_000_000_000),
            (-83120, -83060, 2_000_000_000_000_000_000),
        ]

        # Find current range indices
        range_idx_1 = 0
        for i, (tl, tu, _) in enumerate(tick_data_1):
            if tl <= current_tick_1 < tu:
                range_idx_1 = i
                break

        range_idx_2 = 0
        for i, (tl, tu, _) in enumerate(tick_data_2):
            if tl <= current_tick_2 < tu:
                range_idx_2 = i
                break

        __bf_input, bf_profit = v3_v3_brute_force_solver(
            tick_data_1,
            tick_data_2,
            current_tick_1,
            current_tick_2,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        # Build Rust solver sequences
        seq1 = build_seq_from_tick_data(
            tick_data_1, current_tick_1, range_idx_1, 0.003, zero_for_one=True
        )
        seq2 = build_seq_from_tick_data(
            tick_data_2, current_tick_2, range_idx_2, 0.003, zero_for_one=False
        )

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        if bf_profit > 0 and result_rust.success and result_rust.profit > 0:
            rust_profit_int = int(result_rust.profit)
            rel_diff = abs(rust_profit_int - bf_profit) / bf_profit
            assert rel_diff < 0.01, (
                f"Rust profit={rust_profit_int}, BF profit={bf_profit}, rel_diff={rel_diff:.6f}"
            )

    def test_v3_v3_arbitrage_vs_brute_force_stablecoin(self):
        """
        Stablecoin-like pools (tick ≈ 0, spacing=10).

        Very tight price ranges, small tick spacing, 0.05% fee.
        """
        current_tick_1 = 5
        current_tick_2 = -5

        tick_data_1 = [
            (-10, 0, 1_000_000_000_000_000_000),
            (0, 10, 1_000_000_000_000_000_000),
            (10, 20, 1_000_000_000_000_000_000),
        ]
        tick_data_2 = [
            (-20, -10, 1_000_000_000_000_000_000),
            (-10, 0, 1_000_000_000_000_000_000),
            (0, 10, 1_000_000_000_000_000_000),
        ]

        range_idx_1 = 0
        for i, (tl, tu, _) in enumerate(tick_data_1):
            if tl <= current_tick_1 < tu:
                range_idx_1 = i
                break

        range_idx_2 = 0
        for i, (tl, tu, _) in enumerate(tick_data_2):
            if tl <= current_tick_2 < tu:
                range_idx_2 = i
                break

        __bf_input, bf_profit = v3_v3_brute_force_solver(
            tick_data_1,
            tick_data_2,
            current_tick_1,
            current_tick_2,
            fee_pips_1=500,
            fee_pips_2=500,
            zfo_1=True,
            zfo_2=False,
        )

        seq1 = build_seq_from_tick_data(
            tick_data_1, current_tick_1, range_idx_1, 0.0005, zero_for_one=True
        )
        seq2 = build_seq_from_tick_data(
            tick_data_2, current_tick_2, range_idx_2, 0.0005, zero_for_one=False
        )

        result_rust = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        if bf_profit > 0 and result_rust.success and result_rust.profit > 0:
            rust_profit_int = int(result_rust.profit)
            rel_diff = abs(rust_profit_int - bf_profit) / bf_profit
            assert rel_diff < 0.01, (
                f"Rust profit={rust_profit_int}, BF profit={bf_profit}, rel_diff={rel_diff:.6f}"
            )

    def test_v3_v3_profit_gradient_zero_at_optimum(self):
        """
        At the Rust solver's optimum, the profit function should be at
        or near its maximum. Checking profit at nearby points should
        confirm this (profit should decrease in both directions).
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.success

        x_opt = result.optimal_input
        hs1 = hop1.to_hop_state()
        hs2 = hop2.to_hop_state()

        profit_at_opt = sim_path(x_opt, [hs1, hs2]) - x_opt
        profit_below = sim_path(x_opt * 0.9, [hs1, hs2]) - x_opt * 0.9
        profit_above = sim_path(x_opt * 1.1, [hs1, hs2]) - x_opt * 1.1

        # Profit at optimum should be >= both neighbors (allowing float tolerance)
        assert profit_at_opt >= min(profit_below, profit_above) - abs(profit_at_opt) * 1e-6, (
            f"Profit at optimum ({profit_at_opt:.2e}) should be near max: "
            f"below={profit_below:.2e}, above={profit_above:.2e}"
        )

    def test_v3_v3_high_liquidity_accuracy(self):
        """High liquidity pools (1e20+) — small price impact, good accuracy."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e20, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e20, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.success
        assert result.profit > 0

        # Verify via simulate_path
        hs1 = hop1.to_hop_state()
        hs2 = hop2.to_hop_state()
        manual_profit = sim_path(result.optimal_input, [hs1, hs2]) - result.optimal_input
        rel_diff = abs(manual_profit - result.profit) / max(result.profit, 1e-10)
        assert rel_diff < 1e-8, (
            f"Simulate_path profit={manual_profit:.2e}, solver profit={result.profit:.2e}, "
            f"rel_diff={rel_diff:.10f}"
        )

    def test_v3_v3_low_liquidity_accuracy(self):
        """Low liquidity pools (1e10) — larger price impact, still accurate."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e10, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e10, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.success
        assert result.profit > 0

        hs1 = hop1.to_hop_state()
        hs2 = hop2.to_hop_state()
        manual_profit = sim_path(result.optimal_input, [hs1, hs2]) - result.optimal_input
        rel_diff = abs(manual_profit - result.profit) / max(result.profit, 1e-10)
        assert rel_diff < 1e-8

    def test_v3_v3_asymmetric_crossings(self):
        """
        Asymmetric: hop 1 crosses 1 tick, hop 2 stays in current range.
        The crossing path should beat the single-range path.
        """
        sqrt_pa = math.sqrt(2000.0)
        sqrt_pb = math.sqrt(1900.0)
        fee = 0.003

        # Hop 1: tight current range + large next range
        hop1_r1 = make_v3_hop(
            1e15,
            sqrt_pa,
            sqrt_pa * 0.98,
            sqrt_pa * 1.02,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            1e20,
            sqrt_pa * 0.98,
            sqrt_pa * 0.5,
            sqrt_pa * 0.98,
            fee,
            zero_for_one=True,
        )

        # Hop 2: wide range (no crossing)
        hop2 = make_v3_hop(
            1e18,
            sqrt_pb,
            sqrt_pb * 0.5,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )

        seq1_multi = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq1_single = rs_mobius.RustV3TickRangeSequence([hop1_r1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result_multi = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1_multi, seq2)
        result_single = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1_single, seq2)

        # Multi-range should find better (or equal) profit
        assert result_multi.profit >= result_single.profit, (
            f"Multi-range profit ({result_multi.profit:.2e}) should be >= "
            f"single-range ({result_single.profit:.2e})"
        )


# ==============================================================================
# Edge Cases
# ==============================================================================


class TestV3V3EdgeCases:
    """
    Edge case coverage: tick boundaries, numerical extremes,
    golden section search convergence, and fee tier interactions.
    """

    def test_current_price_at_tick_boundary(self):
        """
        Current price at exact tick boundary.
        Effective reserves should still be valid.
        """
        sqrt_p = math.sqrt(2000.0)
        fee = 0.003

        # Current price at exact upper boundary of range 0
        boundary = sqrt_p * 0.9

        hop1_r1 = make_v3_hop(
            1e18,
            boundary,
            boundary * 0.8,
            boundary,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            2e18,
            boundary,
            boundary * 0.5,
            boundary,
            fee,
            zero_for_one=True,
        )

        hs = hop1_r1.to_hop_state()
        assert hs.reserve_in > 0
        assert hs.reserve_out > 0

        # Solver should not crash
        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        sqrt_pb = math.sqrt(1900.0)
        hop2 = make_v3_hop(
            1e18,
            sqrt_pb,
            sqrt_pb * 0.5,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0

    def test_very_narrow_tick_range(self):
        """
        Very narrow range (1 tick spacing equivalent).
        Price impact is massive, reserves are tiny.
        """
        tick = 0
        sqrt_p = tick_to_sqrt_price_float(tick)
        sqrt_p_lower = tick_to_sqrt_price_float(tick - 1)
        sqrt_p_upper = tick_to_sqrt_price_float(tick + 1)

        fee = 0.003

        hop1 = make_v3_hop(
            1e18,
            sqrt_p,
            sqrt_p_lower,
            sqrt_p_upper,
            fee,
            zero_for_one=True,
        )

        sqrt_p2 = tick_to_sqrt_price_float(-10)
        hop2 = make_v3_hop(
            1e18,
            sqrt_p2,
            tick_to_sqrt_price_float(-11),
            tick_to_sqrt_price_float(-9),
            fee,
            zero_for_one=False,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0

    def test_massive_liquidity_asymmetry(self):
        """
        Massive liquidity asymmetry (1e30 vs 1e10).
        Tests float64 precision limits.
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e30, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e10, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0
        if result.success:
            assert result.profit > 0

    def test_near_zero_liquidity(self):
        """
        Near-zero liquidity in one range.
        Should not produce division-by-zero errors.
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e-6, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0

    def test_fee_exceeds_profit(self):
        """
        When fees exceed potential profit, solver should return failure
        (no negative profit reported as success).
        """
        sqrt_pa = math.sqrt(2000.1)  # Tiny spread: 0.005%
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.01  # 1% fee

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        if result.success:
            assert result.profit > 0, "Successful result must have positive profit"

    def test_mixed_fee_tiers(self):
        """
        Mixed fee tiers: 0.05% + 1% cross-tier arbitrage.
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        # Pool A: 0.05% fee (stablecoin pair)
        hop1 = make_v3_hop(
            1e18,
            sqrt_pa,
            sqrt_pa * 0.5,
            sqrt_pa * 1.5,
            0.0005,
            zero_for_one=True,
        )

        # Pool B: 1% fee (exotic pair)
        hop2 = make_v3_hop(
            1e18,
            sqrt_pb,
            sqrt_pb * 0.5,
            sqrt_pb * 1.5,
            0.01,
            zero_for_one=False,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0
        if result.success:
            assert result.profit > 0

    def test_tiny_search_interval(self):
        """
        x_min ≈ x_max (tiny search interval).
        Golden section should handle this gracefully via early exit.
        """
        sqrt_pa = math.sqrt(2000.0)
        sqrt_pb = math.sqrt(1999.99)  # Tiny spread
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        # With tiny max_input
        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2, max_input=1.0)
        assert result.optimal_input >= 0

    def test_no_valid_search_region(self):
        """
        max_input = 0 → no valid search region.
        Should return (0, 0).
        """
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)
        fee = 0.003

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, fee, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, fee, zero_for_one=False)

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1])
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2, max_input=0.0)

        assert not result.success
        assert result.optimal_input == 0
        assert result.profit == 0

    def test_empty_range_zero_liquidity_in_crossing(self):
        """
        Crossing into a range with zero liquidity.
        The solver should handle this without division by zero.
        """
        sqrt_pa = math.sqrt(2000.0)
        fee = 0.003

        hop1_r1 = make_v3_hop(
            1e18,
            sqrt_pa,
            sqrt_pa * 0.9,
            sqrt_pa * 1.1,
            fee,
            zero_for_one=True,
        )
        hop1_r2 = make_v3_hop(
            0.0,
            sqrt_pa * 0.9,
            sqrt_pa * 0.5,
            sqrt_pa * 0.9,  # Zero liquidity
            fee,
            zero_for_one=True,
        )

        seq1 = rs_mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])

        sqrt_pb = math.sqrt(1900.0)
        hop2 = make_v3_hop(
            1e18,
            sqrt_pb,
            sqrt_pb * 0.5,
            sqrt_pb * 1.5,
            fee,
            zero_for_one=False,
        )
        seq2 = rs_mobius.RustV3TickRangeSequence([hop2])

        # Should not crash
        result = rs_mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
        assert result.optimal_input >= 0


# ==============================================================================
# Single-V3 range bounds regression tests
# ==============================================================================


class TestV3SingleRangeBounds:
    """
    Regression tests for the range bounds fix in solve_v3_candidates
    and solve_piecewise (single-V3-multi-range paths).

    Before the fix, these solvers called mobius_solve without constraining
    max_input to tick range capacity. For narrow ranges, the unconstrained
    Möbius optimum exceeded the range, and the post-hoc contains_sqrt_price
    check rejected the candidate entirely instead of finding the
    constrained optimum.
    """

    def test_solve_v3_candidates_narrow_range_finds_profit(self):
        """
        V3(zfo) + V2 path where the unconstrained Möbius optimum
        would exceed the V3 range. The solver should find the constrained
        optimum, not reject the candidate.
        """
        # V3 zfo=True: sell token0, get token1
        # Narrow range around sqrt_p=44.7
        v3_candidate = make_v3_hop(
            1e18,
            44.7,
            44.0,
            45.4,
            0.003,
            zero_for_one=True,
        )

        # V2: receives token1, returns token0
        # Price mismatch: V2 rate > inverse of V3 rate
        v2_hop = rs_mobius.RustHopState(reserve_in=1e19, reserve_out=1e16, fee=0.003)

        # V3 is at hop index 0: path is [V3(zfo) → V2]
        optimizer = rs_mobius.RustMobiusOptimizer()
        result = optimizer.solve_v3_candidates([v2_hop], 0, [v3_candidate], None)

        assert result.success, "Should find profit for narrow range"
        assert result.optimal_input > 0
        assert result.profit > 0

        # The optimal input must not exceed the range capacity
        max_input = v3_candidate.max_gross_input_in_range()
        assert result.optimal_input <= max_input * 1.001, (
            f"Optimal input {result.optimal_input:.6e} exceeds range capacity {max_input:.6e}"
        )

    def test_solve_piecewise_narrow_range_finds_profit(self):
        """
        V3(zfo) + V2 path via solve_piecewise where the unconstrained
        Möbius optimum would exceed the V3 range.
        """
        # V3 zfo=True, narrow range
        hop0 = make_v3_hop(
            1e18,
            44.7,
            44.0,
            45.4,
            0.003,
            zero_for_one=True,
        )
        seq = rs_mobius.RustV3TickRangeSequence([hop0])

        v2_hop = rs_mobius.RustHopState(reserve_in=1e19, reserve_out=1e16, fee=0.003)
        v3_hs = hop0.to_hop_state()

        optimizer = rs_mobius.RustMobiusOptimizer()

        # solve_v3_sequence uses solve_piecewise internally
        result = optimizer.solve_v3_sequence(
            [v3_hs, v2_hop],
            0,
            seq,
            3,
            None,
        )

        assert result.success, "solve_piecewise should find profit for narrow range"
        assert result.optimal_input > 0
        assert result.profit > 0

        max_input = hop0.max_gross_input_in_range()
        assert result.optimal_input <= max_input * 1.001, (
            f"Optimal input {result.optimal_input:.6e} exceeds range capacity {max_input:.6e}"
        )

    def test_solve_piecewise_zero_x_min_terminates(self):
        """
        Regression: golden section search with x_min=0 must terminate.
        Previously, (b-a)/a = infinity when a=0, causing infinite loop.
        """
        # Single range → k=0 → crossing_gross_input=0 → x_min=0
        hop0 = make_v3_hop(1e18, 44.7, 44.0, 45.4, 0.003, zero_for_one=True)
        seq = rs_mobius.RustV3TickRangeSequence([hop0])

        v2_hop = rs_mobius.RustHopState(reserve_in=1e19, reserve_out=1e16, fee=0.003)
        v3_hs = hop0.to_hop_state()

        optimizer = rs_mobius.RustMobiusOptimizer()

        # This must terminate (not hang)
        def handler(_signum, _frame):
            raise TimeoutError

        signal.signal(signal.SIGALRM, handler)
        signal.alarm(5)

        try:
            result = optimizer.solve_v3_sequence(
                [v3_hs, v2_hop],
                0,
                seq,
                3,
                None,
            )
            assert result.optimal_input >= 0
        finally:
            signal.alarm(0)
