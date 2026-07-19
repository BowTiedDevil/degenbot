"""Arbitrage solvers for different pool types.

ACDWOC retire: the f64 Möbius dispatcher (``ArbSolver``), ``MobiusSolver``,
``PiecewiseMobiusSolver``, and ``NewtonSolver`` are gone — the Rust
``ArbitrageEngine`` owns the EVM-exact U512 solve path via
``register_and_solve_path`` (cross-validated against ``BrentSolver`` in
``tests/arbitrage/test_engine_vs_brent_parity.py``). The kept Python solvers
cover the families the engine doesn't:

- ``BrentSolver`` — SciPy ``minimize_scalar`` over a universal
  ``hop.swap_fn`` simulator (reference oracle for engine cross-validation +
  the production solver for Solidly/Curve/Balancer).
- ``BalancerMultiTokenSolver`` — N-token Balancer weighted-basket arb.
  Now a **delegating shell** (ADR-005) over the Rust core
  ``degenbot._ffi.solve_balancer_weighted_basket`` (QuantAMM Equation 9);
  the Python ``balancer_weighted.py`` math is retired. The delegation wiring
  is gated by ``tests/rust/test_quantamm_basket_parity.py``; the math itself
  by the Rust ``#[cfg(test)]`` corpus in ``balancer_weighted_basket.rs``.
"""

from degenbot.arbitrage.solvers.balancer_multi_token_solver import BalancerMultiTokenSolver
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput, SolveResult, SolverMethod

__all__ = [
    "BalancerMultiTokenSolver",
    "BrentSolver",
    "SolveInput",
    "SolveResult",
    "SolverMethod",
]
