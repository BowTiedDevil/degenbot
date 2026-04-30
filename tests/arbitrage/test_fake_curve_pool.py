"""Tests for FakeCurveStableswapPool synthetic state generation."""

import pytest
from fractions import Fraction

from tests.arbitrage.fake_curve_pool import FakeCurveStableswapPool, FakeCurveToken, FakeCurvePoolState
from degenbot.types.hop_types import CurveStableswapHop, PoolInvariant
from degenbot.arbitrage.optimizers.solver import _simulate_path, _simulate_mixed_path, _simulate_mixed_path_int


class TestFakeCurvePoolConstruction:
    """Test pool initialization and basic properties."""

    def test_two_coin_pool_construction(self):
        """Create a standard 2-coin Curve pool (e.g., USDC/USDT)."""
        token0 = FakeCurveToken("0xUSDC", 6, "USDC")
        token1 = FakeCurveToken("0xUSDT", 6, "USDT")
        
        pool = FakeCurveStableswapPool(
            tokens=(token0, token1),
            balances=(10_000_000 * 10**6, 10_000_000 * 10**6),
            a_coefficient=1000,
            fee=4_000_000,  # 0.04%
        )
        
        assert pool.n_coins == 2
        assert pool.a_coefficient == 1000
        assert pool.fee == 4_000_000
        assert len(pool.precision_multipliers) == 2
        # USDC and USDT both have 6 decimals, so multipliers are 10^12
        assert pool.precision_multipliers == (10**12, 10**12)

    def test_three_coin_pool_construction(self):
        """Create a 3-coin Curve pool (e.g., TriPool: DAI/USDC/USDT)."""
        dai = FakeCurveToken("0xDAI", 18, "DAI")
        usdc = FakeCurveToken("0xUSDC", 6, "USDC")
        usdt = FakeCurveToken("0xUSDT", 6, "USDT")
        
        pool = FakeCurveStableswapPool(
            tokens=(dai, usdc, usdt),
            balances=(
                5_000_000 * 10**18,  # DAI
                5_000_000 * 10**6,   # USDC
                5_000_000 * 10**6,   # USDT
            ),
            a_coefficient=2000,
            fee=3_000_000,  # 0.03%
        )
        
        assert pool.n_coins == 3
        # DAI has 18 decimals -> multiplier 1
        # USDC/USDT have 6 decimals -> multiplier 10^12
        assert pool.precision_multipliers == (1, 10**12, 10**12)

    def test_invalid_token_balance_mismatch(self):
        """Should raise if token count doesn't match balance count."""
        token0 = FakeCurveToken("0xA", 18)
        token1 = FakeCurveToken("0xB", 18)
        
        with pytest.raises(ValueError, match="Token count .* must match balance count"):
            FakeCurveStableswapPool(
                tokens=(token0, token1),
                balances=(10**18,),  # Only one balance for two tokens
            )

    def test_invalid_token_count(self):
        """Should raise if fewer than 2 or more than 8 tokens."""
        with pytest.raises(ValueError, match="Curve pools require 2-8 tokens"):
            FakeCurveStableswapPool(
                tokens=(FakeCurveToken("0xA", 18),),  # Only 1 token
                balances=(10**18,),
            )


class TestCurveMath:
    """Test Curve stableswap invariant calculations."""

    @pytest.fixture
    def balanced_two_coin_pool(self):
        """A balanced 2-coin pool with 1:1 peg."""
        return FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xDAI", 18, "DAI"),
                FakeCurveToken("0xUSDC", 6, "USDC"),
            ),
            balances=(1_000_000 * 10**18, 1_000_000 * 10**6),
            a_coefficient=1000,
            fee=4_000_000,
        )

    def test_d_calculation_balanced_pool(self, balanced_two_coin_pool):
        """D should equal sum of precision-adjusted balances when balanced."""
        pool = balanced_two_coin_pool
        xp = pool._xp(pool.balances)
        d = pool._get_d(xp, pool.a_coefficient)
        
        # For a balanced pool, D ≈ sum(xp)
        assert d == pytest.approx(sum(xp), rel=0.01)

    def test_swap_on_balanced_pool(self, balanced_two_coin_pool):
        """Small swap on balanced pool should give close to 1:1 minus fees."""
        pool = balanced_two_coin_pool
        
        # Swap 1000 DAI -> USDC
        amount_in = 1000 * 10**18
        amount_out = pool._get_dy(0, 1, amount_in)
        
        # Should get approximately 1000 USDC minus fees
        # Fee is 0.04%, so expect ~999.6 USDC
        expected_min = 998 * 10**6  # ~998 USDC (allowing for rounding)
        expected_max = 1000 * 10**6
        
        assert expected_min <= amount_out <= expected_max

    def test_large_swap_price_impact(self, balanced_two_coin_pool):
        """Large swap should show Curve's price stability vs constant-product."""
        pool = balanced_two_coin_pool
        
        # Swap 10% of pool (100k DAI)
        amount_in = 100_000 * 10**18
        amount_out = pool._get_dy(0, 1, amount_in)
        
        # With A=1000, Curve should give much better rate than constant-product
        # Constant product would give: 100k * 1M / (1M + 100k) = ~90.9k
        # Curve should give much closer to 100k
        
        # Should receive at least 99k USDC (vs ~90.9k for constant-product)
        assert amount_out > 99_000 * 10**6

    def test_swap_round_trip(self, balanced_two_coin_pool):
        """Swap there and back should lose fees."""
        pool = balanced_two_coin_pool
        
        # Start with 1000 DAI
        amount_0 = 1000 * 10**18
        
        # DAI -> USDC
        amount_1 = pool._get_dy(0, 1, amount_0)
        
        # USDC -> DAI
        amount_2 = pool._get_dy(1, 0, amount_1)
        
        # Should have less DAI after round-trip (fees)
        assert amount_2 < amount_0
        # Should lose approximately 0.08% (two 0.04% fees)
        assert amount_2 > amount_0 * 0.99  # But not too much


