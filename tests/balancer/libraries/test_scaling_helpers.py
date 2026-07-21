"""Tests for Balancer scaling helpers, ported from the Balancer v2 monorepo.

Scaling helpers normalize token amounts to 18-decimal fixed-point, so that
pools with tokens of different decimals can use uniform math.
"""

from degenbot.balancer.libraries.scaling_helpers import (
    _downscale_down,
    _downscale_up,
    _upscale,
    _upscale_array,
)


class TestUpscale:
    def test_identity_scaling_factor(self):
        """18-decimal tokens have scaling factor = 1e18, so upscale is identity."""
        amount = 5 * 10**18
        scaling_factor = 10**18  # 18-decimal token
        assert _upscale(amount, scaling_factor) == amount

    def test_6_decimal_token(self):
        """USDC (6 decimals) gets scaled up by 1e12."""
        amount_usdc = 1_000_000  # 1 USDC in raw units
        scaling_factor = 10**18 * 10**12  # 18 - 6 = 12, so 1e18 * 1e12
        result = _upscale(amount_usdc, scaling_factor)
        # 1 USDC should scale to 1e18 in 18-decimal fixed point
        assert result == 1 * 10**18

    def test_rounds_down(self):
        """Upscale uses mul_down, which rounds down."""
        # Use values that produce a non-integer intermediate product
        amount = 3
        scaling_factor = 10**18 + 1  # Not divisible evenly
        result = _upscale(amount, scaling_factor)
        # mul_down rounds down
        assert result == (3 * (10**18 + 1)) // 10**18


class TestDownscaleDown:
    def test_identity_scaling_factor(self):
        amount = 5 * 10**18
        scaling_factor = 10**18
        assert _downscale_down(amount, scaling_factor) == amount

    def test_6_decimal_token(self):
        """Reversing the upscale for a 6-decimal token."""
        amount_scaled = 1 * 10**18
        scaling_factor = 10**18 * 10**12
        result = _downscale_down(amount_scaled, scaling_factor)
        # 1 USDC in raw = 1e6
        assert result == 1_000_000

    def test_rounds_down(self):
        """Downscale down uses div_down, which rounds down."""
        # Use a 6-decimal token scaling factor where rounding matters
        # downscale = div_down(amount, factor) = (amount * ONE) // factor
        amount_scaled = 5 * 10**18 + 1  # Not exactly divisible
        scaling_factor = 10**18 * 10**12  # 6-decimal token
        result = _downscale_down(amount_scaled, scaling_factor)
        # (5e18+1) * 1e18 // 1e30 = (5e36 + 1e18) // 1e30 = 5e6 (1e18 // 1e30 rounds to 0)
        assert result == 5_000_000


class TestDownscaleUp:
    def test_rounds_up(self):
        """Downscale up uses div_up, which rounds up."""
        # Use a 6-decimal token scaling factor where rounding matters
        amount_scaled = 5 * 10**18 + 1
        scaling_factor = 10**18 * 10**12
        result = _downscale_up(amount_scaled, scaling_factor)
        # div_up rounds up: ((5e36 + 1e18 - 1) // 1e30) + 1 = 5e6 + 1
        assert result == 5_000_001


class TestUpscaleArray:
    def test_mutates_in_place(self):
        """_upscale_array should mutate the list in-place."""
        amounts = [10**18, 2 * 10**18]
        scaling_factors = [2 * 10**18, 3 * 10**18]
        _upscale_array(amounts, scaling_factors)
        assert amounts[0] == _upscale(10**18, 2 * 10**18)
        assert amounts[1] == _upscale(2 * 10**18, 3 * 10**18)

    def test_identity_scaling(self):
        amounts = [5 * 10**18, 10 * 10**18]
        scaling_factors = [10**18, 10**18]
        _upscale_array(amounts, scaling_factors)
        assert amounts == [5 * 10**18, 10 * 10**18]

    def test_mixed_decimals(self):
        """Simulate a pool with DAI (18 dec) and USDC (6 dec)."""
        dai_amount = 10 * 10**18  # 10 DAI
        usdc_amount = 1_000_000  # 1 USDC
        amounts = [dai_amount, usdc_amount]
        dai_scaling = 10**18  # 18 - 18 = 0, so factor = 1e18
        usdc_scaling = 10**18 * 10**12  # 18 - 6 = 12, so factor = 1e30
        scaling_factors = [dai_scaling, usdc_scaling]
        _upscale_array(amounts, scaling_factors)
        # Both should now be 10e18 and 1e18 in 18-decimal fixed point
        assert amounts[0] == 10 * 10**18
        assert amounts[1] == 1 * 10**18
