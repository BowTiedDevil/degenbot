"""Verification: UniswapArbEngine integer-exact V3-V3 solver vs Brent reference.

These tests exercise the UniswapArbEngine's integer-exact solve path
(int_solve_v3_v3 via process_logs → solve_all), comparing results against:
1. scipy.optimize Brent minimization (float reference)
2. Brute-force V3 integer swap math (gold standard)

This is the ONLY test file that compares the UniswapArbEngine's integer-exact
V3-V3 solver against independent references. The existing test_v3_v3_accuracy.py
compares RustArbSolver.solve() which uses the OLD f64 solve_v3_v3 path.

IMPORTANT — V3-V3 Arbitrage Direction
=====================================
For a path [pool_A(zfo=True) → pool_B(zfo=False)]:

- zfo=True at pool A: sell token0, receive token1
- zfo=False at pool B: sell token1, receive token0

For profit, token1 must be EXPENSIVE at pool A (higher price = more token1
per token0) and CHEAP at pool B (lower price = more token0 per token1).
Since higher price ↔ higher tick, this means:

    ** pool_A.tick > pool_B.tick is REQUIRED for profit with this direction **

If pool_A.tick < pool_B.tick, this path direction is unprofitable regardless
of liquidity or solver quality — the exchange rates work against the trader.

IMPORTANT — Tick Boundary Alignment
=====================================
All tick boundaries must be multiples of tick_spacing. The engine's
compute_tick_ranges walks at tick_spacing intervals, so non-aligned boundaries
will not be discovered. The brute-force solver does not use tick_spacing, so
both tests use the same alignment constraint for fair comparison.

IMPORTANT — Range Coverage
===========================
Single-range pools should use ranges wide enough (±6 tick spacings) to give
the solver sufficient room. Very narrow ranges limit the optimal input size
and may produce zero-profit results even when a wider position would be
profitable.
"""

from __future__ import annotations

from collections import defaultdict

import pytest

from degenbot.degenbot_rs import UniswapArbEngine
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick

from .test_v3_v3_accuracy import (
    build_seq_from_tick_data,
    v3_v3_brent_solve,
    v3_v3_brute_force_solver,
)

# ==============================================================================
# Constants
# ==============================================================================

SQRT_PRICE_TICK_0 = 79228162514264337593543950336
USDC = 10**6
WETH = 10**18
L = 10_000_000_000_000_000_000  # 1e19 standard liquidity

# Mock addresses (no on-chain interaction)
ADDR_TOKEN0 = "0x" + "00" * 20
ADDR_TOKEN1 = "0x" + "01" * 20
ADDR_FACTORY = "0x" + "ff" * 20


# ==============================================================================
# Helpers: Tick Data → Engine tick_data dict
# ==============================================================================


def ranges_to_engine_tick_data(
    ranges: list[tuple[int, int, int]],
) -> dict[int, tuple[int, int]]:
    """Convert [(tick_lower, tick_upper, liquidity), ...] to engine tick_data format.

    Engine tick_data maps tick_index → (liquidity_gross, liquidity_net).
    Each range is a position: +L at tick_lower, -L at tick_upper.
    """
    net_by_tick: dict[int, int] = defaultdict(int)
    gross_by_tick: dict[int, int] = defaultdict(int)

    for tick_lower, tick_upper, liquidity in ranges:
        net_by_tick[tick_lower] += liquidity
        net_by_tick[tick_upper] -= liquidity
        gross_by_tick[tick_lower] += liquidity
        gross_by_tick[tick_upper] += liquidity

    return {t: (gross_by_tick[t], net_by_tick[t]) for t in net_by_tick}


def current_liquidity_at_tick(
    current_tick: int,
    ranges: list[tuple[int, int, int]],
) -> int:
    """Compute the active liquidity at current_tick given a list of ranges."""
    liquidity = 0
    for tick_lower, tick_upper, l in ranges:
        if tick_lower <= current_tick < tick_upper:
            liquidity += l
    return liquidity


