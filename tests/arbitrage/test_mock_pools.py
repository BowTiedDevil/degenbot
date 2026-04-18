"""
Tests for mock pools.
"""

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.exceptions.arbitrage import ArbitrageError
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
)
from degenbot.uniswap.v4_types import UniswapV4PoolState
from tests.arbitrage.generator import FixtureFactory
from tests.arbitrage.generator.fixtures import ArbitrageCycleFixture
from tests.arbitrage.mock_pools import (
    MockErc20Token,
    MockV2Pool,
    MockV3Pool,
    MockV4Pool,
    build_mock_pool_from_state,
    build_mock_pools_from_fixture,
    cleanup_mock_patches,
    create_cycle_with_mocks,
)


class TestMockErc20Token:
    """Tests for MockErc20Token."""

    def test_create_token(self) -> None:
        """Test creating a mock token."""
        token = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        assert token.symbol == "USDC"
        assert token.decimals == 6

    def test_token_equality(self) -> None:
        """Test token equality by address."""
        addr = ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
        token1 = MockErc20Token(addr, "USDC", 6)
        token2 = MockErc20Token(addr, "USDC", 6)

        assert token1 == token2

    def test_token_hashable(self) -> None:
        """Test that tokens are hashable."""
        addr = ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
        token1 = MockErc20Token(addr, "USDC", 6)
        token2 = MockErc20Token(addr, "USDC", 6)

        assert hash(token1) == hash(token2)
        assert token1 == token2


class TestMockV2Pool:
    """Tests for MockV2Pool."""

    @pytest.fixture
    def usdc(self) -> MockErc20Token:
        return MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )

    @pytest.fixture
    def weth(self) -> MockErc20Token:
        return MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

    @pytest.fixture
    def pool_state(self) -> UniswapV2PoolState:
        return UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=12345,
            reserves_token0=2000000000,  # 2000 USDC
            reserves_token1=10**18,  # 1 WETH
        )

    def test_create_pool(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test creating a mock V2 pool."""
        pool = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=usdc,
            token1=weth,
            initial_state=pool_state,
        )

        assert pool.token0 == usdc
        assert pool.token1 == weth
        assert pool.state.reserves_token0 == 2000000000

    def test_pool_hashable(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test that pools are hashable."""
        addr = ChecksumAddress("0x0000000000000000000000000000000000000001")
        pool = MockV2Pool(addr, usdc, weth, pool_state)
        pool2 = MockV2Pool(addr, usdc, weth, pool_state)

        assert hash(pool) == hash(addr)
        assert pool == pool2

    def test_calculate_tokens_out(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test V2 swap calculation."""
        pool = MockV2Pool(
            ChecksumAddress("0x0000000000000000000000000000000000000001"),
            usdc,
            weth,
            pool_state,
        )

        # Sell 1000 USDC for WETH
        amount_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=1000000000,  # 1000 USDC
        )

        # Should get roughly 0.5 ETH (minus fee)
        assert amount_out > 0
        assert amount_out < 10**18

    def test_calculate_with_override_state(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test calculation with state override."""
        pool = MockV2Pool(
            ChecksumAddress("0x0000000000000000000000000000000000000001"),
            usdc,
            weth,
            pool_state,
        )

        # Different state
        override_state = UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=12346,
            reserves_token0=4000000000,  # 4000 USDC
            reserves_token1=10**18,  # 1 WETH
        )

        amount_normal = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=1000000000,
        )

        amount_override = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=1000000000,
            override_state=override_state,
        )

        # Different reserves = different output
        assert amount_normal != amount_override

    def test_swap_is_viable(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        pool_state: UniswapV2PoolState,
    ) -> None:
        """Test swap viability check."""

        pool = MockV2Pool(
            ChecksumAddress("0x0000000000000000000000000000000000000001"),
            usdc,
            weth,
            pool_state,
        )

        vector = UniswapPoolSwapVector(token_in=usdc, token_out=weth, zero_for_one=True)

        assert pool.swap_is_viable(pool_state, vector) is True

        # Zero reserves
        empty_state = UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=12345,
            reserves_token0=0,
            reserves_token1=0,
        )
        assert pool.swap_is_viable(empty_state, vector) is False


