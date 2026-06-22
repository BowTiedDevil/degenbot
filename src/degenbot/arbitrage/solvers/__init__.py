"""Arbitrage solvers for different pool types.

Usage
-----
>>> from degenbot.arbitrage.solvers.solver import ArbSolver
>>> solver = ArbSolver()
>>> result = solver.solve(SolveInput(hops=(...)))
"""

from degenbot.arbitrage.solvers.balancer_multi_token_solver import BalancerMultiTokenSolver
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput, SolveResult, SolverMethod
from degenbot.arbitrage.solvers.mobius_solver import MobiusSolver
from degenbot.arbitrage.solvers.newton_solver import NewtonSolver
from degenbot.arbitrage.solvers.piecewise_mobius_solver import PiecewiseMobiusSolver
from degenbot.arbitrage.solvers.solidly_stable import SolidlyStableSolver
from degenbot.arbitrage.solvers.solver import ArbSolver

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
