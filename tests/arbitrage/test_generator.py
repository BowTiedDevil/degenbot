"""
Unit tests for the pool state generator.
"""

from fractions import Fraction

import pytest
from hexbytes import HexBytes

from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_types import UniswapV3PoolState
from tests.arbitrage.generator.pool_generator import PoolStateGenerator
from tests.arbitrage.generator.types import (
    PoolGenerationConfig,
    PriceDiscrepancyConfig,
    V3PoolGenerationConfig,
)


@pytest.fixture
def generator() -> PoolStateGenerator:
    return PoolStateGenerator()


class TestV2PoolStateGeneration:
    """Tests for V2 pool state generation."""

    def test_generate_v2_pool_state_basic(self, generator: PoolStateGenerator) -> None:
        """Test basic V2 pool state generation."""
        address = "0x0000000000000000000000000000000000000001"
        state = generator.generate_v2_pool_state(
            address=address,
            reserves_token0=1000000000000000000,  # 1 ETH
            reserves_token1=2000000000,  # 2000 USDC
        )

        assert state.address == address
        assert state.reserves_token0 == 1000000000000000000
        assert state.reserves_token1 == 2000000000
        assert state.block == 0

    def test_generate_v2_pool_state_with_block(self, generator: PoolStateGenerator) -> None:
        """Test V2 pool state generation with custom block number."""
        address = "0x0000000000000000000000000000000000000001"
        state = generator.generate_v2_pool_state(
            address=address,
            reserves_token0=1000000000000000000,
            reserves_token1=2000000000,
            block=12345,
        )

        assert state.block == 12345

    def test_generate_v2_pool_state_from_price(self, generator: PoolStateGenerator) -> None:
        """Test V2 pool state generation from target price."""
        address = "0x0000000000000000000000000000000000000002"
        config = PoolGenerationConfig(fee=Fraction(3, 1000))

        state = generator.generate_v2_pool_state_from_price(
            address=address,
            price_token1_per_token0=2000.0,  # 1 ETH = 2000 USDC
            liquidity_base=10**21,
            config=config,
        )

        assert isinstance(state, UniswapV2PoolState)
        # Price should be approximately 2000
        actual_price = state.reserves_token1 / state.reserves_token0
        assert 1900 < actual_price < 2100


class TestV3PoolStateGeneration:
    """Tests for V3 pool state generation."""

    def test_generate_v3_pool_state_basic(self, generator: PoolStateGenerator) -> None:
        """Test basic V3 pool state generation."""
        address = "0x0000000000000000000000000000000000000003"
        state = generator.generate_v3_pool_state(
            address=address,
            sqrt_price_x96=79228162514264337593543950336,  # ~price of 1.0
            liquidity=10**18,
            tick=0,
            tick_spacing=60,
        )

        assert state.address == address
        assert state.liquidity == 10**18
        assert state.tick == 0
        assert len(state.tick_bitmap) > 0
        assert len(state.tick_data) > 0

    def test_generate_v3_pool_state_has_valid_tick_bitmap(
        self, generator: PoolStateGenerator
    ) -> None:
        """Test that generated V3 state has valid tick bitmap."""
        address = "0x0000000000000000000000000000000000000004"
        state = generator.generate_v3_pool_state(
            address=address,
            sqrt_price_x96=79228162514264337593543950336,
            liquidity=10**18,
            tick=0,
            tick_spacing=60,
        )

        # Should have at least one tick on each side
        assert len(state.tick_data) >= 2

        # Tick data should have liquidity_net values
        for data in state.tick_data.values():
            assert data.liquidity_gross > 0

    def test_generate_v3_pool_state_from_price(self, generator: PoolStateGenerator) -> None:
        """Test V3 pool state generation from target price."""
        address = "0x0000000000000000000000000000000000000005"
        config = V3PoolGenerationConfig(
            fee=Fraction(3, 1000),
            tick_spacing=60,
            liquidity_depth=10**18,
        )

        state = generator.generate_v3_pool_state_from_price(
            address=address,
            price_token1_per_token0=2000.0,
            liquidity=10**18,
            config=config,
        )

        assert isinstance(state, UniswapV3PoolState)
        # Tick should be approximately log(2000)/log(1.0001) ≈ 76000
        assert 75000 < state.tick < 77000


