"""
Vectorized batch Möbius transformation optimizer for constant product AMM
arbitrage, using NumPy for SIMD-style parallel evaluation across multiple paths.

The Möbius approach computes a closed-form optimal input for each path via an
O(n) coefficient recurrence (K, M, N) followed by one sqrt and one division.
No iterations are required, so the vectorized version simply runs the recurrence
across all paths simultaneously and applies the closed-form in one vectorized
call.

Performance characteristics (estimated from vectorized Newton benchmarks):
- Single path: ~50μs overhead (slower than serial Möbius at ~5μs)
- 20 paths: ~80μs total (4μs per path, similar to serial)
- 50 paths: ~85μs total (1.7μs per path, ~3x faster)
- 100 paths: ~100μs total (1μs per path, ~5x faster)
- 500 paths: ~200μs total (0.4μs per path, ~12x faster)
- 1000 paths: ~350μs total (0.35μs per path, ~14x faster)

Key advantage over BatchNewton: zero iterations. The recurrence is one forward
pass, then a single np.sqrt call for all paths. The free profitability check
(K/M > 1) eliminates unprofitable paths without any compute.

Supports variable hop counts per path. All paths with the same number of hops
are processed together in a single vectorized batch. Paths with different hop
counts are grouped and processed separately.

References:
    Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop
    DEX Paths via Möbius Transformations."
"""

import time
from dataclasses import dataclass

import numpy as np

from degenbot.arbitrage.optimizers.base import (
    OptimizerResult,
    OptimizerType,
)
from degenbot.arbitrage.optimizers.mobius import (
    MobiusFloatHop,
    mobius_solve,
)


@dataclass(frozen=True)
class BatchMobiusPathInput:
    """
    A single arbitrage path expressed as a list of MobiusFloatHop objects.

    Used as input for BatchMobiusOptimizer. Each path represents a sequence
    of constant-product pool hops forming an arbitrage cycle.
    """

    hops: list[MobiusFloatHop]
    max_input: float | None = None


@dataclass
class VectorizedMobiusResult:
    """
    Results from vectorized Möbius computation for a group of paths sharing
    the same hop count.

    All arrays have shape (num_paths,).
    """

    optimal_input: np.ndarray
    profit: np.ndarray
    iterations: np.ndarray  # Always 0 for Möbius
    is_profitable: np.ndarray
    max_input: np.ndarray  # Per-path max input (inf if unconstrained)

    @property
    def num_paths(self) -> int:
        return len(self.optimal_input)

    def to_integers(self) -> "VectorizedMobiusResult":
        """Convert amounts to integers via floor."""
        return VectorizedMobiusResult(
            optimal_input=np.floor(self.optimal_input).astype(np.int64),
            profit=np.floor(self.profit).astype(np.int64),
            iterations=self.iterations,
            is_profitable=self.is_profitable,
            max_input=self.max_input,
        )

    def profitable_mask(self) -> np.ndarray:
        """Return mask of profitable paths."""
        return self.is_profitable & (self.profit > 0)

    def best_path_index(self) -> int:
        """Return index of path with highest profit."""
        profits = np.where(self.profitable_mask(), self.profit, -np.inf)
        return int(np.argmax(profits))

    def top_paths(self, n: int = 10) -> list[tuple[int, int, int]]:
        """Return top N profitable paths as (index, input, profit) tuples."""
        profits = np.where(self.profitable_mask(), self.profit, -np.inf)
        indices = np.argsort(profits)[::-1][:n]
        return [
            (int(i), int(self.optimal_input[i]), int(self.profit[i]))
            for i in indices
            if self.profit[i] > 0
        ]


