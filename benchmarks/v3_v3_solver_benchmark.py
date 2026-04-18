"""
Benchmark: V3-V3 Rust solver vs Brent vs brute-force integer math.

Compares:
1. Rust solve_v3_v3 with varying max_candidates (3, 5, 10)
2. Brent (scipy.optimize.minimize_scalar) on the full piecewise profit function
3. Brute-force V3 integer math (compute_swap_step scanner)

Scenarios range from single-range (no crossings) to 10+ tick crossings.

Known limitation: For narrow tick ranges, the Möbius baseline (k=0) can
return results exceeding range bounds. This is by design — the Möbius
closed-form assumes unbounded constant-product, while V3 ranges are bounded.
The benchmark includes both scenarios where the comparison is valid (wide
ranges) and scenarios that surface this limitation (narrow ranges).
"""
import time
from dataclasses import dataclass

from degenbot._rs import mobius as rs_mobius
from degenbot.uniswap.v3_libraries.swap_math import compute_swap_step

# ==============================================================================
# Helpers
# ==============================================================================

Q96 = 2**96


def make_v3_hop(
    liquidity: float,
    sqrt_price: float,
    sqrt_lower: float,
    sqrt_upper: float,
    fee: float,
    *,
    zero_for_one: bool,
) -> rs_mobius.RustV3TickRangeHop:
    return rs_mobius.RustV3TickRangeHop(
        liquidity=liquidity,
        sqrt_price_current=sqrt_price,
        sqrt_price_lower=sqrt_lower,
        sqrt_price_upper=sqrt_upper,
        fee=fee,
        zero_for_one=zero_for_one,
    )


def tick_to_sqrt_price_float(tick: int) -> float:
    return 1.0001 ** (tick / 2)


def tick_to_sqrt_price_x96(tick: int) -> int:
    sqrt_price = 1.0001 ** (tick / 2)
    return int(sqrt_price * Q96)


def build_seq_from_tick_data(
    tick_data: list[tuple[int, int, int]],
    current_tick: int,
    current_range_idx: int,
    fee: float,
    zero_for_one: bool,  # noqa: FBT001
) -> rs_mobius.RustV3TickRangeSequence:
    ranges = []
    if zero_for_one:
        # zfo=True: swap goes down in price → ranges from current → lower ticks
        for i in range(current_range_idx, -1, -1):
            tl, tu, lq = tick_data[i]
            sp = (
                tick_to_sqrt_price_float(current_tick)
                if i == current_range_idx
                else tick_to_sqrt_price_float(tu)
            )
            ranges.append(make_v3_hop(
                float(lq), sp,
                tick_to_sqrt_price_float(tl), tick_to_sqrt_price_float(tu),
                fee, zero_for_one=zero_for_one,
            ))
    else:
        # zfo=False: swap goes up in price → ranges from current → upper ticks
        for i in range(current_range_idx, len(tick_data)):
            tl, tu, lq = tick_data[i]
            sp = (
                tick_to_sqrt_price_float(current_tick)
                if i == current_range_idx
                else tick_to_sqrt_price_float(tl)
            )
            ranges.append(make_v3_hop(
                float(lq), sp,
                tick_to_sqrt_price_float(tl), tick_to_sqrt_price_float(tu),
                fee, zero_for_one=zero_for_one,
            ))

    return rs_mobius.RustV3TickRangeSequence(ranges)


# ==============================================================================
# Scenario builders
# ==============================================================================


@dataclass(frozen=True, slots=True)
class Scenario:
    name: str
    seq1: rs_mobius.RustV3TickRangeSequence
    seq2: rs_mobius.RustV3TickRangeSequence
    tick_data_1: list[tuple[int, int, int]]
    tick_data_2: list[tuple[int, int, int]]
    current_tick_1: int
    current_tick_2: int
    current_range_idx_1: int
    current_range_idx_2: int
    fee_pips_1: int
    fee_pips_2: int
    zfo_1: bool
    zfo_2: bool
    num_ranges_1: int
    num_ranges_2: int


