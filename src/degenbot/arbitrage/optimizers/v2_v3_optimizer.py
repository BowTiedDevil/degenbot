"""
V2-V3 Arbitrage Optimizer with Tick Range Prediction.

This optimizer efficiently handles V2-V3 arbitrage by:
1. Estimating equilibrium price from both pools
2. Filtering impossible tick ranges by price bounds
3. Checking top candidate ranges
4. Validating solutions stay in predicted range

Key insight: The equilibrium price can be estimated independently of tick ranges,
allowing us to predict which V3 tick range will be active after arbitrage.
"""

import math
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from degenbot.arbitrage.optimizers.base import (
    OptimizerResult,
    OptimizerType,
)
from degenbot.arbitrage.optimizers.v3_tick_predictor import (
    BoundedProductCFMM,
    TickRange,
    V3PoolState,
    estimate_price_impact,
    sqrt_price_to_tick,
    tick_range_to_bounded_product,
    tick_to_sqrt_price,
)

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


# =============================================================================
# DATA STRUCTURES
# =============================================================================


@dataclass
class V2PoolState:
    """V2 pool state for optimization."""

    reserve0: float
    reserve1: float
    fee: float
    token0_address: str
    token1_address: str

    @property
    def price(self) -> float:
        """Price of token1 in terms of token0."""
        return self.reserve1 / self.reserve0 if self.reserve0 > 0 else 0.0


@dataclass
class CandidateSolution:
    """A candidate solution for a specific tick range."""

    tick_range: TickRange
    optimal_input: float
    optimal_output: float
    profit: float
    final_sqrt_price: float
    stays_in_range: bool
    valid: bool


@dataclass
class V2V3OptimizationResult:
    """Result from V2-V3 optimization."""

    success: bool
    optimal_input: float
    optimal_output: float
    profit: float
    v2_pool_state: V2PoolState
    v3_pool_state: V3PoolState
    predicted_tick_range: TickRange | None
    candidate_solutions: list[CandidateSolution]
    equilibrium_estimate: float
    solve_time_ms: float
    error_message: str | None = None


# =============================================================================
# EQUILIBRIUM PRICE ESTIMATION
# =============================================================================


def estimate_equilibrium_price(
    v2_state: V2PoolState,
    v3_state: V3PoolState,
) -> float:
    """
    Estimate equilibrium price after arbitrage.

    At equilibrium, both pools have the same effective marginal rate.
    For V2: MR = γ * R1 * R0 / (R0 + x)²
    For V3: MR = γ * L² / (R0 + x)²

    Simplifying: equilibrium occurs where prices are equal (adjusted for fees).

    Parameters
    ----------
    v2_state : V2PoolState
        V2 pool state.
    v3_state : V3PoolState
        V3 pool state.

    Returns
    -------
    float
        Estimated equilibrium price (token1/token0).
    """
    v2_price = v2_state.price
    v3_price = v3_state.virtual_reserve1 / v3_state.virtual_reserve0 if v3_state.virtual_reserve0 > 0 else 0.0

    if v2_price <= 0 or v3_price <= 0:
        return v2_price if v2_price > 0 else v3_price

    # Geometric mean as equilibrium estimate
    # This is where the marginal rates would be equal if no fees
    p_eq = math.sqrt(v2_price * v3_price)

    # Adjust for fees
    # The equilibrium will be pulled toward the pool with lower fees
    avg_fee = (v2_state.fee + v3_state.fee) / 2
    fee_adjustment = (1 - avg_fee) ** 0.5

    return p_eq * fee_adjustment


def estimate_equilibrium_sqrt_price(
    v2_state: V2PoolState,
    v3_state: V3PoolState,
) -> float:
    """
    Estimate equilibrium sqrt price.

    Parameters
    ----------
    v2_state : V2PoolState
        V2 pool state.
    v3_state : V3PoolState
        V3 pool state.

    Returns
    -------
    float
        Estimated equilibrium sqrt price.
    """
    p_eq = estimate_equilibrium_price(v2_state, v3_state)
    return math.sqrt(p_eq)


