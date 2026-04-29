"""
Unified solver interface for arbitrage optimization.

All optimizers accept the same `SolveInput` (a sequence of `Hop` objects) and return the same
`SolveResult`. The `ArbSolver` dispatcher automatically selects the best method based on the hop
types.
"""

import importlib.util
import math
import os
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from fractions import Fraction
from typing import Any, ClassVar, override

import numpy as np
from scipy.optimize import minimize_scalar

from degenbot.aerodrome.functions import calc_exact_in_stable as _aerodrome_stable_calc
from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
)
from degenbot.arbitrage.optimizers.hop_types import (
    BalancerMultiTokenHop,
    BoundedProductHop,
    ConstantProductHop,
    HopType,
    PoolInvariant,
    SolidlyStableHop,
    SolverMethod,
    SolveInput,
    Solver,
    SolveResult,
    V3TickRangeInfo,
)
from degenbot.arbitrage.optimizers.mobius import (
    V3TickRangeHop,
    V3TickRangeSequence,
    compute_mobius_coefficients as _float_compute_mobius_coefficients,
    mobius_solve,
)
from degenbot.arbitrage.optimizers.mobius import simulate_path as _mobius_simulate_path
from degenbot.arbitrage.optimizers.v3_tick_predictor import estimate_price_impact
from degenbot.camelot.functions import get_y_camelot, k_camelot
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.degenbot_rs import mobius as _rs_mobius
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import OptimizationError
from degenbot.solidly.solidly_functions import general_calc_exact_in_stable
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_libraries.constants import Q96
from degenbot.uniswap.v3_libraries.tick_bitmap import gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK, get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

# Feature flag: when True, RustArbSolver.solve() receives RustIntHopState objects
# and does float solve + U256 integer refinement in a single Rust call.
# When False, falls back to the old two-step approach (float tuples then
# py_mobius_refine_int separately).
USE_MERGED_INT_REFINEMENT = bool(os.environ.get("DEGENBOT_MERGED_INT_REFINEMENT", "1"))

# Feature flag: when True, pass a flat int array to RustArbSolver.solve_raw()
# instead of creating RustIntHopState Python objects. This eliminates
# ~2.5μs of Python object construction overhead per solve call.
# When False, falls back to RustIntHopState objects.
USE_RAW_ARRAY_MARSHALLING = bool(os.environ.get("DEGENBOT_RAW_ARRAY_MARSHALLING", "1"))

_NUMPY_AVAILABLE = importlib.util.find_spec("numpy") is not None


# ---------------------------------------------------------------------------
# Möbius Solver
# ---------------------------------------------------------------------------


def _infer_zero_for_one(v3_hop: BoundedProductHop) -> bool:
    """Infer swap direction from BoundedProductHop data.

    Uses the stored zero_for_one if available. Otherwise computes it
    from the reserve ratio vs the expected ratio from L/sqrt_price.

    For properly-constructed reserves (from _v3_virtual_reserves):
    - zero_for_one=True: reserve_in/reserve_out = R0/R1 = 1/sqrt_p²
    - zero_for_one=False: reserve_in/reserve_out = R1/R0 = sqrt_p²

    The ratio comparison is scale-invariant (works regardless of Q96 scaling).
    """
    if v3_hop.zero_for_one is not None:
        return v3_hop.zero_for_one

    sqrt_p = float(v3_hop.sqrt_price) / Q96
    price = sqrt_p * sqrt_p
    reserve_ratio = float(v3_hop.reserve_in) / float(v3_hop.reserve_out)
    return abs(reserve_ratio - 1.0 / price) < abs(reserve_ratio - price)


@dataclass(frozen=True, slots=True)
class _MobiusCoefficients:
    """
    Internal Möbius coefficients l(x) = K*x / (M + N*x).

    Computed from Hop data via an O(n) recurrence.
    """

    K: float
    M: float
    N: float
    is_profitable: bool

    def optimal_input(self) -> float:
        if not self.is_profitable:
            return 0.0
        return (math.sqrt(self.K * self.M) - self.M) / self.N

    def path_output(self, x: float) -> float:
        denom = self.M + self.N * x
        if denom <= 0:
            return 0.0
        return self.K * x / denom

    def profit_at(self, x: float) -> float:
        return self.path_output(x) - x


def _hop_to_float_state(hop: HopType) -> tuple[float, float, float]:
    """Convert any Hop variant to (reserve_in, reserve_out, gamma) as floats."""
    return float(hop.reserve_in), float(hop.reserve_out), hop.gamma


def _compute_mobius_coefficients(hops: tuple[HopType, ...]) -> _MobiusCoefficients:
    """
    Compute Möbius transformation coefficients from hops.

    The recurrence:
        Initialize: K = gamma_1 * s_1, M = r_1, N = gamma_1
        Per hop i (i >= 2):
            K_new = K * gamma_i * s_i
            M_new = M * r_i
            N_new = N * r_i + K * gamma_i   (uses K before update)
    """
    if not hops:
        return _MobiusCoefficients(K=0.0, M=1.0, N=0.0, is_profitable=False)

    r0, s0, g0 = _hop_to_float_state(hops[0])
    K = g0 * s0
    M = r0
    N = g0

    for hop in hops[1:]:
        r_i, s_i, g_i = _hop_to_float_state(hop)
        old_K = K
        K = old_K * g_i * s_i
        M *= r_i
        N = N * r_i + old_K * g_i

    is_profitable = K > M
    return _MobiusCoefficients(K=K, M=M, N=N, is_profitable=is_profitable)


def _simulate_path(x: float, hops: tuple[HopType, ...]) -> float:
    """Simulate a swap through all hops for verification."""
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0
        r_i, s_i, g_i = _hop_to_float_state(hop)
        denom = r_i + amount * g_i
        if denom <= 0:
            return 0.0
        amount = amount * g_i * s_i / denom
    return amount


def _rust_integer_refinement(
    x_opt: float,
    hops: tuple[HopType, ...],
    max_input: int | None,
) -> tuple[int, int]:
    """Integer refinement in Rust using EVM-exact U256 arithmetic.

    Converts Python hops to RustIntHopState, calls py_mobius_refine_int
    to search around the float optimum with U256 simulation, and
    returns the best integer result.
    """
    if _rs_mobius is None:
        return 0, 0

    rust_int_hops: list[Any] = []
    for hop in hops:
        fee_numer = hop.fee.numerator
        fee_denom = hop.fee.denominator
        gamma_numer = fee_denom - fee_numer
        gamma_denom = fee_denom
        rust_int_hops.append(
            _rs_mobius.RustIntHopState(hop.reserve_in, hop.reserve_out, gamma_numer, gamma_denom)
        )

    max_input_float = float(max_input) if max_input is not None else None
    result = _rs_mobius.py_mobius_refine_int(x_opt, rust_int_hops, max_input_float)

    if result.success:
        return int(result.optimal_input), int(result.profit)
    return 0, 0


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


