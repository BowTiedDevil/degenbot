"""Delegating shell for N-token Balancer weighted pool basket arbitrage.

This solver is a **thin driver** over the Rust core
(``degenbot.arbitrage.solve_balancer_weighted_basket``, re-exported from
``degenbot._ffi`` per ADR-013) — no math lives here. The closed-form
(QuantAMM Equation 9, Willetts & Harrington 2024) is owned by
``degenbot-bot::solvers::balancer_weighted_basket`` and exposed via the PyO3
wrapper in ``rust/crates/degenbot-python/src/c_api.rs`` (ADR-005 three-layer
architecture).

The shell does only: ``Solver`` ABC dispatch → arg extraction from
``BalancerMultiTokenHop`` → GIL-released Rust call → ``SolveResult`` shaping.
"""

import time
from typing import override

from degenbot.arbitrage import solve_balancer_weighted_basket
from degenbot.arbitrage.solvers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import BalancerMultiTokenHop, PoolInvariant


class BalancerMultiTokenSolver(Solver):
    """Delegating shell for N-token Balancer weighted pool basket arbitrage.

    All solve math is owned by the Rust core
    (``degenbot-bot::solvers::balancer_weighted_basket::solve_balancer_weighted``,
    QuantAMM Equation 9). This class marshals a ``BalancerMultiTokenHop``
    into the ``degenbot.arbitrage.solve_balancer_weighted_basket`` call and
    shapes the integer-trade tuple into a ``SolveResult``.

    Unlike pairwise solvers, this finds optimal basket trades where
    multiple tokens can be deposited/withdrawn simultaneously.

    Usage:
    -----
    >>> from fractions import Fraction
    >>> from degenbot.arbitrage.solvers import (
    ...     BalancerMultiTokenSolver,
    ...     SolveInput,
    ... )
    >>> from degenbot.types.hop_types import BalancerMultiTokenHop
    >>> hop = BalancerMultiTokenHop(
    ...     reserves=(
    ...         100_000_000_000_000_000_000,
    ...         2_000_000_000_000,
    ...         1_000_000_000_000,
    ...     ),  # WETH, USDC, DAI in wei
    ...     weights=(
    ...         500_000_000_000_000_000,
    ...         250_000_000_000_000_000,
    ...         250_000_000_000_000_000,
    ...     ),  # 50%, 25%, 25%
    ...     fee=Fraction(3, 1000),
    ...     market_prices=(2000.0, 1.0, 1.0),  # In USD
    ... )
    >>> solver = BalancerMultiTokenSolver()
    >>> result = solver.solve(SolveInput(hops=(hop,)))
    """

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

        max_input: float | None = None
        if solve_input.max_input is not None:
            max_input = float(solve_input.max_input)

        # Delegating shell (ADR-005): extract hop args → Rust core call.
        # Reserves/weights may arrive as floats (e.g. 100e18 in doctest
        # fixtures) — coerce to int for the u128/u64 PyO3 boundary.
        trades, profit, success, _signature, iterations = solve_balancer_weighted_basket(
            reserves=[int(r) for r in hop.reserves],
            weights=[int(w) for w in hop.weights],
            fee_numer=hop.fee.numerator,
            fee_denom=hop.fee.denominator,
            decimals=list(hop.decimals),
            market_prices=list(hop.market_prices),
            max_input=max_input,
        )

        elapsed_ns = time.perf_counter_ns() - start_ns

        if not success:
            raise OptimizationError(
                message="No profitable basket trade found",
                iterations=iterations,
                method=SolverMethod.BALANCER_MULTI_TOKEN.name,
            )

        # For basket trades, "optimal_input" is the total deposit value
        # and "profit" is the total withdrawal value minus deposits
        total_deposit = sum(max(0, t) * hop.market_prices[i] for i, t in enumerate(trades))

        return SolveResult(
            optimal_input=int(total_deposit),
            profit=int(profit),
            iterations=iterations,
            method=SolverMethod.BALANCER_MULTI_TOKEN,
            solve_time_ns=elapsed_ns,
        )
