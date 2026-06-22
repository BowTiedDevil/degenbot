"""Unified solver interface for arbitrage optimization.

All solvers accept the same ``SolveInput`` (a sequence of ``HopType`` objects)
and return the same ``SolveResult``.  The ``ArbSolver`` dispatcher automatically
selects the best method based on the hop types.

This module re-exports the individual solver implementations that now live in
focused submodules, so existing ``from degenbot.arbitrage.solvers.solver import …``
statements continue to work.
"""

from typing import override

# Re-export internal helpers so existing test imports keep working
from degenbot.arbitrage.solvers.balancer_multi_token_solver import (
    BalancerMultiTokenSolver,
)
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.arbitrage.solvers.mobius_solver import MobiusSolver
from degenbot.arbitrage.solvers.newton_solver import NewtonSolver
from degenbot.arbitrage.solvers.piecewise_mobius_solver import (
    PiecewiseMobiusSolver,
)
from degenbot.arbitrage.solvers.solidly_stable import (
    SolidlyStableSolver,
)
from degenbot.exceptions import OptimizationError

# Explicit exports for type checker and IDE completion
__all__ = [
    "ArbSolver",
    "BalancerMultiTokenSolver",
    "BrentSolver",
    "MobiusSolver",
    "NewtonSolver",
    "PiecewiseMobiusSolver",
    "SolidlyStableSolver",
    "SolveInput",
    "SolveResult",
    "SolverMethod",
]


class ArbSolver(Solver):
    """Top-level solver that dispatches to the best method.

    Each sub-solver tries Rust first and falls back to Python internally.
    ArbSolver is a pure dispatcher.

    Dispatch order:
    1. MobiusSolver (V2 + single-range V3, Rust-accelerated)
    2. PiecewiseMobiusSolver (V3 multi-range, Rust-accelerated)
    3. SolidlyStableSolver (Python only)
    4. BalancerMultiTokenSolver (Python only)
    5. BrentSolver (Python only, handles everything)

    Usage:
    -----
    >>> from degenbot.arbitrage.solvers.solver import ArbSolver, SolveInput
    >>> from degenbot.types.hop_types import ConstantProductHop
    >>> solver = ArbSolver()
    >>> result = solver.solve(
    ...     SolveInput(
    ...         hops=(
    ...             ConstantProductHop(
    ...                 reserve_in=2_000_000e6, reserve_out=1_000e18, fee=Fraction(3, 1000)
    ...             ),
    ...             ConstantProductHop(
    ...                 reserve_in=1_500_000e6, reserve_out=800e18, fee=Fraction(3, 1000)
    ...             ),
    ...         )
    ...     )
    ... )
    """

    MIN_HOPS = 2

    def __init__(self) -> None:
        """Initialize the instance.

        Under ADR-003 the legacy ``RustPoolCache`` mirror is retired (Slice 4
        Option D — deleted, not migrated). ``ArbSolver`` now solves purely
        through the Python f64 sub-solvers; the registered-path / cached solve
        surface (``register_pool`` / ``solve_registered`` / ``solve_cached`` /
        ...) is gone. The Rust production path uses ``UniswapArbEngine``
        directly; library consumers use ``ArbSolver.solve(SolveInput)``.
        """
        self._mobius = MobiusSolver()
        self._piecewise = PiecewiseMobiusSolver()
        self._solidly = SolidlyStableSolver()
        self._balancer_multi = BalancerMultiTokenSolver()
        self._brent = BrentSolver()

    # Solver interface
    # ------------------------------------------------------------------

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops >= self.MIN_HOPS

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        """Solve with automatic method selection.

        Dispatches to sub-solvers in order. Each sub-solver tries
        Rust first, then falls back to Python internally.

        Raises OptimizationError if no solver can find a profitable solution.

        Returns:
            The computed value.

        Raises:
            OptimizationError: If the operation fails.

        """
        for solver in (
            self._mobius,
            self._piecewise,
            self._solidly,
            self._balancer_multi,
            self._brent,
        ):
            if not solver.supports(solve_input):
                continue
            try:
                return solver.solve(solve_input)
            except (OptimizationError, OverflowError):
                continue

        raise OptimizationError(
            message="No solver found a profitable solution",
            iterations=0,
            method="ArbSolver",
        )
