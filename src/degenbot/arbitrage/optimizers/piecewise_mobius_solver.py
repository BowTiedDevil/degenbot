"""Piecewise-Möbius solver for V3 multi-range paths with tick crossings."""

from __future__ import annotations

import math
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import TYPE_CHECKING, override

if TYPE_CHECKING:
    from collections.abc import Callable

import numpy as np

from degenbot.arbitrage.optimizers._mobius_math import (
    MobiusFloatHop,
    V3TickRangeHop,
    V3TickRangeSequence,
    mobius_solve,
)
from degenbot.arbitrage.optimizers._mobius_math import (
    compute_mobius_coefficients as _float_compute_mobius_coefficients,
)
from degenbot.arbitrage.optimizers._mobius_math import simulate_path as _mobius_simulate_path
from degenbot.arbitrage.optimizers._solver_utils import _infer_zero_for_one
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
from degenbot.exceptions import OptimizationError
from degenbot.logging import logger
from degenbot.types.hop_types import (
    BoundedProductHop,
    ConstantProductHop,
    PoolInvariant,
)
from degenbot.uniswap.v3_libraries.constants import Q96

# ---------------------------------------------------------------------------
# V3 sqrt-price estimation (pure math, no I/O)
# ---------------------------------------------------------------------------

_SEQUENTIAL_CANDIDATE_THRESHOLD = 2


def _estimate_sqrt_price_after_swap(
    amount_in: float,
    liquidity: float,
    current_sqrt_price: float,
    fee: float = 0.003,
    zero_for_one: bool = True,  # noqa: FBT001, FBT002
) -> float:
    """Estimate the sqrt price after swapping *amount_in* within a single V3 tick range.

    Uses the V3 swap approximation:
    - zfo (token0→token1): new_sqrt_p = L / (L / sqrt_p + amount_in / gamma)
    - ofo (token1→token0): new_sqrt_p = sqrt_p + amount_in * gamma / L

    Returns *current_sqrt_price* unchanged when *amount_in* ≤ 0 or *liquidity* ≤ 0.

    Returns:
        The computed value.

    """
    if amount_in <= 0 or liquidity <= 0:
        return current_sqrt_price

    gamma = 1.0 - fee
    if zero_for_one:
        return liquidity / (liquidity / current_sqrt_price + amount_in / gamma)
    return current_sqrt_price + amount_in * gamma / liquidity