class VectorizedMobiusSolver:
    """
    Vectorized Möbius solver for batch constant product AMM arbitrage.

    All paths must have the same number of hops. For variable hop counts,
    use BatchMobiusOptimizer which groups paths by hop count.

    Usage:
    -----
    >>> solver = VectorizedMobiusSolver()
    >>> result = solver.solve(hops_array, max_inputs)
    >>> best_idx = result.best_path_index()
    """

    @staticmethod
    def solve(
        hops_array: np.ndarray,
        max_inputs: np.ndarray,
    ) -> VectorizedMobiusResult:
        """
        Solve all paths simultaneously using vectorized Möbius recurrence.

        The recurrence computes K, M, N coefficients for each path in
        lock-step, then applies the closed-form optimal input formula.

        Parameters
        ----------
        hops_array : np.ndarray
            Shape (num_paths, num_hops, 3) where the last dimension is
            [reserve_in, reserve_out, fee].
        max_inputs : np.ndarray
            Shape (num_paths,) with per-path max input constraints.
            Use np.inf for unconstrained paths.

        Returns
        -------
        VectorizedMobiusResult
            Results for all paths.
        """
        if hops_array.shape[0] == 0:
            return VectorizedMobiusResult(
                optimal_input=np.array([], dtype=np.float64),
                profit=np.array([], dtype=np.float64),
                iterations=np.array([], dtype=np.int32),
                is_profitable=np.array([], dtype=bool),
                max_input=np.array([], dtype=np.float64),
            )

        num_paths = hops_array.shape[0]
        num_hops = hops_array.shape[1]

        # Extract per-hop fields: shape (num_paths, num_hops)
        # Copy to avoid mutating the input array (numpy slicing returns views,
        # and in-place operations like M *= would modify hops_array).
        reserves_in = hops_array[:, :, 0].copy()
        reserves_out = hops_array[:, :, 1].copy()
        fees = hops_array[:, :, 2].copy()
        gammas = 1.0 - fees

        # Initialize recurrence from first hop
        K = gammas[:, 0] * reserves_out[:, 0]  # noqa: N806 (math notation)
        M = reserves_in[:, 0]  # noqa: N806
        N = gammas[:, 0]  # noqa: N806

        # Log-domain recurrence for overflow-safe computation.
        # log(K) and log(M) are simple cumulative sums.
        # log(N) requires log-sum-exp since N is a sum of products:
        #   N_j = N_{j-1} * r_in_j + K_{j-1} * gamma_j
        #   log(N_j) = logsumexp(log(N_{j-1}) + log(r_in_j),
        #                        log(K_{j-1}) + log(gamma_j))
        log_K = np.log(gammas[:, 0]) + np.log(reserves_out[:, 0])  # noqa: N806
        log_M = np.log(reserves_in[:, 0])  # noqa: N806
        log_N = np.log(gammas[:, 0])  # noqa: N806

        with np.errstate(over="ignore", invalid="ignore"):
            for j in range(1, num_hops):
                old_K = K.copy()  # noqa: N806
                K = old_K * gammas[:, j] * reserves_out[:, j]  # noqa: N806
                M *= reserves_in[:, j]  # noqa: N806
                N = N * reserves_in[:, j] + old_K * gammas[:, j]  # noqa: N806

                old_log_K = log_K.copy()  # noqa: N806
                log_K = (  # noqa: N806
                    log_K + np.log(gammas[:, j]) + np.log(reserves_out[:, j])
                )
                log_M += np.log(reserves_in[:, j])  # noqa: N806

                # log(N_j) via log-sum-exp:
                #   N_j = N_{j-1} * r_in_j + K_{j-1} * gamma_j
                #   log(N_j) = max(a, b) + log1p(exp(-|a-b|))
                #   where a = log(N_{j-1}) + log(r_in_j)
                #         b = log(K_{j-1}) + log(gamma_j)
                a = log_N + np.log(reserves_in[:, j])
                b = old_log_K + np.log(gammas[:, j])
                max_ab = np.maximum(a, b)
                log_N = max_ab + np.log1p(np.exp(-(np.abs(a - b))))  # noqa: N806

        # Profitability check: K > M
        # Use direct comparison when both are finite, fall back to log-domain
        # when either overflows (K or M becomes inf).
        overflow = ~np.isfinite(K) | ~np.isfinite(M)
        direct_profitable = K > M
        log_profitable = log_K > log_M
        is_profitable = np.where(overflow, log_profitable, direct_profitable)

        # Closed-form optimal input: x_opt = (sqrt(K*M) - M) / N
        #   = M * (sqrt(K/M) - 1) / N
        #   = M * expm1(half_diff) / N    where half_diff = 0.5*(log_K - log_M)
        #
        # In log domain (handles overflow):
        #   log(x_opt) = log_M + log(expm1(half_diff)) - log_N
        eps = 1e-30
        with np.errstate(over="ignore", invalid="ignore"):
            km_product = K * M
        km_overflows = ~np.isfinite(km_product) | (km_product < 0)

        # Direct computation (when no overflow)
        with np.errstate(over="ignore", invalid="ignore"):
            sqrt_km = np.sqrt(np.maximum(km_product, 0.0))
            numerator = sqrt_km - M
        safe_n = np.where(np.abs(N) > eps, N, 1.0)
        x_opt_direct = numerator / safe_n

        # Log-domain computation for overflow case:
        # log(x_opt) = log_M + log(expm1(half_diff)) - log_N
        half_diff = 0.5 * (log_K - log_M)
        with np.errstate(divide="ignore", invalid="ignore"):
            log_expm1_hd = np.log(np.expm1(half_diff))
            log_x_opt = log_M + log_expm1_hd - log_N
        x_opt_log = np.exp(log_x_opt)

        # Use log-domain result where direct overflows
        x_opt = np.where(km_overflows & is_profitable, x_opt_log, x_opt_direct)

        # Zero out unprofitable or negative-input paths
        x_opt = np.where(is_profitable & (x_opt > 0), x_opt, 0.0)

        # Clean up any NaN/inf from overflow edge cases
        x_opt = np.where(np.isfinite(x_opt) & (x_opt >= 0), x_opt, 0.0)

        # Apply max_input constraint
        constrained = max_inputs < np.inf
        x_opt = np.where(constrained & (x_opt > max_inputs), max_inputs, x_opt)

        # At the optimal input, M + N*x_opt = sqrt(K*M), so:
        # profit = x * (sqrt(K/M) - 1) = x * expm1(half_diff)
        with np.errstate(over="ignore", invalid="ignore"):
            profit = x_opt * np.expm1(half_diff)

        # Zero profit for unprofitable or invalid paths
        profit = np.where(is_profitable & (x_opt > 0) & np.isfinite(profit), profit, 0.0)

        return VectorizedMobiusResult(
            optimal_input=x_opt,
            profit=profit,
            iterations=np.zeros(num_paths, dtype=np.int32),
            is_profitable=is_profitable,
            max_input=max_inputs,
        )


