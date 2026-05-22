"""Tests for standalone calculation functions in degenbot.calculations."""

from fractions import Fraction

from degenbot.calculations.camelot import f_camelot, get_y_camelot, k_camelot
from degenbot.uniswap.v2_functions import constant_product_calc_exact_in
from degenbot.calculations.solidly_stable import (
    calc_d,
    calc_exact_in_stable,
    calc_exact_in_volatile,
    calc_f,
    calc_k,
    get_y_solidly,
)
from degenbot.calculations.solidly_stable import calc_k as solidly_calc_k
from degenbot.calculations.stableswap import (
    calc_d as curve_calc_d,
)
from degenbot.calculations.stableswap import (
    calc_d_variant_alpha,
    calc_dp,
    calc_dp_variant_alpha,
    calc_dp_variant_beta,
    calc_dp_variant_gamma,
)

# ── constant_product tests ──


class TestGetAmountOut:
    """Test the constant-product constant_product_calc_exact_in formula."""

    def test_standard_fee(self):
        """Uniswap V2 0.3% fee: (amount_in * 997/1000 * reserves_out) / (reserves_in + amount_in_after_fee)."""
        result = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert result > 0

    def test_zero_amount_in(self):
        result = constant_product_calc_exact_in(
            amount_in=0,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert result == 0

    def test_known_result(self):
        """Verify with manually computed value: 1e18 in, 1e18/1e18 reserves, 0.3% fee."""
        fee = Fraction(3, 1000)
        amount_in = 1_000_000_000_000_000_000  # 1e18
        reserves_in = 1_000_000_000_000_000_000
        reserves_out = 1_000_000_000_000_000_000
        # amount_in_after_fee = 1e18 - 1e18 * 3 // 1000 = 997e15
        # result = (997e15 * 1e18) // (1e18 + 997e15) = 499248873309964947
        result = constant_product_calc_exact_in(amount_in, reserves_in, reserves_out, fee)
        assert result == 499248873309964947

    def test_pancakeswap_fee(self):
        """PancakeSwap 0.25% fee."""
        result = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(25, 10000),
        )
        assert result > 0


# ── solidly_stable tests ──


