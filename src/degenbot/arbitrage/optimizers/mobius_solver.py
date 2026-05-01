"""Möbius closed-form solver for constant-product AMM paths."""

# Feature flags (kept local to avoid circular imports)
import os
import time
from typing import Any, ClassVar, override

from degenbot.arbitrage.optimizers._solver_utils import (
    _compute_mobius_coefficients,
    _rust_integer_refinement,
    _simulate_path,
)
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.degenbot_rs import mobius as _rs_mobius
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import BoundedProductHop, ConstantProductHop, PoolInvariant

USE_MERGED_INT_REFINEMENT = bool(os.environ.get("DEGENBOT_MERGED_INT_REFINEMENT", "1"))
USE_RAW_ARRAY_MARSHALLING = bool(os.environ.get("DEGENBOT_RAW_ARRAY_MARSHALLING", "1"))


class MobiusSolver(Solver):
    """
    Möbius transformation solver for constant product AMM paths.

    Zero-iteration closed-form solution. Works for V2 paths and V3
    single-range paths (where the swap stays within one tick range).

    Tries Rust acceleration first, falls back to pure Python.

    Performance: ~0.86μs (Python), ~0.19μs (Rust)
    """

    MIN_HOPS = 2

    _RUST_METHOD_MAP: ClassVar[dict[int, SolverMethod]] = {
        0: SolverMethod.MOBIUS,
        1: SolverMethod.PIECEWISE_MOBIUS,
        2: SolverMethod.PIECEWISE_MOBIUS,
    }

    def __init__(self) -> None:
        self._rust_solver: Any = None
        if _rs_mobius is not None:
            self._rust_solver = _rs_mobius.RustArbSolver()

    def __getstate__(self) -> dict[str, Any]:
        """Omit the non-pickleable Rust solver; it will be recreated on unpickle."""
        state = self.__dict__.copy()
        state["_rust_solver"] = None
        return state

    def __setstate__(self, state: dict[str, Any]) -> None:
        """Recreate the Rust solver after unpickling if Rust is available."""
        self.__dict__.update(state)
        if _rs_mobius is not None:
            self._rust_solver = _rs_mobius.RustArbSolver()

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

        if self._rust_solver is not None:
            try:
                return self._try_rust_solve(solve_input, start_ns)
            except OptimizationError:
                pass

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

    def _try_rust_solve(self, solve_input: SolveInput, start_ns: int) -> SolveResult:
        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        if USE_RAW_ARRAY_MARSHALLING:
            return self._try_rust_solve_raw(solve_input, start_ns, max_input_float)

        rust_hops: list[Any] = []
        for hop in solve_input.hops:
            if isinstance(hop, ConstantProductHop | BoundedProductHop):
                if USE_MERGED_INT_REFINEMENT:
                    fee_numer = hop.fee.numerator
                    fee_denom = hop.fee.denominator
                    gamma_numer = fee_denom - fee_numer
                    rust_hops.append(
                        _rs_mobius.RustIntHopState(
                            hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom
                        )
                    )
                else:
                    rust_hops.append((
                        float(hop.reserve_in),
                        float(hop.reserve_out),
                        float(hop.fee),
                    ))
            else:
                raise OptimizationError(
                    message=f"Unsupported hop type for Rust: {type(hop).__name__}",
                    iterations=0,
                    method=SolverMethod.MOBIUS.name,
                )

        result = self._rust_solver.solve(rust_hops, None, max_input_float, 10)

        return self._process_rust_result(result, start_ns, solve_input)

    def _try_rust_solve_raw(
        self, solve_input: SolveInput, start_ns: int, max_input_float: float | None
    ) -> SolveResult:
        int_hops_flat: list[int] = []
        for hop in solve_input.hops:
            fee_denom = hop.fee.denominator
            gamma_numer = fee_denom - hop.fee.numerator
            int_hops_flat.extend([hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom])

        try:
            result = self._rust_solver.solve_raw(int_hops_flat, max_input_float)
        except (ValueError, TypeError) as e:
            raise OptimizationError(
                message=f"Rust solve_raw failed: {e}",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            ) from e

        return self._process_rust_result(result, start_ns, solve_input)

    def _process_rust_result(
        self, result: Any, start_ns: int, solve_input: SolveInput
    ) -> SolveResult:
        if not result.supported:
            raise OptimizationError(
                message="Rust solver does not support this path",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            )

        elapsed_ns = time.perf_counter_ns() - start_ns
        method = self._RUST_METHOD_MAP.get(result.method, SolverMethod.MOBIUS)

        if not result.success:
            raise OptimizationError(
                message="Not profitable",
                iterations=result.iterations,
                method=method.name,
            )

        if result.optimal_input_int is not None and result.profit_int is not None:
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return SolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )
            raise OptimizationError(
                message="Not profitable (integer verification failed)",
                iterations=result.iterations,
                method=method.name,
            )

        x_opt = result.optimal_input
        optimal_input, profit = _rust_integer_refinement(
            x_opt, solve_input.hops, solve_input.max_input
        )
        if profit > 0:
            return SolveResult(
                optimal_input=optimal_input,
                profit=profit,
                iterations=result.iterations,
                method=method,
                solve_time_ns=elapsed_ns,
            )
        raise OptimizationError(
            message="Not profitable (integer verification failed)",
            iterations=result.iterations,
            method=method.name,
        )


# ---------------------------------------------------------------------------
# Newton Solver (2-hop V2 fallback)
# ---------------------------------------------------------------------------
