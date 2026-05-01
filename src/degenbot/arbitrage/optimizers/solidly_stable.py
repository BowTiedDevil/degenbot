"""Solidly stable-pool solver and simulation helpers."""

import math
import time
from typing import override

from degenbot.arbitrage.optimizers._solver_utils import (
    _compute_mobius_coefficients,
    _MobiusCoefficients,
)
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import ConstantProductHop, HopType, PoolInvariant


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
    - CurveStableswapHop: uses swap_fn if available

    For integer-exact evaluation, use ``_simulate_mixed_path_int`` instead.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0

        # Prefer exact callable if available (Solidly, Curve, etc.)
        swap_fn = getattr(hop, "swap_fn", None)
        if swap_fn is not None:
            amount = float(swap_fn(int(amount)))
            continue

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

    For hops with ``swap_fn`` (Solidly, Curve), uses the integer-accurate callable.
    For V2 hops, uses integer constant-product formula.
    Falls back to float for hops without integer support.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0

        # Prefer exact callable if available (Solidly, Curve, etc.)
        swap_fn = getattr(hop, "swap_fn", None)
        if swap_fn is not None:
            amount = swap_fn(amount)
            continue

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
