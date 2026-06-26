"""Arbitrage solvers for different pool types.

ACDWOC retire: the f64 Möbius dispatcher (``ArbSolver``), ``MobiusSolver``,
``PiecewiseMobiusSolver``, and ``NewtonSolver`` are gone — the Rust
``UniswapArbEngine`` owns the EVM-exact U512 solve path via
``register_and_solve_path`` (cross-validated against ``BrentSolver`` in
``tests/arbitrage/test_engine_vs_brent_parity.py``). The kept Python solvers
cover the families the engine doesn't:

- ``BrentSolver`` — SciPy ``minimize_scalar`` over a universal
  ``hop.swap_fn`` simulator (reference oracle for engine cross-validation +
  the production solver for Solidly/Curve/Balancer).
- ``SolidlyStableSolver`` — Solidly stable-invariant pools.
- ``BalancerMultiTokenSolver`` — N-token Balancer weighted-basket arb.
"""

from degenbot.arbitrage.solvers.balancer_multi_token_solver import BalancerMultiTokenSolver
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput, SolveResult, SolverMethod
from degenbot.arbitrage.solvers.solidly_stable import SolidlyStableSolver

__all__ = [
    "BalancerMultiTokenSolver",
    "BrentSolver",
    "SolidlyStableSolver",
    "SolveInput",
    "SolveResult",
    "SolverMethod",
]