class PiecewiseMobiusSolver(Solver):
    """Piecewise-Möbius solver for V3 paths with tick crossings.

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
        """Initialize the instance."""
        self._mobius_solver: MobiusSolver | None = None

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
        return solve_input.has_bounded_product

    @staticmethod
    def _has_multi_range(solve_input: SolveInput) -> bool:
        """Check if any V3 hop has multi-range data for tick crossing.

        Returns:
            The computed value.

        """
        for hop in solve_input.hops:
            if (
                hop.invariant == PoolInvariant.BOUNDED_PRODUCT
                and isinstance(hop, BoundedProductHop)
                and hop.has_multi_range
            ):
                return True
        return False

    @staticmethod
    def _find_v3_hop_index(solve_input: SolveInput) -> tuple[int, BoundedProductHop] | None:
        """Find the first V3 hop with multi-range data.

        Returns:
            The computed value.

        """
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

        # Find V3 hop (combines supports, has_multi_range, find_v3_hop_index)
        v3_result = self._find_v3_hop_index(solve_input)

        if v3_result is None:
            # No multi-range V3 found - try single-range fallback
            return self._try_single_range_fallback(solve_input)

        # Python piecewise-Möbius implementation (Rust f64 fast path removed)
        return self._solve_multi_range(solve_input, start_ns)

    def _try_single_range_fallback(self, solve_input: SolveInput) -> SolveResult:
        """Try MobiusSolver for single-range V3.

        Returns:
            The computed value.

        Raises:
            OptimizationError: If the operation fails.

        """
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
        """Solve using Python piecewise-Möbius for tick crossings.

        Returns:
            The computed value.

        Raises:
            OptimizationError: If the operation fails.

        """
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
        """Python-only multi-range solver (fallback when Rust fails).

        Returns:
            The computed value.

        Raises:
            OptimizationError: If the operation fails.

        """
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
                solve_input=solve_input,
                v3_hop_index=v3_hop_index,
                v3_hop=v3_hop,
                current_idx=current_idx,
                candidates=plausible_candidates,
            )
        else:
            # Sequential for single candidate (avoids thread overhead)
            results = [
                self._try_candidate_range(
                    solve_input=solve_input,
                    v3_hop_index=v3_hop_index,
                    v3_hop=v3_hop,
                    end_idx=plausible_candidates[0],
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
        current_idx: int,  # noqa: ARG002
        candidates: list[int],
    ) -> list[SolveResult]:
        """Evaluate multiple candidate ranges.

        Uses sequential evaluation for 1-2 candidates (avoids thread overhead).
        Only uses threads for 3+ candidates where parallelism pays off.

        Returns:
            The computed value.

        """
        results: list[SolveResult] = []

        # Sequential evaluation for small number of candidates
        # Thread overhead exceeds benefit for 1-2 candidates
        if len(candidates) <= _SEQUENTIAL_CANDIDATE_THRESHOLD:
            for end_idx in candidates:
                try:
                    result = self._try_candidate_range(
                        solve_input=solve_input,
                        v3_hop_index=v3_hop_index,
                        v3_hop=v3_hop,
                        end_idx=end_idx,
                    )
                    results.append(result)
                except (OptimizationError, ArithmeticError, ValueError):
                    logger.debug("Candidate range evaluation failed", exc_info=True)
            return results

        # Parallel evaluation only for 3+ candidates
        with ThreadPoolExecutor(max_workers=min(len(candidates), 3)) as executor:
            future_to_idx = {
                executor.submit(
                    self._try_candidate_range,
                    solve_input=solve_input,
                    v3_hop_index=v3_hop_index,
                    v3_hop=v3_hop,
                    end_idx=end_idx,
                ): end_idx
                for end_idx in candidates
            }

            for future in as_completed(future_to_idx):
                try:
                    result = future.result()
                    results.append(result)
                except (OptimizationError, ArithmeticError, ValueError):
                    logger.debug("Parallel evaluation failed", exc_info=True)

        return results

    @staticmethod
    def _is_candidate_plausible(
        solve_input: SolveInput,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
        current_best_profit: int,
    ) -> bool:
        """Cheap check to filter out candidates that can't be profitable.

        Returns False if this candidate can be skipped (saves expensive evaluation).

        Returns:
            The computed value.

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

        # Price impact pruning: quick estimate to check if swap stays in ending range.
        # Uses the V3 swap formula: new_sqrt_p ≈ L / (L / sqrt_p + amount * gamma) for zfo,
        # or sqrt_p + amount * gamma / L for ofo.
        if end_idx > start_idx:
            estimated_sqrt_price = _estimate_sqrt_price_after_swap(
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
        end_idx: int,
    ) -> SolveResult:
        """Try a candidate ending range using proper V3 tick crossing math.

        Uses the exact V3 swap formulas from mobius.py:
        - Computes TickRangeCrossing with proper fee handling
        - Pre-computes Möbius coefficients for before/after hops
        - Uses golden section search for piecewise profit maximization

        Falls back to Rust implementation when available for ~5x speedup.

        Returns:
            The computed value.

        Raises:
            OptimizationError: If the operation fails.

        """
        assert v3_hop.tick_ranges is not None

        # Try Rust implementation first for speed
        # (handled by _try_rust_multi_range at the solve() level)
        # Fall through to Python implementation

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
            """Evaluate profit for input x using piecewise V3 swap.

            Returns:
                The computed value.

            """
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
            final_sqrt_p = _estimate_sqrt_price_after_swap(
                amount_in=remaining,
                liquidity=crossing.ending_range.liquidity,
                current_sqrt_price=crossing.ending_range.sqrt_price_current,
                fee=crossing.ending_range.fee,
                zero_for_one=crossing.ending_range.zero_for_one,
            )
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
        vectorized_result = self._vectorized_bracket_search(
            x_low=x_low,
            x_high=x_high,
            eval_profit_scalar=eval_profit,
        )
        if vectorized_result is not None:
            return vectorized_result

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

    @staticmethod
    def _vectorized_bracket_search(
        *,
        x_low: float,
        x_high: float,
        eval_profit_scalar: Callable[[float], float],
    ) -> SolveResult | None:
        """Vectorized bracket search using NumPy for parallel evaluation.

        Evaluates profit at multiple points simultaneously to quickly
        narrow down the optimal region before golden section refinement.

        Returns SolveResult if successful, None to fall back to scalar search.

        Returns:
            The computed value.

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
        best_idx = int(np.argmax(profits))
        best_profit = profits[best_idx]

        if best_profit <= 0:
            return None  # No profitable solution in bracket

        # Refine around best point with smaller bracket
        # Reduced from 10 to 5 points
        idx_low = max(0, best_idx - 1)
        idx_high = min(n_points - 1, best_idx + 1)
        x_refined = np.linspace(x_points[idx_low], x_points[idx_high], 5)
        profits_refined = np.array([eval_profit_scalar(x) for x in x_refined])

        best_idx_refined = int(np.argmax(profits_refined))
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


# ---------------------------------------------------------------------------
# Solidly Stable Solver (Newton's method for x³y + xy³ ≥ k)
# ---------------------------------------------------------------------------