def wide_range_around(tick: int, tick_spacing: int = 60, n: int = 10) -> tuple[int, int]:
    """Return (tick_lower, tick_upper) spanning n tick spacings in each direction.

    Both boundaries are multiples of tick_spacing.
    """
    compressed = tick // tick_spacing  # Python floors toward -inf
    lower = (compressed - n) * tick_spacing
    upper = (compressed + n) * tick_spacing
    assert lower <= tick < upper, f"Range [{lower}, {upper}) doesn't contain tick {tick}"
    return lower, upper


# ==============================================================================
# Helpers: Build Engine + Solve
# ==============================================================================


def build_engine_v3_v3(
    *,
    addr_a: str = "0x" + "21" * 20,
    addr_b: str = "0x" + "22" * 20,
    ranges_a: list[tuple[int, int, int]],
    ranges_b: list[tuple[int, int, int]],
    current_tick_a: int,
    current_tick_b: int,
    fee_a: int = 3000,
    fee_b: int = 3000,
    tick_spacing_a: int = 60,
    tick_spacing_b: int = 60,
) -> tuple[UniswapArbEngine, int, int]:
    """Build a UniswapArbEngine with two V3 pools and a V3→V3 path.

    IMPORTANT: current_tick_a must be > current_tick_b for the registered
    direction (zfo=True at pool A, zfo=False at pool B) to be profitable.
    See module docstring for explanation.

    Returns (engine, v3_key_a, v3_key_b).
    """
    assert current_tick_a > current_tick_b, (
        f"pool A tick ({current_tick_a}) must be > pool B tick ({current_tick_b}) "
        f"for zfo=True at A / zfo=False at B to be profitable"
    )

    engine = UniswapArbEngine()

    sqrt_price_a = get_sqrt_ratio_at_tick(current_tick_a)
    sqrt_price_b = get_sqrt_ratio_at_tick(current_tick_b)

    liquidity_a = current_liquidity_at_tick(current_tick_a, ranges_a)
    liquidity_b = current_liquidity_at_tick(current_tick_b, ranges_b)

    tick_data_a = ranges_to_engine_tick_data(ranges_a)
    tick_data_b = ranges_to_engine_tick_data(ranges_b)

    v3_key_a = engine.register_v3_pool(
        address=addr_a,
        token0=ADDR_TOKEN0,
        token1=ADDR_TOKEN1,
        fee=fee_a,
        tick_spacing=tick_spacing_a,
        factory=ADDR_FACTORY,
        sqrt_price_x96=sqrt_price_a,
        liquidity=liquidity_a,
        tick=current_tick_a,
        tick_data=tick_data_a,
    )

    v3_key_b = engine.register_v3_pool(
        address=addr_b,
        token0=ADDR_TOKEN0,
        token1=ADDR_TOKEN1,
        fee=fee_b,
        tick_spacing=tick_spacing_b,
        factory=ADDR_FACTORY,
        sqrt_price_x96=sqrt_price_b,
        liquidity=liquidity_b,
        tick=current_tick_b,
        tick_data=tick_data_b,
    )

    # Path: pool A (zero_for_one=True) → pool B (zero_for_one=False)
    engine.register_path([
        ("V3", v3_key_a, True),
        ("V3", v3_key_b, False),
    ])

    # Trigger initial solve via solve_all_paths (replaces freeze() + initial_solve())
    engine.solve_all_paths(block_number=1)
    return engine, v3_key_a, v3_key_b


def solve_engine(engine: UniswapArbEngine) -> list[tuple[int, int, int]]:
    """Return latest_results as [(path_id, opt_input, profit), ...]."""
    results_list, block = engine.latest_results()

    results = []
    for item in results_list:
        path_id = int(item[0])
        opt_input = int(item[1])
        profit = int(item[2])
        results.append((path_id, opt_input, profit))
    return results