def compute_price_bounds(
    v2_state: V2PoolState,
    v3_state: V3PoolState,
) -> tuple[float, float]:
    """
    Compute bounds on possible equilibrium prices.

    After arbitrage, prices must satisfy:
    |P_v2_final - P_v3_final| <= fees

    This gives us bounds on which tick ranges could be optimal.

    Parameters
    ----------
    v2_state : V2PoolState
        V2 pool state.
    v3_state : V3PoolState
        V3 pool state.

    Returns
    -------
    tuple[float, float]
        (price_lower, price_upper) bounds.
    """
    v2_price = v2_state.price
    v3_price = v3_state.virtual_reserve1 / v3_state.virtual_reserve0

    # Use the pool with better price as reference
    reference_price = v2_price if v2_price > 0 else v3_price

    # Fee bounds
    total_fee = v2_state.fee + v3_state.fee

    # Price must be within fee-adjusted bounds of both pools
    # After arbitrage: |P_v2 - P_v3| < total_fee
    # So final price is in intersection of fee-adjusted intervals

    p_lower = reference_price * (1 - total_fee)
    p_upper = reference_price / (1 - total_fee)

    # Also bound by actual pool prices
    p_lower = max(p_lower, min(v2_price, v3_price) * (1 - total_fee))
    p_upper = min(p_upper, max(v2_price, v3_price) / (1 - total_fee))

    return max(p_lower, 0.0), p_upper


# =============================================================================
# TICK RANGE FILTERING
# =============================================================================


def filter_tick_ranges_by_price_bounds(
    tick_ranges: list[TickRange],
    price_lower: float,
    price_upper: float,
) -> list[TickRange]:
    """
    Filter tick ranges that could contain equilibrium.

    A range is possible if its price interval overlaps with [price_lower, price_upper].

    Parameters
    ----------
    tick_ranges : list[TickRange]
        All initialized tick ranges.
    price_lower : float
        Lower price bound.
    price_upper : float
        Upper price bound.

    Returns
    -------
    list[TickRange]
        Filtered candidate ranges.
    """
    if not tick_ranges:
        return []

    candidates = []
    for tick_range in tick_ranges:
        # Convert tick bounds to prices
        range_price_lower = tick_range.sqrt_price_lower**2
        range_price_upper = tick_range.sqrt_price_upper**2

        # Check for overlap
        if range_price_upper >= price_lower and range_price_lower <= price_upper:
            candidates.append(tick_range)

    return candidates


def sort_ranges_by_equilibrium_distance(
    tick_ranges: list[TickRange],
    equilibrium_sqrt_price: float,
) -> list[TickRange]:
    """
    Sort tick ranges by distance to equilibrium price.

    Parameters
    ----------
    tick_ranges : list[TickRange]
        Candidate tick ranges.
    equilibrium_sqrt_price : float
        Estimated equilibrium sqrt price.

    Returns
    -------
    list[TickRange]
        Sorted ranges (closest first).
    """
    if not tick_ranges:
        return []

    def distance_to_range(tick_range: TickRange) -> float:
        """Compute distance from sqrt price to range center."""
        if tick_range.sqrt_price_lower <= equilibrium_sqrt_price <= tick_range.sqrt_price_upper:
            return 0.0  # Inside range
        elif equilibrium_sqrt_price < tick_range.sqrt_price_lower:
            return tick_range.sqrt_price_lower - equilibrium_sqrt_price
        else:
            return equilibrium_sqrt_price - tick_range.sqrt_price_upper

    return sorted(tick_ranges, key=distance_to_range)


# =============================================================================
# V2-V3 ARBITRAGE SOLVER (SINGLE RANGE)
# =============================================================================