class TestHopStateGeneration:
    """Test to_hop_state() creates valid CurveStableswapHop."""

    @pytest.fixture
    def two_coin_pool(self):
        return FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xA", 18, "TKA"),
                FakeCurveToken("0xB", 18, "TKB"),
            ),
            balances=(10**21, 10**21),
            a_coefficient=1000,
            fee=4_000_000,
        )

    def test_zero_for_one_true(self, two_coin_pool):
        """zero_for_one=True should map tokens[0] -> tokens[1]."""
        hop = two_coin_pool.to_hop_state(zero_for_one=True)
        
        assert isinstance(hop, CurveStableswapHop)
        assert hop.token_index_in == 0
        assert hop.token_index_out == 1
        assert hop.reserve_in == two_coin_pool.balances[0]
        assert hop.reserve_out == two_coin_pool.balances[1]

    def test_zero_for_one_false(self, two_coin_pool):
        """zero_for_one=False should map tokens[1] -> tokens[0]."""
        hop = two_coin_pool.to_hop_state(zero_for_one=False)
        
        assert hop.token_index_in == 1
        assert hop.token_index_out == 0
        assert hop.reserve_in == two_coin_pool.balances[1]
        assert hop.reserve_out == two_coin_pool.balances[0]

    def test_hop_has_swap_fn(self, two_coin_pool):
        """Hop should include callable swap_fn."""
        hop = two_coin_pool.to_hop_state(zero_for_one=True)
        
        assert hop.swap_fn is not None
        # Test the swap_fn works
        result = hop.swap_fn(1000 * 10**18)
        assert isinstance(result, int)
        assert result > 0

    def test_hop_fields_populated(self, two_coin_pool):
        """All required Curve fields should be set."""
        hop = two_coin_pool.to_hop_state(zero_for_one=True)
        
        assert hop.curve_a == 1000
        assert hop.curve_n_coins == 2
        assert hop.curve_d > 0  # D should be calculated
        assert len(hop.precisions) == 2
        assert hop.invariant == PoolInvariant.CURVE_STABLESWAP
        assert hop.fee == Fraction(4_000_000, 10**10)

    def test_swap_fn_matches_direct_calculation(self, two_coin_pool):
        """swap_fn in hop should match direct _get_dy call."""
        hop = two_coin_pool.to_hop_state(zero_for_one=True)
        
        amount = 10_000 * 10**18
        via_swap_fn = hop.swap_fn(amount)
        via_direct = two_coin_pool._get_dy(0, 1, amount)
        
        assert via_swap_fn == via_direct


class TestSimulationFunctions:
    """Test integration with solver simulation functions."""

    @pytest.fixture
    def curve_pool(self):
        """Standard 2-coin pool for simulation tests."""
        return FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xDAI", 18, "DAI"),
                FakeCurveToken("0xUSDC", 6, "USDC"),
            ),
            balances=(10_000_000 * 10**18, 10_000_000 * 10**6),
            a_coefficient=1000,
            fee=4_000_000,
        )

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