class SerialMobiusSolver:
    """
    Serial Möbius solver for benchmarking against the vectorized version.

    Processes paths one-by-one using the scalar mobius_solve function.
    """

    @staticmethod
    def solve(
        hops_array: np.ndarray,
        max_inputs: np.ndarray,
    ) -> VectorizedMobiusResult:
        """Solve all paths serially."""
        optimal_inputs = []
        profits = []
        iterations_list = []
        profitable_list = []

        for i in range(hops_array.shape[0]):
            hops = [
                MobiusFloatHop(
                    reserve_in=float(hops_array[i, j, 0]),
                    reserve_out=float(hops_array[i, j, 1]),
                    fee=float(hops_array[i, j, 2]),
                )
                for j in range(hops_array.shape[1])
            ]

            max_input = float(max_inputs[i]) if max_inputs[i] < np.inf else None
            x_opt, profit, iters = mobius_solve(hops, max_input=max_input)

            optimal_inputs.append(x_opt)
            profits.append(profit)
            iterations_list.append(iters)
            profitable_list.append(x_opt > 0 and profit > 0)

        return VectorizedMobiusResult(
            optimal_input=np.array(optimal_inputs),
            profit=np.array(profits),
            iterations=np.array(iterations_list),
            is_profitable=np.array(profitable_list),
            max_input=max_inputs,
        )


