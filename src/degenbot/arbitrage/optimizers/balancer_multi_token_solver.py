"""Closed-form solver for N-token Balancer weighted pool basket arbitrage."""

import time
from typing import override

from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
)
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import BalancerMultiTokenHop, PoolInvariant


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
