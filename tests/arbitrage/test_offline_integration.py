"""Offline integration tests replacing RPC-dependent fork tests.

These tests duplicate the scenarios from tests/arbitrage/integration/ using
synthetic data and offline pool objects that inherit from real pool classes
but bypass RPC initialization, so they can run without an RPC connection.

Covers:
- Pool and cycle construction/validation (from test_uniswap_2pool_cycle, test_uniswap_lp_cycle)
- Direction detection and ROE comparison
- Arbitrage calculation with state overrides
- Unprofitable opportunity rejection
- Subscription/unsubscription patterns
- Edge cases (zero reserves, zero max_input, bad pool type)

NOT covered (requires real pool math or on-chain execution):
- Exact V3 tick-crossing swap calculations → keep as fork test
- V2-V4 transaction execution → keep as fork test
- Camelot pool integration → keep as fork test
- Curve pool calculations → keep as fork test
- ProcessPoolExecutor async calculations → keep as fork test
"""

import pickle
from fractions import Fraction

import pytest

from degenbot.arbitrage._legacy import _UniswapLpCycle as UniswapLpCycle
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.arbitrage import ArbitrageError, RateOfExchangeBelowMinimum
from degenbot.exceptions.base import DegenbotValueError
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.fakes.subscribers import FakeSubscriber
from tests.fakes.tokens import FakeToken
from tests.helpers.v2_pool_factory import make_v2_pool

# ==============================================================================
# Token and Pool Constants
# ==============================================================================

WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC_ADDRESS = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
NATIVE_ADDRESS = get_checksum_address("0x0000000000000000000000000000000000000000")
WBTC_WETH_V2_POOL_ADDRESS = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
WBTC_WETH_V3_POOL_ADDRESS = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")

# ==============================================================================
# Fixtures
# ==============================================================================


@pytest.fixture
def wbtc() -> FakeToken:
    return FakeToken(WBTC_ADDRESS, name="Wrapped BTC", symbol="WBTC", decimals=8)


@pytest.fixture
def weth() -> FakeToken:
    return FakeToken(WETH_ADDRESS, name="Wrapped Ether", symbol="WETH", decimals=18)


@pytest.fixture
def usdc() -> FakeToken:
    return FakeToken(USDC_ADDRESS, name="USD Coin", symbol="USDC", decimals=6)


@pytest.fixture
def ether_placeholder() -> FakeToken:
    return FakeToken(NATIVE_ADDRESS, name="Ether", symbol="ETH", decimals=18)


@pytest.fixture
def wbtc_weth_v2_lp(wbtc: FakeToken, weth: FakeToken) -> LiquidityPool:
    """V2 WBTC/WETH pool matching the integration test fixture."""
    return make_v2_pool(
        address=WBTC_WETH_V2_POOL_ADDRESS,
        token0=wbtc,  # type: ignore[arg-type]
        token1=weth,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=16231137593,
        reserves_token1=2571336301536722443178,
        state_block=0,
    )


@pytest.fixture
def wbtc_weth_v3_lp(wbtc: FakeToken, weth: FakeToken) -> UniswapV3Pool:
    """V3 WBTC/WETH pool with minimal tick data."""
    return UniswapV3Pool(
        address=WBTC_WETH_V3_POOL_ADDRESS,
        token0=wbtc,  # type: ignore[arg-type]
        token1=weth,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=31549217861118002279483878013792428,
        tick=257907,
        liquidity=1612978974357835825,
    )


# ==============================================================================
# Construction and Validation
# ==============================================================================


class TestConstruction:
    """Tests for pool and cycle construction without RPC."""

    def test_create_cycle_with_either_token_input(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
        wbtc: FakeToken,
    ):
        """UniswapLpCycle accepts either token as input.

        Mirrors test_create_with_either_token_input.
        """
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=100 * 10**18,
        )
        assert arb.swap_pools[0] is wbtc_weth_v2_lp
        assert arb.swap_pools[1] is wbtc_weth_v3_lp

        arb = UniswapLpCycle(
            id="test_arb",
            input_token=wbtc,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=100 * 10**18,
        )
        assert arb.swap_pools[0] is wbtc_weth_v2_lp
        assert arb.swap_pools[1] is wbtc_weth_v3_lp

    def test_create_two_pool_cycle_with_pools_in_any_order(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
    ):
        """Two-pool cycle accepts pools in either order.

        Mirrors test_create_arb_with_either_token_input_or_pools_in_any_order.
        """
        # V2 first, V3 second
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=100 * 10**18,
        )
        assert arb.swap_pools[0] is wbtc_weth_v2_lp
        assert arb.swap_pools[1] is wbtc_weth_v3_lp

        # V3 first, V2 second
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v3_lp, wbtc_weth_v2_lp],
            max_input=100 * 10**18,
        )
        assert arb.swap_pools[0] is wbtc_weth_v3_lp
        assert arb.swap_pools[1] is wbtc_weth_v2_lp

    def test_no_max_input_defaults(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
    ):
        """UniswapLpCycle without max_input uses default (mirrors test_no_max_input)."""
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
        )
        assert arb.max_input == 100 * 10**18

    def test_zero_max_input_raises(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
    ):
        """Zero max_input raises DegenbotValueError (mirrors test_zero_max_input)."""
        with pytest.raises(DegenbotValueError, match=r"Maximum input must be positive."):
            UniswapLpCycle(
                id="test_arb",
                input_token=weth,
                swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
                max_input=0,
            )

    def test_duplicate_pools_rejected(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        weth: FakeToken,
    ):
        """Duplicate pools in swap_pools are rejected."""
        with pytest.raises(DegenbotValueError, match=r"Swap pools must not contain duplicates"):
            UniswapLpCycle(
                id="test_arb",
                input_token=weth,
                swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v2_lp],
                max_input=100 * 10**18,
            )


# ==============================================================================
# Test: Direction Detection and ROE
# ==============================================================================


class TestDirectionDetection:
    """Tests for arbitrage direction detection via rate of exchange comparison."""

    def test_v2_v2_profitable_direction(
        self,
        wbtc: FakeToken,
        weth: FakeToken,
    ):
        """V2-V2 cycle: pool with higher ROE for input token is the buy pool.

        Mirrors the logic from test_pre_calc_check in test_uniswap_lp_cycle.py.
        """
        # Pool A: 16000 WBTC / 2500 WETH (WETH cheaper → buy WETH here)
        pool_a = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=16_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )

        # Pool B: 15000 WBTC / 2500 WETH (WETH more expensive)
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=15_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )

        # Pool A has higher ROE → buy WETH in pool A, sell in pool B
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[pool_a, pool_b],
            max_input=100 * 10**18,
        )
        result = arb.calculate()
        assert result.profit_amount > 0

    def test_v2_v2_unprofitable_direction_raises(
        self,
        wbtc: FakeToken,
        weth: FakeToken,
    ):
        """V2-V2 cycle in wrong direction raises RateOfExchangeBelowMinimum.

        Mirrors test_pre_calc_check in test_uniswap_lp_cycle.py.
        """
        pool_a = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=16_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=15_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )

        # Reverse order: pool B first (lower ROE for WETH)
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[pool_b, pool_a],
            max_input=100 * 10**18,
        )
        with pytest.raises(RateOfExchangeBelowMinimum):
            arb.calculate()

    def test_v2_state_override_creates_profitable_opportunity(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc: FakeToken,
        weth: FakeToken,
    ):
        """Manipulating V2 reserves creates an arbitrage opportunity.

        Mirrors test_v2_v4_calculation from test_uniswap_2pool_cycle.py.
        """
        # Create a second V2 pool with more WBTC (cheaper WBTC)
        pool_b = make_v2_pool(
            address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D941",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=16231137593 + 10 * 10**8,  # 10 extra WBTC
            reserves_token1=2571336301536722443178,
            state_block=1,
        )

        arb = UniswapLpCycle(
            id="test_arb",
            input_token=wbtc,
            swap_pools=[wbtc_weth_v2_lp, pool_b],
            max_input=100 * 10**18,
        )
        result = arb.calculate()
        assert result.profit_amount > 0

    def test_v2_state_override_rejects_unprofitable(
        self,
        wbtc: FakeToken,
        weth: FakeToken,
    ):
        """State override that reverses the ROE direction is rejected.

        Mirrors test_v2_v4_calculation_rejects_unprofitable_opportunity.
        """
        # Pool A: 16000 WBTC / 2500 WETH (WETH cheaper here)
        pool_a = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=16_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )
        # Pool B: 15000 WBTC / 2500 WETH (WETH more expensive here)
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=15_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )

        # Create cycle in profitable direction: buy WETH in pool A, sell in pool B
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[pool_a, pool_b],
            max_input=100 * 10**18,
        )

        # Override pool A with MORE WBTC → WETH becomes even cheaper there
        # This makes the arbitrage MORE profitable, not less
        # So we need the OPPOSITE: override pool B to have more WBTC
        # so that WETH is cheaper in pool B, reversing the direction
        override_state = UniswapV2PoolState(
            address=pool_b.address,
            reserves_token0=18_000_000_000,  # Much more WBTC → cheaper WETH
            reserves_token1=2_500 * 10**18,
            block=None,
        )

        # With pool B now having cheaper WETH than pool A,
        # the direction pool_a → pool_b is unprofitable
        with pytest.raises(RateOfExchangeBelowMinimum):
            arb.calculate(state_overrides={pool_b: override_state})