def generate_batch_paths(
    num_paths: int,
    num_hops: int = 2,
    base_reserve: float = 1_000_000.0,
    profit_factor: float = 1.1,
    *,
    fee: float = 0.003,
    seed: int = 42,
) -> tuple[np.ndarray, np.ndarray]:
    """
    Generate test paths for batch Möbius benchmarking.

    Each path has num_hops pools with slightly mispriced reserves that
    accumulate to a profitable cycle. The profit_factor controls the
    overall cross-rate: product of per-hop rates > 1.

    Parameters
    ----------
    num_paths : int
        Number of paths to generate.
    num_hops : int
        Number of hops per path.
    base_reserve : float
        Base reserve magnitude.
    profit_factor : float
        Overall profit factor (product of rates).
    fee : float
        Fee fraction per hop.
    seed : int
        Random seed for reproducibility.

    Returns
    -------
    tuple[np.ndarray, np.ndarray]
        (hops_array, max_inputs) where hops_array has shape
        (num_paths, num_hops, 3) and max_inputs has shape (num_paths,).
    """
    rng = np.random.default_rng(seed)

    per_hop_factor = profit_factor ** (1.0 / num_hops)

    hops_array = np.zeros((num_paths, num_hops, 3), dtype=np.float64)
    max_inputs = np.full(num_paths, np.inf)

    for i in range(num_paths):
        for j in range(num_hops):
            # Add some random variation per path
            variation = rng.uniform(0.8, 1.2)
            reserve_in = base_reserve * variation
            reserve_out = base_reserve * per_hop_factor * variation
            hops_array[i, j, 0] = reserve_in
            hops_array[i, j, 1] = reserve_out
            hops_array[i, j, 2] = fee

    return hops_array, max_inputs