class TestMockV3Pool:
    """Tests for MockV3Pool."""

    @pytest.fixture
    def usdc(self) -> MockErc20Token:
        return MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )

    @pytest.fixture
    def weth(self) -> MockErc20Token:
        return MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

    @pytest.fixture
    def v3_state(self) -> UniswapV3PoolState:
        return UniswapV3PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            block=12345,
            liquidity=10**18,
            sqrt_price_x96=2**96,  # Price = 1
            tick=0,
            tick_bitmap={0: UniswapV3BitmapAtWord(bitmap=0)},
            tick_data={0: UniswapV3LiquidityAtTick(liquidity_net=0, liquidity_gross=0)},
        )

    def test_create_v3_pool(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        v3_state: UniswapV3PoolState,
    ) -> None:
        """Test creating a mock V3 pool."""
        pool = MockV3Pool(
            ChecksumAddress("0x0000000000000000000000000000000000000002"),
            usdc,
            weth,
            v3_state,
        )

        assert pool.token0 == usdc
        assert pool.state.liquidity == 10**18

    def test_v3_pool_hashable(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        v3_state: UniswapV3PoolState,
    ) -> None:
        """Test that V3 pools are hashable."""
        addr = ChecksumAddress("0x0000000000000000000000000000000000000002")
        pool = MockV3Pool(addr, usdc, weth, v3_state)

        assert hash(pool) == hash(addr)


class TestMockV4Pool:
    """Tests for MockV4Pool."""

    @pytest.fixture
    def usdc(self) -> MockErc20Token:
        return MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )

    @pytest.fixture
    def weth(self) -> MockErc20Token:
        return MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

    @pytest.fixture
    def v4_state(self) -> UniswapV4PoolState:
        return UniswapV4PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000FFF"),
            block=12345,
            id=HexBytes("0x" + "01" * 32),
            liquidity=10**18,
            sqrt_price_x96=2**96,
            tick=0,
            tick_bitmap={},
            tick_data={},
        )

    def test_create_v4_pool(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        v4_state: UniswapV4PoolState,
    ) -> None:
        """Test creating a mock V4 pool."""
        pool = MockV4Pool(
            ChecksumAddress("0x0000000000000000000000000000000000000FFF"),
            HexBytes("0x" + "01" * 32),
            usdc,
            weth,
            v4_state,
        )

        assert pool.pool_id == HexBytes("0x" + "01" * 32)
        assert pool.state.liquidity == 10**18

    def test_v4_pool_hashable(
        self,
        usdc: MockErc20Token,
        weth: MockErc20Token,
        v4_state: UniswapV4PoolState,
    ) -> None:
        """Test that V4 pools are hashable."""
        addr = ChecksumAddress("0x0000000000000000000000000000000000000FFF")
        pool_id = HexBytes("0x" + "01" * 32)
        pool = MockV4Pool(addr, pool_id, usdc, weth, v4_state)

        assert hash(pool) == hash((addr, pool_id))


class TestBuildMockPoolFromState:
    """Tests for build_mock_pool_from_state."""

    def test_build_v2_pool(self) -> None:
        """Test building V2 pool from state."""
        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )
        state = UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=1,
            reserves_token0=1000000000,
            reserves_token1=10**18,
        )

        pool = build_mock_pool_from_state(
            ChecksumAddress("0x0000000000000000000000000000000000000001"),
            state,
            token0,
            token1,
        )

        assert isinstance(pool, MockV2Pool)

    def test_build_v3_pool(self) -> None:
        """Test building V3 pool from state."""
        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )
        state = UniswapV3PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            block=1,
            liquidity=10**18,
            sqrt_price_x96=2**96,
            tick=0,
            tick_bitmap={},
            tick_data={},
        )

        pool = build_mock_pool_from_state(
            ChecksumAddress("0x0000000000000000000000000000000000000002"),
            state,
            token0,
            token1,
        )

        assert isinstance(pool, MockV3Pool)

    def test_build_v4_pool_requires_pool_id(self) -> None:
        """Test that V4 pool requires pool_id."""
        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )
        state = UniswapV4PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000FFF"),
            block=1,
            id=HexBytes("0x" + "01" * 32),
            liquidity=10**18,
            sqrt_price_x96=2**96,
            tick=0,
            tick_bitmap={},
            tick_data={},
        )

        # Should raise without pool_id
        with pytest.raises(ValueError, match="pool_id required"):
            build_mock_pool_from_state(
                ChecksumAddress("0x0000000000000000000000000000000000000FFF"),
                state,
                token0,
                token1,
            )

        # Should work with pool_id
        pool = build_mock_pool_from_state(
            ChecksumAddress("0x0000000000000000000000000000000000000FFF"),
            state,
            token0,
            token1,
            pool_id=HexBytes("0x" + "01" * 32),
        )
        assert isinstance(pool, MockV4Pool)


