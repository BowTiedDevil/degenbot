"""Tests for batched RustPoolCache.solve_batch().

Validates that multiple arbitrage paths can be solved in a single
Python → Rust → Python round-trip, amortizing the PyO3 bridge overhead
across all paths. The GIL is released once for the entire batch, instead
of once per path.

Architecture:
- RustPoolCache.solve_batch(paths) takes a list of paths (each a list of pool IDs)
- All paths are looked up, assembled, and solved inside a single py.detach() call
- Returns a list of RustArbResult objects, one per path
- Any path with missing pool IDs is returned as not_supported
- RustArbSolver.solve_raw_batch(all_hops_flat) similarly batches flat int arrays
"""

from fractions import Fraction

import pytest

import degenbot.degenbot_rs as rs_mobius
from degenbot.arbitrage.optimizers.solver import ArbSolver

from .conftest import (
    FEE_0_05_PCT,
    FEE_0_3_PCT,
    FEE_1_PCT,
    USDC_1_5M,
    USDC_2M,
    WETH_800,
    WETH_1000,
)

GAMMA_03, FEE_DENOM_03 = FEE_0_3_PCT.denominator - FEE_0_3_PCT.numerator, FEE_0_3_PCT.denominator
GAMMA_005, FEE_DENOM_005 = FEE_0_05_PCT.denominator - FEE_0_05_PCT.numerator, FEE_0_05_PCT.denominator


# ---------------------------------------------------------------------------
# RustPoolCache.solve_batch
# ---------------------------------------------------------------------------


