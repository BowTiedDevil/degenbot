"""
Unified solver interface for arbitrage optimization.

All optimizers accept the same ``SolveInput`` (a sequence of ``Hop`` objects)
and return the same ``SolveResult``.  The ``ArbSolver`` dispatcher automatically
selects the best method based on the hop types.

This module re-exports the individual solver implementations that now live in
focused submodules, so existing ``from degenbot.arbitrage.optimizers.solver import …``
statements continue to work.
"""

import time
from fractions import Fraction
from typing import Any, ClassVar, override

# Re-export internal helpers so existing test imports keep working
from degenbot.arbitrage.optimizers._solver_utils import (  # noqa: F401
    _compute_mobius_coefficients,
    _infer_zero_for_one,
    _rust_integer_refinement,
    _simulate_path,
)
from degenbot.arbitrage.optimizers._v3_utils import (  # noqa: F401
    _get_cached_tick_ranges,
    _tick_range_cache,
    _v3_get_adjacent_tick_ranges,
    _v3_virtual_reserves,
)
from degenbot.arbitrage.optimizers.balancer_multi_token_solver import (
    BalancerMultiTokenSolver,
)
from degenbot.arbitrage.optimizers.brent_solver import BrentSolver
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
from degenbot.arbitrage.optimizers.newton_solver import NewtonSolver  # noqa: F401
from degenbot.arbitrage.optimizers.piecewise_mobius_solver import (
    PiecewiseMobiusSolver,
)
from degenbot.arbitrage.optimizers.solidly_stable import (
    SolidlyStableSolver,
)
from degenbot.arbitrage.optimizers.solver_hop_builders import (  # noqa: F401
    pool_state_to_hop,
    pool_to_hop,
    pools_to_solve_input,
)
from degenbot.degenbot_rs import mobius as _rs_mobius
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import (  # noqa: F401 — re-exported for backward compatibility
    BalancerMultiTokenHop,
    BoundedProductHop,
    ConstantProductHop,
    Hop,
    HopType,
    PoolInvariant,
    SolidlyStableHop,
    V3TickRangeInfo,
)


class ArbSolver(Solver):
    """
    Top-level solver that dispatches to the best method.

    Each sub-solver owns its own Rust acceleration and falls back to
    Python internally. ArbSolver is a pure dispatcher.

    Dispatch order:
    1. MobiusSolver (V2 + single-range V3, Rust-accelerated)
    2. PiecewiseMobiusSolver (V3 multi-range, Rust-accelerated)
    3. SolidlyStableSolver (Python only)
    4. BalancerMultiTokenSolver (Python only)
    5. BrentSolver (Python only, handles everything)

    Usage:
    -----
    >>> from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput
    >>> solver = ArbSolver()
    >>> result = solver.solve(SolveInput(hops=(
    ...     Hop(reserve_in=2_000_000e6, reserve_out=1_000e18, fee=Fraction(3, 1000)),
    ...     Hop(reserve_in=1_500_000e6, reserve_out=800e18, fee=Fraction(3, 1000)),
    ... )))
    """

    MIN_HOPS = 2

    _RUST_METHOD_MAP: ClassVar[dict[int, SolverMethod]] = {
        0: SolverMethod.MOBIUS,
        1: SolverMethod.PIECEWISE_MOBIUS,
        2: SolverMethod.PIECEWISE_MOBIUS,
    }

    def __init__(self) -> None:
        self._pool_cache: Any = None
        self._next_pool_id: int = 1
        self._pool_id_map: dict[int, int] = {}
        self._mobius = MobiusSolver()
        self._piecewise = PiecewiseMobiusSolver()
        self._solidly = SolidlyStableSolver()
        self._balancer_multi = BalancerMultiTokenSolver()
        self._brent = BrentSolver()
        if _rs_mobius is not None:
            self._pool_cache = _rs_mobius.RustPoolCache()

    # ------------------------------------------------------------------
    # Rust pool cache helpers (ArbSolver-only concern)
    # ------------------------------------------------------------------

    def get_pool_cache(self) -> Any:
        """Return the Rust-side pool state cache.

        The cache can be used to register pool states at update time,
        then solve by pool ID reference without any Python object
        construction on the solve path.
        """
        if self._pool_cache is None:
            raise RuntimeError("Pool cache requires the Rust extension (degenbot_rs)")
        return self._pool_cache

    def register_pool(
        self,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
        *,
        pool_id: int | None = None,
    ) -> int:
        """Register a pool's state in the Rust cache.

        Call this at pool state update time (once per block). The returned
        pool_id can then be used in ``solve_cached()`` calls.

        If pool_id is not provided, a new unique ID is assigned.

        Returns the pool_id (useful when auto-assigning).
        """
        cache = self.get_pool_cache()

        if pool_id is None:
            pool_id = self._next_pool_id
            self._next_pool_id += 1

        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)
        return pool_id

    def update_pool(
        self,
        pool_id: int,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
    ) -> None:
        """Update a previously registered pool's state in the Rust cache.

        Equivalent to register_pool() with an explicit pool_id.
        """
        cache = self.get_pool_cache()
        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)

    def remove_pool(self, pool_id: int) -> bool:
        """Remove a pool from the Rust cache.

        Returns True if the pool was found and removed.
        """
        cache = self.get_pool_cache()
        return cache.remove(pool_id)

    def solve_cached(
        self,
        path: list[int],
        *,
        max_input: int | None = None,
    ) -> SolveResult:
        """Solve an arbitrage path using cached pool states by ID.

        This is the fastest solve path: no Python object construction,
        no per-item extraction, just a list of integer pool IDs passed
        to Rust. Pool states must have been registered beforehand via
        ``register_pool()`` or ``update_pool()``.

        Parameters
        ----------
        path
            Ordered list of pool IDs along the arbitrage path.
        max_input
            Optional maximum input constraint.

        Returns
        -------
        SolveResult
        """
        start_ns = time.perf_counter_ns()
        cache = self.get_pool_cache()

        max_input_float = float(max_input) if max_input is not None else None

        try:
            result = cache.solve(path, max_input_float)
        except (ValueError, TypeError) as e:
            raise OptimizationError(
                message=f"Pool cache solve failed: {e}",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            ) from e

        if not result.supported:
            raise OptimizationError(
                message="Not supported by cache",
                iterations=0,
                method=SolverMethod.MOBIUS.name,
            )

        elapsed_ns = time.perf_counter_ns() - start_ns
        method = self._RUST_METHOD_MAP.get(result.method, SolverMethod.MOBIUS)

        if not result.success:
            raise OptimizationError(
                message="Not profitable",
                iterations=result.iterations,
                method=method.name,
            )

        # Integer refinement results from cache
        if result.optimal_input_int is not None and result.profit_int is not None:
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return SolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )

        raise OptimizationError(
            message="Not profitable",
            iterations=result.iterations,
            method=method.name,
        )

    # ------------------------------------------------------------------
    # Solver interface
    # ------------------------------------------------------------------

    @override
    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops >= self.MIN_HOPS

    @override
    def solve(self, solve_input: SolveInput) -> SolveResult:
        """
        Solve with automatic method selection.

        Dispatches to sub-solvers in order. Each sub-solver tries
        Rust first, then falls back to Python internally.

        Raises OptimizationError if no solver can find a profitable solution.
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
            except OptimizationError:
                continue

        raise OptimizationError(
            message="No solver found a profitable solution",
            iterations=0,
            method="ArbSolver",
        )
