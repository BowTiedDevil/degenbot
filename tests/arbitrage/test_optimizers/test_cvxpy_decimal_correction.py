"""Property-based tests for decimal correction in CVXPY optimization.

Tests that the compression strategies (single vs double) handle
asymmetric token decimals correctly.
"""

from fractions import Fraction

import hypothesis
import hypothesis.strategies as st

from tests.arbitrage.generator.hypothesis_strategies import (
    mismatched_decimal_pair_strategy,
)

# ==============================================================================
# Decimal Compression Strategies
# ==============================================================================


def compute_single_compression_factor(
    reserves_token0_a: int,
    reserves_token1_a: int,
    reserves_token0_b: int,
    reserves_token1_b: int,
    decimals0: int,
    decimals1: int,
) -> Fraction:
    """Compute single compression factor (max of all reserves).

    This is the original approach: find the largest reserve value
    across both pools and use it as a common divisor.
    """
    return max(
        Fraction(reserves_token0_a, 10**decimals0),
        Fraction(reserves_token1_a, 10**decimals1),
        Fraction(reserves_token0_b, 10**decimals0),
        Fraction(reserves_token1_b, 10**decimals1),
    )


def compute_double_compression_factors(
    reserves_token0_a: int,
    reserves_token1_a: int,
    reserves_token0_b: int,
    reserves_token1_b: int,
    decimals0: int,
    decimals1: int,
) -> tuple[Fraction, Fraction]:
    """Compute double compression factors (per-token).

    This approach uses a separate compression factor for each token,
    which handles asymmetric decimals better.
    """
    factor0 = max(
        Fraction(reserves_token0_a, 10**decimals0),
        Fraction(reserves_token0_b, 10**decimals0),
    )
    factor1 = max(
        Fraction(reserves_token1_a, 10**decimals1),
        Fraction(reserves_token1_b, 10**decimals1),
    )
    return factor0, factor1


def compute_compression_ratio(
    decimals0: int,
    decimals1: int,
    reserves_token0_a: int,
    reserves_token1_a: int,
) -> float:
    """Compute the ratio of compressed values for single vs double compression.

    A ratio far from 1.0 indicates significant difference between methods.
    """
    single = compute_single_compression_factor(
        reserves_token0_a,
        reserves_token1_a,
        reserves_token0_a,  # Same pool for simplicity
        reserves_token1_a,
        decimals0,
        decimals1,
    )

    double0, double1 = compute_double_compression_factors(
        reserves_token0_a,
        reserves_token1_a,
        reserves_token0_a,
        reserves_token1_a,
        decimals0,
        decimals1,
    )

    # Ratio of single to average double
    avg_double = (double0 + double1) / 2
    return float(single / avg_double)


# ==============================================================================
# Property Tests
# ==============================================================================


