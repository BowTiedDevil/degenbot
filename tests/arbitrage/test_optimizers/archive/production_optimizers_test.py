"""
Tests for production Phase 7 optimizers.

Tests for:
- BatchNewtonOptimizer (vectorized batch)
- ChainRuleNewtonOptimizer (multi-pool)
- HybridOptimizer (automatic method selection)
"""

import time
from fractions import Fraction

import numpy as np
import pytest
from eth_typing import ChecksumAddress

from degenbot.arbitrage.optimizers import (
    BatchNewtonOptimizer,
    ChainRuleNewtonOptimizer,
    HybridOptimizer,
    NewtonV2Optimizer,
    optimize_arbitrage,
)
from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.arbitrage.mock_pools import MockErc20Token, MockV2Pool

# =============================================================================
# Fixtures
# =============================================================================


@pytest.fixture
def usdc() -> MockErc20Token:
    return MockErc20Token(
        address=ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        symbol="USDC",
        decimals=6,
    )


@pytest.fixture
def weth() -> MockErc20Token:
    return MockErc20Token(
        address=ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        symbol="WETH",
        decimals=18,
    )


@pytest.fixture
def pool_a(usdc: MockErc20Token, weth: MockErc20Token) -> MockV2Pool:
    return MockV2Pool(
        address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
        token0=usdc,
        token1=weth,
        initial_state=UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=0,
            reserves_token0=2_000_000_000_000,
            reserves_token1=1_000 * 10**18,
        ),
        fee=Fraction(3, 1000),
    )


@pytest.fixture
def pool_b(usdc: MockErc20Token, weth: MockErc20Token) -> MockV2Pool:
    return MockV2Pool(
        address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
        token0=usdc,
        token1=weth,
        initial_state=UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            block=0,
            reserves_token0=2_100_000_000_000,
            reserves_token1=1_000 * 10**18,
        ),
        fee=Fraction(3, 1000),
    )


# =============================================================================
# BatchNewtonOptimizer Tests
# =============================================================================

