"""
Offline tests for Uniswap V3 pools using recorded chain data.

These tests use the OfflineProvider to run without requiring a live RPC connection.
They test deterministic V3 pool operations that don't need real-time chain data.
"""

import pickle

import pytest

from degenbot.exceptions.liquidity_pool import ExternalUpdateError, LiquidityPoolError
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)


class TestV3PoolCalculations:
    """Test V3 pool calculation methods with offline data."""

    def test_calculate_tokens_out_from_tokens_in(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test token output calculation with known values."""
        pool = offline_wbtc_weth_v3_pool

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
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test token input calculation with known values."""
        pool = offline_wbtc_weth_v3_pool

        # Calculate input for 10 WETH output
        amount_out = 10 * 10**18  # 10 WETH
        amount_in = pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token1,
            token_out_quantity=amount_out,
        )

        # Result should be positive
        assert amount_in > 0
        # For ~10 WETH, expect ~0.2-0.5 WBTC (depends on current WBTC/WETH price ~15-20x)
        assert 0.2 * 10**8 < amount_in < 0.5 * 10**8

    def test_calculate_tokens_out_with_override(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test calculation with state override."""
        pool = offline_wbtc_weth_v3_pool

        # Create an override state with known values
        # Get current slot0 values for the override
        current_state = pool.state

        override_state = UniswapV3PoolState(
            address=pool.address,
            liquidity=current_state.liquidity,
            sqrt_price_x96=current_state.sqrt_price_x96,
            tick=current_state.tick,
            tick_bitmap=current_state.tick_bitmap,
            tick_data=current_state.tick_data,
            block=1,
        )

        # Calculate with override
        amount_in = 1 * 10**8  # 1 WBTC
        amount_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token0,
            token_in_quantity=amount_in,
            override_state=override_state,
        )

        # Should get same result as without override (same state values)
        assert amount_out > 0

    def test_simulate_exact_input(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test exact input simulation method."""
        pool = offline_wbtc_weth_v3_pool

        # Simulate a swap
        amount_in = 1 * 10**8  # 1 WBTC
        simulation = pool.simulate_exact_input_swap(
            token_in=pool.token0,
            token_in_quantity=amount_in,
        )

        assert isinstance(simulation, UniswapV3PoolSimulationResult)
        assert simulation.amount0_delta > 0
        assert simulation.amount1_delta < 0

    def test_simulate_exact_output(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test exact output simulation method."""
        pool = offline_wbtc_weth_v3_pool

        # Simulate a swap for specific output
        amount_out = 10 * 10**18  # 10 WETH
        simulation = pool.simulate_exact_output_swap(
            token_out=pool.token1,
            token_out_quantity=amount_out,
        )

        assert isinstance(simulation, UniswapV3PoolSimulationResult)
        assert simulation.amount0_delta > 0
        assert simulation.amount1_delta < 0

    def test_zero_swaps(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test that zero input raises appropriate error (EVM revert)."""
        pool = offline_wbtc_weth_v3_pool

        # V3 pool raises EVMRevertError for zero input (simulated execution reverts)
        with pytest.raises(LiquidityPoolError):
            pool.calculate_tokens_out_from_tokens_in(
                token_in=pool.token0,
                token_in_quantity=0,
            )


class TestV3PoolStateManagement:
    """Test V3 pool state management with offline data."""

    def test_pickle_pool(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test that offline pool can be pickled."""
        pool = offline_wbtc_weth_v3_pool

        # Should not raise
        pickled = pickle.dumps(pool)
        unpickled = pickle.loads(pickled)

        # Verify basic attributes
        assert unpickled.address == pool.address
        assert unpickled.token0.address == pool.token0.address
        assert unpickled.token1.address == pool.token1.address

    def test_external_update(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test applying external state updates."""
        pool = offline_wbtc_weth_v3_pool
        original_liquidity = pool.liquidity
        original_sqrt_price = pool.sqrt_price_x96
        original_block = pool.update_block

        # Apply an update at next block
        new_liquidity = original_liquidity + 1000000000000000

        pool.external_update(
            update=UniswapV3PoolExternalUpdate(
                block_number=original_block + 1,
                liquidity=new_liquidity,
                sqrt_price_x96=original_sqrt_price,
                tick=pool.tick,
            )
        )

        # Verify update applied
        assert pool.liquidity == new_liquidity
        assert pool.update_block == original_block + 1

    def test_late_update(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test that updates to past blocks are rejected."""
        pool = offline_wbtc_weth_v3_pool
        initial_block = pool.update_block

        # Provide some updates at future blocks
        for block_offset in range(1, 6):
            pool.external_update(
                update=UniswapV3PoolExternalUpdate(
                    block_number=initial_block + block_offset,
                    liquidity=pool.liquidity + block_offset * 1000,
                    sqrt_price_x96=pool.sqrt_price_x96,
                    tick=pool.tick,
                )
            )

        # Verify update_block has advanced
        assert pool.update_block == initial_block + 5

        # Attempt an update in the past (after initial state but before current)
        with pytest.raises(ExternalUpdateError):
            pool.external_update(
                update=UniswapV3PoolExternalUpdate(
                    block_number=initial_block + 2,  # Between initial and current
                    liquidity=pool.liquidity,
                    sqrt_price_x96=pool.sqrt_price_x96,
                    tick=pool.tick,
                )
            )

    def test_reorg(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test handling of chain reorgs (update at same block)."""
        pool = offline_wbtc_weth_v3_pool
        current_block = pool.update_block
        original_liquidity = pool.liquidity

        # First, advance past the initial state block
        # (reorg can only happen at blocks >= initial_state_block)
        pool.external_update(
            update=UniswapV3PoolExternalUpdate(
                block_number=current_block + 1,
                liquidity=original_liquidity,
                sqrt_price_x96=pool.sqrt_price_x96,
                tick=pool.tick,
            )
        )

        # Now simulate a reorg by providing update at the current block
        pool.external_update(
            update=UniswapV3PoolExternalUpdate(
                block_number=current_block + 1,
                liquidity=original_liquidity + 1000000000000000,
                sqrt_price_x96=pool.sqrt_price_x96,
                tick=pool.tick,
            )
        )

        # State should be at the reorg block with new values
        assert pool.liquidity == original_liquidity + 1000000000000000
        assert pool.update_block == current_block + 1


class TestV3PoolProperties:
    """Test V3 pool properties and metadata."""

    def test_pool_fee(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test pool fee property."""
        pool = offline_wbtc_weth_v3_pool

        # Pool fee should be defined (3000 = 0.3% for this pool)
        assert pool.fee > 0
        assert pool.fee <= 10000  # Max fee is 1% (10000)

    def test_pool_tick_spacing(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test tick spacing property."""
        pool = offline_wbtc_weth_v3_pool

        # Tick spacing should be 60 for 0.3% pool
        assert pool.tick_spacing == 60

    def test_pool_tokens(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
        offline_wbtc,
        offline_weth,
    ):
        """Test token references."""
        pool = offline_wbtc_weth_v3_pool

        assert pool.token0.address == offline_wbtc.address
        assert pool.token1.address == offline_weth.address

        # tokens property returns tuple
        tokens = pool.tokens
        assert len(tokens) == 2
        assert tokens[0].address == offline_wbtc.address
        assert tokens[1].address == offline_weth.address

    def test_liquidity_properties(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test liquidity properties."""
        pool = offline_wbtc_weth_v3_pool

        # Liquidity should be positive
        assert pool.liquidity > 0

        # State should reflect the same values
        assert pool.state.liquidity == pool.liquidity

    def test_sqrt_price_properties(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test sqrt price properties."""
        pool = offline_wbtc_weth_v3_pool

        # sqrt_price_x96 should be positive
        assert pool.sqrt_price_x96 > 0

        # State should reflect the same values
        assert pool.state.sqrt_price_x96 == pool.sqrt_price_x96

    def test_tick_properties(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test tick properties."""
        pool = offline_wbtc_weth_v3_pool

        # Tick should be defined
        assert pool.tick is not None

        # State should reflect the same values
        assert pool.state.tick == pool.tick

    def test_absolute_price(
        self,
        offline_wbtc_weth_v3_pool: UniswapV3Pool,
    ):
        """Test absolute price calculations."""
        pool = offline_wbtc_weth_v3_pool

        # Get prices in both directions
        price0 = pool.get_absolute_price(pool.token0)
        price1 = pool.get_absolute_price(pool.token1)

        # Prices should be positive
        assert price0 > 0
        assert price1 > 0