class TestV4PoolStateGeneration:
    """Tests for V4 pool state generation."""

    def test_generate_v4_pool_state_basic(self, generator: PoolStateGenerator) -> None:
        """Test basic V4 pool state generation."""
        address = "0x0000000000000000000000000000000000000FFF"
        pool_id = HexBytes("0x" + "01" * 32)

        state = generator.generate_v4_pool_state(
            address=address,
            pool_id=pool_id,
            sqrt_price_x96=79228162514264337593543950336,
            liquidity=10**18,
            tick=0,
            tick_spacing=60,
        )

        assert state.address == address
        assert state.id == pool_id
        assert state.liquidity == 10**18
        assert state.tick == 0
        assert len(state.tick_bitmap) > 0
        assert len(state.tick_data) > 0


class TestProfitablePairGeneration:
    """Tests for profitable pair generation."""

    def test_generate_profitable_v2_pair(self, generator: PoolStateGenerator) -> None:
        """Test that V2 pair generation creates arbitrage opportunity."""
        pool_a, pool_b = generator.generate_profitable_v2_pair(
            pool_a_address="0x0000000000000000000000000000000000000001",
            pool_b_address="0x0000000000000000000000000000000000000002",
            fee_a=Fraction(3, 1000),
            fee_b=Fraction(3, 1000),
            price_ratio=1.02,
            liquidity_base=10**21,
        )

        # Check pools have different prices
        price_a = pool_a.reserves_token1 / pool_a.reserves_token0
        price_b = pool_b.reserves_token1 / pool_b.reserves_token0

        # Price ratio should be approximately 1.02
        assert abs(price_a / price_b - 1.02) < 0.01

    def test_generate_profitable_v3_pair(self, generator: PoolStateGenerator) -> None:
        """Test that V3 pair generation creates arbitrage opportunity."""
        pool_a, pool_b = generator.generate_profitable_v3_pair(
            pool_a_address="0x0000000000000000000000000000000000000003",
            pool_b_address="0x0000000000000000000000000000000000000004",
            tick_spacing=60,
            price_ratio=1.02,
            liquidity=10**18,
        )

        # Check pools have different sqrt prices
        assert pool_a.sqrt_price_x96 != pool_b.sqrt_price_x96

    def test_generate_profitable_v4_pair(self, generator: PoolStateGenerator) -> None:
        """Test that V4 pair generation creates arbitrage opportunity."""
        pool_a, pool_b = generator.generate_profitable_v4_pair(
            pool_a_address="0x0000000000000000000000000000000000000FFF",
            pool_b_address="0x0000000000000000000000000000000000000FFF",
            pool_a_id=HexBytes("0x" + "01" * 32),
            pool_b_id=HexBytes("0x" + "02" * 32),
            tick_spacing=60,
            price_ratio=1.02,
            liquidity=10**18,
        )

        # Check pools have different sqrt prices
        assert pool_a.sqrt_price_x96 != pool_b.sqrt_price_x96

    def test_generate_profitable_mixed_pair(self, generator: PoolStateGenerator) -> None:
        """Test mixed V2/V3 pair generation."""
        v2_pool, v3_pool = generator.generate_profitable_mixed_pair(
            v2_pool_address="0x0000000000000000000000000000000000000005",
            v3_pool_address="0x0000000000000000000000000000000000000006",
            v2_fee=Fraction(3, 1000),
            v3_tick_spacing=60,
            price_ratio=1.02,
            liquidity_base=10**21,
            v3_liquidity=10**18,
        )

        assert isinstance(v2_pool, UniswapV2PoolState)
        assert isinstance(v3_pool, UniswapV3PoolState)


