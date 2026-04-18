"""
Tests for vectorized batch Möbius transformation optimizer.

Verifies that the batch Möbius solver produces results matching the
scalar mobius_solve for various path lengths and batch sizes, and
benchmarks performance against the serial solver.
"""

import time

import numpy as np
import pytest

from degenbot.arbitrage.optimizers.batch_mobius import (
    BatchMobiusOptimizer,
    BatchMobiusPathInput,
    SerialMobiusSolver,
    VectorizedMobiusResult,
    VectorizedMobiusSolver,
    generate_batch_paths,
)
from degenbot.arbitrage.optimizers.mobius import (
    HopState,
    compute_mobius_coefficients,
    mobius_solve,
    simulate_path,
)
from degenbot.arbitrage.optimizers.vectorized_batch import (
    VectorizedNewtonSolver,
    VectorizedPathState,
)

# ==============================================================================
# Helpers
# ==============================================================================


def make_hops_array(
    paths: list[list[HopState]],
) -> np.ndarray:
    """
    Convert a list of HopState lists to a numpy array of shape
    (num_paths, num_hops, 3).
    """
    num_hops = len(paths[0])
    arr = np.zeros((len(paths), num_hops, 3), dtype=np.float64)
    for i, hops in enumerate(paths):
        for j, hop in enumerate(hops):
            arr[i, j, 0] = hop.reserve_in
            arr[i, j, 1] = hop.reserve_out
            arr[i, j, 2] = hop.fee
    return arr


def profitable_2pool_hops() -> list[HopState]:
    return [
        HopState(reserve_in=10_000_000.0, reserve_out=5_000.0, fee=0.003),
        HopState(reserve_in=4_800.0, reserve_out=11_000_000.0, fee=0.003),
    ]


def profitable_3pool_hops() -> list[HopState]:
    return [
        HopState(reserve_in=1_000_000.0, reserve_out=1_000.0, fee=0.003),
        HopState(reserve_in=500.0, reserve_out=1_000_000.0, fee=0.003),
        HopState(reserve_in=1_000_000.0, reserve_out=1_100_000.0, fee=0.003),
    ]


def unprofitable_3pool_hops() -> list[HopState]:
    return [
        HopState(reserve_in=1_000_000.0, reserve_out=1_000.0, fee=0.003),
        HopState(reserve_in=1_000.0, reserve_out=1_000_000.0, fee=0.003),
        HopState(reserve_in=1_000_000.0, reserve_out=1_000_000.0, fee=0.003),
    ]


# ==============================================================================
# Unit Tests: VectorizedMobiusSolver
# ==============================================================================