class TestRustPoolCacheSolveBatch:
    """Tests for batched solve on RustPoolCache."""

    def test_batch_single_path(self):
        """Batch with one path should return same result as single solve."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        single = cache.solve([0, 1])
        batch = cache.solve_batch([[0, 1]])

        assert len(batch) == 1
        assert batch[0].supported == single.supported
        assert batch[0].success == single.success
        assert int(batch[0].optimal_input_int) == int(single.optimal_input_int)
        assert int(batch[0].profit_int) == int(single.profit_int)

    def test_batch_multiple_paths(self):
        """Batch with multiple paths should return correct results for each."""
        cache = rs_mobius.RustPoolCache()
        # Path 1: USDC/WETH arb
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        # Path 2: reversed — buy WETH cheap, sell expensive
        cache.insert(2, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(3, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        results = cache.solve_batch([[0, 1], [2, 3]])

        assert len(results) == 2
        # Both paths should be profitable
        assert results[0].success
        assert results[1].success

        # Verify each matches individual solve
        single_0 = cache.solve([0, 1])
        single_1 = cache.solve([2, 3])

        assert int(results[0].optimal_input_int) == int(single_0.optimal_input_int)
        assert int(results[0].profit_int) == int(single_0.profit_int)
        assert int(results[1].optimal_input_int) == int(single_1.optimal_input_int)
        assert int(results[1].profit_int) == int(single_1.profit_int)

    def test_batch_matches_individual_solves_evm_exact(self):
        """Every batch result must be EVM-exact, matching individual solves."""
        cache = rs_mobius.RustPoolCache()
        # Register multiple pools with varied reserves
        for i in range(10):
            factor = 1.0 + i * 0.1
            cache.insert(
                i,
                int(USDC_1_5M * factor),
                int(WETH_800 * factor),
                GAMMA_03,
                FEE_DENOM_03,
            )

        # Build 5 paths: each uses two consecutive pools
        paths = [[i, i + 1] for i in range(9)]

        batch_results = cache.solve_batch(paths)

        for i, (path, result) in enumerate(zip(paths, batch_results, strict=True)):
            if not result.success:
                continue

            # Compare against individual solve
            single = cache.solve(path)
            assert int(result.optimal_input_int) == int(single.optimal_input_int), (
                f"Path {i}: batch input != single input"
            )
            assert int(result.profit_int) == int(single.profit_int), (
                f"Path {i}: batch profit != single profit"
            )

            # EVM-exact verification
            factor_0 = 1.0 + path[0] * 0.1
            factor_1 = 1.0 + path[1] * 0.1
            hops = [
                rs_mobius.RustIntHopState(
                    int(USDC_1_5M * factor_0), int(WETH_800 * factor_0), 997, 1000
                ),
                rs_mobius.RustIntHopState(
                    int(USDC_1_5M * factor_1), int(WETH_800 * factor_1), 997, 1000
                ),
            ]
            evm_output = int(
                rs_mobius.py_int_simulate_path(int(result.optimal_input_int), hops)
            )
            assert evm_output - int(result.optimal_input_int) == int(result.profit_int), (
                f"Path {i}: EVM verification failed"
            )

    def test_batch_with_missing_pool(self):
        """Path with a missing pool ID should return not_supported."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        # Pool 99 is not registered

        results = cache.solve_batch([[0, 1], [0, 99]])

        assert len(results) == 2
        assert results[0].success  # Valid path
        assert not results[1].supported  # Missing pool → not supported

    def test_batch_with_not_profitable(self):
        """Paths that are not profitable should have success=False."""
        cache = rs_mobius.RustPoolCache()
        # Profitable path
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        # Unprofitable path (balanced reserves, same prices)
        cache.insert(2, USDC_2M, WETH_1000, GAMMA_03, FEE_DENOM_03)
        cache.insert(3, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        results = cache.solve_batch([[0, 1], [2, 3]])

        assert results[0].success
        assert not results[1].success

    def test_batch_with_too_few_pools(self):
        """Path with <2 pools should return not_supported."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)

        results = cache.solve_batch([[0, 1], [0]])

        assert len(results) == 2
        # First path has missing pool 1
        assert not results[0].supported
        # Second path has only 1 pool
        assert not results[1].supported

    def test_batch_max_input(self):
        """max_input constraint should be applied to all paths."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        results = cache.solve_batch([[0, 1]], max_input=1000.0)

        assert len(results) == 1
        assert int(results[0].optimal_input_int) <= 1000

    def test_batch_empty_paths(self):
        """Empty batch should return empty list."""
        cache = rs_mobius.RustPoolCache()
        results = cache.solve_batch([])
        assert results == []

    def test_batch_mixed_fees(self):
        """Batch with different fee tiers should work correctly."""
        cache = rs_mobius.RustPoolCache()
        # Path 1: 0.3% / 0.3%
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        # Path 2: 0.05% / 0.3%
        cache.insert(2, USDC_1_5M, WETH_800, GAMMA_005, FEE_DENOM_005)
        cache.insert(3, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        results = cache.solve_batch([[0, 1], [2, 3]])

        # Both should be profitable (0.05% fee is even more profitable)
        assert results[0].success
        assert results[1].success

        # 0.05% path should have more profit (lower fees)
        assert int(results[1].profit_int) > int(results[0].profit_int)

    def test_batch_3hop_paths(self):
        """Batch with 3-hop paths should work correctly."""
        cache = rs_mobius.RustPoolCache()
        gamma, denom = 997, 1000

        cache.insert(0, 2_000_000, 2_100_000, gamma, denom)
        cache.insert(1, 2_000_000, 2_050_000, gamma, denom)
        cache.insert(2, 2_050_000, 2_000_000, gamma, denom)
        cache.insert(3, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(4, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        results = cache.solve_batch([[0, 1, 2], [3, 4]])

        assert len(results) == 2
        assert results[0].success
        assert results[1].success

        # Verify 3-hop against individual
        single_3hop = cache.solve([0, 1, 2])
        assert int(results[0].optimal_input_int) == int(single_3hop.optimal_input_int)
        assert int(results[0].profit_int) == int(single_3hop.profit_int)


# ---------------------------------------------------------------------------
# RustArbSolver.solve_raw_batch
# ---------------------------------------------------------------------------


class TestRustArbSolverSolveRawBatch:
    """Tests for batched solve_raw on RustArbSolver."""

    def test_batch_single_path(self):
        """Batch with one path should return same result as single solve_raw."""
        solver = rs_mobius.RustArbSolver()
        flat = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]

        single = solver.solve_raw(flat)
        batch = solver.solve_raw_batch([flat])

        assert len(batch) == 1
        assert batch[0].success == single.success
        assert int(batch[0].optimal_input_int) == int(single.optimal_input_int)
        assert int(batch[0].profit_int) == int(single.profit_int)

    def test_batch_multiple_paths(self):
        """Batch with multiple paths should return correct results."""
        solver = rs_mobius.RustArbSolver()

        # Path 1
        flat_1 = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                  WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]
        # Path 2 (different reserves, still profitable)
        flat_2 = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                  WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]

        results = solver.solve_raw_batch([flat_1, flat_2])

        assert len(results) == 2
        assert results[0].success
        assert results[1].success

        # Verify against individual calls
        single_1 = solver.solve_raw(flat_1)
        single_2 = solver.solve_raw(flat_2)

        assert int(results[0].optimal_input_int) == int(single_1.optimal_input_int)
        assert int(results[0].profit_int) == int(single_1.profit_int)
        assert int(results[1].optimal_input_int) == int(single_2.optimal_input_int)
        assert int(results[1].profit_int) == int(single_2.profit_int)

    def test_batch_mixed_hop_counts(self):
        """Batch can have 2-hop and 3-hop paths mixed together."""
        solver = rs_mobius.RustArbSolver()

        flat_2hop = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                     WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]
        flat_3hop = [2_000_000, 2_100_000, 997, 1000,
                     2_000_000, 2_050_000, 997, 1000,
                     2_050_000, 2_000_000, 997, 1000]

        results = solver.solve_raw_batch([flat_2hop, flat_3hop])

        assert len(results) == 2
        assert results[0].success
        assert results[1].success

    def test_batch_not_profitable(self):
        """Unprofitable paths in batch should have success=False."""
        solver = rs_mobius.RustArbSolver()

        profitable = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                      WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]
        # Same prices, balanced reserves → unprofitable after fees
        unprofitable = [USDC_2M, WETH_1000, GAMMA_03, FEE_DENOM_03,
                        WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]

        results = solver.solve_raw_batch([profitable, unprofitable])

        assert results[0].success
        assert not results[1].success

    def test_batch_invalid_path_in_batch(self):
        """Path with <2 hops should return not_supported."""
        solver = rs_mobius.RustArbSolver()

        valid = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                 WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]
        # Only 4 ints = 1 hop (too few)
        invalid = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03]

        results = solver.solve_raw_batch([valid, invalid])

        assert results[0].success
        assert not results[1].supported

    def test_batch_empty(self):
        """Empty batch should return empty list."""
        solver = rs_mobius.RustArbSolver()
        results = solver.solve_raw_batch([])
        assert results == []

    def test_batch_max_input(self):
        """max_input applies to all paths in the batch."""
        solver = rs_mobius.RustArbSolver()
        flat = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03,
                WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03]

        results = solver.solve_raw_batch([flat], max_input=1000.0)

        assert len(results) == 1
        assert int(results[0].optimal_input_int) <= 1000


