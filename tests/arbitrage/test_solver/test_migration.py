"""
Tests for the generalized solver migration feature flag.

Validates that ArbSolver.solve() produces identical results when
USE_GENERALIZED_SOLVER is enabled vs disabled.
"""

from contextlib import contextmanager
from fractions import Fraction

import pytest

import degenbot.arbitrage.optimizers.solver
from degenbot.arbitrage.optimizers import ArbSolver, ConstantProductHop, SolveInput
from degenbot.exceptions import OptimizationError

FEE_03 = Fraction(3, 1000)
FEE_05 = Fraction(5, 1000)


@contextmanager
def use_generalized_solver(*, enabled: bool):
    module = degenbot.arbitrage.optimizers.solver
    old_flag = module.USE_GENERALIZED_SOLVER
    try:
        module.USE_GENERALIZED_SOLVER = enabled
        yield ArbSolver()
    finally:
        module.USE_GENERALIZED_SOLVER = old_flag


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
        with use_generalized_solver(enabled=False) as legacy_solver:
            legacy = legacy_solver.solve(SolveInput(hops=hops))
        with use_generalized_solver(enabled=True) as generalized_solver:
            gen = generalized_solver.solve(SolveInput(hops=hops))

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
        with pytest.raises(OptimizationError), use_generalized_solver(enabled=False) as solver:
            solver.solve(SolveInput(hops=hops))
        with pytest.raises(OptimizationError), use_generalized_solver(enabled=True) as solver:
            solver.solve(SolveInput(hops=hops))

    def test_three_hop_matches(self):
        hops = (
            ConstantProductHop(10**18, 2 * 10**18, FEE_03),
            ConstantProductHop(3 * 10**18, 10**18, FEE_03),
            ConstantProductHop(2 * 10**18, 4 * 10**18, FEE_03),
        )
        with use_generalized_solver(enabled=False) as solver:
            legacy = solver.solve(SolveInput(hops=hops))
        with use_generalized_solver(enabled=True) as solver:
            gen = solver.solve(SolveInput(hops=hops))

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_large_reserves_matches(self):
        hops = (
            ConstantProductHop(10**27, 10**27, FEE_03),
            ConstantProductHop(10**24, 2 * 10**27, FEE_03),
        )
        with use_generalized_solver(enabled=False) as legacy_solver:
            legacy = legacy_solver.solve(SolveInput(hops=hops))
        with use_generalized_solver(enabled=True) as generalized_solver:
            gen = generalized_solver.solve(SolveInput(hops=hops))

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
        with use_generalized_solver(enabled=False) as legacy_solver:
            legacy = legacy_solver.solve(SolveInput(hops=hops))
        with use_generalized_solver(enabled=True) as generalized_solver:
            gen = generalized_solver.solve(SolveInput(hops=hops))

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
        with use_generalized_solver(enabled=False) as legacy_solver:
            legacy = legacy_solver.solve(SolveInput(hops=hops, max_input=max_input))
        with use_generalized_solver(enabled=True) as generalized_solver:
            gen = generalized_solver.solve(SolveInput(hops=hops, max_input=max_input))

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit

    def test_feature_flag_default_off(self):
        assert not degenbot.arbitrage.optimizers.solver.USE_GENERALIZED_SOLVER

    def test_five_hop_matches(self):
        hops = tuple(
            ConstantProductHop(
                reserve_in=10**18 + i * 10**15,
                reserve_out=2 * 10**18 - i * 10**15,
                fee=FEE_03,
            )
            for i in range(5)
        )

        with use_generalized_solver(enabled=False) as legacy_solver:
            legacy = legacy_solver.solve(SolveInput(hops=hops))
        with use_generalized_solver(enabled=True) as generalized_solver:
            gen = generalized_solver.solve(SolveInput(hops=hops))

        assert gen.optimal_input == legacy.optimal_input
        assert gen.profit == legacy.profit
