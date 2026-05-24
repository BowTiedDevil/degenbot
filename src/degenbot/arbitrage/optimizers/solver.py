"""Unified solver interface for arbitrage optimization.

All optimizers accept the same ``SolveInput`` (a sequence of ``HopType`` objects)
and return the same ``SolveResult``.  The ``ArbSolver`` dispatcher automatically
selects the best method based on the hop types.

This module re-exports the individual solver implementations that now live in
focused submodules, so existing ``from degenbot.arbitrage.optimizers.solver import …``
statements continue to work.
"""

import time
from fractions import Fraction
from typing import ClassVar, override

# Re-export internal helpers so existing test imports keep working
from degenbot.arbitrage.optimizers.balancer_multi_token_solver import (
    BalancerMultiTokenSolver,
)
from degenbot.arbitrage.optimizers.brent_solver import BrentSolver
from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult, SolverMethod
from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
from degenbot.arbitrage.optimizers.newton_solver import NewtonSolver
from degenbot.arbitrage.optimizers.piecewise_mobius_solver import (
    PiecewiseMobiusSolver,
)
from degenbot.arbitrage.optimizers.solidly_stable import (
    SolidlyStableSolver,
)
from degenbot.degenbot_rs import RustPoolCache as _RustPoolCache
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
    >>> from degenbot.arbitrage.optimizers.solver import ArbSolver, SolveInput
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

    _RUST_METHOD_MAP: ClassVar[dict[int, SolverMethod]] = {
        0: SolverMethod.MOBIUS,
        1: SolverMethod.PIECEWISE_MOBIUS,
        2: SolverMethod.PIECEWISE_MOBIUS,
    }

    def __init__(self) -> None:
        """Initialize the instance."""
        self._pool_cache = _RustPoolCache()
        self._next_pool_id: int = 1
        self._pool_id_map: dict[int, int] = {}
        self._mobius = MobiusSolver()
        self._piecewise = PiecewiseMobiusSolver()
        self._solidly = SolidlyStableSolver()
        self._balancer_multi = BalancerMultiTokenSolver()
        self._brent = BrentSolver()
        self._pool_cache = _RustPoolCache()

    # ------------------------------------------------------------------
    # Rust pool cache helpers (ArbSolver-only concern)
    # ------------------------------------------------------------------

    def get_pool_cache(self) -> _RustPoolCache:
        """Return the Rust-side pool state cache.

        The cache can be used to register pool states at update time,
        then solve by pool ID reference without any Python object
        construction on the solve path.

        Returns:
            The computed value.

        """
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

        Returns:
            The computed value.

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

        Returns:
            The computed value.

        """
        cache = self.get_pool_cache()
        return cache.remove(pool_id)

    def register_path(self, pool_ids: list[int]) -> int:
        """Register an arbitrage path in the Rust cache.

        The path's pool IDs are resolved to concrete IntHopState values
        once at registration time. Subsequent calls to ``solve_registered()``
        use the returned path ID, eliminating all per-solve pool lookups,
        float conversions, and lock acquisitions.

        Call ``update_path()`` or ``update_all_paths()`` after pool state
        changes (e.g., at block boundaries) to re-resolve the path.

        Args:
            pool_ids: Ordered list of pool IDs along the arbitrage path.
                Must have at least 2 pool IDs.

        Returns:
            The auto-assigned path ID.

        """
        cache = self.get_pool_cache()
        return cache.register_path(pool_ids)

    def update_path(self, path_id: int) -> bool:
        """Re-resolve a registered path's pool states.

        Call this after updating pool states (e.g., at block boundaries)
        to refresh the pre-resolved hop states from the pool cache.

        Args:
            path_id: The path ID returned by ``register_path()``.

        Returns:
            True if the path was found and updated, False if not found.

        """
        cache = self.get_pool_cache()
        return cache.update_path(path_id)

    def update_all_paths(self) -> int:
        """Re-resolve all registered paths after a batch pool state update.

        More efficient than calling ``update_path()`` individually because
        it acquires the pool cache lock once for all paths.

        Returns:
            The number of paths updated.

        """
        cache = self.get_pool_cache()
        return cache.update_all_paths()

    def remove_path(self, path_id: int) -> bool:
        """Remove a registered path.

        Args:
            path_id: The path ID to remove.

        Returns:
            True if the path was found and removed, False otherwise.

        """
        cache = self.get_pool_cache()
        return cache.remove_path(path_id)

    def solve_registered(
        self,
        path_ids: list[int],
        *,
        max_input: int | None = None,
    ) -> list[SolveResult]:
        """Solve multiple pre-registered paths by path ID.

        This is the fastest solve path: paths were pre-resolved at
        registration time, so no pool lookups, float conversions, or
        lock acquisitions are needed. The GIL is released once for
        the entire batch.

        Args:
            path_ids: Path IDs returned by ``register_path()``.
            max_input: Optional maximum input constraint (applied to all paths).

        Returns:
            List of SolveResult, one per path. Paths that are not registered
            or not profitable have .profit == 0.

        """
        start_ns = time.perf_counter_ns()
        cache = self.get_pool_cache()

        max_input_float = float(max_input) if max_input is not None else None

        results = cache.solve_registered(path_ids, max_input_float)

        elapsed_ns = time.perf_counter_ns() - start_ns

        solve_results: list[SolveResult] = []

        for result in results:
            method = self._RUST_METHOD_MAP.get(result.method, SolverMethod.MOBIUS)

            if not result.supported or not result.success:
                solve_results.append(
                    SolveResult(
                        optimal_input=0,
                        profit=0,
                        iterations=result.iterations,
                        method=method,
                        solve_time_ns=elapsed_ns,
                    )
                )
                continue

            if result.optimal_input_int is not None and result.profit_int is not None:
                optimal_input = int(result.optimal_input_int)
                profit = int(result.profit_int)
                if profit > 0:
                    solve_results.append(
                        SolveResult(
                            optimal_input=optimal_input,
                            profit=profit,
                            iterations=result.iterations,
                            method=method,
                            solve_time_ns=elapsed_ns,
                        )
                    )
                    continue

            solve_results.append(
                SolveResult(
                    optimal_input=0,
                    profit=0,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )
            )

        return solve_results

    def solve_registered_ints(
        self,
        path_ids: list[int],
        *,
        max_input: int | None = None,
    ) -> list[tuple[int, int]]:
        """Solve multiple pre-registered paths, returning only integer results.

        This is the **minimum-overhead** solve path. It returns a flat list of
        ``(optimal_input, profit)`` tuples, bypassing all ``SolveResult``
        construction, method/iteration tracking, and result field conversion.

        For paths that are not registered, not supported, or not profitable,
        the returned tuple is ``(0, 0)``.

        Args:
            path_ids: Path IDs returned by ``register_path()``.
            max_input: Optional maximum input constraint (applied to all paths).

        Returns:
            List of ``(optimal_input, profit)`` tuples, one per path.

        """
        cache = self.get_pool_cache()

        max_input_float = float(max_input) if max_input is not None else None

        # Rust returns flat [input0, profit0, input1, profit1, ...]
        flat = cache.solve_registered_ints(path_ids, max_input_float)

        # Group into pairs
        return [(flat[i], flat[i + 1]) for i in range(0, len(flat), 2)]

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

        Args:
            path: Ordered list of pool IDs along the arbitrage path.
            max_input: Optional maximum input constraint.

        Returns:
            The solve result.

        Raises:
            OptimizationError: If the solver cannot find a valid solution.

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

    def solve_cached_batch(
        self,
        paths: list[list[int]],
        *,
        max_input: int | None = None,
    ) -> list[SolveResult]:
        """Solve multiple arbitrage paths in a single Python → Rust round-trip.

        All paths are looked up and solved inside a single GIL-release window,
        amortizing the ~1,160ns PyO3 bridge overhead across all paths.

        Args:
            paths: List of paths, each an ordered list of pool IDs.
            max_input: Optional maximum input constraint (applied to all paths).

        Returns:
            List of SolveResult, one per path. Paths that are not supported
            or not profitable are still returned (with .profit == 0), rather
            than raising OptimizationError.

        """
        start_ns = time.perf_counter_ns()
        cache = self.get_pool_cache()

        max_input_float = float(max_input) if max_input is not None else None

        results = cache.solve_batch(paths, max_input_float)

        elapsed_ns = time.perf_counter_ns() - start_ns

        solve_results: list[SolveResult] = []

        for result in results:
            method = self._RUST_METHOD_MAP.get(result.method, SolverMethod.MOBIUS)

            if not result.supported or not result.success:
                solve_results.append(
                    SolveResult(
                        optimal_input=0,
                        profit=0,
                        iterations=result.iterations,
                        method=method,
                        solve_time_ns=elapsed_ns,
                    )
                )
                continue

            if result.optimal_input_int is not None and result.profit_int is not None:
                optimal_input = int(result.optimal_input_int)
                profit = int(result.profit_int)
                if profit > 0:
                    solve_results.append(
                        SolveResult(
                            optimal_input=optimal_input,
                            profit=profit,
                            iterations=result.iterations,
                            method=method,
                            solve_time_ns=elapsed_ns,
                        )
                    )
                    continue

            solve_results.append(
                SolveResult(
                    optimal_input=0,
                    profit=0,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )
            )

        return solve_results

    # ------------------------------------------------------------------
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