def solve_v2_v3_single_range(
    v2_state: V2PoolState,
    v3_cfmm: BoundedProductCFMM,
    v3_current_sqrt_price: float,
    zero_for_one: bool = True,
    max_iterations: int = 50,
    tolerance: float = 1e-9,
) -> tuple[float, float, float]:
    """
    Solve V2-V3 arbitrage assuming V3 stays in one tick range.

    Uses Newton's method with bounded product CFMM representation.

    Parameters
    ----------
    v2_state : V2PoolState
        V2 pool state.
    v3_cfmm : BoundedProductCFMM
        V3 tick range as bounded product CFMM.
    v3_current_sqrt_price : float
        Current V3 sqrt price.
    zero_for_one : bool
        True if swapping token0 for token1 in V3.
    max_iterations : int
        Maximum iterations.
    tolerance : float
        Convergence tolerance.

    Returns
    -------
    tuple[float, float, float]
        (optimal_input, optimal_output, profit)
    """
    # Use V2's Newton-like approach but with V3's bounded product CFMM
    gamma_v2 = 1.0 - v2_state.fee
    gamma_v3 = 1.0 - v2_state.fee  # Assume same fee for simplicity

    R0_v2 = v2_state.reserve0
    R1_v2 = v2_state.reserve1
    L = v3_cfmm.liquidity

    # Initial guess
    x = R0_v2 * 0.01

    best_x = x
    best_profit = 0.0

    for _ in range(max_iterations):
        if x <= 0:
            break

        # Simulate V2 swap: token0 → token1
        y_v2 = x * gamma_v2 * R1_v2 / (R0_v2 + x * gamma_v2)

        # Simulate V3 swap: token1 → token0 (reverse direction)
        # Or V3 swap: token0 → token1 (same direction)
        # For arbitrage: we're equalizing prices

        # Simplified: assume we buy low on one pool, sell high on other
        # Direction depends on which pool has better price

        v2_price = R1_v2 / R0_v2
        v3_price = v3_current_sqrt_price**2

        if v2_price > v3_price:
            # V2 has higher price for token1
            # Buy token1 from V3, sell to V2
            # Input token0 to V3, receive token1, sell token1 to V2

            # V3: token0 → token1
            y_v3 = x * gamma_v3 * L**2 / (L / v3_current_sqrt_price + x * gamma_v3)

            # Profit: sell y_v3 to V2
            z_v2 = y_v3 * gamma_v2 * R0_v2 / (R1_v2 + y_v3 * gamma_v2)
            profit = z_v2 - x
        else:
            # V3 has higher price for token1
            # Buy token1 from V2, sell to V3
            # Input token0 to V2, receive token1, sell token1 to V3

            # V2: token0 → token1
            y_v2 = x * gamma_v2 * R1_v2 / (R0_v2 + x * gamma_v2)

            # V3: token1 → token0
            z_v3 = y_v2 * gamma_v3 * L**2 / (L * v3_current_sqrt_price + y_v2 * gamma_v3)
            profit = z_v3 - x

        if profit > best_profit:
            best_profit = profit
            best_x = x

        # Simple gradient estimation (finite difference)
        eps = x * 1e-6
        if eps < 1.0:
            eps = 1.0

        # Recompute with x + eps
        x_plus = x + eps
        if v2_price > v3_price:
            y_v3_plus = x_plus * gamma_v3 * L**2 / (L / v3_current_sqrt_price + x_plus * gamma_v3)
            z_v2_plus = y_v3_plus * gamma_v2 * R0_v2 / (R1_v2 + y_v3_plus * gamma_v2)
            profit_plus = z_v2_plus - x_plus
        else:
            y_v2_plus = x_plus * gamma_v2 * R1_v2 / (R0_v2 + x_plus * gamma_v2)
            z_v3_plus = y_v2_plus * gamma_v3 * L**2 / (L * v3_current_sqrt_price + y_v2_plus * gamma_v3)
            profit_plus = z_v3_plus - x_plus

        gradient = (profit_plus - profit) / eps

        if abs(gradient) < tolerance:
            break

        # Second derivative for Newton step
        x_minus = max(1.0, x - eps)
        if v2_price > v3_price:
            y_v3_minus = x_minus * gamma_v3 * L**2 / (L / v3_current_sqrt_price + x_minus * gamma_v3)
            z_v2_minus = y_v3_minus * gamma_v2 * R0_v2 / (R1_v2 + y_v3_minus * gamma_v2)
            profit_minus = z_v2_minus - x_minus
        else:
            y_v2_minus = x_minus * gamma_v2 * R1_v2 / (R0_v2 + x_minus * gamma_v2)
            z_v3_minus = y_v2_minus * gamma_v3 * L**2 / (L * v3_current_sqrt_price + y_v2_minus * gamma_v3)
            profit_minus = z_v3_minus - x_minus

        gradient_minus = (profit - profit_minus) / eps
        hessian = (gradient - gradient_minus) / eps

        if abs(hessian) < 1e-30:
            break

        # Newton step
        step = gradient / hessian
        x_new = x - step

        # Bounds
        x = max(1.0, min(x_new, R0_v2 * 0.5))

    return best_x, best_profit, best_profit


