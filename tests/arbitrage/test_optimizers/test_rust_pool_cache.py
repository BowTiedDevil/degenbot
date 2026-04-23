"""
Tests for RustPoolCache — direct pool state to Rust solver.

Validates that pool states registered in a Rust-side cache can be
solved by passing only pool IDs, eliminating all Python object
construction and per-item extraction overhead on the solve path.

This is Item #20 in the arbitrage optimizer plan.

Architecture:
- RustPoolCache stores pool states (reserves + fees) keyed by u64 IDs
- insert() registers/updates a pool's state
- solve() takes a list of pool IDs, looks up cached state, and solves
- No Python objects are created on the solve path — just integer IDs
"""

from fractions import Fraction

import pytest
from degenbot.degenbot_rs import mobius as rs_mobius

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    ConstantProductHop,
    SolveInput,
    SolverMethod,
)

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


def fee_to_gamma(fee: Fraction) -> tuple[int, int]:
    """Convert a Fraction fee to (gamma_numer, fee_denom).

    gamma = 1 - fee, so gamma_numer = fee.denominator - fee.numerator.
    """
    return (fee.denominator - fee.numerator, fee.denominator)


# ---------------------------------------------------------------------------
# Test: RustPoolCache low-level API
# ---------------------------------------------------------------------------


