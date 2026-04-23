"""
Tests for the generalized solver migration feature flag.

Validates that ArbSolver.solve() produces identical results when
USE_GENERALIZED_SOLVER is enabled vs disabled.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import ArbSolver, ConstantProductHop, SolveInput
from degenbot.exceptions import OptimizationError

FEE_03 = Fraction(3, 1000)
FEE_05 = Fraction(5, 1000)


def _solve_with_generalized(hops, max_input=None):
    import degenbot.arbitrage.optimizers.solver as solver_mod

    old_flag = solver_mod.USE_GENERALIZED_SOLVER
    try:
        solver_mod.USE_GENERALIZED_SOLVER = True
        solver = ArbSolver()
        return solver.solve(SolveInput(hops=hops, max_input=max_input))
    finally:
        solver_mod.USE_GENERALIZED_SOLVER = old_flag


def _solve_with_legacy(hops, max_input=None):
    import degenbot.arbitrage.optimizers.solver as solver_mod

    old_flag = solver_mod.USE_GENERALIZED_SOLVER
    try:
        solver_mod.USE_GENERALIZED_SOLVER = False
        solver = ArbSolver()
        return solver.solve(SolveInput(hops=hops, max_input=max_input))
    finally:
        solver_mod.USE_GENERALIZED_SOLVER = old_flag


class TestGeneralizedSolverMigration:
    def test_v2_v2_profitable_matches(self):
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            ConstantProductHop(
                reserve_in=1_500_000_000_000,
                reserve_out=800_000_000_000_000_000,
                fee=FEE_03,
            ),
        )
        legacy = _solve_with_legacy(hops)
        gen = _solve_with_generalized(hops)

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_v2_v2_unprofitable_matches(self):
        hops = (
            ConstantProductHop(
                reserve_in=10**18,
                reserve_out=10**18,
                fee=FEE_03,
            ),
            ConstantProductHop(
                reserve_in=10**18,
                reserve_out=10**18,
                fee=FEE_03,
            ),
        )
        with pytest.raises(OptimizationError):
            _solve_with_legacy(hops)
        with pytest.raises(OptimizationError):
            _solve_with_generalized(hops)

    def test_three_hop_matches(self):
        hops = (
            ConstantProductHop(10**18, 2 * 10**18, FEE_03),
            ConstantProductHop(3 * 10**18, 10**18, FEE_03),
            ConstantProductHop(2 * 10**18, 4 * 10**18, FEE_03),
        )
        legacy = _solve_with_legacy(hops)
        gen = _solve_with_generalized(hops)

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_large_reserves_matches(self):
        hops = (
            ConstantProductHop(10**27, 10**27, FEE_03),
            ConstantProductHop(10**24, 2 * 10**27, FEE_03),
        )
        legacy = _solve_with_legacy(hops)
        gen = _solve_with_generalized(hops)

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_mixed_fees_matches(self):
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            ConstantProductHop(
                reserve_in=1_500_000_000_000,
                reserve_out=800_000_000_000_000_000,
                fee=FEE_05,
            ),
        )
        legacy = _solve_with_legacy(hops)
        gen = _solve_with_generalized(hops)

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_max_input_constraint_matches(self):
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            ConstantProductHop(
                reserve_in=1_500_000_000_000,
                reserve_out=800_000_000_000_000_000,
                fee=FEE_03,
            ),
        )
        max_input = 10**10
        legacy = _solve_with_legacy(hops, max_input=max_input)
        gen = _solve_with_generalized(hops, max_input=max_input)

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_feature_flag_default_off(self):
        import degenbot.arbitrage.optimizers.solver as solver_mod

        assert not solver_mod.USE_GENERALIZED_SOLVER

    def test_five_hop_matches(self):
        hops = tuple(
            ConstantProductHop(
                reserve_in=10**18 + i * 10**15,
                reserve_out=2 * 10**18 - i * 10**15,
                fee=FEE_03,
            )
            for i in range(5)
        )
        legacy = _solve_with_legacy(hops)
        gen = _solve_with_generalized(hops)

        try:
            pass
        except OptimizationError:
            pytest.skip("Five-hop path not profitable")
        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit
