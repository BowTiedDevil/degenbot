"""Tests for standalone Curve StableSwap invariant calculation functions.

These tests verify that the pure functions in `calculations/stableswap.py`
produce identical results to the pool methods they were extracted from.
"""


from degenbot.calculations.stableswap import (
    calc_d,
    calc_d_variant_alpha,
    calc_dp,
    calc_dp_variant_alpha,
    calc_dp_variant_beta,
    calc_dp_variant_gamma,
    stableswap_get_d,
    stableswap_get_y,
    stableswap_get_y_d,
)
from degenbot.curve.types import DVariant, YDVariant, YVariant

# ── Step function tests (pre-existing, kept for regression) ──


class TestCalcD:
    """Test the D-step Newton iteration functions."""

    def test_standard_step(self):
        result = calc_d(
            a_nn=200, s=2_000_000, d=1_500_000, d_p=1_000_000,
            n_coins=2, a_precision=100,
        )
        assert result > 0

    def test_variant_alpha_step(self):
        result = calc_d_variant_alpha(
            a_nn=200, s=2_000_000, d=1_500_000, d_p=1_000_000,
            n_coins=2, a_precision=100,
        )
        assert result > 0


class TestCalcDp:
    """Test the D'-step Newton iteration functions."""

    def test_standard_step(self):
        result = calc_dp(d=2_000_000, d_p=1_000_000, xp=[1_000_000, 1_000_000], n_coins=2)
        assert result > 0

    def test_variant_alpha_step(self):
        result = calc_dp_variant_alpha(d=2_000_000, d_p=1_000_000, xp=[1_000_000, 1_000_000], n_coins=2)
        assert result > 0

    def test_variant_beta_step(self):
        result = calc_dp_variant_beta(d=2_000_000, d_p=1_000_000, xp=[1_000_000, 1_000_000], n_coins=2)
        assert result > 0

    def test_variant_gamma_step(self):
        result = calc_dp_variant_gamma(d=2_000_000, d_p=1_000_000, xp=[1_000_000, 1_000_000], n_coins=2)
        assert result > 0


# ── Iterative solver tests ──


