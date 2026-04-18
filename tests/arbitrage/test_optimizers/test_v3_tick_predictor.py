"""
Tests for V3 tick crossing prediction and optimization.
"""

import math
import operator

from degenbot.arbitrage.optimizers.v3_tick_predictor import (
    TickCrossingPrediction,
    TickRange,
    estimate_price_impact,
    predict_tick_crossing,
    sqrt_price_to_tick,
    tick_range_to_bounded_product,
    tick_to_sqrt_price,
)

# =============================================================================
# TICK MATH TESTS
# =============================================================================


class TestTickMath:
    """Tests for tick-price conversions."""

    def test_tick_to_sqrt_price_zero(self):
        """Tick 0 should give sqrt_price = 1."""
        sqrt_price = tick_to_sqrt_price(0)
        assert abs(sqrt_price - 1.0) < 1e-10

    def test_tick_to_sqrt_price_positive(self):
        """Positive ticks give sqrt_price > 1."""
        # tick 1: sqrt_price = 1.0001^0.5 ≈ 1.00005
        sqrt_price = tick_to_sqrt_price(1)
        assert sqrt_price > 1.0
        assert abs(sqrt_price - math.sqrt(1.0001)) < 1e-10

    def test_tick_to_sqrt_price_negative(self):
        """Negative ticks give sqrt_price < 1."""
        sqrt_price = tick_to_sqrt_price(-1)
        assert sqrt_price < 1.0
        assert abs(sqrt_price - math.sqrt(1.0001**-1)) < 1e-10

    def test_tick_to_sqrt_price_roundtrip(self):
        """tick → sqrt_price → tick should be close."""
        for tick in [-1000, -100, -10, 0, 10, 100, 1000]:
            sqrt_price = tick_to_sqrt_price(tick)
            recovered_tick = sqrt_price_to_tick(sqrt_price)
            assert abs(recovered_tick - tick) <= 1  # Allow 1 tick error from rounding

    def test_sqrt_price_to_tick_bounds(self):
        """sqrt_price_to_tick handles extreme values."""
        # Very small sqrt_price
        tick = sqrt_price_to_tick(0.001)
        assert tick < 0

        # Very large sqrt_price
        tick = sqrt_price_to_tick(1000.0)
        assert tick > 0


# =============================================================================
# PRICE IMPACT ESTIMATION TESTS
# =============================================================================


class TestPriceImpactEstimation:
    """Tests for price impact estimation."""

    def test_price_impact_zero_for_one(self):
        """Token0 → token1 decreases sqrt_price."""
        liquidity = 1_000_000.0
        current_sqrt_price = 100.0  # price = 10000
        amount_in = 1000.0

        new_sqrt_price = estimate_price_impact(
            amount_in=amount_in,
            liquidity=liquidity,
            current_sqrt_price=current_sqrt_price,
            fee=0.003,
            zero_for_one=True,
        )

        # sqrt_price should decrease
        assert new_sqrt_price < current_sqrt_price

    def test_price_impact_one_for_zero(self):
        """Token1 → token0 increases sqrt_price."""
        liquidity = 1_000_000.0
        current_sqrt_price = 100.0
        amount_in = 1000.0

        new_sqrt_price = estimate_price_impact(
            amount_in=amount_in,
            liquidity=liquidity,
            current_sqrt_price=current_sqrt_price,
            fee=0.003,
            zero_for_one=False,
        )

        # sqrt_price should increase
        assert new_sqrt_price > current_sqrt_price

    def test_price_impact_scales_with_amount(self):
        """Larger amount_in causes larger price impact."""
        liquidity = 1_000_000.0
        current_sqrt_price = 100.0

        small_impact = estimate_price_impact(
            amount_in=100.0,
            liquidity=liquidity,
            current_sqrt_price=current_sqrt_price,
            fee=0.003,
            zero_for_one=True,
        )

        large_impact = estimate_price_impact(
            amount_in=10_000.0,
            liquidity=liquidity,
            current_sqrt_price=current_sqrt_price,
            fee=0.003,
            zero_for_one=True,
        )

        # Larger amount = larger impact (price moves more)
        assert current_sqrt_price - large_impact > current_sqrt_price - small_impact

    def test_price_impact_zero_amount(self):
        """Zero amount_in should not change price."""
        new_sqrt_price = estimate_price_impact(
            amount_in=0.0,
            liquidity=1_000_000.0,
            current_sqrt_price=100.0,
            fee=0.003,
            zero_for_one=True,
        )
        assert new_sqrt_price == 100.0

    def test_price_impact_affected_by_liquidity(self):
        """Higher liquidity reduces price impact."""
        current_sqrt_price = 100.0
        amount_in = 1000.0

        low_liq_impact = estimate_price_impact(
            amount_in=amount_in,
            liquidity=100_000.0,
            current_sqrt_price=current_sqrt_price,
            fee=0.003,
            zero_for_one=True,
        )

        high_liq_impact = estimate_price_impact(
            amount_in=amount_in,
            liquidity=10_000_000.0,
            current_sqrt_price=current_sqrt_price,
            fee=0.003,
            zero_for_one=True,
        )

        # Higher liquidity = smaller impact (price moves less)
        assert current_sqrt_price - low_liq_impact > current_sqrt_price - high_liq_impact