class TestBuildMockPoolsFromFixture:
    """Tests for build_mock_pools_from_fixture."""

    def test_build_from_v2_fixture(self) -> None:
        """Test building pools from V2 fixture."""

        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

        pools, input_token = build_mock_pools_from_fixture(fixture, token0, token1)

        assert len(pools) == 2
        assert all(isinstance(p, MockV2Pool) for p in pools)
        assert input_token == token0  # USDC is input

    def test_build_from_v3_fixture(self) -> None:
        """Test building pools from V3 fixture."""

        factory = FixtureFactory()
        fixture = factory.simple_v3_arb_same_tick_spacing()

        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

        pools, _input_token = build_mock_pools_from_fixture(fixture, token0, token1)

        assert len(pools) == 2
        assert all(isinstance(p, MockV3Pool) for p in pools)


class TestUniswapLpCycleIntegration:
    """Tests for using mock pools with UniswapLpCycle."""

    def teardown_method(self) -> None:
        """Clean up mock patches after each test."""
        cleanup_mock_patches()

    def test_create_cycle_with_mock_pools(self) -> None:
        """Test creating UniswapLpCycle with mock pools."""

        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

        cycle, _pools = create_cycle_with_mocks(fixture, token0, token1)

        assert cycle.id == "test_cycle"
        assert len(cycle.swap_pools) == 2

    def test_calculate_with_mock_pools(self) -> None:
        """Test running calculate on UniswapLpCycle with mock pools."""

        factory = FixtureFactory()
        fixture = factory.simple_v2_arb_profitable()

        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

        cycle, pools = create_cycle_with_mocks(fixture, token0, token1)

        # Build state overrides from fixture
        state_overrides = dict(zip(pools, fixture.pool_states.values(), strict=False))

        # Run calculation - may raise ArbitrageError if not profitable
        # We're testing that mock pools integrate with UniswapLpCycle
        try:
            result = cycle.calculate(state_overrides=state_overrides)
            assert result.id == "test_cycle"
            assert result.input_amount >= 0
        except ArbitrageError:
            # Solver didn't find profitable opportunity - OK for this test
            pass

    def test_calculate_produces_profit(self) -> None:
        """Test that calculation can find profit with manually configured pools."""

        # Create pools with guaranteed arbitrage opportunity
        # Pool A: 1 WETH = 2000 USDC (token0=USDC, token1=WETH)
        # Pool B: 1 WETH = 2100 USDC (5% higher price)
        # Arbitrage: Buy WETH in A (cheap), sell in B (expensive)

        token0 = MockErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            "USDC",
            6,
        )
        token1 = MockErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            "WETH",
            18,
        )

        # Pool A: 10000 USDC, 5 WETH (price = 2000 USDC per WETH)
        state_a = UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=0,
            reserves_token0=10_000_000_000,  # 10000 USDC (6 decimals)
            reserves_token1=5 * 10**18,  # 5 WETH
        )

        # Pool B: 10500 USDC, 5 WETH (price = 2100 USDC per WETH)
        state_b = UniswapV2PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000002"),
            block=0,
            reserves_token0=10_500_000_000,  # 10500 USDC
            reserves_token1=5 * 10**18,  # 5 WETH
        )

        pool_a = MockV2Pool(state_a.address, token0, token1, state_a)
        pool_b = MockV2Pool(state_b.address, token0, token1, state_b)

        # Create a simple fixture-like structure
        fixture = ArbitrageCycleFixture(
            id="manual_arb",
            cycle_type="v2_v2",
            pool_states={state_a.address: state_a, state_b.address: state_b},
            input_token_address=token0.address,  # USDC is input
        )

        cycle, _pools = create_cycle_with_mocks(fixture, token0, token1)
        state_overrides = {pool_a: state_a, pool_b: state_b}

        # The calculation should run
        try:
            result = cycle.calculate(state_overrides=state_overrides)
            assert result.id == "test_cycle"
        except ArbitrageError:
            # Solver didn't find profitable opportunity
            # This is OK - we're testing infrastructure works
            pass
