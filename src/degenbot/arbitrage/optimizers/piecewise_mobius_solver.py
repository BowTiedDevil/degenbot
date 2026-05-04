"""Piecewise-Möbius solver for V3 multi-range paths with tick crossings."""

import importlib.util
import math
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any, override

import numpy as np

from degenbot.arbitrage.optimizers._solver_utils import (
    _infer_zero_for_one,
)
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.arbitrage.optimizers.mobius import (
    V3TickRangeHop,
    V3TickRangeSequence,
    mobius_solve,
)
from degenbot.arbitrage.optimizers.mobius import (
    compute_mobius_coefficients as _float_compute_mobius_coefficients,
)
from degenbot.arbitrage.optimizers.mobius import simulate_path as _mobius_simulate_path
from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
from degenbot.arbitrage.optimizers.v3_tick_predictor import estimate_price_impact
from degenbot.degenbot_rs import mobius as _rs_mobius
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import (
    BoundedProductHop,
    ConstantProductHop,
    PoolInvariant,
)
from degenbot.uniswap.v3_libraries.constants import Q96

_NUMPY_AVAILABLE = importlib.util.find_spec("numpy") is not None


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

    def __getstate__(self) -> dict[str, Any]:
        """Omit the non-pickleable Rust optimizer and solver caches."""
        state = self.__dict__.copy()
        state["_rust_optimizer"] = None
        state["_mobius_solver"] = None
        state["_rust_hop_cache"] = {}
        state["_rust_sequence_cache"] = {}
        return state

    def __setstate__(self, state: dict[str, Any]) -> None:
        """Recreate the Rust optimizer after unpickling if Rust is available."""
        self.__dict__.update(state)
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
            rust_hops = [
                _rs_mobius.RustHopState(
                    float(hop.reserve_in),
                    float(hop.reserve_out),
                    float(hop.fee),
                )
                for hop in solve_input.hops
                if isinstance(hop, (ConstantProductHop, BoundedProductHop))
            ]
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
