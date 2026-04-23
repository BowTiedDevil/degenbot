"""
Tests for raw array hop marshalling in RustArbSolver.solve_raw().

Validates that RustArbSolver.solve_raw() accepts a flat list of Python ints
(reserve_in, reserve_out, gamma_numer, fee_denom per hop) and produces
the same EVM-exact results as the object-based solve() method, while
avoiding Python object construction overhead.

This is Item #18 in the arbitrage optimizer plan.

Flat array format:
    [r_in_0, r_out_0, gamma_numer_0, fee_denom_0,
     r_in_1, r_out_1, gamma_numer_1, fee_denom_1, ...]

where gamma_numer = fee_denom - fee.numerator (e.g. 997 for 0.3% fee).
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    Hop,
    SolveInput,
    SolverMethod,
)
from degenbot.degenbot_rs import mobius as rs_mobius

from .conftest import (
    FEE_0_05_PCT,
    FEE_0_3_PCT,
    USDC_1_5M,
    USDC_2M,
    USDC_DECIMALS,
    WETH_800,
    WETH_1000,
    WETH_DECIMALS,
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def int_hops_flat(
    *hops: tuple[int, int, Fraction],
) -> list[int]:
    """Build a flat int array from (reserve_in, reserve_out, fee) tuples.

    Each hop contributes 4 elements: [r_in, r_out, gamma_numer, fee_denom].
    gamma_numer = fee_denom - fee.numerator.
    """
    flat: list[int] = []
    for r_in, r_out, fee in hops:
        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        flat.extend([r_in, r_out, gamma_numer, fee_denom])
    return flat


# ---------------------------------------------------------------------------
# Test: RustArbSolver.solve_raw() low-level API
# ---------------------------------------------------------------------------


class TestRustArbSolverSolveRaw:
    """Tests for RustArbSolver.solve_raw() accepting flat int arrays."""

    def test_solve_raw_basic_v2v2(self):
        """solve_raw with 2 V2 hops should return EVM-exact results."""
        flat = int_hops_flat(
            (1_000_000, 5_000_000, FEE_0_3_PCT),
            (1_500_000, 3_000_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)
        assert result.supported, "Int hops should be supported"
        assert result.optimal_input_int is not None
        assert result.profit_int is not None
        assert int(result.optimal_input_int) > 0
        assert int(result.profit_int) > 0

    def test_solve_raw_evm_exact(self):
        """Integer results from solve_raw must be EVM-exact:
        simulate(x) - x == profit exactly."""
        flat = int_hops_flat(
            (1_000_000, 5_000_000, FEE_0_3_PCT),
            (1_500_000, 3_000_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)

        # Verify EVM-exact via standalone int simulation
        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops_int))
        evm_profit = evm_output - int(result.optimal_input_int)
        assert evm_profit == int(result.profit_int), (
            f"EVM profit {evm_profit} != reported profit {int(result.profit_int)}"
        )

    def test_solve_raw_matches_object_solve(self):
        """solve_raw must produce the same result as solve() with
        RustIntHopState objects."""
        flat = int_hops_flat(
            (1_000_000, 5_000_000, FEE_0_3_PCT),
            (1_500_000, 3_000_000, FEE_0_3_PCT),
        )
        # Object-based solve
        obj_hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        obj_result = rs_mobius.RustArbSolver().solve(obj_hops)

        # Raw array solve
        raw_result = rs_mobius.RustArbSolver().solve_raw(flat)

        assert raw_result.success == obj_result.success
        assert int(raw_result.optimal_input_int) == int(obj_result.optimal_input_int)
        assert int(raw_result.profit_int) == int(obj_result.profit_int)

    def test_solve_raw_full_scale_reserves(self):
        """Full uint256-scale reserves (USDC 6-dec, WETH 18-dec)."""
        r0_a = 100_000_000 * 10**USDC_DECIMALS
        r1_a = 60_000 * 10**WETH_DECIMALS
        r1_b = 40_000 * 10**WETH_DECIMALS
        r0_b = 80_000_000 * 10**USDC_DECIMALS

        flat = int_hops_flat(
            (r0_a, r1_a, FEE_0_3_PCT),
            (r1_b, r0_b, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)
        assert int(result.profit_int) > 0

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops_int))
        assert evm_output - int(result.optimal_input_int) == int(result.profit_int)

    def test_solve_raw_max_input_constraint(self):
        """max_input constraint should be respected in integer refinement."""
        flat = int_hops_flat(
            (1_000_000, 5_000_000, FEE_0_3_PCT),
            (1_500_000, 3_000_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat, max_input=1000.0)
        assert int(result.optimal_input_int) <= 1000

    def test_solve_raw_not_profitable(self):
        """Same-product pools should return unprofitable."""
        flat = int_hops_flat(
            (100_000, 50, FEE_0_3_PCT),
            (50, 100_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)
        assert not result.success
        assert result.profit_int == 0

    def test_solve_raw_3hop(self):
        """3-hop path with integer hops should work with wider search radius."""
        flat = int_hops_flat(
            (2_000_000, 2_100_000, FEE_0_3_PCT),
            (2_000_000, 2_050_000, FEE_0_3_PCT),
            (2_050_000, 2_000_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)
        assert int(result.profit_int) > 0

    def test_solve_raw_best_in_neighborhood(self):
        """No nearby integer input should give better profit."""
        flat = int_hops_flat(
            (1_000_000, 5_000_000, FEE_0_3_PCT),
            (1_500_000, 3_000_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)

        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        x_opt = int(result.optimal_input_int)
        for delta in range(-2, 3):
            candidate = x_opt + delta
            if candidate <= 0:
                continue
            evm_output = int(rs_mobius.py_int_simulate_path(candidate, hops_int))
            candidate_profit = evm_output - candidate
            assert candidate_profit <= int(result.profit_int), (
                f"Neighbor {candidate} profit {candidate_profit} > {int(result.profit_int)}"
            )

    def test_solve_raw_mixed_fee_tiers(self):
        """Mixed fee tiers (0.05% + 0.3%) with EVM-exact verification."""
        flat = int_hops_flat(
            (1_000_000, 5_000_000, FEE_0_05_PCT),
            (1_500_000, 3_000_000, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)
        assert result.optimal_input_int is not None

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 9995, 10000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops_int))
        assert evm_output - int(result.optimal_input_int) == int(result.profit_int)

    def test_solve_raw_invalid_length(self):
        """Flat array length not a multiple of 4 should raise ValueError."""
        with pytest.raises(ValueError, match="multiple of 4"):
            rs_mobius.RustArbSolver().solve_raw([1, 2, 3])

    def test_solve_raw_too_few_hops(self):
        """Flat array with only 1 hop (< 2 hops) should raise ValueError."""
        flat = int_hops_flat((1_000_000, 5_000_000, FEE_0_3_PCT))
        with pytest.raises(ValueError, match="at least 2 hops"):
            rs_mobius.RustArbSolver().solve_raw(flat)

    def test_solve_raw_v3_single_range(self):
        """Single-range V3 hop (bounded product) should work via solve_raw."""
        flat = int_hops_flat(
            (USDC_1_5M, WETH_800, FEE_0_3_PCT),
            (WETH_1000, USDC_2M, FEE_0_3_PCT),
        )
        result = rs_mobius.RustArbSolver().solve_raw(flat)
        assert int(result.profit_int) > 0


# ---------------------------------------------------------------------------
# Test: ArbSolver end-to-end uses raw array marshalling
# ---------------------------------------------------------------------------


class TestArbSolverRawArrayMarshalling:
    """Tests that ArbSolver._try_rust_solve uses the raw array path
    (solve_raw) when the feature flag is enabled, and results remain
    EVM-exact."""

    def test_v2_2hop_evm_exact(self):
        """V2-V2 via ArbSolver with raw arrays must be EVM-exact."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.method == SolverMethod.MOBIUS

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        assert evm_output - result.optimal_input == result.profit

    def test_v2_2hop_large_reserves_evm_exact(self):
        """Full uint256-scale reserves via ArbSolver."""
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
        """3-hop V2 path via ArbSolver with EVM-exact verification."""
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
        """Mixed fee tiers (0.05% + 0.3%) via ArbSolver."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_05_PCT),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
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
        """No nearby integer input gives better profit via ArbSolver."""
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
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