# ==============================================================================
# Test: V2-V2 Arbitrage Calculations
# ==============================================================================


class TestV2V2Arbitrage:
    """Tests for V2-V2 arbitrage calculation accuracy using offline pools."""

    def test_v2_v2_arbitrage_with_known_reserves(
        self,
        usdc: FakeToken,
        weth: FakeToken,
    ):
        """V2-V2 arbitrage with full-precision reserves produces a positive profit."""
        pool_a = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000_000_000,  # 2M USDC (6 dec)
            reserves_token1=1_000 * 10**18,  # 1000 WETH (18 dec)
            state_block=1,
        )
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_040_000_000_000,  # 2.04M USDC
            reserves_token1=1_000 * 10**18,  # 1000 WETH
            state_block=1,
        )

        arb = UniswapLpCycle(
            id="test_arb",
            input_token=usdc,
            swap_pools=[pool_a, pool_b],
            max_input=10**21,
        )
        result = arb.calculate()
        assert result.profit_amount > 0
        assert result.input_amount > 0

    def test_v2_v2_state_override_changes_profit(
        self,
        usdc: FakeToken,
        weth: FakeToken,
    ):
        """State overrides change the arbitrage result.

        Mirrors test_arbitrage_with_overrides from test_uniswap_lp_cycle.py.
        """
        pool_a = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000_000_000,  # 2M USDC
            reserves_token1=1_000 * 10**18,  # 1000 WETH
            state_block=1,
        )
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_040_000_000_000,  # 2.04M USDC (WETH cheaper here)
            reserves_token1=1_000 * 10**18,
            state_block=1,
        )

        arb = UniswapLpCycle(
            id="test_arb",
            input_token=usdc,
            swap_pools=[pool_a, pool_b],
            max_input=10**21,
        )

        # Calculate with original state
        result_original = arb.calculate()

        # Override pool B with 20% more USDC (bigger opportunity in pool B)
        override_state = UniswapV2PoolState(
            address=pool_b.address,
            reserves_token0=2_448_000_000_000,  # 20% more USDC in pool B
            reserves_token1=1_000 * 10**18,
            block=None,
        )

        result_override = arb.calculate(state_overrides={pool_b: override_state})

        # Bigger price discrepancy = more profit
        assert result_override.profit_amount > result_original.profit_amount

    def test_v2_v2_arbitrage_symmetric_viability(
        self,
        usdc: FakeToken,
        weth: FakeToken,
    ):
        """Only one direction is profitable for a given pair of pools."""
        pool_a = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000_000_000,
            reserves_token1=1_000 * 10**18,
            state_block=1,
        )
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_040_000_000_000,
            reserves_token1=1_000 * 10**18,
            state_block=1,
        )

        # Direction 1: Pool A first (profitable)
        arb_1 = UniswapLpCycle(
            id="arb_1",
            input_token=usdc,
            swap_pools=[pool_a, pool_b],
            max_input=10**21,
        )
        result_1 = arb_1.calculate()
        assert result_1.profit_amount > 0

        # Direction 2: Pool B first (unprofitable - opposite ROE)
        arb_2 = UniswapLpCycle(
            id="arb_2",
            input_token=usdc,
            swap_pools=[pool_b, pool_a],
            max_input=10**21,
        )
        with pytest.raises(RateOfExchangeBelowMinimum):
            arb_2.calculate()