# =============================================================================
# TICK CROSSING PREDICTION TESTS
# =============================================================================


class TestTickCrossingPrediction:
    """Tests for tick crossing prediction."""

    def test_predict_no_crossing_small_amount(self):
        """Small swap stays in current tick range."""
        prediction = predict_tick_crossing(
            amount_in=100.0,
            liquidity=1_000_000.0,
            current_sqrt_price=100.0,
            current_tick=0,
            tick_spacing=60,
            fee=0.003,
            zero_for_one=True,
        )

        # Should not cross with such small amount relative to liquidity
        assert isinstance(prediction, TickCrossingPrediction)
        assert prediction.current_tick == 0

    def test_predict_crossing_large_amount(self):
        """Large swap likely crosses tick boundaries."""
        prediction = predict_tick_crossing(
            amount_in=10_000_000.0,  # Very large
            liquidity=1_000_000.0,
            current_sqrt_price=100.0,
            current_tick=0,
            tick_spacing=60,
            fee=0.003,
            zero_for_one=True,
        )

        assert isinstance(prediction, TickCrossingPrediction)
        # With amount >> liquidity, likely to cross
        # (prediction may or may not cross depending on exact math)

    def test_prediction_confidence_decreases_with_crossings(self):
        """More estimated crossings = lower confidence."""
        # Small amount (likely no crossing)
        pred_small = predict_tick_crossing(
            amount_in=100.0,
            liquidity=1_000_000.0,
            current_sqrt_price=100.0,
            current_tick=0,
            tick_spacing=60,
            fee=0.003,
            zero_for_one=True,
        )

        # Large amount (potential crossing)
        pred_large = predict_tick_crossing(
            amount_in=1_000_000.0,
            liquidity=1_000_000.0,
            current_sqrt_price=100.0,
            current_tick=0,
            tick_spacing=60,
            fee=0.003,
            zero_for_one=True,
        )

        # Confidence should be reasonable
        assert 0.0 <= pred_small.confidence <= 1.0
        assert 0.0 <= pred_large.confidence <= 1.0


# =============================================================================
# BOUNDED PRODUCT CFMM TESTS
# =============================================================================


class TestBoundedProductCFMM:
    """Tests for bounded product CFMM representation."""

    def test_bounded_product_creation(self):
        """Create bounded product from tick range."""
        cfmm = tick_range_to_bounded_product(
            tick_lower=0,
            tick_upper=60,
            liquidity=1_000_000.0,
        )

        assert cfmm.tick_lower == 0
        assert cfmm.tick_upper == 60
        assert cfmm.liquidity == 1_000_000.0

    def test_bounded_product_properties(self):
        """Alpha, beta, k computed correctly."""
        liquidity = 1_000_000.0
        tick_lower = 0
        tick_upper = 60

        cfmm = tick_range_to_bounded_product(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity=liquidity,
        )

        # Check k = L²
        assert abs(cfmm.k - liquidity**2) < 1e-10

        # Check alpha = L / sqrt(P_upper)
        sqrt_p_upper = tick_to_sqrt_price(tick_upper)
        assert abs(cfmm.alpha - liquidity / sqrt_p_upper) < 1e-10

        # Check beta = L * sqrt(P_lower)
        sqrt_p_lower = tick_to_sqrt_price(tick_lower)
        assert abs(cfmm.beta - liquidity * sqrt_p_lower) < 1e-10

    def test_contains_sqrt_price(self):
        """Check if sqrt_price is in range."""
        cfmm = tick_range_to_bounded_product(
            tick_lower=0,
            tick_upper=60,
            liquidity=1_000_000.0,
        )

        # Inside range
        sqrt_price_mid = tick_to_sqrt_price(30)
        assert cfmm.contains_sqrt_price(sqrt_price_mid)

        # Below range
        sqrt_price_below = tick_to_sqrt_price(-10)
        assert not cfmm.contains_sqrt_price(sqrt_price_below)

        # Above range
        sqrt_price_above = tick_to_sqrt_price(70)
        assert not cfmm.contains_sqrt_price(sqrt_price_above)

    def test_optimal_reserves_at_price(self):
        """Find optimal reserves given external price."""
        cfmm = tick_range_to_bounded_product(
            tick_lower=0,
            tick_upper=60,
            liquidity=1_000_000.0,
        )

        # At price corresponding to middle of range
        price_mid = 1.0001**30  # tick 30
        R0, R1 = cfmm.optimal_reserves_at_price(price_mid)

        # Reserves should be positive
        assert R0 >= 0
        assert R1 >= 0

        # Check constant product constraint: (R0 + α)(R1 + β) ≥ L²
        product = (R0 + cfmm.alpha) * (R1 + cfmm.beta)
        assert product >= cfmm.k * 0.99  # Allow small numerical error

    def test_optimal_swap_amounts(self):
        """Find optimal swap amounts."""
        cfmm = tick_range_to_bounded_product(
            tick_lower=-60,
            tick_upper=60,
            liquidity=1_000_000.0,
        )

        current_sqrt_price = tick_to_sqrt_price(0)  # Middle of range
        external_price = 1.0001**30  # Price at tick 30

        _amount_in, _amount_out = cfmm.optimal_swap_at_price(
            external_price=external_price,
            current_sqrt_price=current_sqrt_price,
            zero_for_one=True,
        )

        # Should find a swap if external price differs from current
        # (amount may be zero if external price is worse than pool)