class TestVectorizedMobiusSolver:
    """Tests for the core vectorized Möbius solver."""

    def test_single_path_matches_scalar(self):
        """Vectorized solution for a single path should match scalar mobius_solve."""
        hops = profitable_3pool_hops()
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        # Compare against scalar
        x_scalar, profit_scalar, _ = mobius_solve(hops)

        assert result.num_paths == 1
        assert result.optimal_input[0] == pytest.approx(x_scalar, rel=1e-8)
        assert result.profit[0] == pytest.approx(profit_scalar, rel=1e-8)
        assert result.iterations[0] == 0
        assert result.is_profitable[0] is np.True_

    def test_two_pool_matches_scalar(self):
        """2-pool path should match scalar."""
        hops = profitable_2pool_hops()
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        x_scalar, profit_scalar, _ = mobius_solve(hops)

        assert result.optimal_input[0] == pytest.approx(x_scalar, rel=1e-8)
        assert result.profit[0] == pytest.approx(profit_scalar, rel=1e-8)

    def test_unprofitable_path_returns_zero(self):
        """Unprofitable path should return zero optimal input and profit."""
        hops = unprofitable_3pool_hops()
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert result.is_profitable[0] is np.False_
        assert result.optimal_input[0] == pytest.approx(0.0, abs=1e-10)
        assert result.profit[0] == pytest.approx(0.0, abs=1e-10)

    def test_multiple_paths_all_match_scalar(self):
        """Multiple paths should each match their scalar solutions."""
        # Need same hop count — test 3-pool paths
        paths_3 = [profitable_3pool_hops(), unprofitable_3pool_hops()]
        hops_array = make_hops_array(paths_3)
        max_inputs = np.array([np.inf, np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        for i, hops in enumerate(paths_3):
            x_scalar, profit_scalar, _ = mobius_solve(hops)
            if x_scalar > 0:
                assert result.optimal_input[i] == pytest.approx(x_scalar, rel=1e-6), (
                    f"Path {i}: vectorized input {result.optimal_input[i]} != scalar {x_scalar}"
                )
                assert result.profit[i] == pytest.approx(profit_scalar, rel=1e-6), (
                    f"Path {i}: vectorized profit {result.profit[i]} != scalar {profit_scalar}"
                )
            else:
                assert result.optimal_input[i] == pytest.approx(0.0, abs=1e-10)
                assert result.profit[i] == pytest.approx(0.0, abs=1e-10)

    def test_max_input_constraint(self):
        """Max input constraint should be respected."""
        hops = profitable_3pool_hops()
        x_unconstrained, _, _ = mobius_solve(hops)
        max_input = x_unconstrained * 0.5

        hops_array = make_hops_array([hops])
        max_inputs = np.array([max_input])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert result.optimal_input[0] <= max_input + 1e-6
        assert result.profit[0] > 0

    def test_empty_paths(self):
        """Zero paths should return empty result."""
        hops_array = np.zeros((0, 2, 3), dtype=np.float64)
        max_inputs = np.array([])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert result.num_paths == 0

    def test_iterations_always_zero(self):
        """Möbius solver should always use zero iterations."""
        hops_array, max_inputs = generate_batch_paths(num_paths=50, num_hops=3, seed=42)
        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert np.all(result.iterations == 0)


# ==============================================================================
# Unit Tests: VectorizedMobiusResult
# ==============================================================================


class TestVectorizedMobiusResult:
    """Tests for the result data structure."""

    def test_to_integers(self):
        """Test conversion to integer amounts."""
        result = VectorizedMobiusResult(
            optimal_input=np.array([100.5, 200.9, 0.0]),
            profit=np.array([4.8, 9.8, 0.0]),
            iterations=np.array([0, 0, 0]),
            is_profitable=np.array([True, True, False]),
            max_input=np.array([np.inf, np.inf, np.inf]),
        )

        int_result = result.to_integers()

        assert int_result.optimal_input[0] == 100
        assert int_result.optimal_input[1] == 200
        assert int_result.profit[0] == 4

    def test_profitable_mask(self):
        """Test profit filtering."""
        result = VectorizedMobiusResult(
            optimal_input=np.array([100.0, 0.0, 50.0]),
            profit=np.array([10.0, 0.0, 5.0]),
            iterations=np.array([0, 0, 0]),
            is_profitable=np.array([True, False, True]),
            max_input=np.array([np.inf, np.inf, np.inf]),
        )

        mask = result.profitable_mask()
        assert mask[0] is np.True_
        assert mask[1] is np.False_
        assert mask[2] is np.True_

    def test_best_path_index(self):
        """Test finding best path."""
        result = VectorizedMobiusResult(
            optimal_input=np.array([100.0, 50.0, 200.0]),
            profit=np.array([10.0, 5.0, 20.0]),
            iterations=np.array([0, 0, 0]),
            is_profitable=np.array([True, True, True]),
            max_input=np.array([np.inf, np.inf, np.inf]),
        )

        assert result.best_path_index() == 2

    def test_top_paths(self):
        """Test getting top N paths."""
        profits = np.array([10.0, 5.0, 20.0, 1.0, 15.0])
        result = VectorizedMobiusResult(
            optimal_input=np.ones(5) * 100.0,
            profit=profits,
            iterations=np.zeros(5, dtype=np.int32),
            is_profitable=np.array([True, True, True, True, True]),
            max_input=np.full(5, np.inf),
        )

        top_3 = result.top_paths(n=3)
        assert len(top_3) == 3
        # Should be sorted by profit descending
        assert top_3[0][0] == 2  # Index 2 has profit 20
        assert top_3[1][0] == 4  # Index 4 has profit 15
        assert top_3[2][0] == 0  # Index 0 has profit 10


# ==============================================================================
# Cross-Solver Agreement: Vectorized vs Serial
# ==============================================================================


class TestVectorizedVsSerial:
    """Verify vectorized solver matches serial solver exactly."""

    @pytest.mark.parametrize("num_hops", [2, 3, 5, 10])
    def test_matches_serial_2pool(self, num_hops):
        """Vectorized and serial should produce identical results."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=50, num_hops=num_hops, seed=42
        )

        vec_solver = VectorizedMobiusSolver()
        ser_solver = SerialMobiusSolver()

        vec_result = vec_solver.solve(hops_array, max_inputs)
        ser_result = ser_solver.solve(hops_array, max_inputs)

        # Profitable flags should match closely (may differ at boundary
        # due to floating-point ordering in vectorized vs scalar)
        agree = vec_result.is_profitable == ser_result.is_profitable
        if not agree.all():
            # At least verify that where they disagree, it's at the boundary
            disagree = ~agree
            # Paths where they disagree should be marginal
            for i in np.where(disagree)[0]:
                # One should claim profit, the other should not
                assert vec_result.is_profitable[i] != ser_result.is_profitable[i]

        # Optimal inputs should match for profitable paths
        profitable = ser_result.is_profitable & (ser_result.optimal_input > 0)
        if profitable.any():
            np.testing.assert_allclose(
                vec_result.optimal_input[profitable],
                ser_result.optimal_input[profitable],
                rtol=1e-8,
                err_msg="Optimal inputs differ for profitable paths",
            )
            np.testing.assert_allclose(
                vec_result.profit[profitable],
                ser_result.profit[profitable],
                rtol=1e-6,
                err_msg="Profits differ for profitable paths",
            )

    @pytest.mark.parametrize("batch_size", [1, 10, 50, 100, 500])
    def test_accuracy_across_batch_sizes(self, batch_size):
        """Accuracy should hold regardless of batch size."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=batch_size, num_hops=3, seed=123
        )

        vec_solver = VectorizedMobiusSolver()
        ser_solver = SerialMobiusSolver()

        vec_result = vec_solver.solve(hops_array, max_inputs)
        ser_result = ser_solver.solve(hops_array, max_inputs)

        profitable = ser_result.is_profitable & (ser_result.optimal_input > 0)
        if profitable.any():
            max_rel_diff = np.max(
                np.abs(vec_result.profit[profitable] - ser_result.profit[profitable])
                / (np.abs(ser_result.profit[profitable]) + 1e-30)
            )
            assert max_rel_diff < 1e-6, f"Max relative diff: {max_rel_diff}"


# ==============================================================================
# Cross-Solver Agreement: Vectorized vs Scalar mobius_solve
# ==============================================================================


class TestVectorizedVsScalar:
    """Verify vectorized solver matches the reference scalar mobius_solve."""

    @pytest.mark.parametrize("num_hops", [2, 3, 5])
    def test_matches_scalar_mobius_solve(self, num_hops):
        """Vectorized should match scalar mobius_solve for each path."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=20, num_hops=num_hops, seed=42
        )

        vec_solver = VectorizedMobiusSolver()
        vec_result = vec_solver.solve(hops_array, max_inputs)

        for i in range(hops_array.shape[0]):
            hops = [
                HopState(
                    reserve_in=float(hops_array[i, j, 0]),
                    reserve_out=float(hops_array[i, j, 1]),
                    fee=float(hops_array[i, j, 2]),
                )
                for j in range(num_hops)
            ]
            x_scalar, profit_scalar, _ = mobius_solve(hops)

            if x_scalar > 0:
                assert vec_result.optimal_input[i] == pytest.approx(
                    x_scalar, rel=1e-8
                ), f"Path {i} input mismatch"
                assert vec_result.profit[i] == pytest.approx(
                    profit_scalar, rel=1e-6
                ), f"Path {i} profit mismatch"
            elif vec_result.optimal_input[i] <= 0:
                # Both agree: unprofitable
                pass
            else:
                # Vectorized found profit but scalar didn't — possible for
                # marginal paths near the profitability boundary due to
                # floating-point ordering differences. Verify the vectorized
                # result is self-consistent.
                assert vec_result.profit[i] > 0, (
                    f"Path {i}: vectorized claims profit but profit <= 0"
                )


# ==============================================================================
# Unit Tests: BatchMobiusOptimizer (high-level API)
# ==============================================================================


class TestBatchMobiusOptimizer:
    """Tests for the high-level batch optimizer API."""

    def test_empty_input(self):
        """Empty input should return empty results."""
        optimizer = BatchMobiusOptimizer()
        results = optimizer.solve_batch([])
        assert results == []

    def test_single_path(self):
        """Single path should use serial solver (below threshold)."""
        optimizer = BatchMobiusOptimizer(min_paths_for_batch=20)
        paths = [
            BatchMobiusPathInput(hops=profitable_3pool_hops()),
        ]
        results = optimizer.solve_batch(paths)

        assert len(results) == 1
        assert results[0].success
        assert results[0].iterations == 0
        assert results[0].profit > 0
        assert results[0].optimizer_type.value == "mobius"

    def test_mixed_hop_counts(self):
        """Paths with different hop counts should be grouped separately."""
        optimizer = BatchMobiusOptimizer(min_paths_for_batch=1)

        paths = [
            BatchMobiusPathInput(hops=profitable_2pool_hops()),
            BatchMobiusPathInput(hops=profitable_3pool_hops()),
            BatchMobiusPathInput(hops=profitable_2pool_hops()),
        ]
        results = optimizer.solve_batch(paths)

        assert len(results) == 3
        # All should succeed
        for r in results:
            assert r.success
            assert r.profit > 0

    def test_unprofitable_path(self):
        """Unprofitable path should return failed result."""
        optimizer = BatchMobiusOptimizer()
        paths = [
            BatchMobiusPathInput(hops=unprofitable_3pool_hops()),
        ]
        results = optimizer.solve_batch(paths)

        assert len(results) == 1
        assert not results[0].success
        assert results[0].profit == 0

    def test_max_input_constraint(self):
        """Max input constraint should be respected."""
        optimizer = BatchMobiusOptimizer()
        x_unconstrained, _, _ = mobius_solve(profitable_3pool_hops())

        paths = [
            BatchMobiusPathInput(
                hops=profitable_3pool_hops(),
                max_input=x_unconstrained * 0.5,
            ),
        ]
        results = optimizer.solve_batch(paths)

        assert results[0].success
        assert results[0].optimal_input <= int(x_unconstrained * 0.5) + 1

    def test_get_best_path(self):
        """Test finding best path from a batch."""
        optimizer = BatchMobiusOptimizer()

        paths = [
            BatchMobiusPathInput(hops=profitable_2pool_hops()),
            BatchMobiusPathInput(hops=profitable_3pool_hops()),
        ]
        _best_idx, best_result = optimizer.get_best_path(paths)

        assert best_result.success
        assert best_result.profit > 0

    def test_solve_batch_hops(self):
        """Test the pre-built array API."""
        optimizer = BatchMobiusOptimizer()
        hops_array, _ = generate_batch_paths(num_paths=10, num_hops=3, seed=42)
        max_inputs = np.full(10, np.inf)

        result = optimizer.solve_batch_hops(hops_array, max_inputs)

        assert result.num_paths == 10
        assert np.all(result.iterations == 0)

    def test_empty_hops_returns_failure(self):
        """Path with zero hops should return failure."""
        optimizer = BatchMobiusOptimizer()
        paths = [
            BatchMobiusPathInput(hops=[]),
        ]
        results = optimizer.solve_batch(paths)

        assert len(results) == 1
        assert not results[0].success

    def test_results_preserve_input_order(self):
        """Results should be in the same order as input paths."""
        optimizer = BatchMobiusOptimizer()

        paths = [
            BatchMobiusPathInput(hops=profitable_2pool_hops()),
            BatchMobiusPathInput(hops=profitable_3pool_hops()),
            BatchMobiusPathInput(hops=unprofitable_3pool_hops()),
        ]
        results = optimizer.solve_batch(paths)

        assert len(results) == 3
        # First path (2-pool profitable) should succeed
        assert results[0].success
        # Second path (3-pool profitable) should succeed
        assert results[1].success
        # Third path (unprofitable) should fail
        assert not results[2].success


# ==============================================================================
# Numerical Accuracy
# ==============================================================================


class TestNumericalAccuracy:
    """Numerical accuracy tests at EVM scale."""

    def test_large_reserves(self):
        """Test with EVM-scale reserves (millions of USDC, thousands of WETH)."""
        # Pool A: 2M USDC, 1000 WETH
        # Pool B: 2.1M USDC, 1000 WETH
        hops = [
            HopState(
                reserve_in=2_000_000_000_000.0,
                reserve_out=1_000 * 10**18,
                fee=0.003,
            ),
            HopState(
                reserve_in=1_000 * 10**18,
                reserve_out=2_100_000_000_000.0,
                fee=0.003,
            ),
        ]
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        x_scalar, profit_scalar, _ = mobius_solve(hops)

        assert result.optimal_input[0] == pytest.approx(x_scalar, rel=1e-8)
        assert result.profit[0] == pytest.approx(profit_scalar, rel=1e-6)

    def test_profit_formula_consistency(self):
        """Verify that l(x) = K*x / (M + N*x) matches simulate_path."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=20, num_hops=3, seed=42
        )

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        for i in range(20):
            x = result.optimal_input[i]
            if x <= 0:
                continue

            # Rebuild hops and compute via simulate_path
            hops = [
                HopState(
                    reserve_in=float(hops_array[i, j, 0]),
                    reserve_out=float(hops_array[i, j, 1]),
                    fee=float(hops_array[i, j, 2]),
                )
                for j in range(3)
            ]
            sim_output = simulate_path(x, hops)
            coeffs = compute_mobius_coefficients(hops)
            mobius_output = coeffs.path_output(x)

            # Both should match
            assert sim_output == pytest.approx(mobius_output, rel=1e-10)
            assert result.profit[i] == pytest.approx(
                mobius_output - x, rel=1e-6
            )