class PiecewiseMobiusSolver(Solver):
    """
    Piecewise-Möbius solver for V3 paths with tick crossings.

    For V3 swaps that cross tick boundaries, the swap function is
    piecewise-Möbius: fixed crossing output from crossed ranges plus
    variable Möbius output from the ending range. Uses golden section
    search on the bracketed profit function.

    Performance:
    - Python implementation: ~50μs (with all optimizations)
    - Rust implementation: ~9μs end-to-end (~1μs raw computation)
    """

    MIN_HOPS = 2
    GOLDEN_SECTION_ITERATIONS = 25
    PHI = (math.sqrt(5) - 1) / 2  # ~0.618

    def __init__(self) -> None:
        self._rust_optimizer: Any = None
        self._mobius_solver: MobiusSolver | None = None
        self._rust_hop_cache: dict[int, list] = {}
        self._rust_sequence_cache: dict[tuple[tuple[int, ...], int, bool], Any] = {}
        if _rs_mobius is not None:
            self._rust_optimizer = _rs_mobius.RustMobiusOptimizer()

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        if solve_input.num_hops < self.MIN_HOPS:
            return False
        # Supports paths with V3 bounded-product hops
        # (handles both single-range and multi-range V3)
        for hop in solve_input.hops:
            if hop.invariant not in {
                PoolInvariant.CONSTANT_PRODUCT,
                PoolInvariant.BOUNDED_PRODUCT,
            }:
                return False
        return solve_input.has_v3

    def _has_multi_range(self, solve_input: SolveInput) -> bool:
        """Check if any V3 hop has multi-range data for tick crossing."""
        for hop in solve_input.hops:
            if (
                hop.invariant == PoolInvariant.BOUNDED_PRODUCT
                and isinstance(hop, BoundedProductHop)
                and hop.has_multi_range
            ):
                return True
        return False

    def _find_v3_hop_index(self, solve_input: SolveInput) -> tuple[int, BoundedProductHop] | None:
        """Find the first V3 hop with multi-range data."""
        for i, hop in enumerate(solve_input.hops):
            if not isinstance(hop, BoundedProductHop):
                continue
            if hop.invariant == PoolInvariant.BOUNDED_PRODUCT and hop.has_multi_range:
                return i, hop
        return None

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        # Fast path: check we have 2+ hops with V3 data
        if solve_input.num_hops < self.MIN_HOPS:
            raise OptimizationError(
                message="Need 2+ hops",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # V3-V3 fast path: 2-hop path with both V3 multi-range
        if solve_input.num_hops == self.MIN_HOPS and self._rust_optimizer is not None:
            try:
                v3_v3_result = self._try_rust_v3_v3(solve_input, start_ns)
                if v3_v3_result is not None:
                    return v3_v3_result
            except OptimizationError:
                pass  # Fall through to next attempt

        # Find V3 hop (combines supports, has_multi_range, find_v3_hop_index)
        v3_result = self._find_v3_hop_index(solve_input)

        if v3_result is None:
            # No multi-range V3 found - try single-range fallback
            return self._try_single_range_fallback(solve_input, start_ns)

        v3_hop_index, v3_hop = v3_result

        # Multi-range V3: try Rust first with caching
        if self._rust_optimizer is not None:
            try:
                rust_result = self._try_rust_multi_range(
                    solve_input, v3_hop_index, v3_hop, start_ns
                )
                if rust_result is not None:
                    return rust_result
            except OptimizationError:
                pass  # Fall through to Python implementation

        # Fall back to Python implementation
        return self._solve_multi_range(solve_input, start_ns)

    def _try_single_range_fallback(self, solve_input: SolveInput, start_ns: int) -> SolveResult:
        """Try MobiusSolver for single-range V3."""
        if self._mobius_solver is None:
            self._mobius_solver = MobiusSolver()
        try:
            result = self._mobius_solver.solve(solve_input)
            # Wrap successful result with piecewise method
            return SolveResult(
                optimal_input=result.optimal_input,
                profit=result.profit,
                iterations=result.iterations,
                method=SolverMethod.PIECEWISE_MOBIUS,
                solve_time_ns=result.solve_time_ns,
            )
        except OptimizationError:
            raise OptimizationError(
                message="Single-range fallback failed",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            ) from None

    def _solve_multi_range(self, solve_input: SolveInput, start_ns: int) -> SolveResult:
        """Solve using Python piecewise-Möbius for tick crossings."""
        # Find the V3 hop with multi-range data (already found in solve(), but refind for safety)
        v3_result = self._find_v3_hop_index(solve_input)
        if v3_result is None:
            raise OptimizationError(
                message="No multi-range V3 hop found",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        v3_hop_index, v3_hop = v3_result

        # Python-only implementation (Rust is tried first in solve())
        return self._solve_multi_range_python(solve_input, v3_hop_index, v3_hop, start_ns)

    def _solve_multi_range_python(
        self, solve_input: SolveInput, v3_hop_index: int, v3_hop: BoundedProductHop, start_ns: int
    ) -> SolveResult:
        """Python-only multi-range solver (fallback when Rust fails)."""
        # Build candidate crossings from tick_ranges with lazy evaluation
        best_result: SolveResult | None = None
        best_profit = -1

        assert v3_hop.tick_ranges is not None
        current_idx = v3_hop.current_range_index

        # Collect plausible candidates first
        plausible_candidates: list[int] = [
            end_idx
            for end_idx in range(current_idx, min(current_idx + 3, len(v3_hop.tick_ranges)))
            if self._is_candidate_plausible(solve_input, v3_hop, current_idx, end_idx, best_profit)
        ]

        if not plausible_candidates:
            elapsed_ns = time.perf_counter_ns() - start_ns
            raise OptimizationError(
                message="No plausible candidate ranges found",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # Evaluate candidates (parallel if multiple, sequential if single)
        if len(plausible_candidates) > 1:
            # Parallel evaluation for multiple candidates
            results = self._evaluate_candidates_parallel(
                solve_input, v3_hop_index, v3_hop, current_idx, plausible_candidates
            )
        else:
            # Sequential for single candidate (avoids thread overhead)
            results = [
                self._try_candidate_range(
                    solve_input, v3_hop_index, v3_hop, current_idx, plausible_candidates[0]
                )
            ]

        # Find best result
        for result in results:
            if result.profit > best_profit:
                best_profit = result.profit
                best_result = result

        if best_result is None:
            elapsed_ns = time.perf_counter_ns() - start_ns
            raise OptimizationError(
                message="No profitable candidate range found",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # Return best result with updated timing
        elapsed_ns = time.perf_counter_ns() - start_ns
        return SolveResult(
            optimal_input=best_result.optimal_input,
            profit=best_result.profit,
            iterations=best_result.iterations,
            method=SolverMethod.PIECEWISE_MOBIUS,
            solve_time_ns=elapsed_ns,
        )

    def _evaluate_candidates_parallel(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        current_idx: int,
        candidates: list[int],
    ) -> list[SolveResult]:
        """Evaluate multiple candidate ranges.

        Uses sequential evaluation for 1-2 candidates (avoids thread overhead).
        Only uses threads for 3+ candidates where parallelism pays off.
        """
        results: list[SolveResult] = []

        # Sequential evaluation for small number of candidates
        # Thread overhead exceeds benefit for 1-2 candidates
        if len(candidates) <= 2:
            for end_idx in candidates:
                try:
                    result = self._try_candidate_range(
                        solve_input, v3_hop_index, v3_hop, current_idx, end_idx
                    )
                    results.append(result)
                except Exception:
                    pass
            return results

        # Parallel evaluation only for 3+ candidates
        with ThreadPoolExecutor(max_workers=min(len(candidates), 3)) as executor:
            future_to_idx = {
                executor.submit(
                    self._try_candidate_range,
                    solve_input,
                    v3_hop_index,
                    v3_hop,
                    current_idx,
                    end_idx,
                ): end_idx
                for end_idx in candidates
            }

            for future in as_completed(future_to_idx):
                try:
                    result = future.result()
                    results.append(result)
                except Exception:
                    pass

        return results

    @staticmethod
    def _is_candidate_plausible(
        solve_input: SolveInput,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
        current_best_profit: int,
    ) -> bool:
        """
        Cheap check to filter out candidates that can't be profitable.

        Returns False if this candidate can be skipped (saves expensive evaluation).
        """
        assert v3_hop.tick_ranges is not None

        # Current range is always plausible (no crossing cost)
        if end_idx == start_idx:
            return True

        # Check if crossing cost exceeds max_input
        ending_range = v3_hop.tick_ranges[end_idx]
        gamma = 1.0 - float(v3_hop.fee)

        # Rough estimate of crossing cost (linear approximation)
        crossing_input = 0.0
        for i in range(start_idx, end_idx):
            range_info = v3_hop.tick_ranges[i]
            # Simple estimate: price movement across range
            sqrt_p_lower = float(range_info.sqrt_price_lower) / Q96
            sqrt_p_upper = float(range_info.sqrt_price_upper) / Q96
            liq = float(range_info.liquidity)

            # Token0->Token1: price goes down, so we go from upper to lower
            if v3_hop.reserve_in > v3_hop.reserve_out:
                # Approximate input to cross this range
                net_input = liq * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_upper)
            else:
                net_input = liq * (sqrt_p_upper - sqrt_p_lower)

            gross_input = net_input / gamma if gamma > 0 else net_input
            crossing_input += gross_input

        # If crossing cost alone exceeds max_input, skip this candidate
        if solve_input.max_input is not None and crossing_input > float(solve_input.max_input):
            return False

        # If we already have a good profit, skip distant ranges unless they might be better
        if current_best_profit > 0 and end_idx > start_idx + 1:
            # More distant ranges have higher crossing costs, unlikely to beat current best
            # unless there's significantly more liquidity
            current_liq = float(v3_hop.tick_ranges[start_idx].liquidity)
            ending_liq = float(ending_range.liquidity)
            if ending_liq < current_liq * 2:  # Need 2x liquidity to justify 2-range crossing
                return False

        # Price impact pruning: use quick estimate to check if swap stays in ending range
        # This is cheaper than full golden section search
        if end_idx > start_idx:
            # Estimate price after crossing (roughly the input needed for crossing)
            estimated_sqrt_price = estimate_price_impact(
                amount_in=crossing_input * 1.1,  # 10% buffer for safety
                liquidity=float(ending_range.liquidity),
                current_sqrt_price=float(ending_range.sqrt_price_lower) / Q96,
                fee=float(v3_hop.fee),
                zero_for_one=_infer_zero_for_one(v3_hop),
            )

            # Check if estimated price stays within ending range bounds
            sqrt_p_lower = float(ending_range.sqrt_price_lower) / Q96
            sqrt_p_upper = float(ending_range.sqrt_price_upper) / Q96

            if not (sqrt_p_lower <= estimated_sqrt_price <= sqrt_p_upper):
                # Price would go out of range even with just crossing amount
                # This candidate is not viable
                return False

        return True

    def _try_candidate_range(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
    ) -> SolveResult:
        """
        Try a candidate ending range using proper V3 tick crossing math.

        Uses the exact V3 swap formulas from mobius.py:
        - Computes TickRangeCrossing with proper fee handling
        - Pre-computes Möbius coefficients for before/after hops
        - Uses golden section search for piecewise profit maximization

        Falls back to Rust implementation when available for ~5x speedup.
        """
        assert v3_hop.tick_ranges is not None

        # Try Rust implementation first for speed
        if self._rust_optimizer is not None:
            try:
                rust_result = self._try_rust_candidate_range(
                    solve_input, v3_hop_index, v3_hop, start_idx, end_idx
                )
                if rust_result is not None:
                    return rust_result
            except OptimizationError:
                pass  # Fall through to Python implementation

        # Convert V3TickRangeInfo to _V3TickRangeHop
        # Determine swap direction from hop reserves
        zero_for_one = _infer_zero_for_one(v3_hop)

        v3_ranges: list[V3TickRangeHop] = []
        for i, range_info in enumerate(v3_hop.tick_ranges):
            if i == v3_hop.current_range_index:
                sqrt_price_current = float(v3_hop.sqrt_price) / Q96
            elif i < v3_hop.current_range_index:
                sqrt_price_current = float(range_info.sqrt_price_upper) / Q96
            else:
                sqrt_price_current = float(range_info.sqrt_price_lower) / Q96

            v3_ranges.append(
                V3TickRangeHop(
                    liquidity=float(range_info.liquidity),
                    sqrt_price_current=sqrt_price_current,
                    sqrt_price_lower=float(range_info.sqrt_price_lower) / Q96,
                    sqrt_price_upper=float(range_info.sqrt_price_upper) / Q96,
                    fee=float(v3_hop.fee),
                    zero_for_one=zero_for_one,
                )
            )

        sequence = V3TickRangeSequence(tuple(v3_ranges))

        # Compute crossing data for this candidate
        try:
            crossing = sequence.compute_crossing(end_idx)
        except (IndexError, ValueError) as e:
            raise OptimizationError(
                message=f"Invalid crossing range index {end_idx}: {e}",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            ) from e

        # Convert hops before/after V3 to MobiusFloatHop (float-space for mobius.py functions)
        from degenbot.arbitrage.optimizers.mobius import MobiusFloatHop

        hops_before: list[MobiusFloatHop] = [
            MobiusFloatHop(
                reserve_in=float(hop.reserve_in),
                reserve_out=float(hop.reserve_out),
                fee=float(hop.fee),
            )
            for hop in solve_input.hops[:v3_hop_index]
            if isinstance(hop, ConstantProductHop | BoundedProductHop)
        ]

        hops_after: list[MobiusFloatHop] = [
            MobiusFloatHop(
                reserve_in=float(hop.reserve_in),
                reserve_out=float(hop.reserve_out),
                fee=float(hop.fee),
            )
            for hop in solve_input.hops[v3_hop_index + 1 :]
            if isinstance(hop, ConstantProductHop | BoundedProductHop)
        ]

        coeffs_before = _float_compute_mobius_coefficients(hops_before) if hops_before else None
        coeffs_after = _float_compute_mobius_coefficients(hops_after) if hops_after else None

        # Get ending range's MobiusFloatHop
        ending_hop_state = crossing.ending_range.to_hop_state()

        # Compute minimum input to cover crossing
        if crossing.crossing_gross_input > 0 and coeffs_before is not None:
            target = crossing.crossing_gross_input
            if target >= coeffs_before.K / coeffs_before.N:
                raise OptimizationError(
                    message="Crossing requires more than path can deliver",
                    iterations=0,
                    method=SolverMethod.PIECEWISE_MOBIUS.name,
                )
            x_min = target * coeffs_before.M / (coeffs_before.K - target * coeffs_before.N)
        elif crossing.crossing_gross_input > 0:
            x_min = crossing.crossing_gross_input
        else:
            x_min = 0.0

        # Single-range Möbius solve as starting point for bracket
        full_hops = [*hops_before, ending_hop_state, *hops_after]
        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        try:
            x_mobius, _, _ = mobius_solve(full_hops, max_input=max_input_float)
        except (ZeroDivisionError, ValueError):
            x_mobius = x_min + 1.0

        # Build bracket for golden section search
        x_low = max(x_min, 0.0)
        x_high = max(x_mobius * 3, x_low + 1.0) if x_mobius > x_low else max(x_low * 5, x_low + 1.0)

        if max_input_float is not None:
            x_high = min(x_high, max_input_float)

        if x_low >= x_high:
            raise OptimizationError(
                message="Invalid bracket for golden section search",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # Define profit evaluation functions (scalar and vectorized)
        def eval_profit(x: float) -> float:
            """Evaluate profit for input x using piecewise V3 swap."""
            if x <= 0:
                return -x

            # Input to V3 (after hops_before)
            if coeffs_before is not None:
                # Apply Möbius transformation from hops_before
                amt_v3 = coeffs_before.K * x / (coeffs_before.M + coeffs_before.N * x)
            else:
                amt_v3 = x

            # Apply piecewise V3 swap
            if amt_v3 < crossing.crossing_gross_input:
                return -x  # Can't cover crossing

            remaining = amt_v3 - crossing.crossing_gross_input
            variable_out = _mobius_simulate_path(remaining, [ending_hop_state])
            v3_out = crossing.crossing_output + variable_out

            # Validate ending range
            final_sqrt_p = self._estimate_final_sqrt_price(remaining, crossing.ending_range)
            if not crossing.ending_range.contains_sqrt_price(final_sqrt_p):
                return -x  # Out of range

            # Apply hops_after
            if coeffs_after is not None:
                final_out = coeffs_after.K * v3_out / (coeffs_after.M + coeffs_after.N * v3_out)
            else:
                final_out = v3_out

            return final_out - x

        # Try vectorized batch evaluation for initial bracket refinement
        # This uses NumPy to evaluate multiple points simultaneously
        if _NUMPY_AVAILABLE:
            try:
                vectorized_result = self._vectorized_bracket_search(
                    x_low=x_low,
                    x_high=x_high,
                    eval_profit_scalar=eval_profit,
                )
                if vectorized_result is not None:
                    return vectorized_result
            except ImportError:
                pass

        # Golden section search with adaptive convergence
        phi = self.PHI
        max_iterations = self.GOLDEN_SECTION_ITERATIONS
        # Convergence tolerance: stop when profit improvement < 0.01% of current profit
        profit_tolerance = 1e-4

        x1 = x_high - phi * (x_high - x_low)
        x2 = x_low + phi * (x_high - x_low)
        p1 = eval_profit(x1)
        p2 = eval_profit(x2)

        best_profit = max(p1, p2)
        iterations = 0

        for i in range(max_iterations):
            iterations = i + 1
            prev_best = best_profit

            if p1 < p2:
                x_low = x1
                x1 = x2
                p1 = p2
                x2 = x_low + phi * (x_high - x_low)
                p2 = eval_profit(x2)
            else:
                x_high = x2
                x2 = x1
                p2 = p1
                x1 = x_high - phi * (x_high - x_low)
                p1 = eval_profit(x1)

            best_profit = max(p1, p2)

            # Early termination: converged if improvement is tiny
            if best_profit > 0 and prev_best > 0:
                improvement = abs(best_profit - prev_best) / prev_best
                if improvement < profit_tolerance:
                    break

        # Best point
        if p1 > p2:
            x_opt, p_opt = x1, p1
        else:
            x_opt, p_opt = x2, p2

        if p_opt <= 0:
            raise OptimizationError(
                message="No profitable solution in candidate range",
                iterations=iterations,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        optimal_input = int(x_opt)
        actual_profit = int(p_opt)

        return SolveResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            iterations=iterations,
            method=SolverMethod.PIECEWISE_MOBIUS,
            solve_time_ns=0,  # Will be set by caller
        )

    def _vectorized_bracket_search(
        self,
        *,
        x_low: float,
        x_high: float,
        eval_profit_scalar: Callable,
    ) -> SolveResult:
        """
        Vectorized bracket search using NumPy for parallel evaluation.

        Evaluates profit at multiple points simultaneously to quickly
        narrow down the optimal region before golden section refinement.

        Returns SolveResult if successful, None to fall back to scalar search.
        """

        # Number of points for initial vectorized evaluation
        # Reduced from 20 to 10 to minimize overhead
        n_points = 10

        # Create log-spaced points in the bracket
        log_x = np.linspace(np.log1p(x_low), np.log1p(x_high), n_points)
        x_points = np.expm1(log_x)

        # Ensure x_low and x_high are included
        x_points[0] = x_low
        x_points[-1] = x_high

        # Vectorized profit evaluation
        profits = np.array([eval_profit_scalar(x) for x in x_points])

        # Find best point
        best_idx = np.argmax(profits)
        best_profit = profits[best_idx]

        if best_profit <= 0:
            return None  # No profitable solution in bracket

        # Refine around best point with smaller bracket
        # Reduced from 10 to 5 points
        idx_low = max(0, best_idx - 1)
        idx_high = min(n_points - 1, best_idx + 1)
        x_refined = np.linspace(x_points[idx_low], x_points[idx_high], 5)
        profits_refined = np.array([eval_profit_scalar(x) for x in x_refined])

        best_idx_refined = np.argmax(profits_refined)
        best_profit_refined = profits_refined[best_idx_refined]
        best_x_refined = x_refined[best_idx_refined]

        if best_profit_refined <= 0:
            return None

        # Return result without full golden section convergence
        # This is faster but slightly less precise (acceptable for many cases)
        return SolveResult(
            optimal_input=int(best_x_refined),
            profit=int(best_profit_refined),
            iterations=n_points + 10,  # Total evaluations
            method=SolverMethod.PIECEWISE_MOBIUS,
            solve_time_ns=0,  # Will be set by caller
        )

    def _try_rust_candidate_range(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
    ) -> SolveResult:
        """
        Try a candidate ending range using the Rust implementation.

        Raises OptimizationError on failure.
        """
        if self._rust_optimizer is None:
            raise OptimizationError(
                message="Rust optimizer not available",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        rust_hops = [
            _rs_mobius.RustHopState(
                float(hop.reserve_in),
                float(hop.reserve_out),
                float(hop.fee),
            )
            for hop in solve_input.hops
            if isinstance(hop, ConstantProductHop | BoundedProductHop)
        ]

        assert v3_hop.tick_ranges is not None

        ending_range_info = v3_hop.tick_ranges[end_idx]
        zero_for_one = _infer_zero_for_one(v3_hop)

        if end_idx > 0 and v3_hop.tick_ranges:
            prev_range = v3_hop.tick_ranges[end_idx - 1]
            entry_sqrt_price = (
                float(prev_range.sqrt_price_lower) / Q96
                if zero_for_one
                else float(prev_range.sqrt_price_upper) / Q96
            )
        else:
            entry_sqrt_price = float(ending_range_info.sqrt_price_lower) / Q96

        rust_ending_range = _rs_mobius.RustV3TickRangeHop(
            liquidity=float(ending_range_info.liquidity),
            sqrt_price_current=entry_sqrt_price,
            sqrt_price_lower=float(ending_range_info.sqrt_price_lower) / Q96,
            sqrt_price_upper=float(ending_range_info.sqrt_price_upper) / Q96,
            fee=float(v3_hop.fee),
            zero_for_one=zero_for_one,
        )

        # Compute crossing data (simplified approximation)
        crossing_input = 0.0
        crossing_output = 0.0

        for i in range(start_idx, end_idx):
            range_info = v3_hop.tick_ranges[i]
            liq = float(range_info.liquidity)
            sqrt_p_lower = float(range_info.sqrt_price_lower) / Q96
            sqrt_p_upper = float(range_info.sqrt_price_upper) / Q96
            gamma = 1.0 - float(v3_hop.fee)

            if zero_for_one:
                net_input = liq * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_upper)
                output = liq * (sqrt_p_upper - sqrt_p_lower)
            else:
                net_input = liq * (sqrt_p_upper - sqrt_p_lower)
                output = liq * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_upper)

            gross_input = net_input / gamma if gamma > 0 else net_input
            crossing_input += gross_input
            crossing_output += output

        # Build TickRangeCrossing for Rust
        rust_crossing = _rs_mobius.RustTickRangeCrossing(
            crossing_gross_input=crossing_input,
            crossing_output=crossing_output,
            ending_range=rust_ending_range,
        )

        # Call Rust piecewise solve
        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        result = self._rust_optimizer.solve_piecewise(
            rust_hops,
            v3_hop_index,
            [rust_crossing],
            max_input_float,
        )

        if result.success:
            return SolveResult(
                optimal_input=int(result.optimal_input),
                profit=int(result.profit),
                iterations=result.iterations,
                method=SolverMethod.PIECEWISE_MOBIUS,
                solve_time_ns=0,  # Will be set by caller
            )
        raise OptimizationError(
            message="Rust piecewise solve failed",
            iterations=result.iterations if hasattr(result, "iterations") else 0,
            method=SolverMethod.PIECEWISE_MOBIUS.name,
        )

    def _get_cached_rust_hops(self, solve_input: SolveInput) -> list:
        cache_key = hash(
            tuple((hop.reserve_in, hop.reserve_out, float(hop.fee)) for hop in solve_input.hops)
        )

        if cache_key not in self._rust_hop_cache:
            rust_hops = []
            for hop in solve_input.hops:
                if isinstance(hop, (ConstantProductHop, BoundedProductHop)):
                    rust_hops.append(
                        _rs_mobius.RustHopState(
                            float(hop.reserve_in),
                            float(hop.reserve_out),
                            float(hop.fee),
                        )
                    )
            self._rust_hop_cache[cache_key] = rust_hops

        return self._rust_hop_cache[cache_key]

    def _get_cached_rust_sequence(
        self,
        v3_hop: BoundedProductHop,
    ) -> Any:
        assert v3_hop.tick_ranges is not None
        zero_for_one = _infer_zero_for_one(v3_hop)

        # Create cache key from hop identity + tick data
        # Use id() for object identity since V3TickRangeInfo is immutable
        range_ids = tuple(id(r) for r in v3_hop.tick_ranges)
        cache_key = (range_ids, v3_hop.current_range_index, zero_for_one)

        if cache_key not in self._rust_sequence_cache:
            rust_ranges = []
            for i, range_info in enumerate(v3_hop.tick_ranges):
                # Determine current sqrt price for this range
                if i == v3_hop.current_range_index:
                    sqrt_p_current = float(v3_hop.sqrt_price) / Q96
                elif i < v3_hop.current_range_index:
                    sqrt_p_current = float(range_info.sqrt_price_upper) / Q96
                else:
                    sqrt_p_current = float(range_info.sqrt_price_lower) / Q96

                rust_ranges.append(
                    _rs_mobius.RustV3TickRangeHop(
                        liquidity=float(range_info.liquidity),
                        sqrt_price_current=sqrt_p_current,
                        sqrt_price_lower=float(range_info.sqrt_price_lower) / Q96,
                        sqrt_price_upper=float(range_info.sqrt_price_upper) / Q96,
                        fee=float(v3_hop.fee),
                        zero_for_one=zero_for_one,
                    )
                )

            self._rust_sequence_cache[cache_key] = _rs_mobius.RustV3TickRangeSequence(rust_ranges)

        return self._rust_sequence_cache[cache_key]

    def _try_rust_v3_v3(
        self,
        solve_input: SolveInput,
        start_ns: int,
    ) -> SolveResult:
        """
        Try V3-V3 Rust solver for 2-hop paths where both hops are V3.

        Raises OptimizationError on failure.
        """
        if self._rust_optimizer is None:
            raise OptimizationError(
                message="Rust optimizer not available",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # Check both hops are V3 with tick range data
        v3_hops: list[BoundedProductHop] = []
        for hop in solve_input.hops:
            if hop.invariant == PoolInvariant.BOUNDED_PRODUCT and isinstance(
                hop, BoundedProductHop
            ):
                v3_hops.append(hop)
            else:
                raise OptimizationError(
                    message="Non-V3 hop in V3-V3 path",
                    iterations=0,
                    method=SolverMethod.PIECEWISE_MOBIUS.name,
                )

        if len(v3_hops) != self.MIN_HOPS:
            raise OptimizationError(
                message="Need exactly 2 V3 hops",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # Get cached Rust sequences for both hops
        rust_seq1 = self._get_cached_rust_sequence(v3_hops[0])
        rust_seq2 = self._get_cached_rust_sequence(v3_hops[1])

        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        result = self._rust_optimizer.solve_v3_v3(
            rust_seq1,
            rust_seq2,
            max_input_float,
            10,
        )

        if result.success:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=int(result.optimal_input),
                profit=int(result.profit),
                iterations=result.iterations,
                method=SolverMethod.PIECEWISE_MOBIUS,
                solve_time_ns=elapsed_ns,
            )
        raise OptimizationError(
            message="V3-V3 Rust solve failed",
            iterations=result.iterations if hasattr(result, "iterations") else 0,
            method=SolverMethod.PIECEWISE_MOBIUS.name,
        )

    def _try_rust_multi_range(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        start_ns: int,
    ) -> SolveResult:
        """
        Try to solve multi-range V3 using Rust's full sequence solver.

        Uses cached Rust objects to minimize Python-Rust marshalling overhead.
        Raises OptimizationError on failure.
        """
        if self._rust_optimizer is None:
            raise OptimizationError(
                message="Rust optimizer not available",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.name,
            )

        # Get cached Rust objects
        rust_hops = self._get_cached_rust_hops(solve_input)
        rust_sequence = self._get_cached_rust_sequence(v3_hop)

        # Call Rust full sequence solver
        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        result = self._rust_optimizer.solve_v3_sequence(
            rust_hops,
            v3_hop_index,
            rust_sequence,
            3,  # max_candidates
            max_input_float,
        )

        if result.success:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=int(result.optimal_input),
                profit=int(result.profit),
                iterations=result.iterations,
                method=SolverMethod.PIECEWISE_MOBIUS,
                solve_time_ns=elapsed_ns,
            )
        raise OptimizationError(
            message="Rust V3 sequence solve failed",
            iterations=result.iterations if hasattr(result, "iterations") else 0,
            method=SolverMethod.PIECEWISE_MOBIUS.name,
        )

    def _estimate_final_sqrt_price(
        self,
        amount_in: float,
        ending_range: V3TickRangeHop,
    ) -> float:
        """Estimate the final sqrt price after swapping within ending range."""
        if amount_in <= 0:
            return ending_range.sqrt_price_current

        # For constant product, price moves as: 1/sqrt_p_new = 1/sqrt_p + amount/L
        # So: sqrt_p_new = 1 / (1/sqrt_p + amount/L) # noqa: ERA001
        liq = ending_range.liquidity
        sqrt_p = ending_range.sqrt_price_current
        gamma = 1.0 - ending_range.fee

        if ending_range.zero_for_one:
            new_sqrt_p = liq / (liq / sqrt_p + amount_in / gamma)
        else:
            new_sqrt_p = sqrt_p + amount_in * gamma / liq

        return new_sqrt_p


# ---------------------------------------------------------------------------
# Solidly Stable Solver (Newton's method for x³y + xy³ ≥ k)
# ---------------------------------------------------------------------------


def _solidly_swap_output_float(
    *,
    reserve_in: float,
    reserve_out: float,
    amount_in: float,
    gamma: float,
    decimals_in: int,
    decimals_out: int,
) -> float:
    """
    Float approximation of Solidly stable swap output.

    Solves the Solidly invariant x³y + xy³ ≥ k for output given input.
    Uses Newton's method on the implicit equation f(y) = x³y + xy³ - k = 0.

    The reserves and amounts are scaled to 18-decimal internally, matching
    the Solidity contract behavior.
    """
    if amount_in <= 0:
        return 0.0
    if reserve_in <= 0 or reserve_out <= 0:
        return 0.0

    d_in = 10**decimals_in
    d_out = 10**decimals_out
    scale = 1e18

    # Scale reserves to 18-decimal
    r0_scaled = reserve_in * scale / d_in
    r1_scaled = reserve_out * scale / d_out

    # Apply fee
    amount_after_fee = amount_in * gamma

    # Scale amount to 18-decimal
    a_scaled = amount_after_fee * scale / d_in

    # New x (input reserve after deposit)
    x_new = r0_scaled + a_scaled

    # Compute k at original reserves: k = xy * (x² + y²) / 10^18
    # In 18-decimal space: k = (r0 * r1 / 1e18) * ((r0² / 1e18) + (r1² / 1e18)) / 1e18
    xy = r0_scaled * r1_scaled / scale
    x2_y2 = (r0_scaled**2 / scale) + (r1_scaled**2 / scale)
    k = xy * x2_y2 / scale

    if k <= 0:
        return 0.0

    # Solve for y_new such that: x_new³ * y_new + x_new * y_new³ = k
    # f(y) = (x³/1e36) * y + x * (y³/1e36) - k = 0
    # f'(y) = (x³/1e36) + 3x * (y²/1e36)
    x3 = x_new**3 / (scale * scale)
    y = r1_scaled  # initial guess

    for _ in range(100):
        y3 = y**3 / (scale * scale)
        f_y = x3 * y + x_new * y3 - k
        y2 = y**2 / scale
        f_prime = x3 + 3.0 * x_new * y2

        if abs(f_prime) < 1e-30:
            break

        dy = f_y / f_prime
        y -= dy

        # y must be positive
        if y <= 0:
            y = 1.0
            break

        if abs(dy) < 1e-3:
            break

    # Output = old reserve - new reserve
    output_scaled = r1_scaled - y
    if output_scaled <= 0:
        return 0.0

    # Descale from 18-decimal
    output = output_scaled * d_out / scale
    return float(max(output, 0.0))


def _simulate_mixed_path(
    x: float,
    hops: tuple[HopType, ...],
) -> float:
    """
    Simulate a path with mixed hop types using float math.

    For each hop:
    - ConstantProductHop: V2 formula y = gamma*s*x / (r + gamma*x)
    - BoundedProductHop: Same V2 formula (virtual reserves)
    - SolidlyStableHop: float approximation of Solidly swap

    For integer-exact evaluation, use ``_simulate_mixed_path_int`` instead.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0

        if hop.invariant in {
            PoolInvariant.CONSTANT_PRODUCT,
            PoolInvariant.BOUNDED_PRODUCT,
        }:
            r_i = float(hop.reserve_in)
            s_i = float(hop.reserve_out)
            g_i = hop.gamma
            denom = r_i + amount * g_i
            if denom <= 0:
                return 0.0
            amount = amount * g_i * s_i / denom

        elif hop.invariant == PoolInvariant.SOLIDLY_STABLE:
            # Use integer swap if available for better accuracy
            if hop.swap_fn is not None:
                amount_int = hop.swap_fn(int(amount))
                amount = float(amount_int)
            else:
                amount = _solidly_swap_output_float(
                    reserve_in=float(hop.reserve_in),
                    reserve_out=float(hop.reserve_out),
                    amount_in=amount,
                    gamma=hop.gamma,
                    decimals_in=hop.decimals_in,
                    decimals_out=hop.decimals_out,
                )

        else:
            # Unsupported invariant
            return 0.0

    return amount


def _simulate_mixed_path_int(
    x: int,
    hops: tuple[HopType, ...],
) -> int:
    """
    Simulate a path with mixed hop types using integer math.

    For Solidly hops with ``swap_fn``, uses the integer-accurate callable.
    For V2 hops, uses integer constant-product formula.
    Falls back to float for hops without integer support.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0

        if hop.invariant in {
            PoolInvariant.CONSTANT_PRODUCT,
            PoolInvariant.BOUNDED_PRODUCT,
        }:
            r_i = hop.reserve_in
            s_i = hop.reserve_out
            g_num = hop.fee.denominator - hop.fee.numerator
            g_den = hop.fee.denominator
            # V2 formula: y = (gamma * s * x) / (r + gamma * x)
            gamma_x = amount * g_num // g_den
            denom = r_i + gamma_x
            if denom <= 0:
                return 0
            amount = gamma_x * s_i // denom

        elif hop.invariant == PoolInvariant.SOLIDLY_STABLE:
            if hop.swap_fn is not None:
                amount = hop.swap_fn(amount)
            else:
                # Fall back to float (less accurate)
                out = _solidly_swap_output_float(
                    reserve_in=float(hop.reserve_in),
                    reserve_out=float(hop.reserve_out),
                    amount_in=float(amount),
                    gamma=hop.gamma,
                    decimals_in=hop.decimals_in,
                    decimals_out=hop.decimals_out,
                )
                amount = int(out)

        else:
            return 0

    return amount


class SolidlyStableSolver(Solver):
    """
    Solver for paths containing Solidly stable pools (x³y + xy³ ≥ k).

    Uses golden section search on the integer profit function when
    ``swap_fn`` is available on Solidly hops (EVM-exact). Falls back
    to Newton's method with float simulation when no swap_fn is set.

    For mixed paths (V2 + Solidly), the initial bracket comes from
    treating all hops as V2-equivalent and running the Möbius solver.

    Performance:
    - With swap_fn (integer): ~180μs (25 golden section iterations)
    - Without swap_fn (float): ~257μs (Newton's method)
    """

    GOLDEN_SECTION_ITERATIONS = 25
    NEWTON_MAX_ITERATIONS = 30
    NEWTON_TOLERANCE = 1e-6

    def _adaptive_step(self, x: float) -> float:
        """Compute an adaptive finite-difference step size."""
        return max(min(x * 1e-4, 1e12), 1e6)

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        if solve_input.num_hops < self.MIN_HOPS:
            return False
        if not solve_input.has_solidly_stable:
            return False
        for hop in solve_input.hops:
            if hop.invariant not in {
                PoolInvariant.CONSTANT_PRODUCT,
                PoolInvariant.BOUNDED_PRODUCT,
                PoolInvariant.SOLIDLY_STABLE,
            }:
                return False
        return True

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if not self.supports(solve_input):
            raise OptimizationError(
                message="SolidlyStableSolver requires 2+ hops with at least one Solidly stable",
                iterations=0,
                method=SolverMethod.SOLIDLY_STABLE.name,
            )

        # --- Profitability check via V2-equivalent Möbius ---
        v2_equiv_hops: list[HopType] = []
        for hop in solve_input.hops:
            if hop.invariant == PoolInvariant.SOLIDLY_STABLE:
                v2_equiv_hops.append(
                    ConstantProductHop(
                        reserve_in=hop.reserve_in,
                        reserve_out=hop.reserve_out,
                        fee=hop.fee,
                    )
                )
            else:
                v2_equiv_hops.append(hop)

        mobius_coeffs = _compute_mobius_coefficients(tuple(v2_equiv_hops))

        if not mobius_coeffs.is_profitable:
            raise OptimizationError(
                message="Not profitable (V2-equivalent Möbius check failed)",
                iterations=0,
                method=SolverMethod.SOLIDLY_STABLE.name,
            )

        # Check whether all Solidly hops have swap_fn (integer path)
        has_swap_fn = all(
            hop.swap_fn is not None
            for hop in solve_input.hops
            if hop.invariant == PoolInvariant.SOLIDLY_STABLE
        )

        if has_swap_fn:
            return self._solve_golden_section(solve_input, mobius_coeffs, start_ns)
        return self._solve_newton(solve_input, mobius_coeffs, start_ns)

    def _solve_golden_section(
        self,
        solve_input: SolveInput,
        mobius_coeffs: _MobiusCoefficients,
        start_ns: int,
    ) -> SolveResult:
        """Golden section search using integer path evaluation."""
        hops = solve_input.hops

        # Bracket: [1, max_reserve] # noqa: ERA001
        x_low = 1
        max_reserve = max(h.reserve_in for h in hops)
        x_high = max_reserve
        if solve_input.max_input is not None:
            x_high = min(x_high, solve_input.max_input)

        # Narrow bracket using Möbius initial guess
        x_mobius = mobius_coeffs.optimal_input()
        if x_mobius > 0:
            # Center bracket around Möbius estimate (±5x)
            x_center = min(int(x_mobius), x_high)
            x_low = max(1, x_center // 5)
            x_high = min(x_center * 5, x_high)

        phi = (math.sqrt(5) - 1) / 2  # ~0.618
        n_iterations = self.GOLDEN_SECTION_ITERATIONS

        x1 = int(x_high - phi * (x_high - x_low))
        x2 = int(x_low + phi * (x_high - x_low))
        p1 = _simulate_mixed_path_int(x1, hops) - x1
        p2 = _simulate_mixed_path_int(x2, hops) - x2

        for _ in range(n_iterations):
            if p1 < p2:
                x_low = x1
                x1 = x2
                p1 = p2
                x2 = int(x_low + phi * (x_high - x_low))
                p2 = _simulate_mixed_path_int(x2, hops) - x2
            else:
                x_high = x2
                x2 = x1
                p2 = p1
                x1 = int(x_high - phi * (x_high - x_low))
                p1 = _simulate_mixed_path_int(x1, hops) - x1

        x_opt = int((x_low + x_high) / 2)

        # Integer verification: check a few candidates around the optimum
        best_input = 0
        best_profit = 0
        search_radius = 3

        for candidate in range(max(1, x_opt - search_radius), x_opt + search_radius + 2):
            if solve_input.max_input is not None and candidate > solve_input.max_input:
                continue
            output = _simulate_mixed_path_int(candidate, hops)
            profit = output - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        elapsed_ns = time.perf_counter_ns() - start_ns

        if best_profit <= 0:
            raise OptimizationError(
                message="Not profitable (integer verification failed)",
                iterations=n_iterations,
                method=SolverMethod.SOLIDLY_STABLE.name,
            )

        return SolveResult(
            optimal_input=best_input,
            profit=best_profit,
            iterations=n_iterations,
            method=SolverMethod.SOLIDLY_STABLE,
            solve_time_ns=elapsed_ns,
        )

    def _solve_newton(
        self,
        solve_input: SolveInput,
        mobius_coeffs: _MobiusCoefficients,
        start_ns: int,
    ) -> SolveResult:
        """Newton's method with float simulation (fallback when no swap_fn)."""
        hops = solve_input.hops

        x = mobius_coeffs.optimal_input()
        if x <= 0:
            raise OptimizationError(
                message="Möbius initial guess <= 0",
                iterations=0,
                method=SolverMethod.SOLIDLY_STABLE.name,
            )

        if solve_input.max_input is not None and x > float(solve_input.max_input):
            x = float(solve_input.max_input)

        iterations = 0
        for i in range(self.NEWTON_MAX_ITERATIONS):
            h = self._adaptive_step(x)

            output = _simulate_mixed_path(x, hops)
            profit = output - x

            output_plus = _simulate_mixed_path(x + h, hops)
            output_minus = _simulate_mixed_path(max(x - h, 0.0), hops)
            dprofit = (output_plus - output_minus - 2 * h) / (2 * h)

            iterations = i + 1

            if abs(dprofit) < self.NEWTON_TOLERANCE:
                break

            profit_plus = output_plus - (x + h)
            profit_mid = profit
            profit_minus = output_minus - max(x - h, 0.0)
            d2profit = (profit_plus - 2 * profit_mid + profit_minus) / (h * h)

            if abs(d2profit) < 1e-30:
                if abs(dprofit) > 1e-12:
                    x += dprofit * 0.01
                    if x <= 0:
                        x /= 2.0
                else:
                    break
            else:
                step = dprofit / d2profit
                max_step = x * 0.5
                if abs(step) > max_step:
                    step = max_step if step > 0 else -max_step
                x_new = x - step
                if x_new <= 0:
                    x_new = x / 2.0
                x = x_new

        if x <= 0:
            raise OptimizationError(
                message="Newton did not converge to positive input",
                iterations=iterations,
                method=SolverMethod.SOLIDLY_STABLE.name,
            )

        if solve_input.max_input is not None and x > float(solve_input.max_input):
            x = float(solve_input.max_input)

        # Integer verification
        x_floor = int(x)
        best_input = 0
        best_profit = 0
        search_radius = 5

        for candidate in range(max(1, x_floor - search_radius), x_floor + search_radius + 2):
            if solve_input.max_input is not None and candidate > solve_input.max_input:
                continue
            output = _simulate_mixed_path(float(candidate), hops)
            profit = int(output) - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        elapsed_ns = time.perf_counter_ns() - start_ns

        if best_profit <= 0:
            raise OptimizationError(
                message="Not profitable (integer verification failed)",
                iterations=iterations,
                method=SolverMethod.SOLIDLY_STABLE.name,
            )

        return SolveResult(
            optimal_input=best_input,
            profit=best_profit,
            iterations=iterations,
            method=SolverMethod.SOLIDLY_STABLE,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# Balancer Multi-Token Solver (closed-form N-token G3M arbitrage)
# ---------------------------------------------------------------------------


class BalancerMultiTokenSolver(Solver):
    """
    Closed-form solver for N-token Balancer weighted pool basket arbitrage.

    Uses the QuantAMM closed-form solution (Equation 9) from Willetts &
    Harrington's paper for optimal multi-token trades on geometric mean
    market makers.

    Performance:
    - N=3: ~12μs (12 signatures)
    - N=4: ~50μs (50 signatures)
    - N=5: ~180μs (180 signatures)

    Unlike pairwise solvers, this finds optimal basket trades where
    multiple tokens can be deposited/withdrawn simultaneously.

    Usage:
    -----
    >>> from degenbot.arbitrage.optimizers.solver import (
    ...     BalancerMultiTokenHop, BalancerMultiTokenSolver, SolveInput
    ... )
    >>> hop = BalancerMultiTokenHop(
    ...     reserves=(100e18, 2e12, 1e12),  # WETH, USDC, DAI in wei
    ...     weights=(5e17, 25e16, 25e16),   # 50%, 25%, 25%
    ...     fee=Fraction(3, 1000),
    ...     market_prices=(2000.0, 1.0, 1.0),  # In USD
    ... )
    >>> solver = BalancerMultiTokenSolver()
    >>> result = solver.solve(SolveInput(hops=(hop,)))
    """

    def __init__(
        self,
        *,
        use_heuristic_pruning: bool = False,
        max_signatures: int = 500,
    ) -> None:
        """
        Initialize the solver.

        Parameters
        ----------
        use_heuristic_pruning
            If True, use price-ratio heuristic to prune signatures.
            Recommended for N >= 6.
        max_signatures
            Maximum signatures to evaluate before forcing pruning.
        """
        self._solver = BalancerWeightedPoolSolver(
            use_heuristic_pruning=use_heuristic_pruning,
            max_signatures=max_signatures,
        )

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        # Only supports single BalancerMultiTokenHop
        return (
            solve_input.num_hops == 1
            and solve_input.hops[0].invariant == PoolInvariant.BALANCER_MULTI_TOKEN
        )

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if not self.supports(solve_input):
            raise OptimizationError(
                message="BalancerMultiTokenSolver requires single BalancerMultiTokenHop",
                iterations=0,
                method=SolverMethod.BALANCER_MULTI_TOKEN.name,
            )

        hop = solve_input.hops[0]
        assert isinstance(hop, BalancerMultiTokenHop)

        if hop.market_prices is None:
            raise OptimizationError(
                message="BalancerMultiTokenHop requires market_prices",
                iterations=0,
                method=SolverMethod.BALANCER_MULTI_TOKEN.name,
            )

        pool = BalancerMultiTokenState(
            reserves=hop.reserves,
            weights=hop.weights,
            fee=hop.fee,
            decimals=hop.decimals,
        )

        max_input: float | None = None
        if solve_input.max_input is not None:
            max_input = float(solve_input.max_input)

        result = self._solver.solve(pool, hop.market_prices, max_input=max_input)

        elapsed_ns = time.perf_counter_ns() - start_ns

        if not result.success:
            raise OptimizationError(
                message="No profitable basket trade found",
                iterations=result.iterations,
                method=SolverMethod.BALANCER_MULTI_TOKEN.name,
            )

        # For basket trades, "optimal_input" is the total deposit value
        # and "profit" is the total withdrawal value minus deposits
        total_deposit = sum(max(0, t) * hop.market_prices[i] for i, t in enumerate(result.trades))
        profit = result.profit

        return SolveResult(
            optimal_input=int(total_deposit),
            profit=int(profit),
            iterations=result.iterations,
            method=SolverMethod.BALANCER_MULTI_TOKEN,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# ArbSolver — the dispatcher
# ---------------------------------------------------------------------------


class ArbSolver(Solver):
    """
    Top-level solver that dispatches to the best method.

    Each sub-solver owns its own Rust acceleration and falls back to
    Python internally. ArbSolver is a pure dispatcher.

    Dispatch order:
    1. MobiusSolver (V2 + single-range V3, Rust-accelerated)
    2. PiecewiseMobiusSolver (V3 multi-range, Rust-accelerated)
    3. SolidlyStableSolver (Python only)
    4. BalancerMultiTokenSolver (Python only)
    5. BrentSolver (Python only, handles everything)

    Usage:
    -----
    >>> from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput
    >>> solver = ArbSolver()
    >>> result = solver.solve(SolveInput(hops=(
    ...     Hop(reserve_in=2_000_000e6, reserve_out=1_000e18, fee=Fraction(3, 1000)),
    ...     Hop(reserve_in=1_500_000e6, reserve_out=800e18, fee=Fraction(3, 1000)),
    ... )))
    """

    MIN_HOPS = 2

    _RUST_METHOD_MAP: ClassVar[dict[int, SolverMethod]] = {
        0: SolverMethod.MOBIUS,
        1: SolverMethod.PIECEWISE_MOBIUS,
        2: SolverMethod.PIECEWISE_MOBIUS,
    }

    def __init__(self) -> None:
        self._pool_cache: Any = None
        self._next_pool_id: int = 1
        self._pool_id_map: dict[int, int] = {}
        self._mobius = MobiusSolver()
        self._piecewise = PiecewiseMobiusSolver()
        self._solidly = SolidlyStableSolver()
        self._balancer_multi = BalancerMultiTokenSolver()
        self._brent = BrentSolver()
        if _rs_mobius is not None:
            self._pool_cache = _rs_mobius.RustPoolCache()

    def get_pool_cache(self) -> Any:
        """Return the Rust-side pool state cache.

        The cache can be used to register pool states at update time,
        then solve by pool ID reference without any Python object
        construction on the solve path.
        """
        if self._pool_cache is None:
            raise RuntimeError(message="Pool cache requires the Rust extension (degenbot_rs)")
        return self._pool_cache

    def register_pool(
        self,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
        *,
        pool_id: int | None = None,
    ) -> int:
        """Register a pool's state in the Rust cache.

        Call this at pool state update time (once per block). The returned
        pool_id can then be used in `solve_cached()` calls.

        If pool_id is not provided, a new unique ID is assigned.

        Returns the pool_id (useful when auto-assigning).
        """
        cache = self.get_pool_cache()

        if pool_id is None:
            pool_id = self._next_pool_id
            self._next_pool_id += 1

        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)
        return pool_id

    def update_pool(
        self,
        pool_id: int,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
    ) -> None:
        """Update a previously registered pool's state in the Rust cache.

        Equivalent to register_pool() with an explicit pool_id.
        """
        cache = self.get_pool_cache()
        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)

    def remove_pool(self, pool_id: int) -> bool:
        """Remove a pool from the Rust cache.

        Returns True if the pool was found and removed.
        """
        cache = self.get_pool_cache()
        return cache.remove(pool_id)

    def solve_cached(
        self,
        path: list[int],
        *,
        max_input: int | None = None,
    ) -> SolveResult:
        """Solve an arbitrage path using cached pool states by ID.

        This is the fastest solve path: no Python object construction,
        no per-item extraction, just a list of integer pool IDs passed
        to Rust. Pool states must have been registered beforehand via
        `register_pool()` or `update_pool()`.

        Parameters
        ----------
        path
            Ordered list of pool IDs along the arbitrage path.
        max_input
            Optional maximum input constraint.

        Returns
        -------
        SolveResult
        """
        start_ns = time.perf_counter_ns()
        cache = self.get_pool_cache()

        max_input_float = float(max_input) if max_input is not None else None

        try:
            result = cache.solve(path, max_input_float)
        except (ValueError, TypeError) as e:
            raise OptimizationError(
                message=f"Pool cache solve failed: {e}",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            ) from e

        if not result.supported:
            raise OptimizationError(
                message="Not supported by cache",
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

        # Integer refinement results from cache
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
            message="Not profitable",
            iterations=result.iterations,
            method=method.name,
        )

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops >= self.MIN_HOPS

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        """
        Solve with automatic method selection.

        Dispatches to sub-solvers in order. Each sub-solver tries
        Rust first, then falls back to Python internally.

        Raises OptimizationError if no solver can find a profitable solution.
        """
        # MobiusSolver: V2 + single-range V3
        if self._mobius.supports(solve_input):
            try:
                return self._mobius.solve(solve_input)
            except OptimizationError:
                pass

        # PiecewiseMobiusSolver: V3 multi-range
        if self._piecewise.supports(solve_input):
            try:
                return self._piecewise.solve(solve_input)
            except OptimizationError:
                pass

        # SolidlyStableSolver
        if self._solidly.supports(solve_input):
            try:
                return self._solidly.solve(solve_input)
            except OptimizationError:
                pass

        # BalancerMultiTokenSolver
        if self._balancer_multi.supports(solve_input):
            try:
                return self._balancer_multi.solve(solve_input)
            except OptimizationError:
                pass

        # BrentSolver fallback (handles everything)
        if self._brent.supports(solve_input):
            try:
                return self._brent.solve(solve_input)
            except OptimizationError:
                pass

        raise OptimizationError(
            message="All solver methods failed to find a profitable solution",
            iterations=0,
            method=SolverMethod.MOBIUS.name,
        )


# ---------------------------------------------------------------------------
# Conversion Utilities
# ---------------------------------------------------------------------------


def _v3_virtual_reserves(
    *,
    liquidity: int,
    sqrt_price_x96: int,
    zero_for_one: bool,
) -> tuple[int, int]:
    """
    Compute virtual reserves for a V3/V4 tick range.

    For a concentrated-liquidity pool, the effective (virtual) reserves
    within the current tick range are:
        R0_virtual = L / sqrt_price
        R1_virtual = L * sqrt_price

    where sqrt_price = sqrt_price_x96 / 2**96.

    The reserves are returned as integers scaled to match V2 wei-scale
    reserve magnitudes for compatibility with the Möbius solver.

    Parameters
    ----------
    liquidity
        V3/V4 liquidity in this tick range.
    sqrt_price_x96
        Current sqrt price as Q64.96 fixed-point.
    zero_for_one
        True if swapping token0 → token1 (input is token0).

    Returns
    -------
    tuple[int, int]
        (reserve_in, reserve_out) as integers in wei-equivalent scale.
    """
    # Convert X96 to float for virtual reserve computation
    sqrt_price = sqrt_price_x96 / Q96
    liq = float(liquidity)

    r0_virtual = liq / sqrt_price
    r1_virtual = liq * sqrt_price

    # Scale to integer — multiply by Q96 to preserve precision
    # The Möbius solver uses float internally, so the exact integer
    # scale doesn't matter as long as reserves are in the right ratio.
    # Using int(round()) to avoid drift.
    scale = Q96
    if zero_for_one:
        return round(r0_virtual * scale), round(r1_virtual * scale)
    return round(r1_virtual * scale), round(r0_virtual * scale)


# Cache for tick range lookups: (pool_address, current_tick, zero_for_one) -> result
_tick_range_cache: dict[tuple[str, int, bool], tuple[tuple[V3TickRangeInfo, ...], int] | None] = {}
_MAX_TICK_RANGE_CACHE_SIZE = 128


def _get_cached_tick_ranges(
    *,
    pool: UniswapV3Pool | UniswapV4Pool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
    """
    Cached version of _v3_get_adjacent_tick_ranges.

    Uses LRU-style cache keyed by (pool_address, current_tick, zero_for_one).
    Cache is cleared when it exceeds _MAX_TICK_RANGE_CACHE_SIZE entries.
    """
    cache_key = (str(pool.address), pool.tick, zero_for_one)

    # Check cache
    if cache_key in _tick_range_cache:
        return _tick_range_cache[cache_key]

    # Compute and cache result
    result = _v3_get_adjacent_tick_ranges(
        pool=pool,
        zero_for_one=zero_for_one,
        max_ranges=max_ranges,
    )

    # Simple LRU: clear if too large (simplest approach)
    if len(_tick_range_cache) >= _MAX_TICK_RANGE_CACHE_SIZE:
        _tick_range_cache.clear()

    _tick_range_cache[cache_key] = result
    return result


def _v3_get_adjacent_tick_ranges(
    *,
    pool: UniswapV3Pool | UniswapV4Pool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
    """
    Fetch adjacent tick ranges from a V3/V4 pool for multi-range support.

    Returns a tuple of (tick_ranges, current_range_index) where current_range_index
    indicates which range contains the current price. Returns None if the pool
    doesn't have full tick data available (sparse liquidity map).

    Parameters
    ----------
    pool
        A UniswapV3Pool or UniswapV4Pool.
    zero_for_one
        True if swapping token0 → token1.
    max_ranges
        Maximum number of ranges to fetch (including current).

    Returns
    -------
    tuple[tuple[V3TickRangeInfo, ...], int] | None
        Adjacent tick ranges and current range index, or None if sparse.
    """

    # Check if pool has full tick data (sparse pools can't provide adjacent ranges)
    if getattr(pool, "sparse_liquidity_map", True):
        return None

    tick_data = getattr(pool, "tick_data", None)
    tick_bitmap = getattr(pool, "tick_bitmap", None)
    tick_spacing = getattr(pool, "tick_spacing", 0)

    if tick_data is None or tick_bitmap is None or tick_spacing == 0:
        return None

    current_tick = pool.tick

    # Generate ticks in swap direction
    less_than_or_equal = not zero_for_one  # token0→token1: price goes down, tick goes down

    ticks_along_path = gen_ticks(
        tick_data=tick_data,
        starting_tick=current_tick,
        tick_spacing=tick_spacing,
        less_than_or_equal=less_than_or_equal,
    )

    # Build list of initialized ticks
    # Clamp ticks to MIN_TICK/MAX_TICK bounds like real V3 pool does
    initialized_ticks: list[int] = []
    try:
        for tick, is_initialized in ticks_along_path:
            # Clamp to valid tick range (like UniswapV3Pool._calculate_swap)
            clamped_tick = (
                max(MIN_TICK, tick)  # descending ticks
                if less_than_or_equal
                else min(MAX_TICK, tick)  # ascending ticks
            )

            # Stop if we've reached the boundary
            if clamped_tick != tick:
                break

            if len(initialized_ticks) >= max_ranges + 1:
                break
            if is_initialized or tick == current_tick:
                initialized_ticks.append(tick)
    except StopIteration:
        pass

    if len(initialized_ticks) < 2:
        # Not enough range boundaries to form meaningful ranges
        return None

    # Build V3TickRangeInfo for each range
    ranges: list[V3TickRangeInfo] = []
    current_idx = 0

    for i in range(len(initialized_ticks) - 1):
        if zero_for_one:
            tick_lower = initialized_ticks[i + 1]
            tick_upper = initialized_ticks[i]
        else:
            tick_lower = initialized_ticks[i]
            tick_upper = initialized_ticks[i + 1]

        # Get liquidity at this tick
        tick_info = tick_data.get(tick_lower if zero_for_one else tick_upper)
        liquidity = tick_info.liquidity_net if tick_info else pool.liquidity

        # Compute sqrt price bounds
        sqrt_price_lower = int(get_sqrt_ratio_at_tick(tick_lower))
        sqrt_price_upper = int(get_sqrt_ratio_at_tick(tick_upper))

        range_info = V3TickRangeInfo(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity=liquidity,
            sqrt_price_lower=sqrt_price_lower,
            sqrt_price_upper=sqrt_price_upper,
        )
        ranges.append(range_info)

        # Determine if this range contains current price
        if zero_for_one:
            if tick_lower <= current_tick < tick_upper:
                current_idx = i
        elif tick_lower <= current_tick < tick_upper:
            current_idx = i

    if len(ranges) < 1:
        return None

    return (tuple(ranges), current_idx)


def pool_to_hop(
    pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool | CamelotLiquidityPool,
    input_token: Erc20Token,
) -> HopType:
    """
    Convert a pool object to a Hop for the solver.

    For V2/Aerodrome volatile pools: returns ConstantProductHop with actual reserves.
    For Aerodrome stable pools: returns SolidlyStableHop with decimals.
    For Camelot volatile pools: returns ConstantProductHop with asymmetric fees.
    For Camelot stable pools: returns SolidlyStableHop with decimals.
    For V3/V4 pools: returns BoundedProductHop with virtual reserves.

    Parameters
    ----------
    pool
        A UniswapV2Pool, AerodromeV2Pool, UniswapV3Pool, or UniswapV4Pool.
    input_token
        The token being deposited into this pool.

    Returns
    -------
    Hop
        A Hop with reserves oriented for the swap direction.
    """
    zero_for_one = input_token == pool.token0

    # Camelot stable pool — Solidly invariant
    if isinstance(pool, CamelotLiquidityPool) and getattr(pool, "stable_swap", False):
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals

        # Build swap_fn using Camelot's get_y
        reserves0 = pool.state.reserves_token0
        reserves1 = pool.state.reserves_token1
        decimals0 = 10**pool.token0.decimals
        decimals1 = 10**pool.token1.decimals
        fee = pool.fee
        token_in = 0 if zero_for_one else 1

        def _camelot_stable_swap_fn(
            amount_in: int,
            __reserves0: int = reserves0,
            __reserves1: int = reserves1,
            __decimals0: int = decimals0,
            __decimals1: int = decimals1,
            __fee: Fraction = fee,
            __token_in: int = token_in,
        ) -> int:
            return general_calc_exact_in_stable(
                amount_in=amount_in,
                token_in=__token_in,  # type: ignore[arg-type]
                reserves0=__reserves0,
                reserves1=__reserves1,
                decimals0=__decimals0,
                decimals1=__decimals1,
                fee=__fee,
                k_func=k_camelot,
                get_y_func=get_y_camelot,
            )

        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
            swap_fn=_camelot_stable_swap_fn,
        )

    # Camelot volatile pool — constant product with asymmetric fees
    if isinstance(pool, CamelotLiquidityPool):
        # Camelot stores fee as tuple: (Fraction(fee_token0, denom), Fraction(fee_token1, denom))
        # pool.fee is the tuple from super().__init__
        fee_tuple = pool.fee
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            fee_in = fee_tuple[0]  # fee for token0 → token1
            fee_out = fee_tuple[1]  # fee for token1 → token0
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            fee_in = fee_tuple[1]  # fee for token1 → token0
            fee_out = fee_tuple[0]  # fee for token0 → token1
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_in,
            fee_out=fee_out,
        )

    # Aerodrome stable pool — Solidly invariant
    if isinstance(pool, AerodromeV2Pool) and getattr(pool, "stable", False):
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals

        # Build swap_fn using Aerodrome's calc_exact_in_stable
        reserves0 = pool.state.reserves_token0
        reserves1 = pool.state.reserves_token1
        decimals0 = 10**pool.token0.decimals
        decimals1 = 10**pool.token1.decimals
        fee = pool.fee
        token_in = 0 if zero_for_one else 1

        def _aerodrome_stable_swap_fn(
            amount_in: int,
            __reserves0: int = reserves0,
            __reserves1: int = reserves1,
            __decimals0: int = decimals0,
            __decimals1: int = decimals1,
            __fee: Fraction = fee,
            __token_in: int = token_in,
        ) -> int:
            return _aerodrome_stable_calc(
                amount_in=amount_in,
                token_in=__token_in,  # type: ignore[arg-type]
                reserves0=__reserves0,
                reserves1=__reserves1,
                decimals0=__decimals0,
                decimals1=__decimals1,
                fee=__fee,
            )

        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
            swap_fn=_aerodrome_stable_swap_fn,
        )

    if isinstance(pool, UniswapV3Pool):
        # V3: virtual reserves from L and sqrt_price_x96
        # Fee is stored as int pip (e.g. 3000 = 0.3%), denominator 1_000_000
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=pool.liquidity,
            sqrt_price_x96=pool.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
        tick_ranges_result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=zero_for_one,
            max_ranges=3,
        )
        if tick_ranges_result is not None:
            tick_ranges, current_range_index = tick_ranges_result
        else:
            tick_ranges = None
            current_range_index = 0

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_fraction,
            liquidity=pool.liquidity,
            sqrt_price=pool.sqrt_price_x96,
            tick_lower=pool.tick,
            tick_upper=pool.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
            zero_for_one=zero_for_one,
        )

    if isinstance(pool, UniswapV4Pool):
        # V4: same structure as V3 (concentrated liquidity)
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=pool.liquidity,
            sqrt_price_x96=pool.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
        tick_ranges_result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=zero_for_one,
            max_ranges=3,
        )
        if tick_ranges_result is not None:
            tick_ranges, current_range_index = tick_ranges_result
        else:
            tick_ranges = None
            current_range_index = 0

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_fraction,
            liquidity=pool.liquidity,
            sqrt_price=pool.sqrt_price_x96,
            tick_lower=pool.tick,
            tick_upper=pool.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
            zero_for_one=zero_for_one,
        )

    # V2 pool (UniswapV2Pool or AerodromeV2Pool volatile) — actual reserves
    if zero_for_one:
        reserve_in = pool.state.reserves_token0
        reserve_out = pool.state.reserves_token1
    else:
        reserve_in = pool.state.reserves_token1
        reserve_out = pool.state.reserves_token0

    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=pool.fee,
    )