class TestSimulationResult:
    """Test simulate_swap() returns correct SimulationResult."""

    def test_simulate_swap_by_address(self):
        """Find tokens by address and calculate swap."""
        token0 = FakeCurveToken("0x1111111111111111111111111111111111111111", 18, "TK0")
        token1 = FakeCurveToken("0x2222222222222222222222222222222222222222", 18, "TK1")
        
        pool = FakeCurveStableswapPool(
            tokens=(token0, token1),
            balances=(10**21, 10**21),
            a_coefficient=1000,
            fee=4_000_000,
        )
        
        result = pool.simulate_swap(
            token_in=token0.address,
            amount_in=1000 * 10**18,
            token_out=token1.address,
        )
        
        assert result.amount_in == 1000 * 10**18
        assert result.amount_out > 0
        assert result.amount_out < result.amount_in  # Fees apply
        assert isinstance(result.initial_state, FakeCurvePoolState)
        assert isinstance(result.final_state, FakeCurvePoolState)

    def test_simulate_swap_invalid_token(self):
        """Should raise for tokens not in pool."""
        pool = FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xA", 18),
                FakeCurveToken("0xB", 18),
            ),
            balances=(10**18, 10**18),
        )
        
        with pytest.raises(ValueError, match="Token not found"):
            pool.simulate_swap(
                token_in="0x9999999999999999999999999999999999999999",
                amount_in=1000,
                token_out="0xB",
            )


class TestImbalancedPools:
    """Test behavior with imbalanced pool states."""

    def test_imbalanced_pool_calculation(self):
        """Pool with 2:1 imbalance should still calculate correctly."""
        pool = FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xA", 18, "A"),
                FakeCurveToken("0xB", 18, "B"),
            ),
            balances=(2_000_000 * 10**18, 1_000_000 * 10**18),  # 2:1
            a_coefficient=1000,
            fee=4_000_000,
        )
        
        # D should be closer to 2x the lower balance (Curve's stableswap pulls toward balance)
        xp = pool._xp(pool.balances)
        d = pool._get_d(xp, pool.a_coefficient)
        
        # D is calculated from xp (precision-adjusted balances)
        # For 18-decimal tokens: xp = balance / 10^18 (since precision = 1)
        # So xp values are ~2_000_000 and 1_000_000 (much smaller than raw balances)
        # D is also in these same units
        assert 1_000_000 < d < 4_000_000  # D should be between min(xp) and sum(xp)
        
        # Test a swap works in the imbalanced pool
        # (Curve's price stability means we lose less than constant-product would)
        amount_out = pool._get_dy(0, 1, 1000 * 10**18)
        # Should get close to 1000 B minus fees (~998 with 0.04% fee)
        assert 900 * 10**18 < amount_out < 1000 * 10**18


class TestMetapoolSupport:
    """Test metapool composition (simplified)."""

    def test_metapool_construction(self):
        """Create a metapool with base pool."""
        # Base pool (e.g., 3Crv: USDC/USDT/DAI)
        base_tokens = (
            FakeCurveToken("0xUSDC", 6, "USDC"),
            FakeCurveToken("0xUSDT", 6, "USDT"),
            FakeCurveToken("0xDAI", 18, "DAI"),
        )
        base_pool = FakeCurveStableswapPool(
            tokens=base_tokens,
            balances=(3_000_000 * 10**6, 3_000_000 * 10**6, 3_000_000 * 10**18),
            a_coefficient=2000,
            fee=3_000_000,
            address="0xbase_pool",
        )
        
        # Metapool (e.g., FRAX/3Crv where 3Crv is token1)
        metapool = FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xFRAX", 18, "FRAX"),
                FakeCurveToken("0x3CRV", 18, "3CRV"),  # LP token
            ),
            balances=(5_000_000 * 10**18, 5_000_000 * 10**18),
            a_coefficient=1000,
            fee=4_000_000,
            base_pool=base_pool,
        )
        
        assert metapool.base_pool is base_pool
        assert metapool.base_pool.address == "0xbase_pool"


class TestStateOverride:
    """Test state_override parameter in to_hop_state and simulate_swap."""

    def test_state_override_in_to_hop_state(self):
        """Should use overridden state when provided."""
        pool = FakeCurveStableswapPool(
            tokens=(
                FakeCurveToken("0xA", 18, "A"),
                FakeCurveToken("0xB", 18, "B"),
            ),
            balances=(10**21, 10**21),
        )
        
        # Create override with different balances
        override = FakeCurvePoolState(
            address=pool.address,
            block=None,
            balances=(5 * 10**21, 5 * 10**21),  # 5x the original
        )
        
        hop = pool.to_hop_state(zero_for_one=True, state_override=override)
        
        # Hop should use override balances
        assert hop.reserve_in == 5 * 10**21
        assert hop.reserve_out == 5 * 10**21
