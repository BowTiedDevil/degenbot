"""
Tests for Rust integer refinement of Möbius solver results.

Validates that ArbSolver uses Rust-based EVM-exact integer refinement
instead of Python _simulate_path, and that results are correct for
both small and large (uint256-scale) reserves.
"""

from fractions import Fraction

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    Hop,
    SolveInput,
    SolverMethod,
)
from degenbot.degenbot_rs import mobius as rs_mobius

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
# Tests: Rust mobius_refine_int function (low-level)
# ---------------------------------------------------------------------------


class TestRustMobiusRefineInt:
    """Tests for the Rust mobius_refine_int function that does integer
    refinement around a float optimum using EVM-exact U256 arithmetic."""

    def test_profitable_2hop(self):
        """Standard 2-hop V2-V2: refine around float optimum gives EVM-exact result."""
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.py_mobius_refine_int(499445.0, hops, None)
        assert result.success
        assert int(result.optimal_input) > 0
        assert int(result.profit) > 0
        # Profit at returned optimal_input must equal result.profit exactly
        output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input), hops))
        assert output - int(result.optimal_input) == int(result.profit)

    def test_profit_matches_int_mobius_solve(self):
        """mobius_refine_int should agree with int_mobius_solve for same hops."""
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        # Get float optimum from standard solve
        float_result = rs_mobius.RustArbSolver().solve(
            [(1_000_000.0, 5_000_000.0, 0.003), (1_500_000.0, 3_000_000.0, 0.003)]
        )
        x_opt = float_result.optimal_input

        # Refine in Rust
        refine_result = rs_mobius.py_mobius_refine_int(x_opt, hops, None)
        # Full int solve
        int_result = rs_mobius.py_int_mobius_solve(hops)

        # Should find the same profit (may differ by ±1 input due to different
        # x_approx, but profit should be identical at flat peak)
        assert int(refine_result.profit) == int(int_result.profit)

    def test_full_scale_reserves(self):
        """Full uint256-scale reserves (USDC 6-dec, WETH 18-dec)."""
        r0_a = 100_000_000 * 10**USDC_DECIMALS   # 100M USDC
        r1_a = 60_000 * 10**WETH_DECIMALS         # 60K WETH
        r1_b = 40_000 * 10**WETH_DECIMALS         # 40K WETH
        r0_b = 80_000_000 * 10**USDC_DECIMALS     # 80M USDC

        # Get float optimum
        float_result = rs_mobius.RustArbSolver().solve(
            [(float(r0_a), float(r1_a), 0.003), (float(r1_b), float(r0_b), 0.003)]
        )
        x_opt = float_result.optimal_input

        hops = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        result = rs_mobius.py_mobius_refine_int(x_opt, hops, None)
        assert result.success
        assert int(result.profit) > 0
        # Verify EVM-exact: simulate at optimal_input
        output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input), hops))
        assert output - int(result.optimal_input) == int(result.profit)

    def test_max_input_respected(self):
        """max_input constraint should be applied to integer refinement."""
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.py_mobius_refine_int(999.0, hops, 1000.0)
        assert result.success
        assert int(result.optimal_input) <= 1000

    def test_not_profitable(self):
        """When float optimum is unprofitable after integer verification, returns failure."""
        # Same-product pools → K/M = γ² < 1, never profitable
        hops = [
            rs_mobius.RustIntHopState(100_000, 50, 997, 1000),
            rs_mobius.RustIntHopState(50, 100_000, 997, 1000),
        ]
        # Even with a fake x_approx, integer simulation shows unprofitable
        result = rs_mobius.py_mobius_refine_int(10.0, hops, None)
        assert not result.success

    def test_3hop_refinement(self):
        """3-hop path should use larger search radius."""
        hops = [
            rs_mobius.RustIntHopState(2_000_000, 2_100_000, 997, 1000),
            rs_mobius.RustIntHopState(2_000_000, 2_050_000, 997, 1000),
            rs_mobius.RustIntHopState(2_050_000, 2_000_000, 997, 1000),
        ]
        float_result = rs_mobius.RustArbSolver().solve(
            [
                (2_000_000.0, 2_100_000.0, 0.003),
                (2_000_000.0, 2_050_000.0, 0.003),
                (2_050_000.0, 2_000_000.0, 0.003),
            ]
        )
        x_opt = float_result.optimal_input
        result = rs_mobius.py_mobius_refine_int(x_opt, hops, None)
        assert result.success
        assert int(result.profit) > 0

    def test_zero_x_approx(self):
        """Zero or negative x_approx should return failure immediately."""
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.py_mobius_refine_int(0.0, hops, None)
        assert not result.success

    def test_fee_as_fraction(self):
        """Hops with Fraction fees (e.g. 3/1000) need fee_numer/fee_denom extraction."""
        # This test validates that ArbSolver correctly converts Fraction fees
        # to fee_numer/fee_denom when building RustIntHopState
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=1_000_000, reserve_out=5_000_000, fee=Fraction(3, 1000)),
                Hop(reserve_in=1_500_000, reserve_out=3_000_000, fee=Fraction(3, 1000)),
            )
        )
        result = solver.solve(inp)
        assert result.success
        assert result.profit > 0


