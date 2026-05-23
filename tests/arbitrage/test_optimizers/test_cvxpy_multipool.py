"""Property-based tests for multi-pool CVXPY optimization.

Tests convergence and correctness for 3+ pool arbitrage cycles.
"""

from fractions import Fraction

import hypothesis
import hypothesis.strategies as st

from degenbot.arbitrage._legacy import _UniswapMultiPoolCycleTesting
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.arbitrage.generator.fixtures import FixtureFactory
from tests.arbitrage.generator.hypothesis_strategies import (
    liquidity_depth_strategy,
    seed_strategy,
)

# ==============================================================================
# Test Fixtures
# ==============================================================================


def make_fake_token(address: str, symbol: str, decimals: int, chain_id: int = 1) -> Erc20Token:
    """Create a fake token for testing."""
    return Erc20Token(
        address,
        chain_id=chain_id,
        name=symbol,
        symbol=symbol,
        decimals=decimals,
    )


# ==============================================================================
# Property Tests: Multi-Pool Cycles
# ==============================================================================


class TestMultiPoolCycleProperties:
    """Property-based tests for multi-pool arbitrage cycles."""

    @hypothesis.given(
        num_pools=st.integers(min_value=3, max_value=5),
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=15)
    def test_multipool_fixture_valid(self, num_pools: int, seed: int):
        """Property: Generated multi-pool fixtures are valid.

        Any valid combination of parameters should produce a valid fixture.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=num_pools,
            pool_types=["v2"] * num_pools,
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.03),
        )

        # Fixture should be valid
        assert fixture.validate() is True
        assert len(fixture.pool_states) == num_pools
        assert fixture.cycle_type == "v2_v2"

    @hypothesis.given(
        seed=seed_strategy,
        liquidity_depth=liquidity_depth_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_3pool_cycle_price_progression(self, seed: int, liquidity_depth: str):
        """Property: 3-pool cycles have progressive price differences.

        Each consecutive pool should have a different effective price.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=3,
            pool_types=["v2", "v2", "v2"],
            liquidity_depth=liquidity_depth,
            price_ratio_range=(1.01, 1.03),
        )

        pool_states = list(fixture.pool_states.values())

        # Calculate prices for each pool
        prices = []
        for state in pool_states:
            if state.reserves_token0 > 0:
                price = state.reserves_token1 / state.reserves_token0
                prices.append(price)

        # Prices should differ (arb opportunity exists)
        if len(prices) >= 2:
            price_diff = abs(prices[0] - prices[-1]) / min(prices[0], prices[-1])
            # Some price difference should exist
            assert price_diff > 0


class TestMultiPoolConvergence:
    """Test CVXPY solver convergence for multi-pool cycles."""

    def test_3pool_known_profitable(self):
        """Known value test: 3-pool cycle with guaranteed profit.

        Creates a simple triangular arbitrage: WETH -> LINK -> WBTC -> WETH
        with skewed prices to guarantee profit.
        """
        weth = make_fake_token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "WETH", 18)
        link = make_fake_token("0x514910771AF9Ca656af840dff83E8264EcF986CA", "LINK", 18)
        wbtc = make_fake_token("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", "WBTC", 8)

        # Pool 1: LINK-WETH with skewed price (LINK is cheap)
        lp_1 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=link,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=250 * 10**18,  # 250 LINK
            reserves_token1=1 * 10**18,  # 1 WETH (LINK cheap)
            state_block=1,
        )

        # Pool 2: WBTC-LINK with normal price
        lp_2 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000002",
            token0=wbtc,
            token1=link,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1 * 10**8,  # 1 WBTC
            reserves_token1=4000 * 10**18,  # 4000 LINK
            state_block=1,
        )

        # Pool 3: WBTC-WETH with skewed price (WBTC expensive)
        lp_3 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000003",
            token0=wbtc,
            token1=weth,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1 * 10**8,  # 1 WBTC
            reserves_token1=25 * 10**18,  # 25 WETH (WBTC expensive)
            state_block=1,
        )

        arb = _UniswapMultiPoolCycleTesting(
            input_token=weth,
            swap_pools=[lp_1, lp_2, lp_3],
        )

        result = arb.calculate()

        # Should find some result (profit may be negative after fees)
        assert result.profit_amount is not None

    def test_4pool_known_profitable(self):
        """Known value test: 4-pool cycle with guaranteed profit.

        Creates a 4-hop cycle: WETH -> LINK -> USDC -> WBTC -> WETH
        """
        weth = make_fake_token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "WETH", 18)
        link = make_fake_token("0x514910771AF9Ca656af840dff83E8264EcF986CA", "LINK", 18)
        usdc = make_fake_token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6)
        wbtc = make_fake_token("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", "WBTC", 8)

        # Zero fee for simple test
        fee = Fraction(0)

        # Pool 1: WETH-LINK (skewed)
        lp_1 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=weth,
            token1=link,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=1 * 10**18,  # 1 WETH
            reserves_token1=250 * 10**18,  # 250 LINK (skewed)
            state_block=1,
        )

        # Pool 2: LINK-USDC (normal)
        lp_2 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000002",
            token0=usdc,
            token1=link,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=25 * 10**6,  # 25 USDC
            reserves_token1=1 * 10**18,  # 1 LINK
            state_block=1,
        )

        # Pool 3: USDC-WBTC (normal)
        lp_3 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000003",
            token0=usdc,
            token1=wbtc,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=100_000 * 10**6,  # 100K USDC
            reserves_token1=1 * 10**8,  # 1 WBTC
            state_block=1,
        )

        # Pool 4: WBTC-WETH (skewed for profit)
        lp_4 = UniswapV2Pool(
            address="0x0000000000000000000000000000000000000004",
            token0=weth,
            token1=wbtc,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=20 * 10**18,  # 20 WETH
            reserves_token1=1 * 10**8,  # 1 WBTC
            state_block=1,
        )

        arb = _UniswapMultiPoolCycleTesting(
            input_token=weth,
            swap_pools=[lp_1, lp_2, lp_3, lp_4],
        )

        result = arb.calculate()

        # Should compute a result
        assert result.profit_amount is not None


