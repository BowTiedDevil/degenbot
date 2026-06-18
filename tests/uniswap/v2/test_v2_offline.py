"""Offline tests for Uniswap V2 pools using recorded chain data.

These tests use the OfflineProvider to run without requiring a live RPC connection.
They test deterministic pool operations that don't need real-time chain data.
"""

from fractions import Fraction

import pytest

from degenbot.exceptions.pool import InvalidSwapInputAmount
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v2_types import (
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)


class TestV2PoolCalculations:
    """Test V2 pool calculation methods with offline data."""

    def test_calculate_tokens_out_from_tokens_in(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test token output calculation with known values."""
        pool = offline_wbtc_weth_v2_pool

        # Calculate output for 1 WBTC input
        amount_in = 1 * 10**8  # 1 WBTC
        amount_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token0,
            token_in_quantity=amount_in,
        )

        # Result should be positive and reasonable
        assert amount_out > 0
        # For ~1 WBTC, expect ~10-20 WETH (depends on recorded reserves)
        assert 5 * 10**18 < amount_out < 50 * 10**18

    def test_calculate_tokens_in_from_tokens_out(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test token input calculation with known values."""
        pool = offline_wbtc_weth_v2_pool

        # Calculate input for 10 WETH output
        amount_out = 10 * 10**18  # 10 WETH
        amount_in = pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token1,
            token_out_quantity=amount_out,
        )

        # Result should be positive
        assert amount_in > 0
        # For ~10 WETH, expect ~0.5-2 WBTC
        assert 0.3 * 10**8 < amount_in < 3 * 10**8

    def test_calculate_tokens_out_with_override(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test calculation with state override."""
        pool = offline_wbtc_weth_v2_pool

        # Create an override state with known reserves
        override_state = UniswapV2PoolState(
            address=pool.address,
            reserves_token0=1000 * 10**8,  # 1000 WBTC
            reserves_token1=20000 * 10**18,  # 20000 WETH
            block=1,
        )

        # Calculate with override
        amount_in = 1 * 10**8  # 1 WBTC
        amount_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token0,
            token_in_quantity=amount_in,
            override_state=override_state,
        )

        # With 1000:20000 reserves, 1 WBTC should give the exact V2 constant-product result
        fee = Fraction(3, 1000)
        amount_in_with_fee = amount_in * (fee.denominator - fee.numerator)
        expected_output = (amount_in_with_fee * override_state.reserves_token1) // (
            override_state.reserves_token0 * fee.denominator + amount_in_with_fee
        )
        assert amount_out == expected_output

    def test_calculate_tokens_in_with_override(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test input calculation with state override."""
        pool = offline_wbtc_weth_v2_pool

        # Create an override state
        override_state = UniswapV2PoolState(
            address=pool.address,
            reserves_token0=1000 * 10**8,
            reserves_token1=20000 * 10**18,
            block=1,
        )

        # Calculate input for 10 WETH output
        amount_out = 10 * 10**18
        amount_in = pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token1,
            token_out_quantity=amount_out,
            override_state=override_state,
        )

        # With 1000:20000 reserves, the exact V2 constant-product result
        fee = Fraction(3, 1000)
        expected_input = 1 + (override_state.reserves_token0 * amount_out * fee.denominator) // (
            (override_state.reserves_token1 - amount_out) * (fee.denominator - fee.numerator)
        )
        assert amount_in == expected_input

    def test_simulate_exact_input(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test exact input simulation method."""
        pool = offline_wbtc_weth_v2_pool

        # Simulate a swap
        amount_in = 1 * 10**8  # 1 WBTC
        simulation = pool.simulate_exact_input_swap(
            token_in=pool.token0,
            token_in_quantity=amount_in,
        )

        assert isinstance(simulation, UniswapV2PoolSimulationResult)
        assert simulation.amount0_delta > 0
        assert simulation.amount1_delta < 0

    def test_simulate_exact_output(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test exact output simulation method."""
        pool = offline_wbtc_weth_v2_pool

        # Simulate a swap for specific output
        amount_out = 10 * 10**18  # 10 WETH
        simulation = pool.simulate_exact_output_swap(
            token_out=pool.token1,
            token_out_quantity=amount_out,
        )

        assert isinstance(simulation, UniswapV2PoolSimulationResult)
        assert simulation.amount0_delta > 0
        assert simulation.amount1_delta < 0

    def test_swap_for_all(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test swapping for all available liquidity."""
        pool = offline_wbtc_weth_v2_pool

        # Try to calculate output for all of token0
        total_token0 = pool.reserves_token0

        # This should work and give us most of token1
        amount_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token0,
            token_in_quantity=total_token0 // 10,  # 10% of reserves
        )

        assert amount_out > 0
        assert amount_out < pool.reserves_token1

    def test_zero_swaps(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test that zero input raises appropriate error."""
        pool = offline_wbtc_weth_v2_pool

        with pytest.raises(InvalidSwapInputAmount):
            pool.calculate_tokens_out_from_tokens_in(
                token_in=pool.token0,
                token_in_quantity=0,
            )


class TestV2PoolStateManagement:
    """Test V2 pool state management with offline data."""


class TestV2PoolProperties:
    """Test V2 pool properties and metadata."""

    def test_pool_fees(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test pool fee properties."""
        pool = offline_wbtc_weth_v2_pool

        # Default V2 fee is 0.3%
        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(3, 1000)

    def test_pool_tokens(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
        offline_wbtc,
        offline_weth,
    ):
        """Test token references."""
        pool = offline_wbtc_weth_v2_pool

        assert pool.token0.address == offline_wbtc.address
        assert pool.token1.address == offline_weth.address

        # tokens property returns tuple
        tokens = pool.tokens
        assert len(tokens) == 2
        assert tokens[0].address == offline_wbtc.address
        assert tokens[1].address == offline_weth.address

    def test_reserves_properties(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test reserve properties."""
        pool = offline_wbtc_weth_v2_pool

        # Reserves should be positive
        assert pool.reserves_token0 > 0
        assert pool.reserves_token1 > 0

        # State should reflect the same values
        assert pool.state.reserves_token0 == pool.reserves_token0
        assert pool.state.reserves_token1 == pool.reserves_token1

    def test_absolute_price(
        self,
        offline_wbtc_weth_v2_pool: LiquidityPool,
    ):
        """Test absolute price calculations."""
        pool = offline_wbtc_weth_v2_pool

        # Get prices in both directions
        price0 = pool.get_absolute_price(pool.token0)
        price1 = pool.get_absolute_price(pool.token1)

        # Prices should be positive
        assert price0 > 0
        assert price1 > 0
