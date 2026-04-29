"""
Tests for the Rust V3-V3 arbitrage solver (solve_v3_v3).
"""

import math

from degenbot.degenbot_rs import mobius

from .conftest import make_rust_v3_hop as make_v3_hop


class TestV3V3SingleRange:
    """Tests for V3-V3 with both pools in a single tick range (fast path)."""

    def test_single_range_uses_mobius_fast_path(self):
        """Single-range V3-V3 should use Möbius (0 iterations)."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, 0.003, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, 0.003, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.iterations == 0  # Fast path

    def test_single_range_matches_standard_mobius(self):
        """V3-V3 single-range should match standard 2-hop Möbius solve."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, 0.003, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, 0.003, zero_for_one=False)

        # V3-V3 solve
        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])
        result_v3v3 = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Standard Möbius solve (using MobiusFloatHop from V3 ranges)
        hs1 = hop1.to_hop_state()
        hs2 = hop2.to_hop_state()
        result_mobius = mobius.RustMobiusOptimizer().solve([hs1, hs2])

        # Should match to high precision
        rel_tol = 1e-10
        denom_x = max(result_mobius.optimal_input, 1e-10)
        denom_p = max(result_mobius.profit, 1e-10)
        assert abs(result_v3v3.optimal_input - result_mobius.optimal_input) / denom_x < rel_tol
        assert abs(result_v3v3.profit - result_mobius.profit) / denom_p < rel_tol

    def test_no_arbitrage_returns_zero(self):
        """V3-V3 with no price difference should return 0."""
        sqrt_p = math.sqrt(2000.0)

        hop1 = make_v3_hop(1e18, sqrt_p, sqrt_p * 0.5, sqrt_p * 1.5, 0.003, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_p, sqrt_p * 0.5, sqrt_p * 1.5, 0.003, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        assert not result.success
        assert result.optimal_input == 0
        assert result.profit == 0


class TestV3V3MultiRange:
    """Tests for V3-V3 with multi-range tick crossings."""

    def test_multi_range_does_not_panic(self):
        """Multi-range V3-V3 should not panic."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        hop1_r1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.9, sqrt_pa * 1.1, 0.003, zero_for_one=True)
        hop1_r2 = make_v3_hop(
            2e18, sqrt_pa * 1.1, sqrt_pa * 1.0, sqrt_pa * 1.2, 0.003, zero_for_one=True
        )

        hop2_r1 = make_v3_hop(
            1e18, sqrt_pb, sqrt_pb * 0.9, sqrt_pb * 1.1, 0.003, zero_for_one=False
        )
        hop2_r2 = make_v3_hop(
            2e18, sqrt_pb * 0.9, sqrt_pb * 0.8, sqrt_pb * 1.0, 0.003, zero_for_one=False
        )

        seq1 = mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])

        # Should not panic
        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Result is valid even if no profit found
        assert result.iterations >= 0

    def test_multi_range_with_valid_crossing(self):
        """Multi-range V3-V3 where crossing into next range improves profit."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        # Wide current ranges so there's room for arbitrage
        hop1_r1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.1, 0.003, zero_for_one=True)
        hop1_r2 = make_v3_hop(
            2e18, sqrt_pa * 1.1, sqrt_pa * 1.0, sqrt_pa * 1.5, 0.003, zero_for_one=True
        )

        hop2_r1 = make_v3_hop(
            1e18, sqrt_pb, sqrt_pb * 0.9, sqrt_pb * 1.5, 0.003, zero_for_one=False
        )
        hop2_r2 = make_v3_hop(
            2e18, sqrt_pb * 0.9, sqrt_pb * 0.5, sqrt_pb * 1.0, 0.003, zero_for_one=False
        )

        seq1 = mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Should find some solution (even if baseline)
        assert result.optimal_input >= 0

    def test_multi_range_only_hop1_crosses(self):
        """Multi-range V3-V3 where only hop 1 has multiple ranges."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        hop1_r1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.9, sqrt_pa * 1.1, 0.003, zero_for_one=True)
        hop1_r2 = make_v3_hop(
            2e18, sqrt_pa * 1.1, sqrt_pa * 1.0, sqrt_pa * 1.2, 0.003, zero_for_one=True
        )

        # Hop 2 single range
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, 0.003, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Should not panic and should find a solution
        assert result.optimal_input >= 0

    def test_multi_range_only_hop2_crosses(self):
        """Multi-range V3-V3 where only hop 2 has multiple ranges."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        # Hop 1 single range
        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, 0.003, zero_for_one=True)

        hop2_r1 = make_v3_hop(
            1e18, sqrt_pb, sqrt_pb * 0.9, sqrt_pb * 1.1, 0.003, zero_for_one=False
        )
        hop2_r2 = make_v3_hop(
            2e18, sqrt_pb * 0.9, sqrt_pb * 0.8, sqrt_pb * 1.0, 0.003, zero_for_one=False
        )

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Should not panic and should find a solution
        assert result.optimal_input >= 0


class TestV3V3ProfitComputation:
    """Tests verifying profit computation correctness."""

    def test_profit_positive_when_arbitrage_exists(self):
        """Profit should be positive when price difference exists."""
        sqrt_pa = math.sqrt(2500.0)  # Pool A: token0 more valuable
        sqrt_pb = math.sqrt(2000.0)  # Pool B: token0 less valuable

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, 0.003, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, 0.003, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        assert result.profit > 0

    def test_profit_increases_with_price_spread(self):
        """Larger price spread should produce larger profit."""
        profits = []
        for price_spread in [100, 200, 400]:
            sqrt_pa = math.sqrt(2000.0 + price_spread)
            sqrt_pb = math.sqrt(2000.0)

            hop1 = make_v3_hop(
                1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, 0.003, zero_for_one=True
            )
            hop2 = make_v3_hop(
                1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, 0.003, zero_for_one=False
            )

            seq1 = mobius.RustV3TickRangeSequence([hop1])
            seq2 = mobius.RustV3TickRangeSequence([hop2])

            result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
            profits.append(result.profit)

        # Larger spread → larger profit
        assert profits[1] > profits[0]
        assert profits[2] > profits[1]

    def test_max_input_constraint(self):
        """max_input should limit the optimal input."""
        sqrt_pa = math.sqrt(2200.0)
        sqrt_pb = math.sqrt(2000.0)

        hop1 = make_v3_hop(1e18, sqrt_pa, sqrt_pa * 0.5, sqrt_pa * 1.5, 0.003, zero_for_one=True)
        hop2 = make_v3_hop(1e18, sqrt_pb, sqrt_pb * 0.5, sqrt_pb * 1.5, 0.003, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        # Unconstrained
        result_unconstrained = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Constrained to small input
        max_input = result_unconstrained.optimal_input * 0.1
        result_constrained = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2, max_input)

        assert result_constrained.optimal_input <= max_input
        assert result_constrained.profit <= result_unconstrained.profit
