"""Tests for Curve pool integration with solver simulation functions.

Verifies that production CurveStableswapPool works correctly with:
- _simulate_path (float-precision path simulation)
- _simulate_mixed_path (float-precision mixed-invariant path simulation)
- _simulate_mixed_path_int (integer-precision mixed-invariant path simulation)

Uses FakeCurveDataProvider for I/O-free pool construction.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers._solver_utils import _simulate_path
from degenbot.arbitrage.optimizers.solidly_stable import (
    _simulate_mixed_path,
    _simulate_mixed_path_int,
)
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.types import CurveStableswapPoolState
from degenbot.degenbot_rs import PyBot
from degenbot.types.hop_types import PoolInvariant
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20

_PY_BOT = PyBot()

# --- Token fixtures ---

DAI = make_erc20(
    _PY_BOT,
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",
    name="DAI",
    symbol="DAI",
    decimals=18,
)
USDC = make_erc20(
    _PY_BOT,
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    name="USD Coin",
    symbol="USDC",
    decimals=6,
)

# --- Pool construction helpers ---


def _make_curve_pool(
    balances: tuple[int, ...] = (10_000_000 * 10**18, 10_000_000 * 10**6),
    a_coefficient: int = 1000,
    fee: int = 4_000_000,
    state_block: int = 18_000_000,
) -> CurveStableswapPool:
    """Build a production CurveStableswapPool (DAI/USDC) with FakeCurveDataProvider."""
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000)
    return make_curve_pool(
        address="0x00000000000000000000000000000000000000c0",  # type: ignore[arg-type]
        tokens=(DAI, USDC),
        a_coefficient=a_coefficient,
        fee=fee,
        admin_fee=5_000_000_000,
        balances=balances,
        state_block=state_block,
        data_provider=provider,
    )


class TestSimulationFunctions:
    """Test integration with solver simulation functions."""

    @pytest.fixture
    def curve_pool(self):
        """Standard 2-coin pool for simulation tests."""
        return _make_curve_pool()

    def test_simulate_path_with_curve(self, curve_pool):
        """_simulate_path should use swap_fn for Curve hops."""
        hop = curve_pool.to_hop_state(zero_for_one=True)

        amount = 1000 * 10**18
        result = _simulate_path(amount, (hop,))

        # Result should match direct swap_fn call
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


class TestCurveHopGeneration:
    """Test that production CurveStableswapPool.to_hop_state produces valid hops."""

    def test_hop_has_correct_invariant(self):
        """Hop should have CURVE_STABLESWAP invariant."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=True)
        assert hop.invariant == PoolInvariant.CURVE_STABLESWAP

    def test_hop_has_swap_fn(self):
        """Hop should include callable swap_fn."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=True)
        assert hop.swap_fn is not None
        result = hop.swap_fn(1000 * 10**18)
        assert isinstance(result, int)
        assert result > 0

    def test_hop_fields_populated(self):
        """All required Curve fields should be set."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=True)

        assert hop.curve_a == 1000
        assert hop.curve_n_coins == 2
        assert hop.fee == Fraction(4_000_000, 10**10)
        assert hop.token_index_in == 0
        assert hop.token_index_out == 1

    def test_hop_reserve_fields(self):
        """Reserve fields should match pool balances."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=True)

        assert hop.reserve_in == pool.balances[0]
        assert hop.reserve_out == pool.balances[1]

    def test_zero_for_one_false(self):
        """zero_for_one=False should reverse indices."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=False)
        assert hop.token_index_in == 1
        assert hop.token_index_out == 0


class TestCurveSwapOutput:
    """Test that production Curve swap output matches expected behavior."""

    def test_small_swap_on_balanced_pool(self):
        """Small swap on balanced pool should give close to 1:1 minus fees."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=True)

        # Swap 1000 DAI -> USDC
        amount_in = 1000 * 10**18
        amount_out = hop.swap_fn(amount_in)

        # With A=1000 and balanced pool, should get very close to 1000 USDC minus fees
        # Fee is 0.04%, so expect ~999.6 USDC
        expected_min = 998 * 10**6
        expected_max = 1000 * 10**6
        assert expected_min <= amount_out <= expected_max

    def test_curve_gives_better_rates_than_constant_product(self):
        """Curve's stableswap should give better rates than constant-product for same reserves.

        This verifies the Curve math is working correctly — the whole point of
        Curve is to provide lower slippage for stable pairs.
        """
        pool = _make_curve_pool(
            balances=(1_000_000 * 10**18, 1_000_000 * 10**6),
            a_coefficient=2000,
            fee=4_000_000,
        )

        hop = pool.to_hop_state(zero_for_one=True)

        # Swap 100k DAI (10% of pool) — constant product gives ~90.9k
        amount_in = 100_000 * 10**18
        curve_out = hop.swap_fn(amount_in)

        # Curve with A=2000 should give much closer to 100k
        assert curve_out > 99_000 * 10**6

    def test_swap_fn_matches_pool_get_dy(self):
        """swap_fn in hop should match direct pool.get_dy() call."""
        pool = _make_curve_pool()
        hop = pool.to_hop_state(zero_for_one=True)

        amount = 1000 * 10**18
        via_swap_fn = hop.swap_fn(amount)
        via_get_dy = pool.get_dy(0, 1, amount, block_identifier=18_000_000)

        assert via_swap_fn == via_get_dy


class TestCurvePairSelection:
    """Test token_in/token_out pair selection in to_hop_state."""

    def test_explicit_pair_selection_with_fake_token(self):
        """to_hop_state with token_in/token_out selects the specified pair.

        Note: FakeToken inherits from AddressComparable, matching Erc20Token's base.
        The production pool stores Erc20Token objects, but FakeToken with the same
        address satisfies the equality check.
        """
        pool = _make_curve_pool()

        # Production pool's tokens are Erc20Token, but we can use
        # the pool.tokens property directly
        hop = pool.to_hop_state(
            zero_for_one=True,
            token_in=pool.tokens[0],
            token_out=pool.tokens[1],
        )
        assert hop.token_index_in == 0
        assert hop.token_index_out == 1


class TestCurveStateOverride:
    """Test state_override parameter in to_hop_state."""

    def test_state_override_in_to_hop_state(self):
        """Should use overridden state when provided."""
        pool = _make_curve_pool()

        # Create override with different balances
        override = CurveStableswapPoolState(
            address=pool.address,
            balances=(5 * 10**21, 5 * 10**6 * 10**6),  # 5x the original DAI balance
            block=18_000_000,
        )

        hop = pool.to_hop_state(zero_for_one=True, state_override=override)

        # Hop should use override balances
        assert hop.reserve_in == 5 * 10**21
        assert hop.reserve_out == 5 * 10**6 * 10**6
