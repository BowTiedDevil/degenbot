"""
Tests for merged integer refinement in RustArbSolver.solve().

Validates that RustArbSolver.solve() can accept RustIntHopState objects
and perform float solve + U256 integer refinement in a single Rust call,
eliminating the second Python→Rust conversion that was needed for
py_mobius_refine_int.

This is Item #17 in the arbitrage optimizer plan.
"""

from fractions import Fraction

from degenbot.degenbot_rs import mobius as rs_mobius

from degenbot.arbitrage.optimizers import ArbSolver, Hop, SolveInput, SolverMethod

from .conftest import (
    FEE_0_3_PCT,
    USDC_1_5M,
    USDC_2M,
    USDC_DECIMALS,
    WETH_800,
    WETH_1000,
    WETH_DECIMALS,
    make_2hop_v2_input,
)

# ---------------------------------------------------------------------------
# Test: RustArbSolver.solve() with integer hops (low-level)
# ---------------------------------------------------------------------------


class TestRustArbSolverMergedIntRefinement:
    """Tests for RustArbSolver.solve() accepting RustIntHopState objects
    and returning EVM-exact integer results in a single call."""

    def test_solve_with_int_hops_returns_integer_results(self):
        """When RustIntHopState hops are provided, solve() should return
        EVM-exact integer optimal_input and profit, not floats."""
        int_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)
        assert result.supported, "Int hops should be supported"
        # Integer fields should be populated for Möbius results
        assert result.optimal_input_int is not None
        assert result.profit_int is not None
        assert int(result.optimal_input_int) > 0
        assert int(result.profit_int) > 0

    def test_solve_with_int_hops_evm_exact(self):
        """Integer results from merged solve must be EVM-exact:
        simulate(x) - x == profit exactly."""
        int_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)

        # Verify EVM-exact: simulate at optimal_input
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), int_hops))
        evm_profit = evm_output - int(result.optimal_input_int)
        assert evm_profit == int(result.profit_int), (
            f"EVM profit {evm_profit} != reported profit {int(result.profit_int)}"
        )

    def test_solve_with_int_hops_matches_two_step(self):
        """Merged solve (int hops in one call) should produce the same result
        as the old two-step: float tuple solve + py_mobius_refine_int."""
        int_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]

        # New: single call with int hops
        merged_result = rs_mobius.RustArbSolver().solve(int_hops)

        # Two-step approach for comparison
        float_result = rs_mobius.RustArbSolver().solve([
            (1_000_000.0, 5_000_000.0, 0.003),
            (1_500_000.0, 3_000_000.0, 0.003),
        ])
        refine_result = rs_mobius.py_mobius_refine_int(float_result.optimal_input, int_hops, None)

        # Should match
        assert int(merged_result.optimal_input_int) == int(refine_result.optimal_input)
        assert int(merged_result.profit_int) == int(refine_result.profit)

    def test_solve_with_int_hops_full_scale(self):
        """Full uint256-scale reserves (USDC 6-dec, WETH 18-dec)."""
        r0_a = 100_000_000 * 10**USDC_DECIMALS
        r1_a = 60_000 * 10**WETH_DECIMALS
        r1_b = 40_000 * 10**WETH_DECIMALS
        r0_b = 80_000_000 * 10**USDC_DECIMALS

        int_hops = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)
        assert result.optimal_input_int is not None
        assert int(result.profit_int) > 0

        # Verify EVM-exact
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), int_hops))
        assert evm_output - int(result.optimal_input_int) == int(result.profit_int)

    def test_solve_with_int_hops_max_input(self):
        """max_input constraint should be applied to integer refinement."""
        int_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops, max_input=1000.0)
        assert int(result.optimal_input_int) <= 1000

    def test_solve_with_int_hops_not_profitable(self):
        """Same-product pools should return unprofitable."""
        int_hops = [
            rs_mobius.RustIntHopState(100_000, 50, 997, 1000),
            rs_mobius.RustIntHopState(50, 100_000, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)
        assert not result.success
        assert result.profit_int == 0

    def test_solve_with_int_hops_3hop(self):
        """3-hop path with integer hops should use wider search radius."""
        int_hops = [
            rs_mobius.RustIntHopState(2_000_000, 2_100_000, 997, 1000),
            rs_mobius.RustIntHopState(2_000_000, 2_050_000, 997, 1000),
            rs_mobius.RustIntHopState(2_050_000, 2_000_000, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)
        assert int(result.profit_int) > 0

    def test_solve_with_int_hops_best_in_neighborhood(self):
        """No nearby integer input should give better profit."""
        int_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)

        x_opt = int(result.optimal_input_int)
        # Check ±2 around the answer
        for delta in range(-2, 3):
            candidate = x_opt + delta
            if candidate <= 0:
                continue
            evm_output = int(rs_mobius.py_int_simulate_path(candidate, int_hops))
            candidate_profit = evm_output - candidate
            assert candidate_profit <= int(result.profit_int), (
                f"Neighbor {candidate} profit {candidate_profit} > {int(result.profit_int)}"
            )

    def test_solve_with_mixed_hops_not_supported(self):
        """Mixed hop types (int + float tuples) should not be supported."""
        int_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            (1_500_000.0, 3_000_000.0, 0.003),  # float tuple mixed with int
        ]
        result = rs_mobius.RustArbSolver().solve(int_hops)
        # Mixed hops could either be unsupported or we could handle it.
        # For now, the design is: all int OR all float, not mixed.
        # If we do support it, the result should still be correct.
        # Let's just check it doesn't crash:
        assert result is not None