# ==============================================================================
# Test: Subscription Patterns
# ==============================================================================


class TestSubscriptions:
    """Tests for pool-cycle subscription/unsubscription patterns."""

    def test_arbitrage_helper_subscriptions(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
    ):
        """Cycle subscribes to pool state updates.

        Mirrors test_arbitrage_helper_subscriptions from test_uniswap_lp_cycle.py.
        """
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=100 * 10**18,
        )

        assert arb in wbtc_weth_v2_lp._subscribers
        assert arb in wbtc_weth_v3_lp._subscribers

    def test_pool_helper_unsubscriptions(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
    ):
        """Cycle can unsubscribe from pools.

        Mirrors test_pool_helper_unsubscriptions from test_uniswap_lp_cycle.py.
        """
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=100 * 10**18,
        )

        assert arb in wbtc_weth_v2_lp._subscribers
        assert arb in wbtc_weth_v3_lp._subscribers

        wbtc_weth_v2_lp.unsubscribe(arb)
        wbtc_weth_v3_lp.unsubscribe(arb)

        assert arb not in wbtc_weth_v2_lp._subscribers
        assert arb not in wbtc_weth_v3_lp._subscribers

    def test_pool_state_update_notifies_subscribers(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
    ):
        """Pool state updates notify subscribers.

        Mirrors the subscriber notification check in test_arbitrage_helper_subscriptions.
        """
        subscriber = FakeSubscriber()
        subscriber.subscribe(publisher=wbtc_weth_v2_lp)

        assert len(subscriber.inbox) == 0

        # Trigger state update via external_update
        wbtc_weth_v2_lp.external_update(
            update=UniswapV2PoolExternalUpdate(
                block_number=1,
                reserves_token0=69,
                reserves_token1=420,
            )
        )

        # Verify subscriber was notified
        assert len(subscriber.inbox) == 1
        assert subscriber.inbox[0]["from"] == wbtc_weth_v2_lp
        assert isinstance(subscriber.inbox[0]["message"], UniswapV2PoolStateUpdated)


# ==============================================================================
# Test: Edge Cases
# ==============================================================================