# ---------------------------------------------------------------------------
# ArbSolver integration with batched cache solve
# ---------------------------------------------------------------------------


class TestArbSolverSolveBatchCached:
    """Tests for ArbSolver.solve_cached_batch()."""

    def test_batch_matches_individual(self):
        """Batched results must match individual solve_cached calls."""
        solver = ArbSolver()

        # Register pools via the solver's cache
        paths = []
        for i in range(5):
            factor = 1.0 + i * 0.05
            pid0 = solver.register_pool(
                int(USDC_1_5M * factor), int(WETH_800 * factor), FEE_0_3_PCT
            )
            pid1 = solver.register_pool(
                int(WETH_1000 * factor), int(USDC_2M * factor), FEE_0_3_PCT
            )
            paths.append([pid0, pid1])

        # Batch solve
        batch_results = solver.solve_cached_batch(paths)

        assert len(batch_results) == len(paths)

        # Compare against individual solves
        for i, path in enumerate(paths):
            try:
                single = solver.solve_cached(path)
                assert batch_results[i].profit > 0, f"Path {i}: expected profitable"
                assert batch_results[i].optimal_input == single.optimal_input, (
                    f"Path {i}: batch input != single input"
                )
                assert batch_results[i].profit == single.profit, (
                    f"Path {i}: batch profit != single profit"
                )
            except Exception:
                # Not profitable in individual either
                assert batch_results[i].profit == 0, f"Path {i}: expected not profitable"

    def test_batch_with_not_profitable(self):
        """Batch correctly handles not-profitable paths."""
        solver = ArbSolver()

        # Profitable
        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        # Unprofitable (balanced reserves, same prices)
        pid2 = solver.register_pool(USDC_2M, WETH_1000, FEE_0_3_PCT)
        pid3 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        results = solver.solve_cached_batch([[pid0, pid1], [pid2, pid3]])

        assert results[0].profit > 0
        assert results[1].profit == 0  # Not profitable


