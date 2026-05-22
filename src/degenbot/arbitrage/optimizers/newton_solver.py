"""Newton's-method solver for 2-hop constant-product paths."""

import time
from typing import override

from degenbot.arbitrage.optimizers._solver_utils import (
    _compute_mobius_coefficients,
    _hop_to_float_state,
    _simulate_path,
)
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.exceptions import OptimizationError


class NewtonSolver(Solver):
    """
    Newton's method solver for 2-hop V2-V2 arbitrage.

    Converges in 3-4 iterations. Useful as a fallback when Möbius
    is not available or for validation.

    Performance: ~7.5μs
    """

    MAX_ITERATIONS = 10
    TOLERANCE = 1e-9

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops == self.MIN_HOPS and solve_input.all_constant_product

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if not self.supports(solve_input):
            raise OptimizationError(
                message="Newton solver requires exactly 2 V2 hops",
                iterations=0,
                method=SolverMethod.NEWTON.name,
            )

        r0_buy, s0_buy, g0_buy = _hop_to_float_state(solve_input.hops[0])
        r0_sell, s0_sell, g0_sell = _hop_to_float_state(solve_input.hops[1])

        # Initial guess from Möbius closed-form (best available estimate)
        # This ensures Newton starts near the true optimum and converges
        # in 1-2 refinement iterations.
        mobius_coeffs = _compute_mobius_coefficients(solve_input.hops)
        if mobius_coeffs.is_profitable:
            x = mobius_coeffs.optimal_input()
            if x <= 0:
                raise OptimizationError(
                    message="Möbius optimal <= 0",
                    iterations=0,
                    method=SolverMethod.NEWTON.name,
                )
        else:
            raise OptimizationError(
                message="Not profitable (Möbius check failed)",
                iterations=0,
                method=SolverMethod.NEWTON.name,
            )

        iterations = 0
        for i in range(self.MAX_ITERATIONS):
            # Hop 0: input x → forward amount y
            # y = x * gamma * reserve_out / (reserve_in + x * gamma) # noqa: ERA001
            denom_buy = r0_buy + x * g0_buy
            if denom_buy <= 0:
                break
            y = x * g0_buy * s0_buy / denom_buy

            # Hop 1: forward y → output z
            # z = y * gamma * reserve_out / (reserve_in + y * gamma) # noqa: ERA001
            denom_sell = r0_sell + y * g0_sell
            if denom_sell <= 0:
                break
            _output = y * g0_sell * s0_sell / denom_sell

            dy_dx = g0_buy * s0_buy * r0_buy / (denom_buy**2)
            dz_dy = g0_sell * s0_sell * r0_sell / (denom_sell**2)
            dprofit_dx = dz_dy * dy_dx - 1.0

            if abs(dprofit_dx) < self.TOLERANCE:
                iterations = i + 1
                break

            # Second derivative: d²P/dx² = d²z/dy² * (dy/dx)² + dz/dy * d²y/dx²
            d2y_dx2 = -2.0 * g0_buy**2 * s0_buy * r0_buy / (denom_buy**3)
            d2z_dy2 = -2.0 * g0_sell**2 * s0_sell * r0_sell / (denom_sell**3)
            d2profit_dx2 = d2z_dy2 * dy_dx**2 + dz_dy * d2y_dx2

            if abs(d2profit_dx2) < 1e-30:
                iterations = i + 1
                break

            # Newton step
            x_new = x - dprofit_dx / d2profit_dx2
            if x_new <= 0:
                x_new = x / 2.0
            x = x_new
            iterations = i + 1

        if x <= 0:
            raise OptimizationError(
                message="Newton did not converge to positive input",
                iterations=iterations,
                method=SolverMethod.NEWTON.name,
            )

        # Apply max_input constraint
        if solve_input.max_input is not None and x > float(solve_input.max_input):
            x = float(solve_input.max_input)

        # Integer verification
        optimal_input = int(x)
        output = _simulate_path(float(optimal_input), solve_input.hops)
        actual_profit = int(output) - optimal_input

        elapsed_ns = time.perf_counter_ns() - start_ns

        if actual_profit <= 0:
            raise OptimizationError(
                message="Not profitable (integer verification failed)",
                iterations=iterations,
                method=SolverMethod.NEWTON.name,
            )

        return SolveResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            iterations=iterations,
            method=SolverMethod.NEWTON,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# Brent Solver (fallback)
# ---------------------------------------------------------------------------