def make_weth_usdc_scenario(num_ranges: int = 3) -> Scenario:
    """
    WETH/USDC-like pools with 60-tick spacing and wide ranges.
    Current tick is centered so ranges exist in both swap directions.
    The wide ranges ensure Möbius results stay within range bounds.
    """
    tick_spacing = 60
    # Pool 1: higher price (zfo=True sells token0, so pool must have higher p)
    base_tick_1 = -83100
    # Pool 2: lower price (zfo=False sells token1, so pool must have lower p)
    base_tick_2 = -83200

    half = num_ranges // 2
    range_idx_1 = half
    range_idx_2 = half
    current_tick_1 = base_tick_1 + half * tick_spacing + tick_spacing // 2
    current_tick_2 = base_tick_2 + half * tick_spacing + tick_spacing // 2

    liq = 2_000_000_000_000_000_000

    tick_data_1 = [
        (base_tick_1 + i * tick_spacing, base_tick_1 + (i + 1) * tick_spacing, liq)
        for i in range(num_ranges)
    ]
    tick_data_2 = [
        (base_tick_2 + i * tick_spacing, base_tick_2 + (i + 1) * tick_spacing, liq)
        for i in range(num_ranges)
    ]

    seq1 = build_seq_from_tick_data(
        tick_data_1, current_tick_1, range_idx_1, 0.003, zero_for_one=True
    )
    seq2 = build_seq_from_tick_data(
        tick_data_2, current_tick_2, range_idx_2, 0.003, zero_for_one=False
    )

    return Scenario(
        name=f"weth_usdc_{num_ranges}ranges",
        seq1=seq1,
        seq2=seq2,
        tick_data_1=tick_data_1,
        tick_data_2=tick_data_2,
        current_tick_1=current_tick_1,
        current_tick_2=current_tick_2,
        current_range_idx_1=range_idx_1,
        current_range_idx_2=range_idx_2,
        fee_pips_1=3000,
        fee_pips_2=3000,
        zfo_1=True,
        zfo_2=False,
        num_ranges_1=num_ranges,
        num_ranges_2=num_ranges,
    )


def make_stablecoin_scenario(num_ranges: int = 3) -> Scenario:
    """
    Stablecoin-like pools with 10-tick spacing and wide ranges.
    Uses 0.3% fee with sufficient spread for profitable arbitrage.

    Note: For narrow tick ranges (small tick_spacing), the Möbius k=0
    baseline may exceed range bounds. This scenario uses wider ranges
    (100-tick width) to ensure valid comparisons.
    """
    range_width = 100  # 100 ticks per range for stablecoins (wider than typical)
    # Pool 1: higher price
    base_tick_1 = 100
    # Pool 2: lower price
    base_tick_2 = -200

    half = num_ranges // 2
    range_idx_1 = half
    range_idx_2 = half
    current_tick_1 = base_tick_1 + half * range_width + range_width // 2
    current_tick_2 = base_tick_2 + half * range_width + range_width // 2

    liq = 1_000_000_000_000_000_000

    tick_data_1 = [
        (base_tick_1 + i * range_width, base_tick_1 + (i + 1) * range_width, liq)
        for i in range(num_ranges)
    ]
    tick_data_2 = [
        (base_tick_2 + i * range_width, base_tick_2 + (i + 1) * range_width, liq)
        for i in range(num_ranges)
    ]

    seq1 = build_seq_from_tick_data(
        tick_data_1, current_tick_1, range_idx_1, 0.003, zero_for_one=True
    )
    seq2 = build_seq_from_tick_data(
        tick_data_2, current_tick_2, range_idx_2, 0.003, zero_for_one=False
    )

    return Scenario(
        name=f"stablecoin_{num_ranges}ranges",
        seq1=seq1,
        seq2=seq2,
        tick_data_1=tick_data_1,
        tick_data_2=tick_data_2,
        current_tick_1=current_tick_1,
        current_tick_2=current_tick_2,
        current_range_idx_1=range_idx_1,
        current_range_idx_2=range_idx_2,
        fee_pips_1=3000,
        fee_pips_2=3000,
        zfo_1=True,
        zfo_2=False,
        num_ranges_1=num_ranges,
        num_ranges_2=num_ranges,
    )