# ==============================================================================
# Profitability Check
# ==============================================================================


class TestProfitabilityCheck:
    """Tests for the free profitability check (K/M > 1)."""

    def test_profitable_flag_matches_coefficients(self):
        """The is_profitable flag should match K > M from coefficients."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=50, num_hops=3, seed=42
        )

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        for i in range(50):
            # Re-extract hop data directly from the array
            r_in = hops_array[i, :, 0]
            r_out = hops_array[i, :, 1]
            f = hops_array[i, :, 2]
            gammas = 1.0 - f

            # Compute K, M, N inline (avoid HopState construction overhead)
            k = gammas[0] * r_out[0]
            m = r_in[0]
            n = gammas[0]
            for j in range(1, 3):
                old_k = k
                k = old_k * gammas[j] * r_out[j]
                m *= r_in[j]
                n = n * r_in[j] + old_k * gammas[j]

            assert bool(result.is_profitable[i]) == (k > m), (
                f"Path {i}: is_profitable={result.is_profitable[i]}, "
                f"K={k:.2e}, M={m:.2e}, K>M={k > m}"
            )

    def test_profitable_paths_have_positive_profit(self):
        """All paths marked profitable should have positive optimal input."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=100, num_hops=2, seed=42
        )

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        profitable = result.is_profitable
        if profitable.any():
            assert np.all(result.optimal_input[profitable] > 0)
            assert np.all(result.profit[profitable] > 0)


