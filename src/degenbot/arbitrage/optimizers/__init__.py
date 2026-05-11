"""
Arbitrage optimizers for different pool types.

Usage
-----
>>> from degenbot.arbitrage.optimizers.solver import ArbSolver
>>> solver = ArbSolver()
>>> result = solver.solve(SolveInput(hops=(...)))
"""

from degenbot.arbitrage.optimizers.balancer_multi_token_solver import BalancerMultiTokenSolver
from degenbot.arbitrage.optimizers.brent_solver import BrentSolver
from degenbot.arbitrage.optimizers.hop_types import SolveInput, SolveResult, SolverMethod
from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
from degenbot.arbitrage.optimizers.newton_solver import NewtonSolver
from degenbot.arbitrage.optimizers.piecewise_mobius_solver import PiecewiseMobiusSolver
from degenbot.arbitrage.optimizers.solidly_stable import SolidlyStableSolver
from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.arbitrage.optimizers.solver_hop_builders import (
    pool_state_to_hop,
    pool_to_hop,
    pools_to_solve_input,
)

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
    "pool_state_to_hop",
    "pool_to_hop",
    "pools_to_solve_input",
]