def pool_state_to_hop(
    pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool | CamelotLiquidityPool,
    input_token: Erc20Token,
    state_override: Any = None,
) -> HopType:
    """
    Convert a pool object to a Hop, with optional state override.

    Like pool_to_hop() but accepts a PoolState override for the pool's
    current state (used when simulating a different reserve configuration).

    Parameters
    ----------
    pool
        A UniswapV2Pool, AerodromeV2Pool, UniswapV3Pool, or UniswapV4Pool.
    input_token
        The token being deposited into this pool.
    state_override
        Optional PoolState to use instead of pool.state.

    Returns
    -------
    Hop
        A Hop with reserves oriented for the swap direction.
    """
    state = state_override or pool.state
    zero_for_one = input_token == pool.token0

    # Aerodrome stable pool — Solidly invariant
    if isinstance(pool, AerodromeV2Pool) and getattr(pool, "stable", False):
        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals
        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
        )

    if isinstance(pool, UniswapV3Pool | UniswapV4Pool):
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=state.liquidity,
            sqrt_price_x96=state.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
        tick_ranges_result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=zero_for_one,
            max_ranges=3,
        )
        if tick_ranges_result is not None:
            tick_ranges, current_range_index = tick_ranges_result
        else:
            tick_ranges = None
            current_range_index = 0

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_fraction,
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=state.tick,
            tick_upper=state.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
            zero_for_one=zero_for_one,
        )

    # V2 pool (UniswapV2Pool or AerodromeV2Pool volatile)
    if zero_for_one:
        reserve_in = state.reserves_token0
        reserve_out = state.reserves_token1
    else:
        reserve_in = state.reserves_token1
        reserve_out = state.reserves_token0

    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=pool.fee,
    )


def pools_to_solve_input(
    pools: list,
    input_token: Erc20Token,
    max_input: int | None = None,
) -> SolveInput:
    """
    Convert a list of pool objects to a SolveInput.

    Parameters
    ----------
    pools
        Ordered list of pools in the arbitrage path.
    input_token
        The input (profit) token.
    max_input
        Optional maximum input constraint.

    Returns
    -------
    SolveInput
        Solver input with Hop for each pool.
    """
    hops: list[HopType] = []
    current_token = input_token

    for pool in pools:
        hop = pool_to_hop(pool, current_token)
        hops.append(hop)
        # Advance the current token
        current_token = pool.token1 if current_token == pool.token0 else pool.token0

    return SolveInput(hops=tuple(hops), max_input=max_input)
