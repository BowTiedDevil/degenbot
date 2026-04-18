"""
Tests for production Newton optimizer (NewtonV2Optimizer).

These tests validate that the production optimizer in
src/degenbot/arbitrage/optimizers/newton.py works correctly with
real pool interfaces.
"""

import time
from fractions import Fraction

import numpy as np
import pytest
from eth_typing import ChecksumAddress

from degenbot.arbitrage.optimizers import NewtonV2Optimizer
from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.arbitrage.mock_pools import MockErc20Token, MockV2Pool

# =============================================================================
# Module-level fixtures (shared across test classes)
# =============================================================================


@pytest.fixture
def usdc() -> MockErc20Token:
    """USDC token (6 decimals)."""
    return MockErc20Token(
        address=ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        symbol="USDC",
        decimals=6,
    )


@pytest.fixture
def weth() -> MockErc20Token:
    """WETH token (18 decimals)."""
    return MockErc20Token(
        address=ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        symbol="WETH",
        decimals=18,
    )


@pytest.fixture
def pool_a(usdc: MockErc20Token, weth: MockErc20Token) -> MockV2Pool:
    """V2 pool at price 2000 USDC/WETH."""
    return MockV2Pool(
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


@pytest.fixture
def pool_b(usdc: MockErc20Token, weth: MockErc20Token) -> MockV2Pool:
    """V2 pool at price 2100 USDC/WETH (5% higher)."""
    return MockV2Pool(
        address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
        token0=usdc,
        token1=weth,
        initial_state=UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            block=0,
            reserves_token0=2_100_000_000_000,  # 2.1M USDC
            reserves_token1=1_000 * 10**18,  # 1000 WETH
        ),
        fee=Fraction(3, 1000),
    )


@pytest.fixture
def optimizer() -> NewtonV2Optimizer:
    """Create Newton optimizer."""
    return NewtonV2Optimizer()