class TestRustPoolCache:
    """Tests for the RustPoolCache struct."""

    def test_cache_creation(self):
        """RustPoolCache should be constructable."""
        cache = rs_mobius.RustPoolCache()
        assert cache is not None

    def test_insert_and_solve(self):
        """Insert two pools, then solve by ID."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        result = cache.solve([0, 1])
        assert result.supported, "Path should be supported"
        assert result.optimal_input_int is not None
        assert result.profit_int is not None
        assert int(result.optimal_input_int) > 0
        assert int(result.profit_int) > 0

    def test_solve_evm_exact(self):
        """Results from cache.solve must be EVM-exact."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        result = cache.solve([0, 1])

        # Verify EVM-exact via standalone int simulation
        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops_int))
        assert evm_output - int(result.optimal_input_int) == int(result.profit_int)

    def test_solve_matches_solve_raw(self):
        """Cache solve must produce the same result as solve_raw."""
        cache = rs_mobius.RustPoolCache()
        solver = rs_mobius.RustArbSolver()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        cache_result = cache.solve([0, 1])

        flat = [
            1_000_000,
            5_000_000,
            gamma_numer,
            fee_denom,
            1_500_000,
            3_000_000,
            gamma_numer,
            fee_denom,
        ]
        raw_result = solver.solve_raw(flat)

        assert cache_result.success == raw_result.success
        assert int(cache_result.optimal_input_int) == int(raw_result.optimal_input_int)
        assert int(cache_result.profit_int) == int(raw_result.profit_int)

    def test_solve_matches_object_solve(self):
        """Cache solve must match the object-based RustArbSolver.solve()."""
        cache = rs_mobius.RustPoolCache()
        solver = rs_mobius.RustArbSolver()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        cache_result = cache.solve([0, 1])

        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        obj_result = solver.solve(hops_int)

        assert int(cache_result.optimal_input_int) == int(obj_result.optimal_input_int)
        assert int(cache_result.profit_int) == int(obj_result.profit_int)

    def test_full_scale_reserves(self):
        """Full uint256-scale reserves (USDC 6-dec, WETH 18-dec)."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        r0_a = 100_000_000 * 10**USDC_DECIMALS
        r1_a = 60_000 * 10**WETH_DECIMALS
        r1_b = 40_000 * 10**WETH_DECIMALS
        r0_b = 80_000_000 * 10**USDC_DECIMALS

        cache.insert(0, r0_a, r1_a, gamma_numer, fee_denom)
        cache.insert(1, r1_b, r0_b, gamma_numer, fee_denom)

        result = cache.solve([0, 1])
        assert int(result.profit_int) > 0

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops_int))
        assert evm_output - int(result.optimal_input_int) == int(result.profit_int)

    def test_max_input_constraint(self):
        """max_input constraint should be respected."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        result = cache.solve([0, 1], max_input=1000.0)
        assert int(result.optimal_input_int) <= 1000

    def test_not_profitable(self):
        """Same-product pools should return unprofitable."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 100_000, 50, gamma_numer, fee_denom)
        cache.insert(1, 50, 100_000, gamma_numer, fee_denom)

        result = cache.solve([0, 1])
        assert not result.success
        assert result.profit_int == 0

    def test_3hop_path(self):
        """3-hop path should work with wider search radius."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 2_000_000, 2_100_000, gamma_numer, fee_denom)
        cache.insert(1, 2_000_000, 2_050_000, gamma_numer, fee_denom)
        cache.insert(2, 2_050_000, 2_000_000, gamma_numer, fee_denom)

        result = cache.solve([0, 1, 2])
        assert int(result.profit_int) > 0

    def test_best_in_neighborhood(self):
        """No nearby integer input should give better profit."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        result = cache.solve([0, 1])

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

    def test_mixed_fee_tiers(self):
        """Mixed fee tiers (0.05% + 0.3%)."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer_a, fee_denom_a = fee_to_gamma(FEE_0_05_PCT)
        gamma_numer_b, fee_denom_b = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer_a, fee_denom_a)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer_b, fee_denom_b)

        result = cache.solve([0, 1])

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 9995, 10000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops_int))
        assert evm_output - int(result.optimal_input_int) == int(result.profit_int)

    def test_update_overwrites(self):
        """Updating a pool ID should overwrite the previous state."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        # Insert with small reserves (not profitable)
        cache.insert(0, 100_000, 50, gamma_numer, fee_denom)
        cache.insert(1, 50, 100_000, gamma_numer, fee_denom)

        result_before = cache.solve([0, 1])
        assert not result_before.success

        # Update pool 0 with profitable reserves
        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        result_after = cache.solve([0, 1])

        assert result_before != result_after

    def test_remove_pool(self):
        """Removing a pool should make it unavailable for solve."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)

        cache.remove(0)

        with pytest.raises(ValueError, match="not found"):
            cache.solve([0, 1])

    def test_solve_missing_pool_id(self):
        """Solving with a non-existent pool ID should raise ValueError."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        # Pool 1 not inserted

        with pytest.raises(ValueError, match="not found"):
            cache.solve([0, 1])

    def test_solve_too_few_pools(self):
        """Solving with <2 pool IDs should raise ValueError."""
        cache = rs_mobius.RustPoolCache()
        with pytest.raises(ValueError, match="at least 2"):
            cache.solve([0])

    def test_reuse_pool_across_paths(self):
        """Same pool ID used in different paths should give correct results."""
        cache = rs_mobius.RustPoolCache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, 1_000_000, 5_000_000, gamma_numer, fee_denom)
        cache.insert(1, 1_500_000, 3_000_000, gamma_numer, fee_denom)
        cache.insert(2, 2_000_000, 4_000_000, gamma_numer, fee_denom)

        # Path A: pools 0-1
        result_a = cache.solve([0, 1])

        # Path B: pools 0-2 (same pool 0, different pool 2)
        result_b = cache.solve([0, 2])

        # Should produce different results (different reserves in pool 1 vs 2)
        # but both valid
        hops_a = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        hops_b = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(2_000_000, 4_000_000, 997, 1000),
        ]

        evm_a = int(rs_mobius.py_int_simulate_path(int(result_a.optimal_input_int), hops_a))
        assert evm_a - int(result_a.optimal_input_int) == int(result_a.profit_int)

        evm_b = int(rs_mobius.py_int_simulate_path(int(result_b.optimal_input_int), hops_b))
        assert evm_b - int(result_b.optimal_input_int) == int(result_b.profit_int)


# ---------------------------------------------------------------------------
# Test: ArbSolver integration with pool cache
# ---------------------------------------------------------------------------


class TestArbSolverPoolCache:
    """Tests that ArbSolver can use the pool cache for direct solves."""

    def test_arb_solver_with_cache(self):
        """ArbSolver should be able to use a RustPoolCache for direct solves."""
        solver = ArbSolver()
        cache = solver.get_pool_cache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, USDC_1_5M, WETH_800, gamma_numer, fee_denom)
        cache.insert(1, WETH_1000, USDC_2M, gamma_numer, fee_denom)

        result = solver.solve_cached([0, 1])
        assert result.method == SolverMethod.MOBIUS

        # EVM-exact verification
        hops_int = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, 997, 1000),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, 997, 1000),
        ]
        evm_output = int(rs_mobius.py_int_simulate_path(result.optimal_input, hops_int))
        assert evm_output - result.optimal_input == result.profit

    def test_arb_solver_cache_matches_standard_solve(self):
        """Cache-based solve should produce the same result as standard ArbSolver.solve()."""
        solver = ArbSolver()
        cache = solver.get_pool_cache()
        gamma_numer, fee_denom = fee_to_gamma(FEE_0_3_PCT)

        cache.insert(0, USDC_1_5M, WETH_800, gamma_numer, fee_denom)
        cache.insert(1, WETH_1000, USDC_2M, gamma_numer, fee_denom)

        cached_result = solver.solve_cached([0, 1])

        standard_result = solver.solve(
            SolveInput(
                hops=(
                    ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
                    ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
                )
            )
        )

        assert cached_result.optimal_input == standard_result.optimal_input
        assert cached_result.profit == standard_result.profit