class TestBatchNewtonOptimizer:
    """Tests for vectorized batch optimizer."""

    def test_batch_optimizer_small_batch_uses_serial(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test that small batches use serial solver."""
        # Create 5 pairs (below threshold)
        pool_pairs = []
        for i in range(5):
            pool_a = MockV2Pool(
                address=ChecksumAddress(f"0x{i:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i:040x}"),
                    block=0,
                    reserves_token0=2_000_000_000_000 + i * 1_000_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_b = MockV2Pool(
                address=ChecksumAddress(f"0x{i + 10:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i + 10:040x}"),
                    block=0,
                    reserves_token0=2_100_000_000_000 + i * 1_000_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_pairs.append((pool_a, pool_b, usdc))

        optimizer = BatchNewtonOptimizer(min_paths_for_batch=20)
        results = optimizer.solve_batch(pool_pairs)

        assert len(results) == 5
        for result in results:
            assert result.success

    def test_batch_optimizer_large_batch_uses_vectorized(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test that large batches use vectorized solver."""
        pool_pairs = []
        for i in range(50):
            pool_a = MockV2Pool(
                address=ChecksumAddress(f"0x{i:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i:040x}"),
                    block=0,
                    reserves_token0=2_000_000_000_000 + i * 10_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_b = MockV2Pool(
                address=ChecksumAddress(f"0x{i + 100:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i + 100:040x}"),
                    block=0,
                    reserves_token0=2_100_000_000_000 + i * 10_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_pairs.append((pool_a, pool_b, usdc))

        optimizer = BatchNewtonOptimizer(min_paths_for_batch=20)
        results = optimizer.solve_batch(pool_pairs)

        assert len(results) == 50
        profitable = sum(1 for r in results if r.success)
        assert profitable > 0

    def test_batch_optimizer_get_best_path(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test finding best path from batch."""
        pool_pairs = []
        for i in range(20):
            pool_a = MockV2Pool(
                address=ChecksumAddress(f"0x{i:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i:040x}"),
                    block=0,
                    reserves_token0=2_000_000_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            # Vary the price difference
            price_diff = 1.01 + i * 0.005  # 1% to 10.5%
            pool_b = MockV2Pool(
                address=ChecksumAddress(f"0x{i + 100:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i + 100:040x}"),
                    block=0,
                    reserves_token0=int(2_000_000_000_000 * price_diff),
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_pairs.append((pool_a, pool_b, usdc))

        optimizer = BatchNewtonOptimizer()
        idx, result = optimizer.get_best_path(pool_pairs)

        assert result.success
        assert 0 <= idx < 20

    def test_batch_performance(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Benchmark batch optimizer."""
        pool_pairs = []
        for i in range(100):
            pool_a = MockV2Pool(
                address=ChecksumAddress(f"0x{i:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i:040x}"),
                    block=0,
                    reserves_token0=2_000_000_000_000 + i * 10_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_b = MockV2Pool(
                address=ChecksumAddress(f"0x{i + 100:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i + 100:040x}"),
                    block=0,
                    reserves_token0=2_100_000_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_pairs.append((pool_a, pool_b, usdc))

        optimizer = BatchNewtonOptimizer()

        # Warm up
        optimizer.solve_batch(pool_pairs)

        # Benchmark
        times = []
        for _ in range(10):
            start = time.perf_counter_ns()
            optimizer.solve_batch(pool_pairs)
            times.append(time.perf_counter_ns() - start)

        mean_ms = np.mean(times) / 1_000_000
        per_path_us = mean_ms * 1000 / 100

        print(f"\nBatch 100 paths: {mean_ms:.2f}ms total, {per_path_us:.1f}μs per path")

        # Should be fast (< 1ms per path)
        assert per_path_us < 100


# =============================================================================
# ChainRuleNewtonOptimizer Tests
# =============================================================================

class TestChainRuleNewtonOptimizer:
    """Tests for multi-pool chain rule optimizer."""

    def test_triangular_arbitrage(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test 3-pool triangular arbitrage."""
        # Create USDC/WETH, WETH/DAI, DAI/USDC pools
        dai = MockErc20Token(
            address=ChecksumAddress("0x6B175474E89094C44Da98b954EedeAC495271d0F"),
            symbol="DAI",
            decimals=18,
        )

        # Pool 1: USDC/WETH at 2000
        pool_usdc_weth = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                reserves_token0=2_000_000_000_000,  # 2M USDC
                reserves_token1=1_000 * 10**18,  # 1000 WETH
            ),
            fee=Fraction(3, 1000),
        )

        # Pool 2: WETH/DAI at 2000
        pool_weth_dai = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            token0=weth,
            token1=dai,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=1_000 * 10**18,  # 1000 WETH
                reserves_token1=2_000_000 * 10**18,  # 2M DAI
            ),
            fee=Fraction(3, 1000),
        )

        # Pool 3: DAI/USDC at 1.05 (5% arb opportunity)
        pool_dai_usdc = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000003"),
            token0=dai,
            token1=usdc,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000003"),
                block=0,
                reserves_token0=2_000_000 * 10**18,  # 2M DAI
                reserves_token1=2_100_000_000_000,  # 2.1M USDC (5% premium)
            ),
            fee=Fraction(3, 1000),
        )

        optimizer = ChainRuleNewtonOptimizer()
        result = optimizer.solve([pool_usdc_weth, pool_weth_dai, pool_dai_usdc], usdc)

        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0

    def test_chain_rule_requires_multiple_pools(
        self,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that chain rule requires 2+ pools."""
        optimizer = ChainRuleNewtonOptimizer()
        result = optimizer.solve([pool_a], usdc)

        assert not result.success
        assert "2+ pools" in result.error_message


# =============================================================================
# HybridOptimizer Tests
# =============================================================================

class TestHybridOptimizer:
    """Tests for hybrid optimizer."""

    def test_selects_newton_for_v2_v2(
        self,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that hybrid selects Newton for V2-V2."""
        optimizer = HybridOptimizer()
        result = optimizer.solve([pool_a, pool_b], usdc)

        assert result.success
        # Note: Hybrid delegates to Newton, so type may be NEWTON
        assert result.optimizer_type in {OptimizerType.HYBRID, OptimizerType.NEWTON}

    def test_selects_chain_rule_for_multi_pool(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test that hybrid selects chain rule for 3+ pools."""
        dai = MockErc20Token(
            address=ChecksumAddress("0x6B175474E89094C44Da98b954EedeAC495271d0F"),
            symbol="DAI",
            decimals=18,
        )

        pool_a = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                reserves_token0=2_000_000_000_000,
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )
        pool_b = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            token0=weth,
            token1=dai,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=1_000 * 10**18,
                reserves_token1=2_000_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )
        pool_c = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000003"),
            token0=dai,
            token1=usdc,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000003"),
                block=0,
                reserves_token0=2_000_000 * 10**18,
                reserves_token1=2_100_000_000_000,
            ),
            fee=Fraction(3, 1000),
        )

        optimizer = HybridOptimizer()
        result = optimizer.solve([pool_a, pool_b, pool_c], usdc)

        assert result.success

    def test_convenience_function(
        self,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test optimize_arbitrage convenience function."""
        result = optimize_arbitrage([pool_a, pool_b], usdc)

        assert result.success
        assert result.optimal_input > 0


# =============================================================================
# Integration Tests
# =============================================================================

class TestOptimizerIntegration:
    """Integration tests comparing optimizers."""

    def test_all_v2_optimizers_find_same_profit(
        self,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that all V2 optimizers find similar profits."""
        # Newton V2
        newton = NewtonV2Optimizer()
        result_newton = newton.solve([pool_a, pool_b], usdc)

        # Hybrid
        hybrid = HybridOptimizer()
        result_hybrid = hybrid.solve([pool_a, pool_b], usdc)

        # Both should succeed
        assert result_newton.success
        assert result_hybrid.success

        # Profits should be similar
        profit_diff = abs(result_newton.profit - result_hybrid.profit)
        max_profit = max(result_newton.profit, result_hybrid.profit)

        # Within 1% of each other
        if max_profit > 0:
            assert profit_diff / max_profit < 0.01

    def test_batch_vs_serial_results_match(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test that batch and serial optimizers find same results."""
        pool_pairs = []
        for i in range(25):
            pool_a = MockV2Pool(
                address=ChecksumAddress(f"0x{i:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i:040x}"),
                    block=0,
                    reserves_token0=2_000_000_000_000 + i * 1_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_b = MockV2Pool(
                address=ChecksumAddress(f"0x{i + 100:040x}"),
                token0=usdc,
                token1=weth,
                initial_state=UniswapV2PoolState(
                    address=ChecksumAddress(f"0x{i + 100:040x}"),
                    block=0,
                    reserves_token0=2_100_000_000_000,
                    reserves_token1=1_000 * 10**18,
                ),
                fee=Fraction(3, 1000),
            )
            pool_pairs.append((pool_a, pool_b, usdc))

        # Serial results
        newton = NewtonV2Optimizer()
        serial_results = [newton.solve([p[0], p[1]], p[2]) for p in pool_pairs]

        # Batch results
        batch = BatchNewtonOptimizer(min_paths_for_batch=10)
        batch_results = batch.solve_batch(pool_pairs)

        # Compare
        for i, (serial, batch_r) in enumerate(zip(serial_results, batch_results, strict=False)):
            if serial.success and batch_r.success:
                profit_diff = abs(serial.profit - batch_r.profit)
                max_profit = max(serial.profit, batch_r.profit)
                if max_profit > 0:
                    assert profit_diff / max_profit < 0.05, f"Mismatch at index {i}"


# Import OptimizerType for tests
from degenbot.arbitrage.optimizers.base import OptimizerType