# ---------------------------------------------------------------------------
# RustPoolCache registered path tests
# ---------------------------------------------------------------------------


class TestRustPoolCacheRegisteredPaths:
    """Tests for register_path + solve_registered on RustPoolCache."""

    def test_register_and_solve_single_path(self):
        """Register a path and solve it by path ID."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])
        assert isinstance(path_id, int)
        assert path_id >= 1

        results = cache.solve_registered([path_id])
        assert len(results) == 1
        assert results[0].success
        assert results[0].supported

    def test_registered_matches_individual(self):
        """Registered path results must match individual cache.solve."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])

        # Compare against individual cache.solve
        single = cache.solve([0, 1])
        results = cache.solve_registered([path_id])

        assert int(results[0].optimal_input_int) == int(single.optimal_input_int)
        assert int(results[0].profit_int) == int(single.profit_int)

    def test_registered_matches_batch(self):
        """Registered path results must match cache.solve_batch."""
        cache = rs_mobius.RustPoolCache()

        for i in range(10):
            factor = 1.0 + i * 0.05
            cache.insert(i * 2, int(USDC_1_5M * factor), int(WETH_800 * factor), GAMMA_03, FEE_DENOM_03)
            cache.insert(i * 2 + 1, int(WETH_1000 * factor), int(USDC_2M * factor), GAMMA_03, FEE_DENOM_03)

        path_ids = []
        for i in range(10):
            pid = cache.register_path([i * 2, i * 2 + 1])
            path_ids.append(pid)

        # Compare registered solve vs batch
        pool_paths = [[i * 2, i * 2 + 1] for i in range(10)]
        batch_results = cache.solve_batch(pool_paths)
        registered_results = cache.solve_registered(path_ids)

        for i in range(10):
            if not batch_results[i].success:
                continue
            assert int(registered_results[i].optimal_input_int) == int(batch_results[i].optimal_input_int), (
                f"Path {i}: registered input != batch input"
            )
            assert int(registered_results[i].profit_int) == int(batch_results[i].profit_int), (
                f"Path {i}: registered profit != batch profit"
            )

    def test_registered_multiple_paths(self):
        """Multiple registered paths solve correctly in batch."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        cache.insert(2, USDC_1_5M, WETH_800, GAMMA_005, FEE_DENOM_005)
        cache.insert(3, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id_1 = cache.register_path([0, 1])
        path_id_2 = cache.register_path([2, 3])

        results = cache.solve_registered([path_id_1, path_id_2])

        assert len(results) == 2
        assert results[0].success
        assert results[1].success
        # 0.05% fee path should be more profitable
        assert int(results[1].profit_int) > int(results[0].profit_int)

    def test_registered_3hop_path(self):
        """3-hop registered path solves correctly."""
        cache = rs_mobius.RustPoolCache()
        gamma, denom = 997, 1000
        cache.insert(0, 2_000_000, 2_100_000, gamma, denom)
        cache.insert(1, 2_000_000, 2_050_000, gamma, denom)
        cache.insert(2, 2_050_000, 2_000_000, gamma, denom)

        path_id = cache.register_path([0, 1, 2])
        results = cache.solve_registered([path_id])

        assert len(results) == 1
        assert results[0].success

        # Verify EVM-exact
        single = cache.solve([0, 1, 2])
        assert int(results[0].optimal_input_int) == int(single.optimal_input_int)
        assert int(results[0].profit_int) == int(single.profit_int)

    def test_registered_unknown_path_id(self):
        """Unknown path ID returns not_supported."""
        cache = rs_mobius.RustPoolCache()
        results = cache.solve_registered([9999])
        assert len(results) == 1
        assert not results[0].supported

    def test_registered_empty_path_ids(self):
        """Empty path ID list returns empty results."""
        cache = rs_mobius.RustPoolCache()
        results = cache.solve_registered([])
        assert results == []

    def test_register_path_missing_pool(self):
        """Registering path with missing pool creates empty hops (not supported)."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        # Pool 1 not inserted yet

        path_id = cache.register_path([0, 1])
        results = cache.solve_registered([path_id])
        assert not results[0].supported

        # After inserting the missing pool and updating
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        cache.update_path(path_id)
        results = cache.solve_registered([path_id])
        assert results[0].success

    def test_update_path_after_pool_state_change(self):
        """update_path() re-resolves pool states from cache."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])

        # Get baseline result
        results_before = cache.solve_registered([path_id])

        # Update pool state (different reserves)
        cache.insert(0, USDC_2M, WETH_1000, GAMMA_03, FEE_DENOM_03)
        updated = cache.update_path(path_id)
        assert updated

        # Result should change
        results_after = cache.solve_registered([path_id])
        assert int(results_after[0].optimal_input_int) != int(results_before[0].optimal_input_int)

    def test_update_all_paths(self):
        """update_all_paths() re-resolves all registered paths."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        cache.insert(2, USDC_1_5M, WETH_800, GAMMA_005, FEE_DENOM_005)
        cache.insert(3, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        pid0 = cache.register_path([0, 1])
        pid1 = cache.register_path([2, 3])

        # Update pool 0
        cache.insert(0, USDC_2M, WETH_1000, GAMMA_03, FEE_DENOM_03)
        updated = cache.update_all_paths()
        assert updated == 2

        # Results should reflect new state
        results = cache.solve_registered([pid0, pid1])
        assert len(results) == 2

    def test_remove_path(self):
        """remove_path() removes a registered path."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])

        results = cache.solve_registered([path_id])
        assert results[0].success

        removed = cache.remove_path(path_id)
        assert removed

        results = cache.solve_registered([path_id])
        assert not results[0].supported

    def test_register_path_too_few_pools(self):
        """register_path with <2 pools should raise ValueError."""
        cache = rs_mobius.RustPoolCache()
        with pytest.raises(ValueError, match="at least 2"):
            cache.register_path([0])

    def test_registered_max_input(self):
        """max_input constraint works with registered paths."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])

        results_unconstrained = cache.solve_registered([path_id])
        results_constrained = cache.solve_registered([path_id], max_input=1000.0)

        assert int(results_constrained[0].optimal_input_int) <= 1000
        assert int(results_unconstrained[0].optimal_input_int) > 1000

    def test_registered_evm_exact_verification(self):
        """EVM-exact verification for registered path results."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])
        results = cache.solve_registered([path_id])

        assert results[0].success
        hops = [
            rs_mobius.RustIntHopState(USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03),
            rs_mobius.RustIntHopState(WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03),
        ]
        evm_output = int(
            rs_mobius.py_int_simulate_path(int(results[0].optimal_input_int), hops)
        )
        assert evm_output - int(results[0].optimal_input_int) == int(results[0].profit_int)

    def test_registered_ints_basic(self):
        """solve_registered_ints returns flat [input, profit, ...] list."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])
        results = cache.solve_registered_ints([path_id])

        # Returns flat [optimal_input, profit] per path
        assert len(results) == 2
        assert results[0] > 0  # optimal_input
        assert results[1] > 0  # profit

    def test_registered_ints_matches_registered(self):
        """solve_registered_ints values match solve_registered."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])

        full_results = cache.solve_registered([path_id])
        int_results = cache.solve_registered_ints([path_id])

        # Flat list: [optimal_input, profit]
        assert int(int_results[0]) == int(full_results[0].optimal_input_int)
        assert int(int_results[1]) == int(full_results[0].profit_int)

    def test_registered_ints_not_profitable(self):
        """Not-profitable paths return [0, 0]."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_2M, WETH_1000, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        path_id = cache.register_path([0, 1])
        results = cache.solve_registered_ints([path_id])

        assert results == [0, 0]

    def test_registered_ints_unknown_path(self):
        """Unknown path ID returns [0, 0]."""
        cache = rs_mobius.RustPoolCache()
        results = cache.solve_registered_ints([9999])
        assert results == [0, 0]

    def test_registered_ints_empty(self):
        """Empty path IDs returns empty list."""
        cache = rs_mobius.RustPoolCache()
        results = cache.solve_registered_ints([])
        assert results == []

    def test_registered_ints_multiple_paths(self):
        """Multiple paths return flat list with 2 ints per path."""
        cache = rs_mobius.RustPoolCache()
        cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM_03)
        cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)
        cache.insert(2, USDC_1_5M, WETH_800, GAMMA_005, FEE_DENOM_005)
        cache.insert(3, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM_03)

        pid0 = cache.register_path([0, 1])
        pid1 = cache.register_path([2, 3])

        results = cache.solve_registered_ints([pid0, pid1])
        # 2 paths × 2 ints = 4 elements
        assert len(results) == 4
        assert results[0] > 0  # path 0 input
        assert results[1] > 0  # path 0 profit
        assert results[2] > 0  # path 1 input
        assert results[3] > 0  # path 1 profit
