"""Möbius closed-form solver for constant-product AMM paths."""

import time
from typing import ClassVar, override

from degenbot.arbitrage.solvers._solver_utils import (
    _compute_mobius_coefficients,
    _simulate_path,
)
from degenbot.arbitrage.solvers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import (
    BoundedProductHop,
    PoolInvariant,
)


class MobiusSolver(Solver):
    """Möbius transformation solver for constant product AMM paths.

    Zero-iteration closed-form solution. Works for V2 paths and V3
    single-range paths (where the swap stays within one tick range).

    The f64 Möbius recurrence lives in pure Python
    (:mod:`degenbot.arbitrage.solvers._solver_utils`); the former
    Rust f64 fast path has been removed (see ``rust/CONTEXT.md`` ruling
    "f64 vs U512 Möbius solver stack"). EVM-exact integer arbitrage now
    flows exclusively through the Rust :class:`~degenbot.degenbot_rs.UniswapArbEngine`
    U512 solver; this Python class remains a thin orchestrator-era f64
    solver for the not-yet-ported paths.
    """

    MIN_HOPS = 2

    _RUST_METHOD_MAP: ClassVar[dict[int, SolverMethod]] = {
        0: SolverMethod.MOBIUS,
        1: SolverMethod.PIECEWISE_MOBIUS,
        2: SolverMethod.PIECEWISE_MOBIUS,
    }

    def __init__(self) -> None:
        """Initialize the instance."""

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        if solve_input.num_hops < self.MIN_HOPS:
            return False
        for hop in solve_input.hops:
            if hop.invariant not in {
                PoolInvariant.CONSTANT_PRODUCT,
                PoolInvariant.BOUNDED_PRODUCT,
            }:
                return False
            if (
                hop.invariant == PoolInvariant.BOUNDED_PRODUCT
                and isinstance(hop, BoundedProductHop)
                and hop.has_multi_range
            ):
                return False
        return True

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if solve_input.num_hops < self.MIN_HOPS:
            raise OptimizationError(
                message="Möbius solver requires 2+ hops",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            )

        return self._solve_python(solve_input, start_ns)

    def _solve_python(self, solve_input: SolveInput, start_ns: int) -> SolveResult:
        coeffs = _compute_mobius_coefficients(solve_input.hops)

        if not coeffs.is_profitable:
            raise OptimizationError(
                message="Not profitable (K/M <= 1)",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            )

        x_opt = coeffs.optimal_input()

        if x_opt <= 0:
            raise OptimizationError(
                message="Optimal input <= 0",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            )

        if solve_input.max_input is not None and x_opt > float(solve_input.max_input):
            x_opt = float(solve_input.max_input)

        num_hops = solve_input.num_hops
        search_radius = 1 if num_hops <= self.MIN_HOPS else min(num_hops, 5)

        x_floor = int(x_opt)
        best_input = x_floor
        best_profit = -1

        for candidate in range(max(1, x_floor - search_radius), x_floor + search_radius + 2):
            if solve_input.max_input is not None and candidate > solve_input.max_input:
                continue
            output = _simulate_path(float(candidate), solve_input.hops)
            profit = int(output) - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        elapsed_ns = time.perf_counter_ns() - start_ns

        if best_profit <= 0:
            raise OptimizationError(
                message="Not profitable (integer verification failed)",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            )

        return SolveResult(
            optimal_input=best_input,
            profit=best_profit,
            iterations=0,
            method=SolverMethod.MOBIUS,
            solve_time_ns=elapsed_ns,
        )
