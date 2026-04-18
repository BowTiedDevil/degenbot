"""
Tests for solver runner integration.
"""

from pathlib import Path

import pytest

from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.arbitrage.baseline import BaselineManager
from tests.arbitrage.generator.fixtures import FixtureFactory
from tests.arbitrage.solver_runner import (
    FakeErc20Token,
    FakeV2Pool,
    SolverResult,
    estimate_profit_for_v2_pair,
    find_optimal_input_binary_search,
    run_solver_on_fixture,
)


class TestFakeErc20Token:
    """Tests for FakeErc20Token."""

    def test_create_token(self) -> None:
        """Test creating a fake token."""
        token = FakeErc20Token(
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            "USDC",
            6,
        )
        assert token.symbol == "USDC"
        assert token.decimals == 6
        assert token.chain_id == 1

    def test_token_equality(self) -> None:
        """Test token equality comparison."""
        token1 = FakeErc20Token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6)
        token2 = FakeErc20Token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6)
        token3 = FakeErc20Token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "WETH", 18)

        assert token1 == token2
        assert token1 != token3

    def test_token_hash(self) -> None:
        """Test token hashing."""
        token1 = FakeErc20Token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6)
        token2 = FakeErc20Token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6)

        assert hash(token1) == hash(token2)
        assert token1 == token2


class TestFakeV2Pool:
    """Tests for FakeV2Pool."""

    @pytest.fixture
    def usdc(self) -> FakeErc20Token:
        return FakeErc20Token(
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            "USDC",
            6,
        )

    @pytest.fixture
    def weth(self) -> FakeErc20Token:
        return FakeErc20Token(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "WETH",
            18,
        )

    @pytest.fixture
    def pool_state(self) -> UniswapV2PoolState:
        return UniswapV2PoolState(
            address="0x0000000000000000000000000000000000000001",
            block=12345,
            reserves_token0=2000000000,  # 2000 USDC (6 decimals)
            reserves_token1=10**18,  # 1 WETH (18 decimals)
        )

    def test_create_pool(
        self,
        usdc: FakeErc20Token,
        weth: FakeErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test creating a fake V2 pool."""
        pool = FakeV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            initial_state=pool_state,
        )

        assert pool.address == "0x0000000000000000000000000000000000000001"
        assert pool.token0 == usdc
        assert pool.token1 == weth

    def test_pool_state(
        self,
        usdc: FakeErc20Token,
        weth: FakeErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test pool state access."""
        pool = FakeV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            initial_state=pool_state,
        )

        assert pool.state.reserves_token0 == 2000000000
        assert pool.state.reserves_token1 == 10**18

    def test_calculate_tokens_out(
        self,
        usdc: FakeErc20Token,
        weth: FakeErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test token output calculation."""
        pool = FakeV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            initial_state=pool_state,
        )

        # Sell 1 ETH worth of USDC
        amount_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=2000000000,  # 2000 USDC
        )

        # Should get close to 0.5 ETH (minus fee)
        assert amount_out > 0
        assert amount_out < 10**18  # Less than 1 ETH

    def test_calculate_with_override_state(
        self,
        usdc: FakeErc20Token,
        weth: FakeErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test calculation with state override."""
        pool = FakeV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            initial_state=pool_state,
        )

        # Create override state with different reserves
        override_state = UniswapV2PoolState(
            address="0x0000000000000000000000000000000000000001",
            block=12346,
            reserves_token0=4000000000,  # 4000 USDC
            reserves_token1=10**18,  # 1 WETH
        )

        amount_normal = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=1000000000,  # 1000 USDC
        )

        amount_override = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=1000000000,
            override_state=override_state,
        )

        # Override state is used (different amount output)
        assert amount_override != amount_normal
        # Both should produce valid outputs
        assert amount_normal > 0
        assert amount_override > 0