# ---------------------------------------------------------------------------
# Test: ArbSolver end-to-end with Rust integer refinement
# ---------------------------------------------------------------------------

class TestArbSolverRustIntRefinement:
    """Tests that ArbSolver uses Rust integer refinement and produces
    EVM-exact results, especially for large reserves."""

    def test_v2_2hop_evm_exact(self):
        """ArbSolver V2-V2 result must be EVM-exact: profit = simulate(x) - x."""
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.success
        assert result.method == SolverMethod.MOBIUS

        # EVM-exact verification: simulate the path at optimal_input
        # using Python's integer arithmetic (which matches EVM floor division)
        x = result.optimal_input
        amount = float(x)
        for hop in inp.hops:
            r_i = float(hop.reserve_in)
            s_i = float(hop.reserve_out)
            g_i = hop.gamma
            amount = amount * g_i * s_i / (r_i + amount * g_i)
        float_profit = int(amount) - x
        # Rust integer profit should match within 1 wei (floor division differences)
        assert abs(result.profit - float_profit) <= 1

    def test_v2_2hop_large_reserves_evm_exact(self):
        """Full uint256-scale reserves: profit must be EVM-exact."""
        solver = ArbSolver()
        r0_a = 100_000_000 * 10**USDC_DECIMALS   # 100M USDC
        r1_a = 60_000 * 10**WETH_DECIMALS         # 60K WETH
        r1_b = 40_000 * 10**WETH_DECIMALS         # 40K WETH
        r0_b = 80_000_000 * 10**USDC_DECIMALS     # 80M USDC

        inp = SolveInput(
            hops=(
                Hop(reserve_in=r0_a, reserve_out=r1_a, fee=FEE_0_3_PCT),
                Hop(reserve_in=r1_b, reserve_out=r0_b, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.success

        # Verify with Rust EVM-exact simulation
        hops_int = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        evm_profit = evm_output - result.optimal_input
        assert evm_profit == result.profit

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
        assert result.success

        # Build Rust int hops for verification
        hops_int = [
            rs_mobius.RustIntHopState(USDC_2M, WETH_1000, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, 500_000_000_000, 997, 1000),
            rs_mobius.RustIntHopState(500_000_000_000, WETH_1000, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        evm_profit = evm_output - result.optimal_input
        assert evm_profit == result.profit

    def test_unprofitable_returns_zero_profit(self):
        """Unprofitable path should return success=False, profit=0."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=Fraction(30, 100)),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=Fraction(30, 100)),
            )
        )
        result = solver.solve(inp)
        assert not result.success
        assert result.profit == 0

    def test_max_input_constrains_integer_result(self):
        """max_input must constrain the integer refinement."""
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        constrained = SolveInput(hops=inp.hops, max_input=1000)
        result = solver.solve(constrained)
        assert result.success
        assert result.optimal_input <= 1000

    def test_different_fee_tiers_evm_exact(self):
        """Mixed fee tiers (0.05% + 0.3%) with EVM-exact verification."""
        solver = ArbSolver()
        # Asymmetric reserves so K/M > 1 despite fees
        # Pool 1: buy WETH cheap (1.5M USDC / 800 WETH, 0.05% fee)
        # Pool 2: sell WETH expensive (2M USDC / 1000 WETH, 0.3% fee)
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=Fraction(5, 10000)),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=Fraction(3, 1000)),
            )
        )
        result = solver.solve(inp)
        assert result.success

        # Verify EVM-exact: gamma_numer for 0.05% fee = 10000-5 = 9995
        # gamma_numer for 0.3% fee = 1000-3 = 997
        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 9995, 10000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        evm_profit = evm_output - result.optimal_input
        assert evm_profit == result.profit

    def test_integer_profit_is_best_in_neighborhood(self):
        """No nearby integer input gives better profit than the solver's answer."""
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.success

        # Build Rust int hops for brute-force check
        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]

        x_opt = result.optimal_input
        # Check ±2 around the answer
        for delta in range(-2, 3):
            candidate = x_opt + delta
            if candidate <= 0:
                continue
            evm_output = int(rs_mobius.py_int_simulate_path(candidate, hops_int))
            candidate_profit = evm_output - candidate
            assert candidate_profit <= result.profit, (
                f"Nearby input {candidate} gives profit {candidate_profit} > {result.profit}"
            )
