"""Brent's-method solver via scipy — handles all pool types."""

import time
from typing import override

from scipy.optimize import minimize_scalar

from degenbot.arbitrage.optimizers._solver_utils import _simulate_path
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.exceptions import OptimizationError


class BrentSolver(Solver):
    """
    Brent's method solver via scipy. Handles all pool types including
    V3-V3 with tick crossings.

    Performance: ~194μs (V2-V2), ~390μs (V3-V3)
    """

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops >= self.MIN_HOPS

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if solve_input.num_hops < self.MIN_HOPS:
            raise OptimizationError(
                message="Brent solver requires 2+ hops",
                iterations=0,
                method=SolverMethod.BRENT.name,
            )

        def neg_profit(x: float) -> float:
            """Negative profit for minimization."""
            if x <= 0:
                return 0.0
            output = _simulate_path(x, solve_input.hops)
            return -(output - x)

        # Estimate upper bound from largest input reserve
        max_reserve = max(h.reserve_in for h in solve_input.hops)
        upper = float(max_reserve)

        if solve_input.max_input is not None:
            upper = min(upper, float(solve_input.max_input))

        if upper <= 0:
            raise OptimizationError(
                message="Upper bound is zero or negative",
                iterations=0,
                method=SolverMethod.BRENT.name,
            )

        result = minimize_scalar(
            neg_profit,
            method="bounded",
            bounds=(0, upper),
            options={"xatol": 1.0},
        )

        elapsed_ns = time.perf_counter_ns() - start_ns

        if not result.success and result.fun >= 0:
            raise OptimizationError(
                message="No profitable solution found",
                iterations=result.nfev if hasattr(result, "nfev") else 0,
                method=SolverMethod.BRENT.name,
            )

        x_opt = result.x
        optimal_input = int(x_opt)
        output = _simulate_path(float(optimal_input), solve_input.hops)
        actual_profit = int(output) - optimal_input

        if actual_profit <= 0:
            raise OptimizationError(
                message="Not profitable (integer verification failed)",
                iterations=result.nfev if hasattr(result, "nfev") else 0,
                method=SolverMethod.BRENT.name,
            )

        return SolveResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            iterations=result.nfev if hasattr(result, "nfev") else 0,
            method=SolverMethod.BRENT,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# PiecewiseMobiusSolver — V3 multi-range with tick crossing
# ---------------------------------------------------------------------------