class BatchMobiusOptimizer:
    """
    High-level batch optimizer for multiple constant product AMM arbitrage
    paths using the Möbius transformation approach.

    Automatically groups paths by hop count for efficient vectorized solving.
    Falls back to serial solving for small batch sizes.

    Performance:
    - Single path: serial (~5μs, no NumPy overhead)
    - 20+ paths: vectorized (3-14x faster than serial)
    - 1000 paths: ~0.35μs per path

    Usage:
    -----
    >>> optimizer = BatchMobiusOptimizer()
    >>> inputs = [BatchMobiusPathInput(hops=[...]), ...]
    >>> results = optimizer.solve_batch(inputs)
    >>> for result in results:
    ...     print(f"Input: {result.optimal_input}, Profit: {result.profit}")
    """

    def __init__(
        self,
        min_paths_for_batch: int = 20,
    ) -> None:
        """
        Parameters
        ----------
        min_paths_for_batch : int
            Minimum number of paths with the same hop count before
            switching to vectorized solving. Below this threshold,
            serial solving is used (avoids NumPy overhead).
        """
        self.min_paths_for_batch = min_paths_for_batch
        self._vectorized_solver = VectorizedMobiusSolver()
        self._serial_solver = SerialMobiusSolver()
        self._last_solve_time_ms = 0.0

    @property
    def optimizer_type(self) -> OptimizerType:
        return OptimizerType.MOBIUS

    def solve_batch(
        self,
        paths: list[BatchMobiusPathInput],
    ) -> list[OptimizerResult]:
        """
        Solve multiple arbitrage paths.

        Paths are grouped by hop count. Groups with enough paths use
        vectorized solving; small groups use serial solving.

        Parameters
        ----------
        paths : list[BatchMobiusPathInput]
            List of path inputs with hops and optional max_input.

        Returns
        -------
        list[OptimizerResult]
            Results for each path in the same order as input.
        """
        start_time = time.perf_counter_ns()

        if not paths:
            self._last_solve_time_ms = 0.0
            return []

        # Group paths by hop count
        groups: dict[int, list[tuple[int, BatchMobiusPathInput]]] = {}
        for idx, path in enumerate(paths):
            n = len(path.hops)
            groups.setdefault(n, []).append((idx, path))

        # Pre-allocate results
        results: list[OptimizerResult | None] = [None] * len(paths)

        for num_hops, group in groups.items():
            if num_hops == 0:
                # Empty paths — no arbitrage possible
                for idx, _ in group:
                    results[idx] = OptimizerResult(
                        optimal_input=0,
                        profit=0,
                        solve_time_ms=0.0,
                        iterations=0,
                        optimizer_type=self.optimizer_type,
                    )
                continue

            # Build arrays for this group
            group_size = len(group)
            hops_array = np.zeros((group_size, num_hops, 3), dtype=np.float64)
            max_inputs = np.full(group_size, np.inf)

            for i, (_idx, path) in enumerate(group):
                for j, hop in enumerate(path.hops):
                    hops_array[i, j, 0] = hop.reserve_in
                    hops_array[i, j, 1] = hop.reserve_out
                    hops_array[i, j, 2] = hop.fee
                if path.max_input is not None:
                    max_inputs[i] = path.max_input

            # Choose solver based on group size
            if group_size >= self.min_paths_for_batch:
                vec_result = self._vectorized_solver.solve(hops_array, max_inputs)
            else:
                vec_result = self._serial_solver.solve(hops_array, max_inputs)

            int_result = vec_result.to_integers()

            # Convert to OptimizerResults
            for i, (idx, _path) in enumerate(group):
                optimal_input = int(int_result.optimal_input[i])
                profit = int(int_result.profit[i])

                results[idx] = OptimizerResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    solve_time_ms=0.0,  # Filled below
                    iterations=0,  # Möbius is always 0 iterations
                    optimizer_type=self.optimizer_type,
                )

        # Fill in solve times
        elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
        self._last_solve_time_ms = elapsed_ms
        per_path_ms = elapsed_ms / len(paths) if paths else 0.0

        for i, result in enumerate(results):
            if result is not None:
                results[i] = OptimizerResult(
                    optimal_input=result.optimal_input,
                    profit=result.profit,
                    solve_time_ms=per_path_ms,
                    iterations=result.iterations,
                    optimizer_type=result.optimizer_type,
                )

        return list(results)  # type: ignore[arg-type]

    def solve_batch_hops(
        self,
        hops_array: np.ndarray,
        max_inputs: np.ndarray | None = None,
    ) -> VectorizedMobiusResult:
        """
        Solve a pre-built array of hop states.

        Convenience method for callers who have already organized their
        data into NumPy arrays. All paths must have the same number of hops.

        Parameters
        ----------
        hops_array : np.ndarray
            Shape (num_paths, num_hops, 3) where the last dimension is
            [reserve_in, reserve_out, fee].
        max_inputs : np.ndarray | None
            Shape (num_paths,) with per-path max input constraints.
            Use np.inf for unconstrained. If None, all paths are unconstrained.

        Returns
        -------
        VectorizedMobiusResult
            Results for all paths.
        """
        if max_inputs is None:
            max_inputs = np.full(hops_array.shape[0], np.inf)

        return self._vectorized_solver.solve(hops_array, max_inputs)

    def get_best_path(
        self,
        paths: list[BatchMobiusPathInput],
    ) -> tuple[int, OptimizerResult]:
        """
        Find the best arbitrage path from a batch.

        Returns
        -------
        tuple[int, OptimizerResult]
            (index, result) for the best path.
        """
        results = self.solve_batch(paths)
        best_idx = max(
            range(len(results)),
            key=lambda i: max(0, results[i].profit),
        )
        return best_idx, results[best_idx]