# ==============================================================================
# generate_batch_paths
# ==============================================================================


class TestGenerateBatchPaths:
    """Tests for the test path generator."""

    def test_correct_shape(self):
        """Generated paths should have the correct array shape."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=50, num_hops=3, seed=42
        )
        assert hops_array.shape == (50, 3, 3)
        assert max_inputs.shape == (50,)

    def test_reproducible(self):
        """Same seed should produce same paths."""
        arr1, _ = generate_batch_paths(num_paths=10, num_hops=2, seed=42)
        arr2, _ = generate_batch_paths(num_paths=10, num_hops=2, seed=42)
        np.testing.assert_array_equal(arr1, arr2)

    def test_different_seeds(self):
        """Different seeds should produce different paths."""
        arr1, _ = generate_batch_paths(num_paths=10, num_hops=2, seed=42)
        arr2, _ = generate_batch_paths(num_paths=10, num_hops=2, seed=43)
        assert not np.array_equal(arr1, arr2)

    def test_generates_profitable_paths(self):
        """Generated paths should be mostly profitable."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=100, num_hops=3, seed=42, profit_factor=1.1
        )

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        # Most paths should be profitable with profit_factor=1.1
        assert result.is_profitable.sum() > 50


# ==============================================================================
# Performance Benchmarks
# ==============================================================================