def validate_solution_in_range(
    optimal_input: float,
    v2_state: V2PoolState,
    v3_cfmm: BoundedProductCFMM,
    v3_current_sqrt_price: float,
    tick_range: TickRange,
) -> tuple[bool, float]:
    """
    Validate that solution stays within tick range.

    Parameters
    ----------
    optimal_input : float
        Optimal input amount.
    v2_state : V2PoolState
        V2 pool state.
    v3_cfmm : BoundedProductCFMM
        V3 bounded product CFMM.
    v3_current_sqrt_price : float
        Current V3 sqrt price.
    tick_range : TickRange
        The tick range we're checking.

    Returns
    -------
    tuple[bool, float]
        (is_valid, final_sqrt_price)
    """
    if optimal_input <= 0:
        return True, v3_current_sqrt_price

    # Estimate final sqrt price after swap
    # This is approximate - actual would require full V3 swap simulation
    final_sqrt_price = estimate_price_impact(
        amount_in=optimal_input,
        liquidity=v3_cfmm.liquidity,
        current_sqrt_price=v3_current_sqrt_price,
        fee=v2_state.fee,  # Use V2 fee as approximation
        zero_for_one=True,  # Simplified
    )

    # Check if within range
    is_valid = tick_range.sqrt_price_lower <= final_sqrt_price <= tick_range.sqrt_price_upper

    return is_valid, final_sqrt_price


# =============================================================================
# MAIN OPTIMIZER
# =============================================================================