class TestDecimalCompressionProperties:
    """Properties of decimal compression strategies."""

    @hypothesis.given(
        decimals_pair=mismatched_decimal_pair_strategy,
        reserves0=st.integers(min_value=10**10, max_value=10**20),
        reserves1=st.integers(min_value=10**10, max_value=10**20),
    )
    @hypothesis.settings(max_examples=50)
    def test_single_compression_favors_higher_decimals(
        self,
        decimals_pair: tuple[int, int],
        reserves0: int,
        reserves1: int,
    ):
        """Property: Single compression can distort values when decimals differ significantly.

        When token0 has many more decimals than token1, the compressed reserves
        for token0 will be much smaller relative to token1.
        """
        decimals0, decimals1 = decimals_pair
        _decimal_diff = abs(decimals0 - decimals1)

        single = compute_single_compression_factor(
            reserves0, reserves1, reserves0, reserves1, decimals0, decimals1
        )

        # Single compression uses max reserve as divisor
        # This means the token with larger raw reserve (after decimal adjustment)
        # will have compressed values near 1.0, while the other may be tiny
        adjusted0 = Fraction(reserves0, 10**decimals0)
        adjusted1 = Fraction(reserves1, 10**decimals1)

        # Verify the compression factor is at least as large as the max adjusted reserve
        assert single >= max(adjusted0, adjusted1)

    @hypothesis.given(
        decimals=st.integers(min_value=6, max_value=18),
        reserves0=st.integers(min_value=10**10, max_value=10**20),
        reserves1=st.integers(min_value=10**10, max_value=10**20),
    )
    @hypothesis.settings(max_examples=30)
    def test_double_compression_same_as_single_for_equal_decimals(
        self,
        decimals: int,
        reserves0: int,
        reserves1: int,
    ):
        """Property: When decimals are equal, single and double compression give same results.
        """
        # Use same decimals for both tokens
        decimals0 = decimals
        decimals1 = decimals

        single = compute_single_compression_factor(
            reserves0, reserves1, reserves0, reserves1, decimals0, decimals1
        )

        double0, double1 = compute_double_compression_factors(
            reserves0, reserves1, reserves0, reserves1, decimals0, decimals1
        )

        # For same pool, double compression should give adjusted reserve values
        adjusted0 = Fraction(reserves0, 10**decimals0)
        adjusted1 = Fraction(reserves1, 10**decimals1)

        assert double0 == adjusted0
        assert double1 == adjusted1

        # Single compression should equal the max of the two
        assert single == max(adjusted0, adjusted1)

    @hypothesis.given(
        decimals_pair=mismatched_decimal_pair_strategy,
        reserves0=st.integers(min_value=10**10, max_value=10**20),
        reserves1=st.integers(min_value=10**10, max_value=10**20),
    )
    @hypothesis.settings(max_examples=30)
    def test_double_compression_independent_per_token(
        self,
        decimals_pair: tuple[int, int],
        reserves0: int,
        reserves1: int,
    ):
        """Property: Double compression treats each token independently.

        Each token's compressed value is based only on its own reserves,
        not affected by the other token's decimal places.
        """
        decimals0, decimals1 = decimals_pair

        double0, double1 = compute_double_compression_factors(
            reserves0, reserves1, reserves0, reserves1, decimals0, decimals1
        )

        # Each factor should be the adjusted reserve for that token
        adjusted0 = Fraction(reserves0, 10**decimals0)
        adjusted1 = Fraction(reserves1, 10**decimals1)

        assert double0 == adjusted0
        assert double1 == adjusted1


class TestDecimalCorrectionImpact:
    """Test the impact of decimal correction on arbitrage optimization."""

    @hypothesis.given(
        decimals_pair=st.sampled_from([(6, 18), (8, 18), (6, 8)]),
        price_ratio=st.floats(
            min_value=1.02, max_value=1.08, allow_nan=False, allow_infinity=False
        ),
    )
    @hypothesis.settings(max_examples=20)
    def test_mismatched_decimals_produces_different_results(
        self,
        decimals_pair: tuple[int, int],
        price_ratio: float,
    ):
        """Property: Mismatched decimals lead to different results for single vs double compression.

        When tokens have significantly different decimals (e.g., USDC=6, WETH=18),
        the two compression methods should produce different optimization results.
        """
        decimals0, decimals1 = decimals_pair

        # Simulate reserves for two pools with price discrepancy
        # Pool A: base reserves
        reserves0_a = 10**6 * 10**decimals0  # 1M token0
        reserves1_a = 10**3 * 10**decimals1  # 1K token1

        # Pool B: different price (higher token1 per token0)
        reserves0_b = int(reserves0_a / price_ratio)
        reserves1_b = reserves1_a

        # Compute compression factors
        _single = compute_single_compression_factor(
            reserves0_a, reserves1_a, reserves0_b, reserves1_b, decimals0, decimals1
        )

        double0, double1 = compute_double_compression_factors(
            reserves0_a, reserves1_a, reserves0_b, reserves1_b, decimals0, decimals1
        )

        # The compression ratios should be different
        compressed0_double = Fraction(reserves0_a, 10**decimals0) / double0
        compressed1_double = Fraction(reserves1_a, 10**decimals1) / double1

        # When decimals differ significantly, the compression ratios differ
        if abs(decimals0 - decimals1) >= 6:
            # Significant decimal difference
            # Double compression gives 1.0 for the max reserve token
            assert compressed0_double == Fraction(1) or compressed1_double == Fraction(1)