def make_asymmetric_scenario(num_ranges_1: int = 5, num_ranges_2: int = 2) -> Scenario:
    """
    Asymmetric: one pool has many ranges, the other has few.
    Tests the quadratic expansion of (k1, k2) combinations.
    Both pools have at least 2 ranges in the swap direction.
    """
    tick_spacing = 60
    base_tick_1 = -83100
    base_tick_2 = -83200

    half_1 = num_ranges_1 // 2
    half_2 = max(num_ranges_2 // 2, 1)  # Ensure at least 1 range below
    range_idx_1 = half_1
    range_idx_2 = half_2
    current_tick_1 = base_tick_1 + half_1 * tick_spacing + tick_spacing // 2
    current_tick_2 = base_tick_2 + half_2 * tick_spacing + tick_spacing // 2

    liq = 2_000_000_000_000_000_000

    tick_data_1 = [
        (base_tick_1 + i * tick_spacing, base_tick_1 + (i + 1) * tick_spacing, liq)
        for i in range(num_ranges_1)
    ]
    tick_data_2 = [
        (base_tick_2 + i * tick_spacing, base_tick_2 + (i + 1) * tick_spacing, liq)
        for i in range(num_ranges_2)
    ]

    seq1 = build_seq_from_tick_data(
        tick_data_1, current_tick_1, range_idx_1, 0.003, zero_for_one=True
    )
    seq2 = build_seq_from_tick_data(
        tick_data_2, current_tick_2, range_idx_2, 0.003, zero_for_one=False
    )

    return Scenario(
        name=f"asymmetric_{num_ranges_1}x{num_ranges_2}ranges",
        seq1=seq1,
        seq2=seq2,
        tick_data_1=tick_data_1,
        tick_data_2=tick_data_2,
        current_tick_1=current_tick_1,
        current_tick_2=current_tick_2,
        current_range_idx_1=range_idx_1,
        current_range_idx_2=range_idx_2,
        fee_pips_1=3000,
        fee_pips_2=3000,
        zfo_1=True,
        zfo_2=False,
        num_ranges_1=num_ranges_1,
        num_ranges_2=num_ranges_2,
    )


# ==============================================================================
# Solvers
# ==============================================================================


def compute_v3_v3_profit_manual(
    x: float,
    crossing1: rs_mobius.RustTickRangeCrossing | None,
    crossing2: rs_mobius.RustTickRangeCrossing | None,
    hop1_ending: rs_mobius.RustV3TickRangeHop,
    hop2_ending: rs_mobius.RustV3TickRangeHop,
) -> float:
    """Manually compute V3-V3 profit for Brent evaluation."""
    if crossing1 is not None:
        if x < crossing1.crossing_gross_input:
            return -1e30
        remaining1 = x - crossing1.crossing_gross_input
        ending1_hs = crossing1.ending_range.to_hop_state()
        var_out1 = rs_mobius.py_simulate_path(remaining1, [ending1_hs])
        output1 = crossing1.crossing_output + var_out1
    else:
        output1 = rs_mobius.py_simulate_path(x, [hop1_ending.to_hop_state()])

    if crossing2 is not None:
        if output1 < crossing2.crossing_gross_input:
            return -1e30
        remaining2 = output1 - crossing2.crossing_gross_input
        ending2_hs = crossing2.ending_range.to_hop_state()
        var_out2 = rs_mobius.py_simulate_path(remaining2, [ending2_hs])
        output2 = crossing2.crossing_output + var_out2
    else:
        output2 = rs_mobius.py_simulate_path(output1, [hop2_ending.to_hop_state()])

    return output2 - x


def get_crossings(
    seq: rs_mobius.RustV3TickRangeSequence, max_k: int = 10,
) -> list[rs_mobius.RustTickRangeCrossing]:
    crossings = []
    for k in range(max_k):
        try:
            c = seq.compute_crossing(k)
            crossings.append(c)
        except (ValueError, RuntimeError):
            break
    return crossings


def brent_solve(scenario: Scenario) -> tuple[float, float, bool]:
    """Brent-based V3-V3 solver using scipy."""
    from scipy.optimize import minimize_scalar

    crossings1 = get_crossings(scenario.seq1)
    crossings2 = get_crossings(scenario.seq2)
    hop1_current = crossings1[0].ending_range
    hop2_current = crossings2[0].ending_range

    def neg_profit(x: float) -> float:
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

    hop1_hs = hop1_current.to_hop_state()
    upper = hop1_hs.reserve_in * 10

    result = minimize_scalar(
        neg_profit,
        method="bounded",
        bounds=(0, upper),
        options={"xatol": 1.0},
    )

    if not result.success or result.fun >= 0:
        return (0.0, 0.0, False)

    x_opt = result.x
    profit = -result.fun
    if profit <= 0:
        return (0.0, 0.0, False)

    return (x_opt, profit, True)


def simulate_v3_hop_with_crossings(
    amount_in: int,
    tick_data: list[tuple[int, int, int]],
    current_tick: int,
    current_range_index: int,
    fee_pips: int,
    zero_for_one: bool,  # noqa: FBT001
) -> tuple[int, int]:
    """Simulate a V3 swap with tick crossings using compute_swap_step."""
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
            remaining -= total_consumed
            sqrt_price_x96 = new_sqrt_price
            range_idx += -1 if zero_for_one else 1
        else:
            remaining = 0

    return (total_output, amount_in - remaining)


def brute_force_solve(scenario: Scenario, scan_steps: int = 1000) -> tuple[int, int]:
    """Brute-force V3-V3 solver using V3 integer swap math."""
    # Compute generous upper bound from all range capacities
    total_capacity = 0.0
    for tl, tu, lq in scenario.tick_data_1:
        sl = tick_to_sqrt_price_float(tl)
        su = tick_to_sqrt_price_float(tu)
        if scenario.zfo_1:
            total_capacity += abs(lq * (1.0 / sl - 1.0 / su))
        else:
            total_capacity += lq * (su - sl)

    upper = int(total_capacity * 20)
    if upper <= 0:
        return (0, 0)

    best_profit = 0
    best_input = 0

    # Coarse scan
    for i in range(1, scan_steps + 1):
        usdc_in = i * upper // scan_steps

        output1, _ = simulate_v3_hop_with_crossings(
            amount_in=usdc_in,
            tick_data=scenario.tick_data_1,
            current_tick=scenario.current_tick_1,
            current_range_index=scenario.current_range_idx_1,
            fee_pips=scenario.fee_pips_1,
            zero_for_one=scenario.zfo_1,
        )
        if output1 <= 0:
            continue

        output2, _ = simulate_v3_hop_with_crossings(
            amount_in=output1,
            tick_data=scenario.tick_data_2,
            current_tick=scenario.current_tick_2,
            current_range_index=scenario.current_range_idx_2,
            fee_pips=scenario.fee_pips_2,
            zero_for_one=scenario.zfo_2,
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
                tick_data=scenario.tick_data_1,
                current_tick=scenario.current_tick_1,
                current_range_index=scenario.current_range_idx_1,
                fee_pips=scenario.fee_pips_1,
                zero_for_one=scenario.zfo_1,
            )
            if output1 <= 0:
                continue

            output2, _ = simulate_v3_hop_with_crossings(
                amount_in=output1,
                tick_data=scenario.tick_data_2,
                current_tick=scenario.current_tick_2,
                current_range_index=scenario.current_range_idx_2,
                fee_pips=scenario.fee_pips_2,
                zero_for_one=scenario.zfo_2,
            )

            profit = output2 - usdc_in
            if profit > best_profit:
                best_profit = profit
                best_input = usdc_in

    return (best_input, best_profit)


# ==============================================================================
# Benchmark runner
# ==============================================================================


def bench_rust_solve(
    scenario: Scenario,
    max_candidates: int,
    n_iter: int = 10000,
) -> list[int]:
    """Benchmark Rust solve_v3_v3."""
    optimizer = rs_mobius.RustMobiusOptimizer()
    times = []
    for _ in range(n_iter):
        t0 = time.perf_counter_ns()
        optimizer.solve_v3_v3(
            scenario.seq1,
            scenario.seq2,
            max_candidates=max_candidates,
        )
        t1 = time.perf_counter_ns()
        times.append(t1 - t0)
    return times


def bench_brent_solve(scenario: Scenario, n_iter: int = 100) -> list[int]:
    """Benchmark Brent solver."""
    times = []
    for _ in range(n_iter):
        t0 = time.perf_counter_ns()
        brent_solve(scenario)
        t1 = time.perf_counter_ns()
        times.append(t1 - t0)
    return times


def bench_brute_force(scenario: Scenario, n_iter: int = 5) -> list[int]:
    """Benchmark brute-force solver."""
    times = []
    for _ in range(n_iter):
        t0 = time.perf_counter_ns()
        brute_force_solve(scenario)
        t1 = time.perf_counter_ns()
        times.append(t1 - t0)
    return times


def main():
    import numpy as np

    scenarios: list[Scenario] = []

    # Range complexity sweep: 1, 2, 3, 5, 10 ranges per hop
    for n in [1, 2, 3, 5, 10]:
        scenarios.extend((make_weth_usdc_scenario(n), make_stablecoin_scenario(n)))

    # Asymmetric scenarios (ensure both pools have ranges in swap direction)
    scenarios.extend([
        make_asymmetric_scenario(5, 3),
        make_asymmetric_scenario(10, 3),
        make_asymmetric_scenario(10, 5),
    ])

    rust_iter = 10000
    brent_iter = 100
    bf_iter = 5

    optimizer = rs_mobius.RustMobiusOptimizer()

    print(f"{'=' * 110}")
    print("V3-V3 Solver Benchmark: Rust vs Brent vs Brute-Force")
    print(f"{'=' * 110}")
    print()
    print(
        f"{'Scenario':<30} "
        f"{'Ranges':>7} "
        f"{'Rust(3)':>10} "
        f"{'Rust(10)':>10} "
        f"{'Brent':>10} "
        f"{'BF':>10} "
        f"{'Rust$':>12} "
        f"{'BF$':>12} "
        f"{'Match':>6}"
    )
    print("-" * 120)

    for scenario in scenarios:
        # Rust solve
        rust_times_3 = bench_rust_solve(scenario, max_candidates=3, n_iter=rust_iter)
        rust_times_10 = bench_rust_solve(scenario, max_candidates=10, n_iter=rust_iter)

        result_10 = optimizer.solve_v3_v3(scenario.seq1, scenario.seq2, max_candidates=10)

        # Brent (skip for 10+ range scenarios — too slow)
        brent_times: list[int] = []
        if scenario.num_ranges_1 <= 5 and scenario.num_ranges_2 <= 5:
            brent_times = bench_brent_solve(scenario, n_iter=brent_iter)

        # Brute-force
        bf_times = bench_brute_force(scenario, n_iter=bf_iter)
        _bf_input, bf_profit = brute_force_solve(scenario)

        # Accuracy check
        match = "—"
        if bf_profit > 0 and result_10.success and result_10.profit > 0:
            rust_profit_int = int(result_10.profit)
            rel_diff = abs(rust_profit_int - bf_profit) / bf_profit
            if rel_diff < 0.01:
                match = "✓"
            elif rel_diff < 0.05:
                match = "~"
            else:
                match = f"{rel_diff:.0%}"

        # Format
        ranges_str = f"{scenario.num_ranges_1}x{scenario.num_ranges_2}"
        rust3_str = f"{np.median(rust_times_3) / 1e3:.0f}μs"
        rust10_str = f"{np.median(rust_times_10) / 1e3:.0f}μs"
        brent_str = f"{np.median(brent_times) / 1e3:.0f}μs" if brent_times else "—"
        bf_str = f"{np.median(bf_times) / 1e3:.0f}μs" if bf_times else "—"
        rust_profit_str = f"{result_10.profit:.2e}" if result_10.success else "0"
        bf_profit_str = f"{bf_profit:.2e}" if bf_profit > 0 else "0"

        print(
            f"{scenario.name:<30} "
            f"{ranges_str:>7} "
            f"{rust3_str:>10} "
            f"{rust10_str:>10} "
            f"{brent_str:>10} "
            f"{bf_str:>10} "
            f"{rust_profit_str:>12} "
            f"{bf_profit_str:>12} "
            f"{match:>6}"
        )

    # Scaling analysis
    print()
    print(f"{'=' * 80}")
    print("Scaling: Rust median time vs number of candidate ranges")
    print(f"{'=' * 80}")
    print()

    scale_scenario = make_weth_usdc_scenario(10)
    print(f"{'max_candidates':>15} {'Median':>12} {'Profit':>14} {'Success':>8}")
    print("-" * 55)
    for max_cand in [1, 2, 3, 5, 7, 10]:
        times = bench_rust_solve(scale_scenario, max_candidates=max_cand, n_iter=rust_iter)
        result = optimizer.solve_v3_v3(
            scale_scenario.seq1,
            scale_scenario.seq2,
            max_candidates=max_cand,
        )
        print(
            f"{max_cand:>15} "
            f"{np.median(times) / 1e3:>10.1f}μs "
            f"{result.profit:>14.2e} "
            f"{'✓' if result.success else '✗':>8}"
        )

    # Speedup summary
    print()
    print(f"{'=' * 80}")
    print("Speedup: Rust (10 candidates) vs Brent vs Brute-Force")
    print(f"{'=' * 80}")
    print()

    for scenario in [make_weth_usdc_scenario(3), make_weth_usdc_scenario(5),
                     make_stablecoin_scenario(3), make_stablecoin_scenario(5)]:
        rust_times = bench_rust_solve(scenario, max_candidates=10, n_iter=rust_iter)
        brent_times = bench_brent_solve(scenario, n_iter=brent_iter)
        bf_times = bench_brute_force(scenario, n_iter=bf_iter)

        rust_med = np.median(rust_times)
        brent_med = np.median(brent_times) if brent_times else 0
        bf_med = np.median(bf_times) if bf_times else 0

        brent_speedup = f"{brent_med / rust_med:.0f}x" if rust_med > 0 and brent_med > 0 else "—"
        bf_speedup = f"{bf_med / rust_med:.0f}x" if rust_med > 0 and bf_med > 0 else "—"

        print(
            f"{scenario.name:<30} "
            f"Rust={rust_med / 1e3:.1f}μs  "
            f"Brent={brent_med / 1e3:.0f}μs ({brent_speedup})  "
            f"BF={bf_med / 1e3:.0f}μs ({bf_speedup})"
        )


if __name__ == "__main__":
    main()