class V2V3Optimizer:
    """
    V2-V3 Arbitrage optimizer with tick range prediction.

    This optimizer efficiently handles V2-V3 arbitrage by:
    1. Estimating equilibrium price
    2. Filtering impossible tick ranges
    3. Checking top candidates
    4. Validating solutions

    Usage:
    -----
    >>> optimizer = V2V3Optimizer()
    >>> result = optimizer.optimize(v2_pool, v3_pool, input_token)
    """

    def __init__(
        self,
        max_candidates: int = 3,
        max_iterations: int = 50,
        tolerance: float = 1e-9,
    ):
        """
        Parameters
        ----------
        max_candidates : int
            Maximum candidate tick ranges to check.
        max_iterations : int
            Maximum iterations for optimization.
        tolerance : float
            Convergence tolerance.
        """
        self.max_candidates = max_candidates
        self.max_iterations = max_iterations
        self.tolerance = tolerance
        self._last_solve_time_ms = 0.0

    @property
    def optimizer_type(self) -> OptimizerType:
        return OptimizerType.NEWTON

    def optimize(
        self,
        v2_pool: "UniswapV2Pool",
        v3_pool: "UniswapV3Pool",
        input_token: "Erc20Token",
        tick_ranges: list[TickRange] | None = None,
    ) -> V2V3OptimizationResult:
        """
        Optimize V2-V3 arbitrage with tick range prediction.

        Parameters
        ----------
        v2_pool : UniswapV2Pool
            The V2 pool.
        v3_pool : UniswapV3Pool
            The V3 pool.
        input_token : Erc20Token
            The input token.
        tick_ranges : list[TickRange] | None
            V3 tick ranges. If None, uses current range only.

        Returns
        -------
        V2V3OptimizationResult
            Optimization result.
        """
        start_time = time.perf_counter_ns()

        # Extract pool states
        v2_state = V2PoolState(
            reserve0=float(v2_pool.state.reserves_token0),
            reserve1=float(v2_pool.state.reserves_token1),
            fee=float(v2_pool.fee),
            token0_address=v2_pool.token0.address,
            token1_address=v2_pool.token1.address,
        )

        from degenbot.arbitrage.optimizers.v3_tick_predictor import extract_v3_pool_state

        v3_state = extract_v3_pool_state(v3_pool)

        # Get tick ranges
        if tick_ranges is None:
            # Use current range only
            current_tick = v3_state.tick
            tick_spacing = v3_pool.tick_spacing
            tick_lower = (current_tick // tick_spacing) * tick_spacing
            tick_upper = tick_lower + tick_spacing

            current_range = TickRange(
                tick_lower=tick_lower,
                tick_upper=tick_upper,
                liquidity=v3_state.liquidity,
                sqrt_price_lower=tick_to_sqrt_price(tick_lower),
                sqrt_price_upper=tick_to_sqrt_price(tick_upper),
            )
            tick_ranges = [current_range]

        # Step 1: Estimate equilibrium
        p_eq = estimate_equilibrium_price(v2_state, v3_state)
        sqrt_p_eq = math.sqrt(p_eq)

        # Step 2: Compute price bounds
        p_lower, p_upper = compute_price_bounds(v2_state, v3_state)

        # Step 3: Filter candidates
        candidates = filter_tick_ranges_by_price_bounds(tick_ranges, p_lower, p_upper)

        if not candidates:
            # No valid candidates
            candidates = tick_ranges[:1] if tick_ranges else []

        # Step 4: Sort by distance to equilibrium
        candidates = sort_ranges_by_equilibrium_distance(candidates, sqrt_p_eq)

        # Step 5: Check top candidates
        candidate_solutions = []
        best_solution = None

        for tick_range in candidates[: self.max_candidates]:
            # Convert to bounded product CFMM
            cfmm = tick_range_to_bounded_product(
                tick_range.tick_lower,
                tick_range.tick_upper,
                tick_range.liquidity,
            )

            # Solve assuming this range
            optimal_input, optimal_output, profit = solve_v2_v3_single_range(
                v2_state=v2_state,
                v3_cfmm=cfmm,
                v3_current_sqrt_price=v3_state.sqrt_price,
                max_iterations=self.max_iterations,
                tolerance=self.tolerance,
            )

            # Validate
            is_valid, final_sqrt_price = validate_solution_in_range(
                optimal_input=optimal_input,
                v2_state=v2_state,
                v3_cfmm=cfmm,
                v3_current_sqrt_price=v3_state.sqrt_price,
                tick_range=tick_range,
            )

            solution = CandidateSolution(
                tick_range=tick_range,
                optimal_input=optimal_input,
                optimal_output=optimal_output,
                profit=profit,
                final_sqrt_price=final_sqrt_price,
                stays_in_range=is_valid,
                valid=is_valid and profit > 0,
            )
            candidate_solutions.append(solution)

            if solution.valid and (best_solution is None or profit > best_solution.profit):
                best_solution = solution

        elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
        self._last_solve_time_ms = elapsed_ms

        if best_solution is None:
            return V2V3OptimizationResult(
                success=False,
                optimal_input=0,
                optimal_output=0,
                profit=0,
                v2_pool_state=v2_state,
                v3_pool_state=v3_state,
                predicted_tick_range=None,
                candidate_solutions=candidate_solutions,
                equilibrium_estimate=p_eq,
                solve_time_ms=elapsed_ms,
                error_message="No profitable arbitrage found",
            )

        return V2V3OptimizationResult(
            success=True,
            optimal_input=best_solution.optimal_input,
            optimal_output=best_solution.optimal_output,
            profit=best_solution.profit,
            v2_pool_state=v2_state,
            v3_pool_state=v3_state,
            predicted_tick_range=best_solution.tick_range,
            candidate_solutions=candidate_solutions,
            equilibrium_estimate=p_eq,
            solve_time_ms=elapsed_ms,
        )


# =============================================================================
# CONVENIENCE FUNCTIONS
# =============================================================================


def optimize_v2_v3_arbitrage(
    v2_pool: "UniswapV2Pool",
    v3_pool: "UniswapV3Pool",
    input_token: "Erc20Token",
    tick_ranges: list[TickRange] | None = None,
) -> V2V3OptimizationResult:
    """
    Optimize V2-V3 arbitrage with tick range prediction.

    This is a convenience function that creates a V2V3Optimizer
    and solves the arbitrage.

    Parameters
    ----------
    v2_pool : UniswapV2Pool
        The V2 pool.
    v3_pool : UniswapV3Pool
        The V3 pool.
    input_token : Erc20Token
        The input token.
    tick_ranges : list[TickRange] | None
        V3 tick ranges. If None, uses current range only.

    Returns
    -------
    V2V3OptimizationResult
        Optimization result.

    Example
    -------
    >>> from degenbot.arbitrage.optimizers import optimize_v2_v3_arbitrage
    >>> result = optimize_v2_v3_arbitrage(v2_pool, v3_pool, usdc)
    >>> if result.success:
    ...     print(f"Optimal: {result.optimal_input}, Profit: {result.profit}")
    ...     print(f"Predicted tick range: {result.predicted_tick_range.tick_lower}")
    """
    optimizer = V2V3Optimizer()
    return optimizer.optimize(v2_pool, v3_pool, input_token, tick_ranges)