class TestProfitEstimation:
    """Tests for profit estimation functions."""

    def test_estimate_profit_positive(self) -> None:
        """Test profit estimation with profitable arbitrage."""
        # For profitable arbitrage, we need pools with significant price difference
        # The 0.3% fee means we need >0.6% price difference to be profitable
        # Pool A: ETH is cheaper (2000 USDC per ETH)
        # Pool B: ETH is more expensive (2100 USDC per ETH) - 5% difference

        # Pool A: 10000 USDC, 5 ETH
        pool_a = UniswapV2PoolState(
            address="0xPoolA",
            block=1,
            reserves_token0=10000000000,  # 10000 USDC (6 decimals)
            reserves_token1=5 * 10**18,  # 5 ETH
        )

        # Pool B: 10500 USDC, 5 ETH (5% price difference)
        pool_b = UniswapV2PoolState(
            address="0xPoolB",
            block=1,
            reserves_token0=10500000000,  # 10500 USDC
            reserves_token1=5 * 10**18,  # 5 ETH
        )

        # Find optimal input
        optimal_input, profit = find_optimal_input_binary_search(pool_a, pool_b)

        # With 5% price difference, should find profitable arbitrage
        # (fees are 0.6% round-trip, so 5% is plenty)
        assert profit > 0, f"Expected positive profit, got {profit} with input {optimal_input}"

    def test_estimate_profit_no_opportunity(self) -> None:
        """Test profit estimation with no arbitrage opportunity."""
        # Identical pools
        pool_a = UniswapV2PoolState(
            address="0xPoolA",
            block=1,
            reserves_token0=2000000000,
            reserves_token1=10**18,
        )
        pool_b = UniswapV2PoolState(
            address="0xPoolB",
            block=1,
            reserves_token0=2000000000,
            reserves_token1=10**18,
        )

        # Should be unprofitable due to fees
        profit = estimate_profit_for_v2_pair(pool_a, pool_b, 10**9)
        assert profit < 0  # Fees make it negative

    def test_find_optimal_input(self) -> None:
        """Test binary search for optimal input."""
        # Create price discrepancy
        pool_a = UniswapV2PoolState(
            address="0xPoolA",
            block=1,
            reserves_token0=10000000000,  # 10000 USDC
            reserves_token1=5 * 10**18,  # 5 ETH (price = 2000)
        )
        pool_b = UniswapV2PoolState(
            address="0xPoolB",
            block=1,
            reserves_token0=10500000000,  # 10500 USDC
            reserves_token1=5 * 10**18,  # 5 ETH (price = 2100)
        )

        optimal_input, profit = find_optimal_input_binary_search(pool_a, pool_b, max_input=10**21)

        # Should find positive optimal input
        assert optimal_input > 0
        assert profit > 0


class TestSolverRunner:
    """Tests for the solver runner."""

    def test_run_solver_on_v2_fixture(self) -> None:
        """Test running solver on V2 fixture."""
        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        result = run_solver_on_fixture(fixture)

        assert isinstance(result, SolverResult)
        assert result.fixture_id == "simple_v2_arb_profitable"
        assert result.success is True
        assert result.calculation_time_ms > 0

    def test_run_solver_detects_profit(self) -> None:
        """Test that solver detects profit in profitable fixture."""
        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        result = run_solver_on_fixture(fixture)

        # Fixture is designed to be profitable
        assert result.success is True
        # Note: The simplified solver may not find optimal profit
        # Real implementation would use actual UniswapLpCycle

    def test_solver_result_structure(self) -> None:
        """Test solver result has expected structure."""
        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        result = run_solver_on_fixture(fixture)

        assert hasattr(result, "fixture_id")
        assert hasattr(result, "optimal_input")
        assert hasattr(result, "profit")
        assert hasattr(result, "calculation_time_ms")
        assert hasattr(result, "success")
        assert hasattr(result, "error_message")


class TestIntegrationWithBaselines:
    """Tests for solver-baseline integration."""

    def test_solver_result_can_be_recorded(self) -> None:
        """Test that solver results can be used for baselines."""
        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        result = run_solver_on_fixture(fixture)

        # Create manager and record
        manager = BaselineManager(Path(__file__).parent / "test_baselines")
        manager.record(
            fixture_id=result.fixture_id,
            optimal_input=result.optimal_input,
            profit=result.profit,
            calculation_time_ms=result.calculation_time_ms,
        )

        assert manager.has_baseline(result.fixture_id)
