"""
Cross-validation tests for MobiusSolver against the existing ArbSolver.

Each test constructs identical paths using both the new MobiusHopState types
and the old ConstantProductHop/BoundedProductHop types, solves with both
solvers, and asserts the results match.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    ConstantProductHop,
    SolveInput,
)
from degenbot.arbitrage.solver import MobiusSolver
from degenbot.arbitrage.solver.types import (
    MobiusHopState,
    SolverMethod,
)

FEE_03 = Fraction(3, 1000)
FEE_05 = Fraction(5, 1000)
FEE_01 = Fraction(1, 1000)


def _solve_old(hops: tuple, max_input: int | None = None) -> tuple[int, int, bool]:
    solver = ArbSolver()
    result = solver.solve(SolveInput(hops=hops, max_input=max_input))
    return result.optimal_input, result.profit, result.success


def _solve_new(hops, max_input: int | None = None) -> tuple[int, int, bool, SolverMethod]:
    solver = MobiusSolver()
    result = solver.solve(hops, max_input=max_input)
    return result.optimal_input, result.profit, result.is_profitable, result.method


class TestMobiusSolverSupports:
    def test_rejects_single_hop(self):
        solver = MobiusSolver()
        assert not solver.supports([MobiusHopState(1000, 2000, FEE_03)])

    def test_rejects_empty(self):
        solver = MobiusSolver()
        assert not solver.supports([])

    def test_accepts_v2_v2(self):
        solver = MobiusSolver()
        assert solver.supports([
            MobiusHopState(1000, 2000, FEE_03),
            MobiusHopState(2000, 1000, FEE_03),
        ])


class TestV2V2:
    def test_basic_profitable(self):
        new_hops = (
            MobiusHopState(
                reserve_in=2_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            MobiusHopState(
                reserve_in=1_500_000_000_000,
                reserve_out=800_000_000_000_000_000,
                fee=FEE_03,
            ),
        )
        old_hops = tuple(
            ConstantProductHop(
                reserve_in=h.reserve_in,
                reserve_out=h.reserve_out,
                fee=h.fee,
            )
            for h in new_hops
        )

        old_input, old_profit, old_ok = _solve_old(old_hops)
        new_input, new_profit, new_ok, method = _solve_new(new_hops)

        assert old_ok
        assert new_ok
        assert new_input == old_input
        assert new_profit == old_profit
        assert method == SolverMethod.MOBIUS

    def test_symmetric_unprofitable(self):
        new_hops = (
            MobiusHopState(
                reserve_in=1_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            MobiusHopState(
                reserve_in=1_000_000_000_000_000_000,
                reserve_out=1_000_000_000_000,
                fee=FEE_03,
            ),
        )
        old_hops = tuple(
            ConstantProductHop(
                reserve_in=h.reserve_in,
                reserve_out=h.reserve_out,
                fee=h.fee,
            )
            for h in new_hops
        )

        _, _, old_ok = _solve_old(old_hops)
        _, _, new_ok, _ = _solve_new(new_hops)

        assert not old_ok
        assert not new_ok

    def test_large_reserves(self):
        new_hops = (
            MobiusHopState(
                reserve_in=10**27,
                reserve_out=10**27,
                fee=FEE_03,
            ),
            MobiusHopState(
                reserve_in=10**24,
                reserve_out=2 * 10**27,
                fee=FEE_03,
            ),
        )
        old_hops = tuple(
            ConstantProductHop(
                reserve_in=h.reserve_in,
                reserve_out=h.reserve_out,
                fee=h.fee,
            )
            for h in new_hops
        )

        old_input, old_profit, old_ok = _solve_old(old_hops)
        new_input, new_profit, new_ok, _ = _solve_new(new_hops)

        assert old_ok
        assert new_ok
        assert new_input == old_input
        assert new_profit == old_profit

    def test_mixed_fees(self):
        new_hops = (
            MobiusHopState(
                reserve_in=2_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            MobiusHopState(
                reserve_in=1_500_000_000_000,
                reserve_out=800_000_000_000_000_000,
                fee=FEE_05,
            ),
        )
        old_hops = tuple(
            ConstantProductHop(
                reserve_in=h.reserve_in,
                reserve_out=h.reserve_out,
                fee=h.fee,
            )
            for h in new_hops
        )

        old_input, old_profit, old_ok = _solve_old(old_hops)
        new_input, new_profit, new_ok, _ = _solve_new(new_hops)

        assert old_ok
        assert new_ok
        assert new_input == old_input
        assert new_profit == old_profit


class TestMultiHop:
    def test_three_hop(self):
        new_hops = (
            MobiusHopState(10**18, 2 * 10**18, FEE_03),
            MobiusHopState(3 * 10**18, 10**18, FEE_03),
            MobiusHopState(2 * 10**18, 4 * 10**18, FEE_03),
        )
        old_hops = tuple(ConstantProductHop(h.reserve_in, h.reserve_out, h.fee) for h in new_hops)

        old_input, old_profit, old_ok = _solve_old(old_hops)
        new_input, new_profit, new_ok, _ = _solve_new(new_hops)

        assert old_ok
        assert new_ok
        assert new_input == old_input
        assert new_profit == old_profit

    def test_five_hop(self):
        new_hops = tuple(
            MobiusHopState(
                reserve_in=10**18 + i * 10**15,
                reserve_out=2 * 10**18 - i * 10**15,
                fee=FEE_03,
            )
            for i in range(5)
        )
        old_hops = tuple(ConstantProductHop(h.reserve_in, h.reserve_out, h.fee) for h in new_hops)

        old_input, old_profit, old_ok = _solve_old(old_hops)
        new_input, new_profit, new_ok, _ = _solve_new(new_hops)

        assert old_ok
        assert new_ok
        assert new_input == old_input
        assert new_profit == old_profit


class TestMaxInput:
    def test_max_input_constrains(self):
        new_hops = (
            MobiusHopState(
                reserve_in=2_000_000_000_000,
                reserve_out=1_000_000_000_000_000_000,
                fee=FEE_03,
            ),
            MobiusHopState(
                reserve_in=1_500_000_000_000,
                reserve_out=800_000_000_000_000_000,
                fee=FEE_03,
            ),
        )
        old_hops = tuple(ConstantProductHop(h.reserve_in, h.reserve_out, h.fee) for h in new_hops)

        max_input = 10**10

        old_input, old_profit, old_ok = _solve_old(old_hops, max_input=max_input)
        new_input, new_profit, new_ok, _ = _solve_new(new_hops, max_input=max_input)

        assert old_ok
        assert new_ok
        assert new_input <= max_input
        assert new_input == old_input
        assert new_profit == old_profit


class TestMobiusCoefficients:
    def test_profitability_check(self):
        from degenbot.arbitrage.solver.mobius_solver import _compute_mobius_coefficients

        profitable_hops = (
            MobiusHopState(2_000_000, 1_000_000_000, FEE_03),
            MobiusHopState(1_500_000, 800_000_000, FEE_03),
        )
        coeffs = _compute_mobius_coefficients(profitable_hops)
        assert coeffs.is_profitable
        assert coeffs.K > coeffs.M

    def test_unprofitable_check(self):
        from degenbot.arbitrage.solver.mobius_solver import _compute_mobius_coefficients

        unprofitable_hops = (
            MobiusHopState(10**18, 10**18, FEE_03),
            MobiusHopState(10**18, 10**18, FEE_03),
        )
        coeffs = _compute_mobius_coefficients(unprofitable_hops)
        assert not coeffs.is_profitable

    def test_optimal_input_positive(self):
        from degenbot.arbitrage.solver.mobius_solver import _compute_mobius_coefficients

        hops = (
            MobiusHopState(2_000_000, 1_000_000_000, FEE_03),
            MobiusHopState(1_500_000, 800_000_000, FEE_03),
        )
        coeffs = _compute_mobius_coefficients(hops)
        assert coeffs.optimal_input() > 0

    def test_path_output_matches_simulation(self):
        from degenbot.arbitrage.solver.mobius_solver import (
            _compute_mobius_coefficients,
            _simulate_path,
        )

        hops = (
            MobiusHopState(2_000_000, 1_000_000_000, FEE_03),
            MobiusHopState(1_500_000, 800_000_000, FEE_03),
        )
        coeffs = _compute_mobius_coefficients(hops)
        x = coeffs.optimal_input()

        output_coeffs = coeffs.path_output(x)
        output_sim = _simulate_path(x, hops)

        assert output_coeffs == pytest.approx(output_sim, rel=1e-10)


class TestPiecewiseSolverCaching:
    def test_piecewise_solver_cached_on_instance(self):
        solver = MobiusSolver()
        from degenbot.arbitrage.solver.mobius_solver import _SENTINEL

        assert solver._piecewise_solver is _SENTINEL
        ps = solver._get_piecewise_solver()
        assert ps is not _SENTINEL
        assert solver._get_piecewise_solver() is ps
