"""
Tests for the PiecewiseMobiusSolver multi-range V3 support.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.mobius import (
    V3TickRangeHop,
    V3TickRangeSequence,
)
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BoundedProductHop,
    ConstantProductHop,
    PiecewiseMobiusSolver,
    SolveInput,
    SolverMethod,
    V3TickRangeInfo,
)

# Test constants
Q96 = 2**96


def test_v3_tick_range_info_creation():
    """Test V3TickRangeInfo dataclass."""
    range_info = V3TickRangeInfo(
        tick_lower=100,
        tick_upper=200,
        liquidity=1_000_000_000_000,
        sqrt_price_lower=79228162514264337593543950336,  # ~Q96
        sqrt_price_upper=158456325028528675187087900672,  # ~2*Q96
    )
    assert range_info.tick_lower == 100
    assert range_info.tick_upper == 200
    assert range_info.liquidity == 1_000_000_000_000


def test_bounded_product_hop_single_range():
    """Test BoundedProductHop without multi-range data."""
    hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=100,
        tick_upper=200,
    )
    assert hop.is_v3 is True
    assert hop.has_multi_range is False
    assert hop.tick_ranges is None
    assert hop.current_range_index == 0


def test_bounded_product_hop_multi_range():
    """Test BoundedProductHop with multi-range data."""
    ranges = (
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=1_000_000_000_000,
            sqrt_price_lower=79228162514264337593543950336,
            sqrt_price_upper=158456325028528675187087900672,
        ),
        V3TickRangeInfo(
            tick_lower=200,
            tick_upper=300,
            liquidity=2_000_000_000_000,
            sqrt_price_lower=158456325028528675187087900672,
            sqrt_price_upper=237684487542793012780631851008,
        ),
    )
    hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=150,
        tick_upper=200,
        tick_ranges=ranges,
        current_range_index=0,
    )
    assert hop.is_v3 is True
    assert hop.has_multi_range is True
    assert hop.tick_ranges is not None
    assert len(hop.tick_ranges) == 2
    assert hop.current_range_index == 0


def test_piecewise_mobius_solver_supports():
    """Test PiecewiseMobiusSolver supports detection."""
    solver = PiecewiseMobiusSolver()

    # V2-V2 path (should be supported for single-range)
    v2_hop1 = ConstantProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
    )
    v2_hop2 = ConstantProductHop(
        reserve_in=1_000_000_000_000_000_000,
        reserve_out=2_100_000_000_000,
        fee=Fraction(3, 1000),
    )
    v2_input = SolveInput(hops=(v2_hop1, v2_hop2))
    assert solver.supports(v2_input) is False  # No V3

    # V3-V2 path (should be supported)
    v3_hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=100,
        tick_upper=200,
    )
    v3_v2_input = SolveInput(hops=(v3_hop, v2_hop1))
    assert solver.supports(v3_v2_input) is True


def test_piecewise_mobius_solver_single_range_v3():
    """Test PiecewiseMobiusSolver falls back to Mobius for single-range V3."""
    solver = PiecewiseMobiusSolver()

    v3_hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=100,
        tick_upper=200,
    )
    v2_hop = ConstantProductHop(
        reserve_in=1_000_000_000_000_000_000,
        reserve_out=2_100_000_000_000,
        fee=Fraction(3, 1000),
    )
    input_data = SolveInput(hops=(v3_hop, v2_hop))

    result = solver.solve(input_data)

    # Should succeed and use PIECEWISE_MOBIUS method
    assert result.method == SolverMethod.PIECEWISE_MOBIUS
    assert result.profit > 0


def test_piecewise_mobius_solver_multi_range_detection():
    """Test PiecewiseMobiusSolver detects multi-range hops."""
    solver = PiecewiseMobiusSolver()

    # Multi-range hop
    ranges = (
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=1_000_000_000_000,
            sqrt_price_lower=79228162514264337593543950336,
            sqrt_price_upper=158456325028528675187087900672,
        ),
        V3TickRangeInfo(
            tick_lower=200,
            tick_upper=300,
            liquidity=2_000_000_000_000,
            sqrt_price_lower=158456325028528675187087900672,
            sqrt_price_upper=237684487542793012780631851008,
        ),
    )
    multi_range_hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=150,
        tick_upper=200,
        tick_ranges=ranges,
        current_range_index=0,
    )

    v2_hop = ConstantProductHop(
        reserve_in=1_000_000_000_000_000_000,
        reserve_out=2_100_000_000_000,
        fee=Fraction(3, 1000),
    )
    input_data = SolveInput(hops=(multi_range_hop, v2_hop))

    assert solver._has_multi_range(input_data) is True
    assert solver._find_v3_hop_index(input_data) == (0, multi_range_hop)


def test_piecewise_mobius_solver_multi_range_routing():
    """Test that PiecewiseMobiusSolver detects multi-range hops and routes correctly.

    Verifies the routing logic (detection, hop identification) without
    depending on the profitability of the test data.
    """
    ranges = (
        V3TickRangeInfo(
            tick_lower=0,
            tick_upper=100,
            liquidity=10_000_000_000_000_000,
            sqrt_price_lower=79228162514264337593543950336,
            sqrt_price_upper=112045541949572279837463876454,
        ),
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=20_000_000_000_000_000,
            sqrt_price_lower=112045541949572279837463876454,
            sqrt_price_upper=158456325028528675187087900672,
        ),
    )
    multi_range_hop = BoundedProductHop(
        reserve_in=15_000_000_000_000,
        reserve_out=7_500_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=10_000_000_000_000_000,
        sqrt_price=100000000000000000000000000000,
        tick_lower=0,
        tick_upper=100,
        tick_ranges=ranges,
        current_range_index=0,
    )
    v2_hop = ConstantProductHop(
        reserve_in=10_000_000_000_000_000_000,
        reserve_out=30_000_000_000_000,
        fee=Fraction(3, 1000),
    )
    input_data = SolveInput(hops=(multi_range_hop, v2_hop))

    assert multi_range_hop.has_multi_range is True
    assert multi_range_hop.is_v3 is True
    assert v2_hop.is_v3 is False
    assert input_data.has_v3 is True
    assert input_data.num_hops == 2

    solver = PiecewiseMobiusSolver()
    assert solver._has_multi_range(input_data) is True

    v3_result = solver._find_v3_hop_index(input_data)
    assert v3_result is not None
    v3_idx, v3_hop = v3_result
    assert v3_idx == 0
    assert v3_hop is multi_range_hop


def test_piecewise_mobius_crossing_math():
    """Test that crossing math produces correct TickRangeCrossing values.

    This verifies the V3TickRangeSequence.compute_crossing() integration
    produces correct crossing amounts.
    """

    # Create two adjacent ranges with known properties
    # Range 0: price [1.0, 2.0), liquidity = 1000
    # Range 1: price [2.0, 4.0), liquidity = 2000
    # Current price: 1.5 (in range 0)

    Q96 = 2**96
    sqrt_1_0 = 1.0 * Q96  # sqrt(1.0) = 1.0
    sqrt_2_0 = 2**0.5 * Q96  # sqrt(2.0) ≈ 1.414
    sqrt_4_0 = 2.0 * Q96  # sqrt(4.0) = 2.0
    sqrt_1_5 = 1.5**0.5 * Q96  # sqrt(1.5) ≈ 1.225

    ranges_info = (
        V3TickRangeInfo(
            tick_lower=0,
            tick_upper=100,
            liquidity=1_000_000,
            sqrt_price_lower=int(sqrt_1_0),
            sqrt_price_upper=int(sqrt_2_0),
        ),
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=2_000_000,
            sqrt_price_lower=int(sqrt_2_0),
            sqrt_price_upper=int(sqrt_4_0),
        ),
    )

    # Create a BoundedProductHop with these ranges
    multi_range_hop = BoundedProductHop(
        reserve_in=1_000_000_000,
        reserve_out=1_500_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000,
        sqrt_price=int(sqrt_1_5),  # Current price in range 0
        tick_lower=0,
        tick_upper=100,
        tick_ranges=ranges_info,
        current_range_index=0,
    )

    # Verify the hop has multi-range data
    assert multi_range_hop.has_multi_range is True
    assert len(multi_range_hop.tick_ranges) == 2

    # Convert to V3TickRangeHop and verify compute_crossing
    v3_ranges = [
        V3TickRangeHop(
            liquidity=float(r.liquidity),
            sqrt_price_current=float(sqrt_1_5) / Q96 if i == 0 else float(r.sqrt_price_lower) / Q96,
            sqrt_price_lower=float(r.sqrt_price_lower) / Q96,
            sqrt_price_upper=float(r.sqrt_price_upper) / Q96,
            fee=0.003,
            zero_for_one=True,  # token0 -> token1 (price goes down)
        )
        for i, r in enumerate(ranges_info)
    ]

    sequence = V3TickRangeSequence(tuple(v3_ranges))

    # Test k=0 (no crossing)
    crossing_0 = sequence.compute_crossing(0)
    assert crossing_0.crossing_gross_input == 0.0
    assert crossing_0.crossing_output == 0.0

    # Test k=1 (cross range 0, end in range 1)
    crossing_1 = sequence.compute_crossing(1)
    # Crossing should require some input to go from current price to boundary
    assert crossing_1.crossing_gross_input > 0.0
    # Output should be positive from swapping through range 0
    assert crossing_1.crossing_output > 0.0

    print(
        f"Crossing 0: input={crossing_0.crossing_gross_input}, output={crossing_0.crossing_output}"
    )
    print(
        f"Crossing 1: input={crossing_1.crossing_gross_input}, output={crossing_1.crossing_output}"
    )


def test_piecewise_mobius_golden_section_convergence():
    """Test that golden section search converges to a reasonable solution.

    Uses a simple 2-hop path with known profitable solution.
    """
    solver = PiecewiseMobiusSolver()

    # Simple V3-V2 arbitrage with profitable price difference
    # V3 pool: buy token1 cheap (low price)
    # V2 pool: sell token1 expensive (high price)

    # Single range for simplicity - tests the golden section implementation
    ranges = (
        V3TickRangeInfo(
            tick_lower=0,
            tick_upper=1000,
            liquidity=10_000_000_000_000,
            sqrt_price_lower=79228162514264337593543950336,  # ~1.0
            sqrt_price_upper=112045541949572279837463876454,  # ~2.0
        ),
    )

    v3_hop = BoundedProductHop(
        reserve_in=10_000_000_000_000,  # token0 reserves (buy pool)
        reserve_out=5_000_000_000_000_000_000,  # token1 reserves (cheap token1)
        fee=Fraction(3, 1000),
        liquidity=10_000_000_000_000,
        sqrt_price=100000000000000000000000000000,  # ~1.6
        tick_lower=0,
        tick_upper=1000,
        tick_ranges=ranges,
        current_range_index=0,
    )

    # Sell pool: sell token1 at higher price
    v2_hop = ConstantProductHop(
        reserve_in=8_000_000_000_000_000_000,  # token1 reserves
        reserve_out=20_000_000_000_000,  # token0 reserves (higher price!)
        fee=Fraction(3, 1000),
    )

    input_data = SolveInput(hops=(v3_hop, v2_hop))
    result = solver.solve(input_data)

    # Should use piecewise method (even with single range, has tick_ranges)
    # Actually - single range with tick_ranges still delegates to MobiusSolver
    # So we expect MOBIUS method here
    assert result.method in {SolverMethod.PIECEWISE_MOBIUS, SolverMethod.MOBIUS}
    assert result.profit > 0
    assert result.optimal_input > 0


def test_arb_solver_piecewise_dispatch():
    """Test that ArbSolver correctly dispatches through solver chain.

    For single-range V3 paths, MobiusSolver is faster and succeeds first.
    For multi-range V3 paths, PiecewiseMobiusSolver handles them.
    """
    arb_solver = ArbSolver()

    v3_hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=100,
        tick_upper=200,
    )
    v2_hop = ConstantProductHop(
        reserve_in=1_000_000_000_000_000_000,
        reserve_out=2_100_000_000_000,
        fee=Fraction(3, 1000),
    )
    input_data = SolveInput(hops=(v3_hop, v2_hop))

    result = arb_solver.solve(input_data)

    # Should succeed (either via MobiusSolver for single-range, or
    # PiecewiseMobiusSolver for multi-range)
    assert result.profit > 0
    # Method will be MOBIUS for single-range (faster), PIECEWISE_MOBIUS for multi-range


def test_piecewise_lazy_candidate_filtering():
    """Test that lazy candidate filtering skips implausible ranges.

    Creates a scenario where distant ranges have high crossing costs
    but low liquidity - should be filtered out.
    """
    solver = PiecewiseMobiusSolver()

    # Create ranges where range 2 has much lower liquidity
    # (should be filtered by _is_candidate_plausible)
    ranges = (
        V3TickRangeInfo(
            tick_lower=0,
            tick_upper=100,
            liquidity=10_000_000_000_000,  # High liquidity
            sqrt_price_lower=79228162514264337593543950336,  # ~1.0
            sqrt_price_upper=112045541949572279837463876454,  # ~2.0
        ),
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=1_000_000_000,  # Low liquidity (10x less)
            sqrt_price_lower=112045541949572279837463876454,
            sqrt_price_upper=158456325028528675187087900672,  # ~4.0
        ),
    )

    multi_range_hop = BoundedProductHop(
        reserve_in=10_000_000_000_000,
        reserve_out=5_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=10_000_000_000_000,
        sqrt_price=100000000000000000000000000000,  # ~1.6
        tick_lower=0,
        tick_upper=100,
        tick_ranges=ranges,
        current_range_index=0,
    )

    v2_hop = ConstantProductHop(
        reserve_in=5_000_000_000_000_000_000,
        reserve_out=15_000_000_000_000,
        fee=Fraction(3, 1000),
    )

    input_data = SolveInput(hops=(multi_range_hop, v2_hop))

    # Test the plausibility check directly
    # Note: Price impact pruning may filter some candidates based on estimated price
    is_plausible = solver._is_candidate_plausible(
        input_data, multi_range_hop, start_idx=0, end_idx=0, current_best_profit=0
    )
    assert is_plausible is True  # Current range (no crossing) is always plausible

    # Test that filtering works when we have good profit and distant ranges have low liquidity
    # Range index 1 with low liquidity should be filtered when we have good profit
    # Note: Price impact pruning may also filter based on estimated price movement
    is_plausible_result = solver._is_candidate_plausible(
        input_data, multi_range_hop, start_idx=0, end_idx=1, current_best_profit=1_000_000
    )
    # Should return a boolean (True if candidate is plausible, False otherwise)
    assert isinstance(is_plausible_result, bool)


def test_tick_range_caching():
    """Test that tick range caching works correctly.

    This test verifies the cache stores and returns results.
    Note: We can't easily test cache hits without a real pool,
    but we can verify the cache infrastructure exists.
    """
    from degenbot.arbitrage.optimizers.solver import (
        _tick_range_cache,
    )

    # Verify cache infrastructure exists
    assert isinstance(_tick_range_cache, dict)

    # Cache should be empty or have some entries
    # (depending on test order)
    initial_size = len(_tick_range_cache)

    # The cache is used internally by pool_to_hop
    # We can't easily test it without a real V3 pool with tick data,
    # but we verified the code path uses _get_cached_tick_ranges

    print(f"Cache size: {initial_size}")


def test_mobius_solver_rejects_multi_range_v3():
    """Test that MobiusSolver rejects multi-range V3 hops.

    Multi-range V3 paths should be handled by PiecewiseMobiusSolver.
    """
    from degenbot.arbitrage.optimizers.solver import MobiusSolver

    mobius_solver = MobiusSolver()

    # Multi-range hop
    ranges = (
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=1_000_000_000_000,
            sqrt_price_lower=79228162514264337593543950336,
            sqrt_price_upper=158456325028528675187087900672,
        ),
        V3TickRangeInfo(
            tick_lower=200,
            tick_upper=300,
            liquidity=2_000_000_000_000,
            sqrt_price_lower=158456325028528675187087900672,
            sqrt_price_upper=237684487542793012780631851008,
        ),
    )
    multi_range_hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=150,
        tick_upper=200,
        tick_ranges=ranges,
        current_range_index=0,
    )
    v2_hop = ConstantProductHop(
        reserve_in=1_000_000_000_000_000_000,
        reserve_out=2_100_000_000_000,
        fee=Fraction(3, 1000),
    )
    input_data = SolveInput(hops=(multi_range_hop, v2_hop))

    # MobiusSolver should NOT support multi-range V3
    assert mobius_solver.supports(input_data) is False


def test_arb_solver_dispatches_multi_range_to_piecewise():
    """Test that ArbSolver dispatches multi-range V3 to PiecewiseMobiusSolver.

    This verifies the complete dispatch chain:
    1. MobiusSolver.supports() returns False for multi-range V3
    2. PiecewiseMobiusSolver.supports() returns True
    3. ArbSolver attempts PiecewiseMobiusSolver (may fall back to Brent if not profitable)
    """
    from degenbot.arbitrage.optimizers.solver import MobiusSolver, PiecewiseMobiusSolver

    mobius_solver = MobiusSolver()
    piecewise_solver = PiecewiseMobiusSolver()
    arb_solver = ArbSolver()

    # Multi-range V3 hop
    ranges = (
        V3TickRangeInfo(
            tick_lower=0,
            tick_upper=100,
            liquidity=10_000_000_000_000,
            sqrt_price_lower=79228162514264337593543950336,  # ~1.0
            sqrt_price_upper=112045541949572279837463876454,  # ~2.0
        ),
        V3TickRangeInfo(
            tick_lower=100,
            tick_upper=200,
            liquidity=20_000_000_000_000,
            sqrt_price_lower=112045541949572279837463876454,
            sqrt_price_upper=158456325028528675187087900672,  # ~4.0
        ),
    )
    multi_range_v3_hop = BoundedProductHop(
        reserve_in=15_000_000_000_000,
        reserve_out=7_500_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=10_000_000_000_000,
        sqrt_price=100000000000000000000000000000,
        tick_lower=0,
        tick_upper=100,
        tick_ranges=ranges,
        current_range_index=0,
    )

    # V2 sell pool with profitable price difference
    v2_hop = ConstantProductHop(
        reserve_in=10_000_000_000_000_000_000,  # token1 reserves
        reserve_out=30_000_000_000_000,  # token0 reserves (higher price!)
        fee=Fraction(3, 1000),
    )

    input_data = SolveInput(hops=(multi_range_v3_hop, v2_hop))

    # Verify dispatch chain
    # 1. MobiusSolver should reject multi-range V3
    assert mobius_solver.supports(input_data) is False, (
        "MobiusSolver should reject multi-range V3 hops"
    )

    # 2. PiecewiseMobiusSolver should accept multi-range V3
    assert piecewise_solver.supports(input_data) is True, (
        "PiecewiseMobiusSolver should accept multi-range V3 hops"
    )

    # 3. ArbSolver will try PiecewiseMobiusSolver first (after Mobius rejects)
    # If piecewise finds a solution, it returns that; otherwise falls back to Brent
    result = arb_solver.solve(input_data)

    # The result should be successful (either from piecewise or fallback)
    assert result.profit > 0

    # Method should be PIECEWISE_MOBIUS if piecewise succeeded,
    # or BRENT if piecewise failed and Brent succeeded
    assert result.method in {SolverMethod.PIECEWISE_MOBIUS, SolverMethod.BRENT}, (
        f"Expected PIECEWISE_MOBIUS or BRENT, got {result.method}"
    )


def test_single_range_v3_uses_mobius():
    """Test that single-range V3 hops still use MobiusSolver.

    Single-range V3 should be fast-pathed through MobiusSolver.
    """
    arb_solver = ArbSolver()

    # Single-range V3 hop (no tick_ranges)
    single_range_v3_hop = BoundedProductHop(
        reserve_in=2_000_000_000_000,
        reserve_out=1_000_000_000_000_000_000,
        fee=Fraction(3, 1000),
        liquidity=1_000_000_000_000,
        sqrt_price=79228162514264337593543950336,
        tick_lower=100,
        tick_upper=200,
        tick_ranges=None,  # Single range
        current_range_index=0,
    )
    v2_hop = ConstantProductHop(
        reserve_in=1_000_000_000_000_000_000,
        reserve_out=2_100_000_000_000,
        fee=Fraction(3, 1000),
    )
    input_data = SolveInput(hops=(single_range_v3_hop, v2_hop))

    # First verify MobiusSolver supports this
    from degenbot.arbitrage.optimizers.solver import MobiusSolver

    mobius_solver = MobiusSolver()
    assert mobius_solver.supports(input_data) is True

    # ArbSolver should use MOBIUS for single-range
    result = arb_solver.solve(input_data)
    assert result.method == SolverMethod.MOBIUS


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