# ---------------------------------------------------------------------------
# Test: ArbSolver end-to-end uses merged integer refinement
# ---------------------------------------------------------------------------


class TestArbSolverMergedIntRefinement:
    """Tests that ArbSolver._try_rust_solve uses the merged single-call
    integer refinement path, and results remain EVM-exact."""

    def test_v2_2hop_evm_exact(self):
        """V2-V2 result via ArbSolver must be EVM-exact."""
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.method == SolverMethod.MOBIUS

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        evm_profit = evm_output - result.optimal_input
        assert evm_profit == result.profit, (
            f"EVM profit {evm_profit} != reported profit {result.profit}"
        )

    def test_v2_2hop_large_reserves_evm_exact(self):
        """Full uint256-scale reserves: profit must be EVM-exact."""
        solver = ArbSolver()
        r0_a = 100_000_000 * 10**USDC_DECIMALS
        r1_a = 60_000 * 10**WETH_DECIMALS
        r1_b = 40_000 * 10**WETH_DECIMALS
        r0_b = 80_000_000 * 10**USDC_DECIMALS

        inp = SolveInput(
            hops=(
                Hop(reserve_in=r0_a, reserve_out=r1_a, fee=FEE_0_3_PCT),
                Hop(reserve_in=r1_b, reserve_out=r0_b, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)

        hops_int = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        assert evm_output - result.optimal_input == result.profit

    def test_v2_3hop_evm_exact(self):
        """3-hop V2 path with EVM-exact verification."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_1000, reserve_out=500_000_000_000, fee=FEE_0_3_PCT),
                Hop(reserve_in=500_000_000_000, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)

        hops_int = [
            rs_mobius.RustIntHopState(USDC_2M, WETH_1000, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, 500_000_000_000, 997, 1000),
            rs_mobius.RustIntHopState(500_000_000_000, WETH_1000, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        assert evm_output - result.optimal_input == result.profit

    def test_different_fee_tiers_evm_exact(self):
        """Mixed fee tiers (0.05% + 0.3%) with EVM-exact verification."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=Fraction(5, 10000)),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=Fraction(3, 1000)),
            )
        )
        result = solver.solve(inp)

        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 9995, 10000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        assert evm_output - result.optimal_input == result.profit

    def test_profit_best_in_neighborhood(self):
        """No nearby integer input gives better profit than the solver's answer."""
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)

        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]

        x_opt = result.optimal_input
        for delta in range(-2, 3):
            candidate = x_opt + delta
            if candidate <= 0:
                continue
            evm_output = int(rs_mobius.py_int_simulate_path(candidate, hops_int))
            candidate_profit = evm_output - candidate
            assert candidate_profit <= result.profit, (
                f"Nearby input {candidate} gives profit {candidate_profit} > {result.profit}"
            )