class TestStableswapGetD:
    """Test the full D invariant solver."""

    def test_equal_balances_high_amplification(self):
        """With equal balances and high A, D ≈ sum(xp)."""
        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=1000, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
        # D should be very close to 2e18 for equal balances at high A
        assert d == 2_000_000_000_000_000_000

    def test_equal_balances_moderate_amplification(self):
        """With A=100, equal balances still yield D = sum(xp)."""
        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
        assert d == 2_000_000_000_000_000_000

    def test_unequal_balances(self):
        """With unequal balances, D < sum(xp) due to the invariant curvature."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
        # D must be between the smaller balance and the sum
        assert 1_000_000_000_000_000_000 < d < 3_000_000_000_000_000_000
        assert d == 2_912_328_492_271_816_922  # verified against pool method

    def test_zero_balances(self):
        """Zero balances should return 0 immediately."""
        d = stableswap_get_d([0, 0], amp=100, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
        assert d == 0

    def test_three_coin_pool(self):
        """3-coin pool with equal balances."""
        xp = [
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000,
        ]
        d = stableswap_get_d(xp, amp=100, n_coins=3, a_precision=100, d_variant=DVariant.STANDARD)
        assert d == 3_000_000_000_000_000_000

    def test_variant_alpha(self):
        """VARIANT_ALPHA uses alternate D-step formula."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.VARIANT_ALPHA)
        assert d == 2_998_146_985_239_894_576

    def test_variant_alpha_dp_alpha(self):
        """VARIANT_ALPHA_DP_ALPHA uses alternate D-step and D'-step formulas."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.VARIANT_ALPHA_DP_ALPHA)
        assert d == 2_998_146_985_239_894_576

    def test_variant_dp_alpha(self):
        """VARIANT_DP_ALPHA uses standard D-step but alternate D'-step."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.VARIANT_DP_ALPHA)
        # dp_alpha variant but standard d_func → same as STANDARD
        assert d == 2_912_328_492_271_816_922

    def test_variant_beta_dp(self):
        """VARIANT_BETA_DP uses beta D'-step (2-coin shortcut)."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.VARIANT_BETA_DP)
        assert d == 2_912_328_492_271_816_922

    def test_variant_gamma_dp(self):
        """VARIANT_GAMMA_DP uses gamma D'-step (n^n instead of n^2)."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.VARIANT_GAMMA_DP)
        # For n_coins=2, n^n = n^2, so gamma_dp should equal beta_dp for 2-coin pools
        assert d == 2_912_328_492_271_816_922

    def test_higher_amplification_closer_to_sum(self):
        """Higher A → D closer to sum(xp) for unequal balances."""
        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d_low_a = stableswap_get_d(xp, amp=100, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
        d_high_a = stableswap_get_d(xp, amp=1000, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
        # Higher amplification → D closer to 3e18 (the sum)
        assert d_high_a > d_low_a


# ── Y solver tests ──


class TestStableswapGetY:
    """Test the full Y invariant solver."""

    def test_equal_balances_swap_in_equals_swap_out(self):
        """For equal balances at high A, swapping x[i]+dx → y[j]-dy preserves symmetry."""

        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        # With A=100 (in A_PRECISION units), amp=100
        y = stableswap_get_y(
            0, 1,
            x=1_500_000_000_000_000_000, xp=xp,
            amp=100, n_coins=2, a_precision=100,
            y_variant=YVariant.STANDARD, d_variant=DVariant.STANDARD,
        )
        # y should be less than xp[1] since we're adding to xp[0]
        assert y < xp[1]
        assert y > 0

    def test_y_decreases_as_input_increases(self):
        """More input → less output (basic AMM property)."""

        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        y_small = stableswap_get_y(
            0, 1,
            x=1_100_000_000_000_000_000, xp=xp,
            amp=100, n_coins=2, a_precision=100,
            y_variant=YVariant.STANDARD, d_variant=DVariant.STANDARD,
        )
        y_large = stableswap_get_y(
            0, 1,
            x=1_500_000_000_000_000_000, xp=xp,
            amp=100, n_coins=2, a_precision=100,
            y_variant=YVariant.STANDARD, d_variant=DVariant.STANDARD,
        )
        # More input → less y (output)
        assert y_large < y_small

    def test_y_variant_1(self):
        """VARIANT_1 uses amp without A_PRECISION divisor."""

        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        y = stableswap_get_y(
            0, 1,
            x=1_500_000_000_000_000_000, xp=xp,
            amp=10000, n_coins=2, a_precision=100,  # amp=100*100 for VARIANT_1
            y_variant=YVariant.VARIANT_1, d_variant=DVariant.STANDARD,
        )
        assert y > 0

    def test_y_is_positive(self):
        """Y should always be positive for valid inputs."""

        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        y = stableswap_get_y(
            0, 1,
            x=2_500_000_000_000_000_000, xp=xp,
            amp=100, n_coins=2, a_precision=100,
            y_variant=YVariant.STANDARD, d_variant=DVariant.STANDARD,
        )
        assert y > 0


class TestStableswapGetYD:
    """Test the Y_D invariant solver."""

    def test_equal_balances_returns_original(self):
        """For equal balances and D, y_d should return the original balance."""

        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = 2_000_000_000_000_000_000
        y_d = stableswap_get_y_d(
            100, 0,
            xp=xp, d=d, n_coins=2, a_precision=100, yd_variant=YDVariant.STANDARD,
        )
        assert y_d == 1_000_000_000_000_000_000

    def test_y_d_is_positive(self):
        """Y_D should always be positive for valid inputs."""

        xp = [2_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = 2_800_000_000_000_000_000
        y_d = stableswap_get_y_d(
            100, 0,
            xp=xp, d=d, n_coins=2, a_precision=100, yd_variant=YDVariant.STANDARD,
        )
        assert y_d > 0

    def test_variant_0(self):
        """VARIANT_0 uses A_PRECISION in b/c formulas."""

        xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
        d = 2_000_000_000_000_000_000
        y_d = stableswap_get_y_d(
            100, 0,
            xp=xp, d=d, n_coins=2, a_precision=100, yd_variant=YDVariant.VARIANT_0,
        )
        assert y_d > 0