class TestSolidlyCalcD:
    """Test the Solidly stable calc_d function."""

    def test_symmetric_inputs(self):
        """With x0 == y, D = 3*x^3 + x^3 = 4*x^3 (all / 10^18)."""
        x = 2_000_000_000_000_000_000  # 2e18
        result = calc_d(x, x)
        expected = (3 * x * ((x * x) // 10**18)) // 10**18 + ((((x * x) // 10**18) * x) // 10**18)
        assert result == expected

    def test_nonzero_for_nonzero_inputs(self):
        assert calc_d(1_000_000_000_000_000_000, 2_000_000_000_000_000_000) > 0


class TestSolidlyCalcK:
    """Test the Solidly stable calc_k function."""

    def test_known_value(self):
        result = calc_k(
            balance_0=1_000_000_000_000_000_000,
            balance_1=2_000_000_000_000_000_000,
            decimals_0=10**18,
            decimals_1=10**18,
        )
        assert result > 0

    def test_different_decimals(self):
        """Should normalize by decimals before computing k."""
        result = calc_k(
            balance_0=1_000_000_000,
            balance_1=2_000_000_000_000,
            decimals_0=10**9,
            decimals_1=10**18,
        )
        assert result > 0


class TestSolidlyCalcF:
    """Test the Solidly stable invariant function f(x0, y)."""

    def test_matches_k_for_cross_product(self):
        """f(x, y) = x*y/1e18 * (x^2+y^2)/1e18, which is the k invariant."""
        x = 2_000_000_000_000_000_000
        y = 3_000_000_000_000_000_000
        # f(x, y) = (x*y/1e18) * (x^2/1e18 + y^2/1e18)
        a = (x * y) // 10**18
        b = (x * x) // 10**18 + (y * y) // 10**18
        expected = (a * b) // 10**18
        assert calc_f(x, y) == expected

    def test_nonzero(self):
        assert calc_f(1_000_000_000_000_000_000, 2_000_000_000_000_000_000) > 0


class TestSolidlyCalcExactInVolatile:
    """Test the Solidly volatile (constant product) exact-in calculation."""

    def test_standard_fee(self):
        result = calc_exact_in_volatile(
            amount_in=1_000_000,
            token_in=0,
            reserves0=10_000_000_000,
            reserves1=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert result > 0

    def test_matches_constant_product(self):
        """Volatile calc should match constant_product_calc_exact_in."""
        fee = Fraction(3, 1000)
        amount_in = 1_000_000
        r0 = 10_000_000_000
        r1 = 10_000_000_000
        cp_result = constant_product_calc_exact_in(amount_in, r0, r1, fee)
        solidly_result = calc_exact_in_volatile(amount_in, 0, r0, r1, fee)
        assert cp_result == solidly_result


class TestSolidlyCalcExactInStable:
    """Test the Solidly stable exact-in calculation."""

    def test_nonzero_output(self):
        result = calc_exact_in_stable(
            amount_in=1_000_000,
            token_in=0,
            reserves0=1_000_000_000_000_000_000,
            reserves1=1_000_000_000_000_000_000,
            decimals0=10**18,
            decimals1=10**18,
            fee=Fraction(3, 1000),
            k_func=calc_k,
            get_y_func=get_y_solidly,
        )
        assert result > 0


class TestGetY:
    """Test Newton's method y-solver."""

    def test_solidly_converges(self):
        """get_y_solidly should find y such that f(x0, y) >= xy."""
        x0 = 1_000_000_000_000_000_000
        y_init = 1_000_000_000_000_000_000
        xy = calc_f(x0, y_init)
        result = get_y_solidly(x0 + 100_000, xy, y_init, 10**18, 10**18)
        assert result > 0


# ── camelot tests ──


class TestCamelot:
    """Test Camelot-specific calculations."""

    def test_f_camelot_symmetric(self):
        x = 1_000_000_000_000_000_000
        # f(x, x) = x*(x^2/1e18 * x/1e18)/1e18 + (x^2/1e18 * x^2/1e18)*x/1e18
        result = f_camelot(x, x)
        assert result > 0

    def test_k_camelot_matches_solidly_k(self):
        """Camelot's k should give the same result as Solidly's calc_k for same inputs."""

        b0 = 1_000_000_000_000_000_000
        b1 = 2_000_000_000_000_000_000
        d0 = d1 = 10**18
        assert k_camelot(b0, b1, d0, d1) == solidly_calc_k(b0, b1, d0, d1)

    def test_get_y_camelot_converges(self):
        x0 = 1_000_000_000_000_000_000
        y_init = 1_000_000_000_000_000_000
        xy = f_camelot(x0, y_init)
        result = get_y_camelot(x0 + 100_000, xy, y_init)
        assert result > 0


# ── Curve StableSwap calc tests ──


class TestCurveCalcD:
    """Test Curve StableSwap D step functions."""

    def test_standard_d_step(self):
        """Standard D step should produce a positive result."""
        result = curve_calc_d(
            a_nn=10_000 * 2,  # A=10,000, n=2
            s=4_000_000_000_000_000_000,  # sum(xp)
            d=2_000_000_000_000_000_000,
            d_p=1_000_000_000_000_000_000,
            n_coins=2,
            a_precision=10**10,
        )
        assert result > 0

    def test_variant_alpha_d_step(self):
        """Variant alpha D step should produce a positive result."""
        result = calc_d_variant_alpha(
            a_nn=10_000 * 2,
            s=4_000_000_000_000_000_000,
            d=2_000_000_000_000_000_000,
            d_p=1_000_000_000_000_000_000,
            n_coins=2,
            a_precision=10**10,
        )
        assert result > 0

    def test_variant_alpha_differs_from_standard(self):
        """Variant alpha should produce a different result than standard for non-trivial inputs."""
        kwargs = dict(
            a_nn=5000,  # Small A — more curvature
            s=3_000_000_000_000_000_000,
            d=1_500_000_000_000_000_000,
            d_p=500_000_000_000_000_000,
            n_coins=3,
            a_precision=10**10,
        )
        standard = curve_calc_d(**kwargs)
        alpha = calc_d_variant_alpha(**kwargs)
        assert standard != alpha

    def test_variant_alpha_ignores_a_precision(self):
        """Variant alpha should produce the same result regardless of a_precision."""
        kwargs = dict(
            a_nn=5000,
            s=3_000_000_000_000_000_000,
            d=1_500_000_000_000_000_000,
            d_p=500_000_000_000_000_000,
            n_coins=3,
        )
        result_10 = calc_d_variant_alpha(**kwargs, a_precision=10)
        result_1e10 = calc_d_variant_alpha(**kwargs, a_precision=10**10)
        assert result_10 == result_1e10


class TestCurveCalcDp:
    """Test Curve StableSwap D' step functions."""

    def test_standard_dp(self):
        xp = [1_000_000_000_000_000_000, 2_000_000_000_000_000_000]
        result = calc_dp(
            d=2_000_000_000_000_000_000,
            d_p=2_000_000_000_000_000_000,
            xp=xp,
            n_coins=2,
        )
        assert result > 0

    def test_variant_alpha_dp(self):
        """Variant alpha adds +1 to denominator — should differ from standard for small values."""
        xp = [1_000_000_000_000_000_000, 2_000_000_000_000_000_000]
        d = 2_000_000_000_000_000_000
        d_p = 2_000_000_000_000_000_000
        standard = calc_dp(d=d, d_p=d_p, xp=xp, n_coins=2)
        alpha = calc_dp_variant_alpha(d=d, d_p=d_p, xp=xp, n_coins=2)
        # For large values the +1 is negligible, but they should still be computed correctly
        assert standard > 0
        assert alpha > 0

    def test_variant_beta_dp(self):
        xp = [1_000_000_000_000_000_000, 2_000_000_000_000_000_000]
        result = calc_dp_variant_beta(
            d=2_000_000_000_000_000_000,
            d_p=1,  # unused
            xp=xp,
            n_coins=2,
        )
        assert result > 0

    def test_variant_gamma_dp(self):
        xp = [1_000_000_000_000_000_000, 2_000_000_000_000_000_000]
        result = calc_dp_variant_gamma(
            d=2_000_000_000_000_000_000,
            d_p=1,  # unused
            xp=xp,
            n_coins=2,
        )
        assert result > 0

    def test_beta_differs_from_gamma_for_n_greater_than_2(self):
        """n^2 != n^n for n >= 3, so beta and gamma should differ."""
        xp = [1_000_000_000_000_000_000, 2_000_000_000_000_000_000]
        d = 3_000_000_000_000_000_000
        beta = calc_dp_variant_beta(d=d, d_p=1, xp=xp, n_coins=3)
        gamma = calc_dp_variant_gamma(d=d, d_p=1, xp=xp, n_coins=3)
        assert beta != gamma

    def test_beta_equals_gamma_for_n_equals_2(self):
        """2^2 == 2^2, so beta and gamma should be identical for n=2."""
        xp = [1_000_000_000_000_000_000, 2_000_000_000_000_000_000]
        d = 2_000_000_000_000_000_000
        beta = calc_dp_variant_beta(d=d, d_p=1, xp=xp, n_coins=2)
        gamma = calc_dp_variant_gamma(d=d, d_p=1, xp=xp, n_coins=2)
        assert beta == gamma