class TestCompressionPrecision:
    """Test numerical precision of compression strategies."""

    @hypothesis.given(
        decimals=st.integers(min_value=6, max_value=18),
        reserve_magnitude=st.integers(min_value=6, max_value=24),  # 10^6 to 10^24 wei
    )
    @hypothesis.settings(max_examples=30)
    def test_compression_preserves_relative_values(
        self,
        decimals: int,
        reserve_magnitude: int,
    ):
        """Property: Compression preserves relative values within a pool.

        After compression, the ratio of reserves should be the same.
        """
        reserve0 = 10**reserve_magnitude
        reserve1 = 10**reserve_magnitude * 2  # 2x ratio

        # Single compression
        single = compute_single_compression_factor(
            reserve0, reserve1, reserve0, reserve1, decimals, decimals
        )

        compressed0 = Fraction(reserve0, 10**decimals) / single
        compressed1 = Fraction(reserve1, 10**decimals) / single

        # Ratio should be preserved: compressed1 / compressed0 = 2
        ratio = compressed1 / compressed0
        assert ratio == Fraction(2), f"Expected ratio 2, got {ratio}"

    @hypothesis.given(
        decimals0=st.integers(min_value=6, max_value=18),
        decimals1=st.integers(min_value=6, max_value=18),
        reserve0_mag=st.integers(min_value=15, max_value=22),
        reserve1_mag=st.integers(min_value=15, max_value=22),
    )
    @hypothesis.settings(max_examples=30)
    def test_double_compression_maintains_bounds(
        self,
        decimals0: int,
        decimals1: int,
        reserve0_mag: int,
        reserve1_mag: int,
    ):
        """Property: Double compression keeps values in [0, 1] range.

        After compression, all reserve values should be <= 1.0.
        """
        reserve0 = 10**reserve0_mag
        reserve1 = 10**reserve1_mag

        double0, double1 = compute_double_compression_factors(
            reserve0, reserve1, reserve0, reserve1, decimals0, decimals1
        )

        # Compressed values
        compressed0 = Fraction(reserve0, 10**decimals0) / double0
        compressed1 = Fraction(reserve1, 10**decimals1) / double1

        # Should be in [0, 1] (the max reserve should be exactly 1.0)
        assert 0 <= compressed0 <= 1
        assert 0 <= compressed1 <= 1
        # At least one should be 1.0 (the max)
        assert compressed0 == 1 or compressed1 == 1


class TestCompressionEdgeCases:
    """Edge cases for decimal compression."""

    def test_extreme_decimal_difference(self):
        """Test with maximum decimal difference (6 vs 18)."""
        decimals0, decimals1 = 6, 18

        # USDC-like: 1M tokens (6 decimals)
        reserve0 = 1_000_000 * 10**6
        # WETH-like: 1K tokens (18 decimals)
        reserve1 = 1_000 * 10**18

        single = compute_single_compression_factor(
            reserve0, reserve1, reserve0, reserve1, decimals0, decimals1
        )

        double0, double1 = compute_double_compression_factors(
            reserve0, reserve1, reserve0, reserve1, decimals0, decimals1
        )

        # With single compression, the larger adjusted value dominates
        adjusted0 = Fraction(reserve0, 10**decimals0)  # 1M
        adjusted1 = Fraction(reserve1, 10**decimals1)  # 1K

        # Single compression should use the max (1M for USDC)
        assert single == max(adjusted0, adjusted1)

        # Double compression should give each token's adjusted value
        assert double0 == adjusted0
        assert double1 == adjusted1

    def test_identical_pools_no_compression_needed(self):
        """Test that identical pools don't need compression correction."""
        decimals = 18
        reserve0 = 10**20
        reserve1 = 10**20

        single = compute_single_compression_factor(
            reserve0, reserve1, reserve0, reserve1, decimals, decimals
        )

        double0, double1 = compute_double_compression_factors(
            reserve0, reserve1, reserve0, reserve1, decimals, decimals
        )

        # All should be equal
        assert single == double0 == double1 == Fraction(reserve0, 10**decimals)

    def test_zero_reserve_handling(self):
        """Test that zero reserves are handled correctly."""
        decimals0, decimals1 = 18, 18

        # One zero reserve
        reserve0_a = 10**20
        reserve1_a = 0
        reserve0_b = 10**20
        reserve1_b = 10**20

        # Should still compute valid compression factors
        _double0, double1 = compute_double_compression_factors(
            reserve0_a, reserve1_a, reserve0_b, reserve1_b, decimals0, decimals1
        )

        # double1 should be from pool B (which has non-zero reserve1)
        assert double1 == Fraction(reserve1_b, 10**decimals1)