# ---------------------------------------------------------------------------
# ArbSolver registered path tests
# ---------------------------------------------------------------------------


class TestArbSolverRegisteredPaths:
    """Tests for ArbSolver.register_path + solve_registered."""

    def test_register_and_solve(self):
        """Register a path and solve via ArbSolver."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        results = solver.solve_registered([path_id])
        assert len(results) == 1
        assert results[0].profit > 0

    def test_registered_matches_solve_cached(self):
        """Registered path results match solve_cached results."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        cached = solver.solve_cached([pid0, pid1])
        registered = solver.solve_registered([path_id])

        assert registered[0].optimal_input == cached.optimal_input
        assert registered[0].profit == cached.profit

    def test_registered_multiple_paths(self):
        """Multiple registered paths solve via ArbSolver."""
        solver = ArbSolver()

        path_ids = []
        for i in range(5):
            factor = 1.0 + i * 0.05
            pid0 = solver.register_pool(int(USDC_1_5M * factor), int(WETH_800 * factor), FEE_0_3_PCT)
            pid1 = solver.register_pool(int(WETH_1000 * factor), int(USDC_2M * factor), FEE_0_3_PCT)
            rid = solver.register_path([pid0, pid1])
            path_ids.append(rid)

        results = solver.solve_registered(path_ids)
        assert len(results) == 5

        # Verify matches individual calls
        for i, path_id in enumerate(path_ids):
            try:
                single = solver.solve_cached(path_ids)  # noqa: F841
            except Exception:
                pass
            # Each should be profitable
            assert results[i].profit > 0, f"Path {i}: expected profitable"

    def test_registered_not_profitable(self):
        """Registered path with balanced reserves returns profit=0."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_2M, WETH_1000, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        results = solver.solve_registered([path_id])
        assert results[0].profit == 0

    def test_registered_update_path(self):
        """update_path() refreshes path state after pool update."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])
        results_before = solver.solve_registered([path_id])

        # Update pool state
        solver.update_pool(pid0, USDC_2M, WETH_1000, FEE_0_3_PCT)
        solver.update_path(path_id)

        results_after = solver.solve_registered([path_id])
        assert results_after[0].optimal_input != results_before[0].optimal_input

    def test_registered_update_all_paths(self):
        """update_all_paths() refreshes all paths after batch update."""
        solver = ArbSolver()

        path_ids = []
        for i in range(3):
            factor = 1.0 + i * 0.05
            pid0 = solver.register_pool(int(USDC_1_5M * factor), int(WETH_800 * factor), FEE_0_3_PCT)
            pid1 = solver.register_pool(int(WETH_1000 * factor), int(USDC_2M * factor), FEE_0_3_PCT)
            rid = solver.register_path([pid0, pid1])
            path_ids.append(rid)

        updated = solver.update_all_paths()
        assert updated == 3

    def test_registered_remove_path(self):
        """remove_path() removes a registered path."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        results = solver.solve_registered([path_id])
        assert results[0].profit > 0

        removed = solver.remove_path(path_id)
        assert removed

        results = solver.solve_registered([path_id])
        assert results[0].profit == 0

    def test_registered_ints_basic(self):
        """solve_registered_ints returns (optimal_input, profit) tuples."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        results = solver.solve_registered_ints([path_id])
        assert len(results) == 1
        assert isinstance(results[0], tuple)
        assert results[0][0] > 0  # optimal_input
        assert results[0][1] > 0  # profit

    def test_registered_ints_matches_solve_registered(self):
        """solve_registered_ints matches solve_registered values."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        full_results = solver.solve_registered([path_id])
        int_results = solver.solve_registered_ints([path_id])

        assert int_results[0][0] == full_results[0].optimal_input
        assert int_results[0][1] == full_results[0].profit

    def test_registered_ints_not_profitable(self):
        """Not-profitable paths return (0, 0)."""
        solver = ArbSolver()

        pid0 = solver.register_pool(USDC_2M, WETH_1000, FEE_0_3_PCT)
        pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)

        path_id = solver.register_path([pid0, pid1])

        results = solver.solve_registered_ints([path_id])
        assert results[0] == (0, 0)

    def test_registered_ints_unknown_path(self):
        """Unknown path ID returns (0, 0)."""
        solver = ArbSolver()

        results = solver.solve_registered_ints([9999])
        assert results[0] == (0, 0)

    def test_registered_ints_multiple_paths(self):
        """Multiple paths solved via solve_registered_ints."""
        solver = ArbSolver()

        path_ids = []
        for i in range(5):
            f = 1.0 + i * 0.05
            pid0 = solver.register_pool(int(USDC_1_5M * f), int(WETH_800 * f), FEE_0_3_PCT)
            pid1 = solver.register_pool(int(WETH_1000 * f), int(USDC_2M * f), FEE_0_3_PCT)
            rid = solver.register_path([pid0, pid1])
            path_ids.append(rid)

        results = solver.solve_registered_ints(path_ids)
        assert len(results) == 5
        for i, (opt_input, profit) in enumerate(results):
            assert profit > 0, f"Path {i}: expected profitable"

    def test_registered_ints_empty(self):
        """Empty path list returns empty results."""
        solver = ArbSolver()
        results = solver.solve_registered_ints([])
        assert results == []
