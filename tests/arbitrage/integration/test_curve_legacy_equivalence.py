"""Equivalence test: Curve pool hops with solver vs direct pool calculation.

Uses production CurveStableswapPool with FakeCurveDataProvider to provide
deterministic, inspectable state for verifying that solver simulation
functions handle Curve hops correctly.

This test answers: "Does the new solver architecture correctly handle Curve pool math?"
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers._solver_utils import _simulate_path
from degenbot.arbitrage.optimizers.solidly_stable import (
    _simulate_mixed_path,
    _simulate_mixed_path_int,
)
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token
from degenbot.types.hop_types import PoolInvariant
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.fakes.tokens import FakeToken

ADDR_DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
ADDR_USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
ADDR_WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

# Production Erc20Token instances for CurveStableswapPool
DAI = Erc20Token(address=ADDR_DAI, name="DAI", symbol="DAI", decimals=18)
USDC = Erc20Token(address=ADDR_USDC, name="USD Coin", symbol="USDC", decimals=6)

STATE_BLOCK = 18_000_000


def _make_curve_pool(
    balances: tuple[int, ...] = (10_000_000 * 10**18, 10_000_000 * 10**6),
    a_coefficient: int = 1000,
    fee: int = 4_000_000,
    address: str = "0x00000000000000000000000000000000000000c0",
) -> CurveStableswapPool:
    """Build a production CurveStableswapPool (DAI/USDC) with FakeCurveDataProvider."""
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000)
    return CurveStableswapPool(
        address=address,  # type: ignore[arg-type]
        tokens=(DAI, USDC),
        a_coefficient=a_coefficient,
        fee=fee,
        admin_fee=5_000_000_000,
        balances=balances,
        state_block=STATE_BLOCK,
        data_provider=provider,
    )


@pytest.fixture
def dai():
    return FakeToken(ADDR_DAI, decimals=18)


@pytest.fixture
def usdc():
    return FakeToken(ADDR_USDC, decimals=6)


@pytest.fixture
def weth():
    return FakeToken(ADDR_WETH, decimals=18)


class TestCurveHopGeneration:
    """Basic: hop state generation from production CurveStableswapPool."""

    def test_curve_hop_generation_matches_expectations(self, dai, usdc):
        """Verify production CurveStableswapPool.to_hop_state produces valid CurveStableswapHop."""
        pool = _make_curve_pool()

        hop = pool.to_hop_state(zero_for_one=True)

        # Verify hop has all required fields for Curve calculation
        assert hop.invariant.name == "CURVE_STABLESWAP"
        assert hop.swap_fn is not None
        assert hop.curve_a == 1000
        assert hop.curve_n_coins == 2
        assert hop.token_index_in == 0
        assert hop.token_index_out == 1

        # Test swap_fn gives reasonable output
        result = hop.swap_fn(1000 * 10**18)
        # Production Curve math: 1000 DAI -> ~999.6 USDC (after 0.04% fee)
        assert 998 * 10**6 <= result <= 1000 * 10**6


class TestCurveSimulationFunctions:
    """Test solver simulation functions with Curve hops."""

    @pytest.fixture
    def curve_pool(self):
        return _make_curve_pool()

    def test_simulate_path_with_curve(self, curve_pool):
        """_simulate_path should use swap_fn for Curve hops."""
        hop = curve_pool.to_hop_state(zero_for_one=True)

        amount = 1000 * 10**18
        result = _simulate_path(amount, (hop,))

        expected = float(hop.swap_fn(int(amount)))
        assert result == pytest.approx(expected, rel=1e-9)

    def test_simulate_mixed_path_with_curve(self, curve_pool):
        """_simulate_mixed_path should handle Curve hops."""
        hop = curve_pool.to_hop_state(zero_for_one=True)

        amount = 1000 * 10**18
        result = _simulate_mixed_path(amount, (hop,))

        expected = float(hop.swap_fn(int(amount)))
        assert result == pytest.approx(expected, rel=1e-9)

    def test_simulate_mixed_path_int_with_curve(self, curve_pool):
        """_simulate_mixed_path_int should handle Curve hops with integer precision."""
        hop = curve_pool.to_hop_state(zero_for_one=True)

        amount = 1000 * 10**18
        result = _simulate_mixed_path_int(amount, (hop,))

        expected = hop.swap_fn(amount)
        assert result == expected


class TestCurveVsConstantProductBehavior:
    """Compare Curve stableswap behavior vs constant-product."""

    def test_curve_gives_better_rates_than_constant_product(self, dai, usdc):
        """Curve's stableswap should give better rates than constant-product for same reserves."""
        # Setup: 1M DAI / 1M USDC in both pools
        initial_dai = 1_000_000 * 10**18
        initial_usdc = 1_000_000 * 10**6

        # Curve pool
        curve_pool = _make_curve_pool(
            balances=(initial_dai, initial_usdc),
            a_coefficient=2000,
            fee=4_000_000,
        )

        # V2 constant-product pool with same reserves
        fee = Fraction(4, 10000)
        v2_pool = UniswapV2Pool(
            address="0x00000000000000000000000000000000000000e3",  # type: ignore[arg-type]
            token0=dai,  # type: ignore[arg-type]
            token1=usdc,  # type: ignore[arg-type]
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=fee,
            fee_token1=fee,
            reserves_token0=initial_dai,
            reserves_token1=initial_usdc,
            state_block=1,
        )

        # Swap 100k DAI through both
        amount_in = 100_000 * 10**18

        # Curve output
        hop = curve_pool.to_hop_state(zero_for_one=True)
        curve_out = hop.swap_fn(amount_in)

        # V2 output
        v2_sim = v2_pool.simulate_swap(dai.address, amount_in, usdc.address)
        v2_out = v2_sim.amount_out

        # Curve should give significantly better rate
        print(f"Curve output: {curve_out / 10**6} USDC")
        print(f"V2 output: {v2_out / 10**6} USDC")

        assert curve_out > v2_out * 1.05  # At least 5% better
        assert curve_out > 99_000 * 10**6

    def test_curve_price_stability_with_imbalanced_pools(self, dai, usdc):
        """Curve maintains stable prices even with imbalanced reserves."""
        curve_pool = _make_curve_pool(
            balances=(2_000_000 * 10**18, 1_000_000 * 10**6),
        )

        hop = curve_pool.to_hop_state(zero_for_one=True)

        # Swap 1000 DAI (small relative to pool)
        small_swap = 1_000 * 10**18
        out_small = hop.swap_fn(small_swap)

        # Should get very close to 1000 USDC despite 2:1 imbalance
        assert 990 * 10**6 < out_small < 1010 * 10**6


class TestCurveSwapConsistency:
    """Verify swap_fn and simulate_swap agree on production CurveStableswapPool."""

    def test_swap_fn_matches_simulate_swap(self, dai, usdc):
        """swap_fn output should match pool.simulate_swap()."""
        pool = _make_curve_pool()

        hop = pool.to_hop_state(zero_for_one=True)
        swap_fn_result = hop.swap_fn(1000 * 10**18)

        sim = pool.simulate_swap(
            token_in=pool.tokens[0].address,
            amount_in=1000 * 10**18,
            token_out=pool.tokens[1].address,
        )

        assert swap_fn_result == sim.amount_out

    def test_swap_fn_matches_get_dy(self, dai, usdc):
        """swap_fn output should match pool.get_dy()."""
        pool = _make_curve_pool()

        hop = pool.to_hop_state(zero_for_one=True)
        swap_fn_result = hop.swap_fn(1000 * 10**18)

        get_dy_result = pool.get_dy(0, 1, 1000 * 10**18, block_identifier=STATE_BLOCK)

        assert swap_fn_result == get_dy_result
