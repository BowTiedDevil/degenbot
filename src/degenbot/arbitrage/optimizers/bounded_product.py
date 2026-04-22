"""
Bounded Product CFMM optimizer for V3 tick ranges.

Each V3 tick range is a bounded product CFMM with trading function:
    φ(R) = (R₀ + α)(R₁ + β) ≥ k

where:
- α = L / sqrt(P_upper) is the lower bound on R₀
- β = L * sqrt(P_lower) is the lower bound on R₁
- k = L² is the effective constant product

Closed-form optimal arbitrage:
    R₁_opt = L × sqrt(external_price) - β
    R₀_opt = L / sqrt(external_price) - α

This provides O(1) optimization per tick range, similar to V2's Newton method.
"""

import math
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from degenbot.arbitrage.optimizers.base import (
    OptimizerResult,
    OptimizerType,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token


@dataclass(frozen=True)
class BoundedProductCFMM:
    """
    Bounded product CFMM representation of a V3 tick range.

    Attributes
    ----------
    liquidity : int
        Liquidity in the tick range.
    sqrt_price_lower : int
        Lower sqrt price bound (X96 format).
    sqrt_price_upper : int
        Upper sqrt price bound (X96 format).
    tick_lower : int
        Lower tick boundary.
    tick_upper : int
        Upper tick boundary.
    """

    liquidity: int
    sqrt_price_lower: int
    sqrt_price_upper: int
    tick_lower: int
    tick_upper: int

    @property
    def alpha(self) -> float:
        """Lower bound on R0: L / sqrt(P_upper)."""
        return self.liquidity / (self.sqrt_price_upper / (2**96))

    @property
    def beta(self) -> float:
        """Lower bound on R1: L * sqrt(P_lower)."""
        return self.liquidity * (self.sqrt_price_lower / (2**96))

    @property
    def k(self) -> float:
        """Effective constant product: L²."""
        return float(self.liquidity) ** 2

    def find_optimal_reserves(
        self,
        external_price: float,
    ) -> tuple[float, float]:
        """
        Find optimal reserves at given external price.

        Uses closed-form solution from bounded product CFMM theory.

        Parameters
        ----------
        external_price : float
            External market price (token1/token0).

        Returns
        -------
        tuple[float, float]
            (optimal_R0, optimal_R1) reserves.
        """
        sqrt_price_external = math.sqrt(external_price)

        # Optimal reserves from closed-form solution
        # R1 + β = L × sqrt(P_external)
        # R0 + α = L / sqrt(P_external)
        R1_opt = self.liquidity * sqrt_price_external - self.beta
        R0_opt = self.liquidity / sqrt_price_external - self.alpha

        return max(R0_opt, 0.0), max(R1_opt, 0.0)

    def contains_sqrt_price(self, sqrt_price: float) -> bool:
        """Check if sqrt price is within this tick range."""
        sqrt_p_lower = self.sqrt_price_lower / (2**96)
        sqrt_p_upper = self.sqrt_price_upper / (2**96)
        return sqrt_p_lower <= sqrt_price <= sqrt_p_upper


def v3_tick_range_to_bounded_product(
    liquidity: int,
    sqrt_price_lower: int,
    sqrt_price_upper: int,
    tick_lower: int,
    tick_upper: int,
) -> BoundedProductCFMM:
    """
    Convert V3 tick range to bounded product CFMM.

    Parameters
    ----------
    liquidity : int
        Liquidity in the range.
    sqrt_price_lower : int
        Lower sqrt price bound (X96).
    sqrt_price_upper : int
        Upper sqrt price bound (X96).
    tick_lower : int
        Lower tick.
    tick_upper : int
        Upper tick.

    Returns
    -------
    BoundedProductCFMM
        Bounded product representation.
    """
    return BoundedProductCFMM(
        liquidity=liquidity,
        sqrt_price_lower=sqrt_price_lower,
        sqrt_price_upper=sqrt_price_upper,
        tick_lower=tick_lower,
        tick_upper=tick_upper,
    )


class BoundedProductOptimizer:
    """
    Optimizer for V3 pools using bounded product CFMM approach.

    This optimizer finds optimal arbitrage for V3 pools by treating
    each tick range as a bounded product CFMM with closed-form solution.

    Usage:
    -----
    >>> optimizer = BoundedProductOptimizer()
    >>> result = optimizer.solve([v3_pool], input_token)
    """

    def __init__(self) -> None:
        self._last_solve_time_ms = 0.0

    @property
    def optimizer_type(self) -> OptimizerType:
        return OptimizerType.NEWTON  # Reuse for closed-form

    def solve(
        self,
        pools: list[Any],
        input_token: "Erc20Token",
        external_price: float | None = None,
    ) -> OptimizerResult:
        """
        Find optimal V3 arbitrage using bounded product CFMM.

        Parameters
        ----------
        pools : list
            List containing a single V3 pool.
        input_token : Erc20Token
            The input token.
        external_price : float | None
            External price for the forward token. If None, uses pool's
            implied price from the other pool in an arbitrage pair.

        Returns
        -------
        OptimizerResult
            Optimization result.

        Notes
        -----
        This is a simplified implementation. Full V3 arbitrage requires:
        1. Iterating through all initialized tick ranges
        2. Finding the range containing the equilibrium price
        3. Checking if optimal solution crosses tick boundaries
        4. If crossing, checking neighboring ranges

        For now, this implements single-tick-range optimization.
        """
        start_time = time.perf_counter_ns()

        if len(pools) != 1:
            elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
            return OptimizerResult(
                optimal_input=0,
                profit=0,
                solve_time_ms=elapsed_ms,
                iterations=0,
                success=False,
                optimizer_type=self.optimizer_type,
                error_message="Bounded product optimizer requires single V3 pool",
            )

        pool = pools[0]

        if not isinstance(pool, UniswapV3Pool):
            elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
            return OptimizerResult(
                optimal_input=0,
                profit=0,
                solve_time_ms=elapsed_ms,
                iterations=0,
                success=False,
                optimizer_type=self.optimizer_type,
                error_message="Pool must be UniswapV3Pool",
            )

        try:
            # Get current state
            state = pool.state
            liquidity = state.liquidity
            current_tick = state.tick
            sqrt_price_x96 = state.sqrt_price_x96

            # For simplicity, use the current tick range
            # In production, would need to iterate through all ranges
            tick_spacing = pool.tick_spacing
            tick_lower = (current_tick // tick_spacing) * tick_spacing
            tick_upper = tick_lower + tick_spacing

            # Convert ticks to sqrt prices
            sqrt_price_lower = int(math.sqrt(1.0001**tick_lower) * (2**96))
            sqrt_price_upper = int(math.sqrt(1.0001**tick_upper) * (2**96))

            # Create bounded product CFMM
            cfmm = BoundedProductCFMM(
                liquidity=liquidity,
                sqrt_price_lower=sqrt_price_lower,
                sqrt_price_upper=sqrt_price_upper,
                tick_lower=tick_lower,
                tick_upper=tick_upper,
            )

            # If no external price provided, can't optimize
            if external_price is None:
                elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
                return OptimizerResult(
                    optimal_input=0,
                    profit=0,
                    solve_time_ms=elapsed_ms,
                    iterations=0,
                    success=False,
                    optimizer_type=self.optimizer_type,
                    error_message="External price required for bounded product optimization",
                )

            # Find optimal reserves
            R0_opt, R1_opt = cfmm.find_optimal_reserves(external_price)

            # Calculate current reserves
            sqrt_price = sqrt_price_x96 / (2**96)
            R0_current = liquidity / sqrt_price
            R1_current = liquidity * sqrt_price

            # Calculate trade needed
            delta_R0 = R0_opt - R0_current
            delta_R1 = R1_current - R1_opt

            elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
            self._last_solve_time_ms = elapsed_ms

            # Determine optimal input (which token to input)
            if input_token == pool.token0:
                optimal_input = int(abs(delta_R0))
                profit = int(delta_R1) if delta_R0 > 0 else 0
            else:
                optimal_input = int(abs(delta_R1))
                profit = int(delta_R0) if delta_R1 > 0 else 0

            if optimal_input <= 0 or profit <= 0:
                return OptimizerResult(
                    optimal_input=0,
                    profit=0,
                    solve_time_ms=elapsed_ms,
                    iterations=1,
                    success=False,
                    optimizer_type=self.optimizer_type,
                    error_message="No profitable arbitrage",
                )

            return OptimizerResult(
                optimal_input=optimal_input,
                profit=profit,
                solve_time_ms=elapsed_ms,
                iterations=1,  # Closed-form
                success=True,
                optimizer_type=self.optimizer_type,
            )

        except Exception as e:
            elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
            return OptimizerResult(
                optimal_input=0,
                profit=0,
                solve_time_ms=elapsed_ms,
                iterations=0,
                success=False,
                optimizer_type=self.optimizer_type,
                error_message=str(e),
            )