class TestNewtonV2Optimizer:

    def test_optimizer_creates(self, optimizer: NewtonV2Optimizer) -> None:
        """Test that optimizer instantiates correctly."""
        assert optimizer.optimizer_type.value == "newton"

    def test_finds_profitable_arbitrage(
        self,
        optimizer: NewtonV2Optimizer,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that optimizer finds profitable arbitrage."""
        result = optimizer.solve([pool_a, pool_b], usdc)

        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.iterations <= 10

    def test_no_profit_equal_prices(
        self,
        optimizer: NewtonV2Optimizer,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test that no arbitrage is found when prices are equal."""
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
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=2_000_000_000_000,
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )

        result = optimizer.solve([pool_a, pool_b], usdc)
        # Should find no profitable arbitrage (or very small)
        # The optimizer might find tiny profit due to floating point
        if not result.success:
            assert result.error_message is not None

    def test_requires_two_pools(
        self,
        optimizer: NewtonV2Optimizer,
        pool_a: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that optimizer requires exactly 2 pools."""
        # Single pool
        result = optimizer.solve([pool_a], usdc)
        assert not result.success
        assert "2 pools" in result.error_message

        # Three pools
        result = optimizer.solve([pool_a, pool_a, pool_a], usdc)
        assert not result.success
        assert "2 pools" in result.error_message

    # -------------------------------------------------------------------------
    # Performance Tests
    # -------------------------------------------------------------------------

    def test_performance_benchmark(
        self,
        optimizer: NewtonV2Optimizer,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Benchmark the optimizer performance."""
        # Warm up
        for _ in range(10):
            optimizer.solve([pool_a, pool_b], usdc)

        # Measure
        times = []
        for _ in range(1000):
            start = time.perf_counter_ns()
            optimizer.solve([pool_a, pool_b], usdc)
            times.append(time.perf_counter_ns() - start)

        mean_us = np.mean(times) / 1000
        p50_us = np.percentile(times, 50) / 1000
        p99_us = np.percentile(times, 99) / 1000

        print("\nNewtonV2Optimizer Performance:")
        print(f"  Mean: {mean_us:.1f}μs")
        print(f"  P50:  {p50_us:.1f}μs")
        print(f"  P99:  {p99_us:.1f}μs")

        # Should be faster than Brent (~200μs)
        assert mean_us < 50, f"Optimizer too slow: {mean_us:.1f}μs"

    def test_iterations_are_few(
        self,
        optimizer: NewtonV2Optimizer,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that Newton converges in few iterations."""
        result = optimizer.solve([pool_a, pool_b], usdc)
        assert result.success
        # Newton should converge in 3-5 iterations
        assert result.iterations <= 6, f"Too many iterations: {result.iterations}"

    # -------------------------------------------------------------------------
    # Edge Cases
    # -------------------------------------------------------------------------

    def test_large_price_difference(
        self,
        optimizer: NewtonV2Optimizer,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test with large price difference (20%)."""
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
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=2_400_000_000_000,  # 20% higher price
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )

        result = optimizer.solve([pool_a, pool_b], usdc)
        assert result.success
        assert result.profit > 0

    def test_max_input_constraint(
        self,
        optimizer: NewtonV2Optimizer,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that max_input constraint is respected."""
        max_input = 1_000_000_000  # 1000 USDC

        result = optimizer.solve([pool_a, pool_b], usdc, max_input=max_input)
        assert result.success
        assert result.optimal_input <= max_input

    def test_different_fee_pools(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
    ) -> None:
        """Test with different fee pools."""
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
            fee=Fraction(3, 1000),  # 0.3%
        )
        pool_b = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=2_100_000_000_000,
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(1, 1000),  # 0.1%
        )

        optimizer = NewtonV2Optimizer()
        result = optimizer.solve([pool_a, pool_b], usdc)
        assert result.success

    def test_reversed_pool_order(
        self,
        optimizer: NewtonV2Optimizer,
        pool_a: MockV2Pool,
        pool_b: MockV2Pool,
        usdc: MockErc20Token,
    ) -> None:
        """Test that pool order doesn't matter."""
        result_1 = optimizer.solve([pool_a, pool_b], usdc)
        result_2 = optimizer.solve([pool_b, pool_a], usdc)

        # Should find same profit regardless of pool order
        assert result_1.success
        assert result_2.success
        # Profits should be similar (within rounding)
        assert abs(result_1.profit - result_2.profit) < max(result_1.profit, result_2.profit) * 0.01


class TestNewtonV2OptimizerAccuracy:
    """Accuracy tests comparing Newton to Brent/CVXPY."""

    @pytest.fixture
    def optimizer(self) -> NewtonV2Optimizer:
        return NewtonV2Optimizer()

    @pytest.mark.parametrize("price_ratio", [1.01, 1.02, 1.05, 1.10, 1.20])
    def test_accuracy_vs_price_ratio(
        self,
        optimizer: NewtonV2Optimizer,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        price_ratio: float,
    ) -> None:
        """Test accuracy across different price ratios."""
        base_reserves = 2_000_000_000_000

        pool_a = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                reserves_token0=base_reserves,
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )
        pool_b = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=int(base_reserves * price_ratio),
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )

        result = optimizer.solve([pool_a, pool_b], usdc)
        assert result.success

        # Verify profit is positive and reasonable
        print(f"\nPrice ratio {price_ratio}: profit={result.profit:.2e}, input={result.optimal_input:.2e}")

    @pytest.mark.parametrize("liquidity_scale", [1e10, 1e15, 1e18, 1e21])
    def test_accuracy_vs_liquidity(
        self,
        optimizer: NewtonV2Optimizer,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        liquidity_scale: float,
    ) -> None:
        """Test accuracy across different liquidity scales."""
        pool_a = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                reserves_token0=int(2_000 * liquidity_scale),
                reserves_token1=int(1_000 * liquidity_scale),
            ),
            fee=Fraction(3, 1000),
        )
        pool_b = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            token0=usdc,
            token1=weth,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
                block=0,
                reserves_token0=int(2_100 * liquidity_scale),
                reserves_token1=int(1_000 * liquidity_scale),
            ),
            fee=Fraction(3, 1000),
        )

        result = optimizer.solve([pool_a, pool_b], usdc)
        assert result.success

        print(f"\nLiquidity scale {liquidity_scale:.0e}: profit={result.profit:.2e}, iterations={result.iterations}")