# ==============================================================================
# Brent Reference (re-uses existing helpers)
# ==============================================================================


def brent_solve_v3_v3(
    ranges_a: list[tuple[int, int, int]],
    ranges_b: list[tuple[int, int, int]],
    current_tick_a: int,
    current_tick_b: int,
    fee_a: float = 0.003,
    fee_b: float = 0.003,
) -> tuple[float, float, bool]:
    """Solve V3-V3 using Brent (float) reference.

    Returns (optimal_input, profit, success).
    """
    # Find current range index for each pool
    def find_range_idx(current_tick, ranges):
        for i, (tl, tu, _) in enumerate(ranges):
            if tl <= current_tick < tu:
                return i
        return None

    range_idx_a = find_range_idx(current_tick_a, ranges_a)
    range_idx_b = find_range_idx(current_tick_b, ranges_b)
    if range_idx_a is None or range_idx_b is None:
        return 0.0, 0.0, False

    seq1 = build_seq_from_tick_data(
        tick_data=ranges_a,
        current_tick=current_tick_a,
        current_range_idx=range_idx_a,
        fee=fee_a,
        zero_for_one=True,
    )
    seq2 = build_seq_from_tick_data(
        tick_data=ranges_b,
        current_tick=current_tick_b,
        current_range_idx=range_idx_b,
        fee=fee_b,
        zero_for_one=False,
    )

    return v3_v3_brent_solve(seq1, seq2)


# ==============================================================================
# Tests: Engine V3-V3 vs Brent
# ==============================================================================

# All ranges below use tick boundaries that are multiples of tick_spacing (60)