class TestPriceDiscrepancyInjection:
    """Tests for price discrepancy injection."""

    def test_inject_price_discrepancy_creates_arb(self, generator: PoolStateGenerator) -> None:
        """Test that price discrepancy creates arbitrage opportunity."""
        pool_a = generator.generate_v2_pool_state(
            address="0x0000000000000000000000000000000000000001",
            reserves_token0=1000000000000000000,
            reserves_token1=2000000000000,
        )

        discrepancy = PriceDiscrepancyConfig(price_ratio=1.02)

        pool_b = generator.inject_price_discrepancy(
            pool_a_state=pool_a,
            pool_b_address="0x0000000000000000000000000000000000000002",
            discrepancy=discrepancy,
        )

        # Pools should have different prices
        price_a = pool_a.reserves_token1 / pool_a.reserves_token0
        price_b = pool_b.reserves_token1 / pool_b.reserves_token0

        assert price_a != price_b


class TestProfitValidation:
    """Tests for profit validation."""

    def test_validate_arbitrage_opportunity_v2(self, generator: PoolStateGenerator) -> None:
        """Test V2 arbitrage opportunity validation."""
        pool_a, pool_b = generator.generate_profitable_v2_pair(
            pool_a_address="0x0000000000000000000000000000000000000001",
            pool_b_address="0x0000000000000000000000000000000000000002",
            fee_a=Fraction(3, 1000),
            fee_b=Fraction(3, 1000),
            price_ratio=1.02,
            liquidity_base=10**21,
        )

        is_profitable = generator.validate_arbitrage_opportunity(
            pool_a_state=pool_a,
            pool_b_state=pool_b,
            pool_a_fee=Fraction(3, 1000),
            pool_b_fee=Fraction(3, 1000),
        )

        assert is_profitable

    def test_validate_arbitrage_opportunity_v3(self, generator: PoolStateGenerator) -> None:
        """Test V3 arbitrage opportunity validation."""
        pool_a, pool_b = generator.generate_profitable_v3_pair(
            pool_a_address="0x0000000000000000000000000000000000000001",
            pool_b_address="0x0000000000000000000000000000000000000002",
            tick_spacing=60,
            price_ratio=1.02,
            liquidity=10**18,
        )

        is_profitable = generator.validate_arbitrage_opportunity(
            pool_a_state=pool_a,
            pool_b_state=pool_b,
            pool_a_fee=Fraction(3, 1000),
            pool_b_fee=Fraction(3, 1000),
        )

        assert is_profitable


class TestDeterminism:
    """Tests for deterministic generation."""

    def test_v2_generation_deterministic(self, generator: PoolStateGenerator) -> None:
        """Test that V2 generation is deterministic with same inputs."""
        address = "0x0000000000000000000000000000000000000001"

        state1 = generator.generate_v2_pool_state(
            address=address,
            reserves_token0=1000000000000000000,
            reserves_token1=2000000000,
        )

        state2 = generator.generate_v2_pool_state(
            address=address,
            reserves_token0=1000000000000000000,
            reserves_token1=2000000000,
        )

        assert state1 == state2

    def test_v3_generation_deterministic(self, generator: PoolStateGenerator) -> None:
        """Test that V3 generation is deterministic with same inputs."""
        address = "0x0000000000000000000000000000000000000001"
        sqrt_price = 79228162514264337593543950336

        state1 = generator.generate_v3_pool_state(
            address=address,
            sqrt_price_x96=sqrt_price,
            liquidity=10**18,
            tick=0,
            tick_spacing=60,
        )

        state2 = generator.generate_v3_pool_state(
            address=address,
            sqrt_price_x96=sqrt_price,
            liquidity=10**18,
            tick=0,
            tick_spacing=60,
        )

        assert state1 == state2

    def test_profitable_pair_deterministic(self, generator: PoolStateGenerator) -> None:
        """Test that profitable pair generation is deterministic."""
        kwargs = {
            "pool_a_address": "0x0000000000000000000000000000000000000001",
            "pool_b_address": "0x0000000000000000000000000000000000000002",
            "fee_a": Fraction(3, 1000),
            "fee_b": Fraction(3, 1000),
            "price_ratio": 1.02,
            "liquidity_base": 10**21,
        }

        pair1 = generator.generate_profitable_v2_pair(**kwargs)
        pair2 = generator.generate_profitable_v2_pair(**kwargs)

        assert pair1[0] == pair2[0]
        assert pair1[1] == pair2[1]