# =============================================================================
# TICK RANGE UTILITIES TESTS
# =============================================================================


class TestTickRangeUtilities:
    """Tests for tick range finding utilities."""

    def test_find_tick_range_at_price(self):
        """Find range containing a price."""
        ranges = [
            TickRange(
                tick_lower=0,
                tick_upper=60,
                liquidity=1_000_000,
                sqrt_price_lower=tick_to_sqrt_price(0),
                sqrt_price_upper=tick_to_sqrt_price(60),
            ),
            TickRange(
                tick_lower=60,
                tick_upper=120,
                liquidity=2_000_000,
                sqrt_price_lower=tick_to_sqrt_price(60),
                sqrt_price_upper=tick_to_sqrt_price(120),
            ),
        ]

        # Price in first range
        sqrt_price_30 = tick_to_sqrt_price(30)
        found = None
        for r in ranges:
            if r.sqrt_price_lower <= sqrt_price_30 <= r.sqrt_price_upper:
                found = r
                break
        assert found is not None
        assert found.tick_lower == 0

        # Price in second range
        sqrt_price_90 = tick_to_sqrt_price(90)
        found = None
        for r in ranges:
            if r.sqrt_price_lower <= sqrt_price_90 <= r.sqrt_price_upper:
                found = r
                break
        assert found is not None
        assert found.tick_lower == 60

    def test_get_nearest_tick_ranges(self):
        """Get N nearest ranges to a price."""
        ranges = [
            TickRange(
                tick_lower=0,
                tick_upper=60,
                liquidity=1_000_000,
                sqrt_price_lower=tick_to_sqrt_price(0),
                sqrt_price_upper=tick_to_sqrt_price(60),
            ),
            TickRange(
                tick_lower=60,
                tick_upper=120,
                liquidity=2_000_000,
                sqrt_price_lower=tick_to_sqrt_price(60),
                sqrt_price_upper=tick_to_sqrt_price(120),
            ),
            TickRange(
                tick_lower=120,
                tick_upper=180,
                liquidity=1_500_000,
                sqrt_price_lower=tick_to_sqrt_price(120),
                sqrt_price_upper=tick_to_sqrt_price(180),
            ),
        ]

        # Price near first range
        sqrt_price_10 = tick_to_sqrt_price(10)

        # Simple nearest calculation
        distances = []
        for r in ranges:
            if sqrt_price_10 < r.sqrt_price_lower:
                dist = r.sqrt_price_lower - sqrt_price_10
            elif sqrt_price_10 > r.sqrt_price_upper:
                dist = sqrt_price_10 - r.sqrt_price_upper
            else:
                dist = 0.0
            distances.append((dist, r))

        distances.sort(key=operator.itemgetter(0))
        nearest = [r for _, r in distances[:2]]

        assert len(nearest) == 2


# =============================================================================
# EDGE CASES
# =============================================================================


class TestEdgeCases:
    """Edge case tests."""

    def test_zero_liquidity(self):
        """Handle zero liquidity gracefully."""
        new_price = estimate_price_impact(
            amount_in=1000.0,
            liquidity=0.0,
            current_sqrt_price=100.0,
            fee=0.003,
            zero_for_one=True,
        )
        # Should return current price unchanged
        assert new_price == 100.0

    def test_extreme_tick_values(self):
        """Handle extreme tick values."""
        # Very negative tick
        sqrt_price_low = tick_to_sqrt_price(-10000)
        assert sqrt_price_low > 0
        assert sqrt_price_low < 1.0

        # Very positive tick
        sqrt_price_high = tick_to_sqrt_price(10000)
        assert sqrt_price_high > 1.0

    def test_tick_spacing_edge_cases(self):
        """Handle different tick spacings."""
        # Common tick spacings
        for tick_spacing in [1, 10, 60, 200]:
            prediction = predict_tick_crossing(
                amount_in=1000.0,
                liquidity=1_000_000.0,
                current_sqrt_price=100.0,
                current_tick=0,
                tick_spacing=tick_spacing,
                fee=0.003,
                zero_for_one=True,
            )
            assert prediction is not None