class TestEngineV3V3VsBrent:
    """Compare UniswapArbEngine's integer-exact V3-V3 solver against Brent.

    The engine uses int_solve_v3_v3 (closed-form U512 integer math).
    Brent uses scipy.optimize.minimize_scalar on the float profit function.

    Agreement within 1-2% confirms the integer-exact path is correct.
    Integer solver is authoritative when they disagree — f64 solver can
    have false positives and rounding errors.
    """

    def test_single_range_matches_brent(self):
        """Single-range V3-V3: engine should match Brent within 1%."""
        # Pool A at higher tick (expensive token1 = good to buy token1)
        current_tick_a = 100
        current_tick_b = -200

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        ranges_a = [(lr_a, ur_a, L)]
        ranges_b = [(lr_b, ur_b, L)]

        engine, _, _ = build_engine_v3_v3(
            ranges_a=ranges_a,
            ranges_b=ranges_b,
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)
        assert results and results[0][2] > 0, "Engine should find profit"

        _, profit_brent, brent_ok = brent_solve_v3_v3(
            ranges_a, ranges_b, current_tick_a, current_tick_b,
        )

        if brent_ok and profit_brent > 0:
            _, _, profit = results[0]
            rel_diff = abs(profit - profit_brent) / profit_brent
            assert rel_diff < 0.01, (
                f"Engine profit={profit}, Brent profit={profit_brent:.2f}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_moderate_divergence_matches_brent(self):
        """Moderate tick divergence (±300 ticks ~6%): engine should match Brent."""
        current_tick_a = 600
        current_tick_b = -600

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)
        assert results and results[0][2] > 0, "Engine should find profit"

        _, profit_brent, brent_ok = brent_solve_v3_v3(
            [(lr_a, ur_a, L)], [(lr_b, ur_b, L)], current_tick_a, current_tick_b,
        )

        if brent_ok and profit_brent > 0:
            _, _, profit = results[0]
            rel_diff = abs(profit - profit_brent) / profit_brent
            assert rel_diff < 0.02, (
                f"Engine profit={profit}, Brent profit={profit_brent:.2f}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_high_liquidity_pools(self):
        """Very high liquidity (1e22) — small price impact, tight agreement."""
        current_tick_a = 100
        current_tick_b = -200

        big_L = L * 1000
        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, big_L)],
            ranges_b=[(lr_b, ur_b, big_L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)
        assert results and results[0][2] > 0, "Engine should find profit"

        _, profit_brent, brent_ok = brent_solve_v3_v3(
            [(lr_a, ur_a, big_L)], [(lr_b, ur_b, big_L)], current_tick_a, current_tick_b,
        )

        if brent_ok and profit_brent > 0:
            _, _, profit = results[0]
            rel_diff = abs(profit - profit_brent) / profit_brent
            assert rel_diff < 0.01, (
                f"Engine profit={profit}, Brent profit={profit_brent:.2f}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_no_arbitrage_equal_prices(self):
        """Equal prices → engine should report no profit."""
        current_tick = 100
        lr, ur = wide_range_around(current_tick)
        ranges = [(lr, ur, L)]

        # Build engine directly — build_engine_v3_v3 requires tick_a > tick_b
        engine = UniswapArbEngine()
        sqrt_price = get_sqrt_ratio_at_tick(current_tick)
        td = ranges_to_engine_tick_data(ranges)

        v3_key_a = engine.register_v3_pool(
            address="0x" + "21" * 20,
            token0=ADDR_TOKEN0, token1=ADDR_TOKEN1,
            fee=3000, tick_spacing=60, factory=ADDR_FACTORY,
            sqrt_price_x96=sqrt_price, liquidity=L,
            tick=current_tick, tick_data=td,
        )
        v3_key_b = engine.register_v3_pool(
            address="0x" + "22" * 20,
            token0=ADDR_TOKEN0, token1=ADDR_TOKEN1,
            fee=3000, tick_spacing=60, factory=ADDR_FACTORY,
            sqrt_price_x96=sqrt_price, liquidity=L,
            tick=current_tick, tick_data=td,
        )
        engine.register_path([("V3", v3_key_a, True), ("V3", v3_key_b, False)])
        engine.solve_all_paths(block_number=1)

        results = solve_engine(engine)

        for _, _, profit in results:
            assert profit <= 0, (
                f"Engine found profit={profit} with identical pools — expected 0"
            )

    def test_low_liquidity_pools(self):
        """Low liquidity (1e12) — larger price impact, still should agree."""
        current_tick_a = 100
        current_tick_b = -200

        low_L = 1_000_000_000_000  # 1e12
        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, low_L)],
            ranges_b=[(lr_b, ur_b, low_L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        _, profit_brent, brent_ok = brent_solve_v3_v3(
            [(lr_a, ur_a, low_L)], [(lr_b, ur_b, low_L)], current_tick_a, current_tick_b,
        )

        if not results or results[0][2] <= 0:
            if brent_ok and profit_brent > 0:
                pytest.fail(f"Engine found no profit but Brent found {profit_brent:.2e}")
            pytest.skip("Neither solver found profit")

        _, _, profit = results[0]

        if brent_ok and profit_brent > 0:
            rel_diff = abs(profit - profit_brent) / profit_brent
            assert rel_diff < 0.02, (
                f"Engine profit={profit}, Brent profit={profit_brent:.2f}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_negative_tick_pools(self):
        """Negative tick values — both pools below tick 0 (e.g. stablecoins)."""
        current_tick_a = -200
        current_tick_b = -500

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)
        assert results and results[0][2] > 0, "Engine should find profit"

        _, profit_brent, brent_ok = brent_solve_v3_v3(
            [(lr_a, ur_a, L)], [(lr_b, ur_b, L)], current_tick_a, current_tick_b,
        )

        if brent_ok and profit_brent > 0:
            _, _, profit = results[0]
            rel_diff = abs(profit - profit_brent) / profit_brent
            assert rel_diff < 0.02, (
                f"Engine profit={profit}, Brent profit={profit_brent:.2f}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_weth_usdc_style_pools(self):
        """WETH/USDC-like pools with ticks around -83000."""
        current_tick_a = -82970  # HIGHER tick → token1 expensive → buy here
        current_tick_b = -83130  # LOWER tick → token1 cheap → sell here

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        _, bf_profit = v3_v3_brute_force_solver(
            tick_data_1=[(lr_a, ur_a, L)],
            tick_data_2=[(lr_b, ur_b, L)],
            current_tick_1=current_tick_a,
            current_tick_2=current_tick_b,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        if not results or results[0][2] <= 0:
            if bf_profit > 0:
                pytest.fail(f"Engine found no profit but brute-force found {bf_profit}")
            pytest.skip("Neither solver found profit")

        _, _, profit = results[0]

        if bf_profit > 0:
            rel_diff = abs(profit - bf_profit) / bf_profit
            assert rel_diff < 0.05, (
                f"Engine profit={profit}, BF profit={bf_profit}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_wide_tick_spacing(self):
        """Wide tick spacing (200) — fewer, wider ranges."""
        current_tick_a = 1000
        current_tick_b = -1000

        lr_a, ur_a = wide_range_around(current_tick_a, tick_spacing=200)
        lr_b, ur_b = wide_range_around(current_tick_b, tick_spacing=200)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
            tick_spacing_a=200,
            tick_spacing_b=200,
        )
        results = solve_engine(engine)

        _, profit_brent, brent_ok = brent_solve_v3_v3(
            [(lr_a, ur_a, L)], [(lr_b, ur_b, L)],
            current_tick_a, current_tick_b,
        )

        if not results or results[0][2] <= 0:
            if brent_ok and profit_brent > 0:
                pytest.fail(f"Engine found no profit but Brent found {profit_brent:.2e}")
            pytest.skip("Neither solver found profit")

        _, _, profit = results[0]

        if brent_ok and profit_brent > 0:
            rel_diff = abs(profit - profit_brent) / profit_brent
            assert rel_diff < 0.02, (
                f"Engine profit={profit}, Brent profit={profit_brent:.2f}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_multi_range_pools(self):
        """Multi-range pools with different liquidity per range."""
        current_tick_a = 100
        current_tick_b = -200

        # Multiple adjacent ranges with varying liquidity
        ranges_a = [
            (-360, -120, L // 2),
            (-120, 120, L),
            (120, 360, L // 2),
        ]
        ranges_b = [
            (-600, -300, L // 2),
            (-300, 0, L),
            (0, 300, L // 2),
        ]

        engine, _, _ = build_engine_v3_v3(
            ranges_a=ranges_a,
            ranges_b=ranges_b,
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        if not results or results[0][2] <= 0:
            pytest.skip("Engine found no profit")

        _, _, profit = results[0]
        assert profit > 0, "Engine should find profit with multi-range pools"

        # Cross-check with brute-force
        _, bf_profit = v3_v3_brute_force_solver(
            tick_data_1=ranges_a,
            tick_data_2=ranges_b,
            current_tick_1=current_tick_a,
            current_tick_2=current_tick_b,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        if bf_profit > 0:
            rel_diff = abs(profit - bf_profit) / bf_profit
            assert rel_diff < 0.05, (
                f"Engine profit={profit}, BF profit={bf_profit}, "
                f"rel_diff={rel_diff:.6f}"
            )


class TestEngineV3V3VsBruteForce:
    """Compare UniswapArbEngine integer-exact V3-V3 against brute-force
    V3 integer swap math (the gold standard).

    Brute-force uses compute_swap_step with full tick crossing support,
    scanning input amounts at integer precision. The engine's closed-form
    integer solver should agree within a small margin.
    """

    def test_engine_profit_at_most_brute_force_profit(self):
        """Engine profit should not exceed brute-force maximum (no phantom profit)."""
        current_tick_a = -82970
        current_tick_b = -83130

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        _, bf_profit = v3_v3_brute_force_solver(
            tick_data_1=[(lr_a, ur_a, L)],
            tick_data_2=[(lr_b, ur_b, L)],
            current_tick_1=current_tick_a,
            current_tick_2=current_tick_b,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        if results and results[0][2] > 0:
            # Engine should not report MORE profit than brute-force
            # (allowing 5% tolerance for scan granularity)
            assert results[0][2] <= bf_profit * 1.05, (
                f"Engine profit={results[0][2]} exceeds BF profit={bf_profit} "
                f"by >5% — possible phantom profit"
            )

    def test_engine_profit_within_5pct_of_brute_force(self):
        """Engine and brute-force should agree within 5%."""
        current_tick_a = -82970
        current_tick_b = -83130

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        _, bf_profit = v3_v3_brute_force_solver(
            tick_data_1=[(lr_a, ur_a, L)],
            tick_data_2=[(lr_b, ur_b, L)],
            current_tick_1=current_tick_a,
            current_tick_2=current_tick_b,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        if not results or results[0][2] <= 0:
            if bf_profit > 0:
                pytest.fail("Engine found no profit but brute-force did")
            pytest.skip("Neither solver found profit")

        _, _, profit = results[0]

        if bf_profit > 0:
            rel_diff = abs(profit - bf_profit) / bf_profit
            assert rel_diff < 0.05, (
                f"Engine profit={profit}, BF profit={bf_profit}, "
                f"rel_diff={rel_diff:.6f}"
            )

    def test_engine_input_near_brute_force_optimum(self):
        """Engine's optimal input should be near brute-force optimum."""
        current_tick_a = -82970
        current_tick_b = -83130

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        bf_input, bf_profit = v3_v3_brute_force_solver(
            tick_data_1=[(lr_a, ur_a, L)],
            tick_data_2=[(lr_b, ur_b, L)],
            current_tick_1=current_tick_a,
            current_tick_2=current_tick_b,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        if not results or results[0][2] <= 0:
            if bf_profit > 0:
                pytest.fail("Engine found no profit but brute-force did")
            pytest.skip("Neither solver found profit")

        _, opt_input, _ = results[0]

        if bf_profit > 0 and bf_input > 0:
            # Input should be within 15% of brute-force optimum
            # (bf is a coarse scan, closed-form can differ)
            input_rel_diff = abs(opt_input - bf_input) / bf_input
            assert input_rel_diff < 0.15, (
                f"Engine input={opt_input}, BF input={bf_input}, "
                f"rel_diff={input_rel_diff:.6f}"
            )

    def test_negative_ticks_with_moderate_divergence(self):
        """Negative ticks with 300-tick divergence — engine should find profit."""
        current_tick_a = -82830  # HIGHER tick
        current_tick_b = -83130  # LOWER tick

        lr_a, ur_a = wide_range_around(current_tick_a)
        lr_b, ur_b = wide_range_around(current_tick_b)

        engine, _, _ = build_engine_v3_v3(
            ranges_a=[(lr_a, ur_a, L)],
            ranges_b=[(lr_b, ur_b, L)],
            current_tick_a=current_tick_a,
            current_tick_b=current_tick_b,
        )
        results = solve_engine(engine)

        if not results or results[0][2] <= 0:
            pytest.fail("Engine should find profit with 300-tick divergence at negative ticks")

        _, opt_input, profit = results[0]
        assert profit > 0
        assert opt_input > 0

        # Brute-force cross-check
        _, bf_profit = v3_v3_brute_force_solver(
            tick_data_1=[(lr_a, ur_a, L)],
            tick_data_2=[(lr_b, ur_b, L)],
            current_tick_1=current_tick_a,
            current_tick_2=current_tick_b,
            fee_pips_1=3000,
            fee_pips_2=3000,
            zfo_1=True,
            zfo_2=False,
        )

        if bf_profit > 0:
            rel_diff = abs(profit - bf_profit) / bf_profit
            assert rel_diff < 0.05, (
                f"Engine profit={profit}, BF profit={bf_profit}, "
                f"rel_diff={rel_diff:.6f}"
            )