class TestEdgeCases:
    """Edge case tests from the integration suite."""

    def test_pickle_cycle(
        self,
        wbtc_weth_v2_lp: LiquidityPool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth: FakeToken,
    ):
        """Arbitrage cycle can be pickled.

        Mirrors test_pickle_arb from test_uniswap_curve_cycle.py.
        """
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=100 * 10**18,
        )
        data = pickle.dumps(arb)
        restored = pickle.loads(data)
        assert restored.id == "test_arb"

    def test_v2_pool_zero_reserves_not_viable(
        self,
        wbtc: FakeToken,
        weth: FakeToken,
    ):
        """V2 pool with zero reserves is not viable for swaps.

        Mirrors test_arb_calculation_pre_checks_v2.
        """
        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=0,
            reserves_token1=0,
            state_block=1,
        )

        # Zero-reserve pool should not be viable
        pool_b = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=15_000_000_000,
            reserves_token1=2_500 * 10**18,
            state_block=1,
        )

        # Creating a cycle with a zero-reserve pool should raise during viability check
        arb = UniswapLpCycle(
            id="test_arb",
            input_token=weth,
            swap_pools=[pool, pool_b],
            max_input=100 * 10**18,
        )

        # Calculation should fail due to non-viable pool
        with pytest.raises(ArbitrageError):
            arb.calculate()

    def test_offline_erc20_token_equality(self):
        """FakeToken equality by address."""
        token1 = FakeToken(WETH_ADDRESS, "WETH", 18)
        token2 = FakeToken(WETH_ADDRESS, "WETH", 18)
        assert token1 == token2
        assert hash(token1) == hash(token2)
        assert token1 == token2

    def test_offline_erc20_token_inequality(self):
        """Different addresses are not equal."""
        token1 = FakeToken(WETH_ADDRESS, "WETH", 18)
        token2 = FakeToken(WBTC_ADDRESS, "WBTC", 8)
        assert token1 != token2
        assert hash(token1) != hash(token2)


# ==============================================================================
# Test: V2 Pool Calculation Accuracy
# ==============================================================================


class TestV2PoolAccuracy:
    """Verify LiquidityPool calculations match the Uniswap V2 constant product formula.

    Since LiquidityPool inherits from LiquidityPool, its calculation methods
    should produce identical results to the real pool class.
    """

    def test_v2_swap_matches_formula(
        self,
        usdc: FakeToken,
        weth: FakeToken,
    ):
        """LiquidityPool swap calculation matches manual V2 formula."""
        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000_000,  # 2000 USDC
            reserves_token1=10**18,  # 1 WETH
            state_block=1,
        )

        amount_in = 1_000_000_000
        fee_numer = 997
        fee_denom = 1000
        reserves_in = 2_000_000_000
        reserves_out = 10**18
        expected_out = (amount_in * fee_numer * reserves_out) // (
            reserves_in * fee_denom + amount_in * fee_numer
        )

        actual_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=amount_in,
        )

        assert actual_out == expected_out

    def test_v2_reverse_swap(
        self,
        usdc: FakeToken,
        weth: FakeToken,
    ):
        """LiquidityPool reverse swap (WETH for USDC) matches manual formula."""
        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000_000,
            reserves_token1=10**18,
            state_block=1,
        )

        # Sell 0.5 WETH for USDC
        amount_in = 500_000_000_000_000_000  # 0.5 WETH
        fee_numer = 997
        fee_denom = 1000
        reserves_in = 10**18  # WETH is token1 (input)
        reserves_out = 2_000_000_000  # USDC is token0 (output)
        expected_out = (amount_in * fee_numer * reserves_out) // (
            reserves_in * fee_denom + amount_in * fee_numer
        )

        actual_out = pool.calculate_tokens_out_from_tokens_in(
            token_in=weth,
            token_in_quantity=amount_in,
        )

        assert actual_out == expected_out

    def test_v2_exchange_rate(
        self,
        usdc: FakeToken,
        weth: FakeToken,
    ):
        """LiquidityPool exchange rate matches manual calculation."""
        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=usdc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000_000,
            reserves_token1=10**18,
            state_block=1,
        )

        # Exchange rate for WETH (token1) = reserve1 / reserve0
        weth_rate = pool.get_absolute_exchange_rate(token=weth)
        expected_rate = Fraction(10**18, 2_000_000_000)
        assert weth_rate == expected_rate

        # Exchange rate for USDC (token0) = reserve0 / reserve1
        usdc_rate = pool.get_absolute_exchange_rate(token=usdc)
        expected_usdc_rate = Fraction(2_000_000_000, 10**18)
        assert usdc_rate == expected_usdc_rate