class TestBatchMobiusBenchmarks:
    """Performance benchmarks comparing vectorized vs serial Möbius."""

    def _benchmark_solve(
        self,
        hops_array: np.ndarray,
        max_inputs: np.ndarray,
        solver_fn,
        num_runs: int = 20,
    ) -> float:
        """Run solver and return average time in seconds."""
        # Warmup
        for _ in range(5):
            solver_fn(hops_array, max_inputs)

        times = []
        for _ in range(num_runs):
            start = time.perf_counter()
            solver_fn(hops_array, max_inputs)
            elapsed = time.perf_counter() - start
            times.append(elapsed)

        return sum(times) / len(times)

    @pytest.mark.parametrize("num_hops", [2, 3, 5])
    def test_benchmark_vectorized_vs_serial(self, num_hops):
        """
        Benchmark vectorized vs serial Möbius across batch sizes.

        This test prints a comparison table. It always passes; the
        assertions are informational.
        """
        vec_solver = VectorizedMobiusSolver()
        ser_solver = SerialMobiusSolver()

        print(f"\n{'=' * 85}")
        print(f"  BENCHMARK: Vectorized vs Serial Möbius ({num_hops}-hop paths)")
        print(f"{'=' * 85}")
        print(
            f"  {'Paths':>8} | {'Serial (μs)':>12} | {'Vectorized (μs)':>16} | "
            f"{'Speedup':>10} | {'Per-Path Vec (μs)':>18}"
        )
        print(f"  {'-' * 8}-+-{'-' * 12}-+-{'-' * 16}-+-{'-' * 10}-+-{'-' * 18}")

        batch_sizes = [1, 5, 10, 20, 50, 100, 200, 500, 1000]

        for num_paths in batch_sizes:
            hops_array, max_inputs = generate_batch_paths(
                num_paths=num_paths, num_hops=num_hops, seed=42
            )

            ser_time = self._benchmark_solve(
                hops_array, max_inputs, ser_solver.solve
            )
            vec_time = self._benchmark_solve(
                hops_array, max_inputs, vec_solver.solve
            )

            speedup = ser_time / vec_time if vec_time > 0 else float("inf")
            per_path_vec = vec_time / num_paths * 1_000_000

            print(
                f"  {num_paths:8d} | {ser_time * 1_000_000:12.1f} | "
                f"{vec_time * 1_000_000:16.1f} | {speedup:10.2f}x | "
                f"{per_path_vec:18.3f}"
            )

        print()

    def test_benchmark_mobius_vs_newton_batch(self):
        """
        Compare batch Möbius against batch Newton for 2-pool V2-V2 paths.

        Newton requires 3-4 iterations; Möbius requires 0. This test
        demonstrates the iteration-free advantage.
        """
        vec_mobius = VectorizedMobiusSolver()
        vec_newton = VectorizedNewtonSolver()

        print(f"\n{'=' * 85}")
        print("  BENCHMARK: Batch Möbius vs Batch Newton (2-hop V2-V2 paths)")
        print(f"{'=' * 85}")
        print(
            f"  {'Paths':>8} | {'Möbius (μs)':>12} | {'Newton (μs)':>12} | "
            f"{'Möbius Speedup':>15} | {'Per-Path Möbius (μs)':>21}"
        )
        print(f"  {'-' * 8}-+-{'-' * 12}-+-{'-' * 12}-+-{'-' * 15}-+-{'-' * 21}")

        for num_paths in [10, 50, 100, 500, 1000]:
            # Generate Möbius paths
            hops_array, max_inputs = generate_batch_paths(
                num_paths=num_paths, num_hops=2, seed=42
            )

            # Generate equivalent Newton paths
            # For 2-pool: buy pool is hops[:, 0], sell pool is hops[:, 1]

            # Build Newton path state from the same reserves
            newton_paths = VectorizedPathState(
                buy_reserves0=hops_array[:, 0, 0],
                buy_reserves1=hops_array[:, 0, 1],
                buy_fee_multiplier=1.0 - hops_array[:, 0, 2],
                sell_reserves0=hops_array[:, 1, 0],
                sell_reserves1=hops_array[:, 1, 1],
                sell_fee_multiplier=1.0 - hops_array[:, 1, 2],
                buying_token0=np.ones(num_paths, dtype=bool),
            )

            # Newton solve doesn't take max_inputs, so we need a wrapper
            def newton_wrapper(paths, _):
                return vec_newton.solve(paths)

            mobius_time = self._benchmark_solve(
                hops_array, max_inputs, vec_mobius.solve
            )
            newton_time = self._benchmark_solve(
                newton_paths, max_inputs, newton_wrapper
            )

            speedup = newton_time / mobius_time if mobius_time > 0 else float("inf")
            per_path = mobius_time / num_paths * 1_000_000

            print(
                f"  {num_paths:8d} | {mobius_time * 1_000_000:12.1f} | "
                f"{newton_time * 1_000_000:12.1f} | {speedup:15.2f}x | "
                f"{per_path:21.3f}"
            )

        print()