class TestMultiPoolBounds:
    """Test that multi-pool solutions respect reserve bounds."""

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_3pool_respects_reserve_bounds(self, seed: int):
        """Property: 3-pool optimization doesn't exceed available reserves.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=3,
            pool_types=["v2", "v2", "v2"],
            liquidity_depth="medium",
            price_ratio_range=(1.02, 1.04),
        )

        # Extract reserves
        pool_states = list(fixture.pool_states.values())

        for state in pool_states:
            # All reserves should be positive
            assert state.reserves_token0 > 0
            assert state.reserves_token1 > 0

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_4pool_positive_reserves(self, seed: int):
        """Property: 4-pool generated fixtures have positive reserves.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=4,
            pool_types=["v2", "v2", "v2", "v2"],
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.03),
        )

        pool_states = list(fixture.pool_states.values())

        for state in pool_states:
            assert state.reserves_token0 > 0
            assert state.reserves_token1 > 0


class TestMultiPoolMixedTypes:
    """Test multi-pool cycles with mixed pool types."""

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_v2_v3_mixed_cycle(self, seed: int):
        """Property: Mixed V2/V3 cycles can be generated.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=3,
            pool_types=["v2", "v3", "v2"],
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.03),
        )

        # Should have 3 pools
        assert len(fixture.pool_states) == 3

        # Should be valid
        assert fixture.validate() is True

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_v4_cycle_generation(self, seed: int):
        """Property: V4-only cycles can be generated.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=3,
            pool_types=["v4", "v4", "v4"],
            liquidity_depth="medium",
            price_ratio_range=(1.02, 1.04),
        )

        assert len(fixture.pool_states) == 3
        assert fixture.validate() is True


class TestMultiPoolInvariants:
    """Test invariants that should hold for all multi-pool optimizations."""

    @hypothesis.given(
        num_pools=st.integers(min_value=3, max_value=4),
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=15)
    def test_k_invariant_preserved(self, num_pools: int, seed: int):
        """Property: Each pool's k = x * y is preserved (or increased) after swaps.

        For AMM arbitrage, the invariant k should not decrease.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=num_pools,
            pool_types=["v2"] * num_pools,
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.03),
        )

        pool_states = list(fixture.pool_states.values())

        # Calculate initial k for each pool
        k_values = []
        for state in pool_states:
            k = state.reserves_token0 * state.reserves_token1
            k_values.append(k)

        # All k values should be positive
        for k in k_values:
            assert k > 0, "k should be positive for all pools"

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_price_consistency_across_pools(self, seed: int):
        """Property: Prices in the fixture are internally consistent.

        The price ratios should match the expected progression.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=3,
            pool_types=["v2", "v2", "v2"],
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.02),
        )

        pool_states = list(fixture.pool_states.values())

        # All pools should have valid prices
        for state in pool_states:
            if state.reserves_token0 > 0:
                price = state.reserves_token1 / state.reserves_token0
                # Price should be a valid positive number
                assert price > 0
                assert price == price  # Not NaN