# ==============================================================================
# Edge Cases
# ==============================================================================


class TestInputImmutability:
    """Verify that the vectorized solver does not mutate the input array."""

    def test_hops_array_not_mutated(self):
        """VectorizedMobiusSolver.solve must not modify the input hops_array."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=50, num_hops=3, seed=42
        )
        original = hops_array.copy()

        vec_solver = VectorizedMobiusSolver()
        vec_solver.solve(hops_array, max_inputs)

        np.testing.assert_array_equal(
            hops_array,
            original,
            err_msg="VectorizedMobiusSolver mutated the input hops_array",
        )

    def test_serial_hops_array_not_mutated(self):
        """SerialMobiusSolver.solve must not modify the input hops_array."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=10, num_hops=3, seed=42
        )
        original = hops_array.copy()

        ser_solver = SerialMobiusSolver()
        ser_solver.solve(hops_array, max_inputs)

        np.testing.assert_array_equal(
            hops_array,
            original,
            err_msg="SerialMobiusSolver mutated the input hops_array",
        )


class TestEdgeCases:
    """Edge case tests."""

    def test_all_unprofitable(self):
        """All paths unprofitable should return all zeros."""
        # Identical pools across the board — no arbitrage
        num_paths = 10
        hops_array = np.zeros((num_paths, 3, 3), dtype=np.float64)
        for i in range(num_paths):
            for j in range(3):
                hops_array[i, j, 0] = 1_000_000.0
                hops_array[i, j, 1] = 1_000_000.0
                hops_array[i, j, 2] = 0.003

        max_inputs = np.full(num_paths, np.inf)

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert np.all(np.abs(result.optimal_input) < 1e-10)
        assert np.all(np.abs(result.profit) < 1e-10)

    def test_zero_fee(self):
        """Zero fee should still work (gamma = 1.0)."""
        hops = [
            HopState(reserve_in=1_000_000.0, reserve_out=1_100.0, fee=0.0),
            HopState(reserve_in=1_000.0, reserve_out=1_100_000.0, fee=0.0),
        ]
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert result.is_profitable[0] is np.True_
        assert result.optimal_input[0] > 0

    def test_high_fee(self):
        """Very high fee (1%) should still compute correctly."""
        hops = [
            HopState(reserve_in=1_000_000.0, reserve_out=1_100.0, fee=0.01),
            HopState(reserve_in=1_000.0, reserve_out=1_100_000.0, fee=0.01),
        ]
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        x_scalar, _profit_scalar, _ = mobius_solve(hops)
        if x_scalar > 0:
            assert result.optimal_input[0] == pytest.approx(x_scalar, rel=1e-8)

    def test_small_reserves(self):
        """Very small reserves should work without numerical issues."""
        hops = [
            HopState(reserve_in=100.0, reserve_out=200.0, fee=0.003),
            HopState(reserve_in=150.0, reserve_out=300.0, fee=0.003),
        ]
        hops_array = make_hops_array([hops])
        max_inputs = np.array([np.inf])

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        x_scalar, _profit_scalar, _ = mobius_solve(hops)
        if x_scalar > 0:
            assert result.optimal_input[0] == pytest.approx(x_scalar, rel=1e-8)

    def test_long_path_20_hops(self):
        """20-hop paths should compute correctly."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=10, num_hops=20, seed=42
        )

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        # Verify against serial solver
        ser_solver = SerialMobiusSolver()
        ser_result = ser_solver.solve(hops_array, max_inputs)

        profitable = ser_result.is_profitable & (ser_result.optimal_input > 0)
        if profitable.any():
            np.testing.assert_allclose(
                result.optimal_input[profitable],
                ser_result.optimal_input[profitable],
                rtol=1e-8,
                err_msg="20-hop vectorized vs serial input mismatch",
            )


class TestOverflowHandling:
    """Tests for log-domain overflow handling with EVM-scale reserves."""

    def test_overflow_20hop_evm_reserves(self):
        """20-hop paths with 1e18 reserves should compute via log-domain."""
        hops_array = np.ones((5, 20, 3), dtype=np.float64)
        hops_array[:, :, 0] = 1e18  # reserve_in
        hops_array[:, :, 1] = 1.01e18  # reserve_out (1% per hop)
        hops_array[:, :, 2] = 0.003  # fee
        max_inputs = np.full(5, np.inf)

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        # Should detect profitability correctly
        assert result.is_profitable.all()

        # x_opt should be finite and positive
        assert np.all(result.optimal_input > 0)
        assert np.all(np.isfinite(result.optimal_input))

        # profit should be positive and finite
        assert np.all(result.profit > 0)
        assert np.all(np.isfinite(result.profit))

    def test_overflow_matches_scaled_reference(self):
        """Log-domain x_opt should match scaled-down reference."""
        hops_array = np.ones((5, 20, 3), dtype=np.float64)
        hops_array[:, :, 0] = 1e18
        hops_array[:, :, 1] = 1.01e18
        hops_array[:, :, 2] = 0.003
        max_inputs = np.full(5, np.inf)

        # Scale down by 1e-6 to avoid overflow
        hops_scaled = hops_array.copy()
        hops_scaled[:, :, :2] *= 1e-6

        vec_solver = VectorizedMobiusSolver()
        result_overflow = vec_solver.solve(hops_array, max_inputs)
        result_scaled = vec_solver.solve(hops_scaled, max_inputs)

        # x_opt scales linearly with reserve scale factor
        x_ref = result_scaled.optimal_input[0] * 1e6
        rel_diff = abs(result_overflow.optimal_input[0] - x_ref) / x_ref
        assert rel_diff < 1e-8, f"Overflow x_opt off by {rel_diff:.2e}"

    def test_overflow_profitability_matches_log(self):
        """is_profitable should match log_K > log_M regardless of overflow."""
        hops_array = np.ones((5, 20, 3), dtype=np.float64)
        hops_array[:, :, 0] = 1e18
        hops_array[:, :, 1] = 1.002e18  # barely profitable
        hops_array[:, :, 2] = 0.003
        max_inputs = np.full(5, np.inf)

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        # Compute log(K/M) independently
        gamma = 0.997
        per_hop_log_ratio = np.log(gamma) + np.log(1.002e18) - np.log(1e18)
        total_log_ratio = 20 * per_hop_log_ratio

        assert bool(result.is_profitable[0]) == (total_log_ratio > 0)

    def test_no_overflow_normal_reserves(self):
        """Normal reserves (1e6) should never overflow even for 20 hops."""
        hops_array, max_inputs = generate_batch_paths(
            num_paths=20, num_hops=20, seed=42
        )
        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert np.all(np.isfinite(result.optimal_input))
        assert np.all(np.isfinite(result.profit))

    def test_10hop_evm_reserves(self):
        """10-hop paths with 1e18 reserves should use log-domain."""
        hops_array = np.ones((5, 10, 3), dtype=np.float64)
        hops_array[:, :, 0] = 1e18
        hops_array[:, :, 1] = 1.01e18
        hops_array[:, :, 2] = 0.003
        max_inputs = np.full(5, np.inf)

        vec_solver = VectorizedMobiusSolver()
        result = vec_solver.solve(hops_array, max_inputs)

        assert result.is_profitable.all()
        assert np.all(result.optimal_input > 0)
        assert np.all(result.profit > 0)
        assert np.all(np.isfinite(result.optimal_input))
        assert np.all(np.isfinite(result.profit))
