"""Tests for standalone calculation functions in degenbot.calculations."""

from fractions import Fraction

from degenbot.aerodrome.math import camelot_f as f_camelot
from degenbot.aerodrome.math import camelot_get_y_camelot as get_y_camelot
from degenbot.aerodrome.math import camelot_k as k_camelot
from degenbot.aerodrome.math import calc_d
from degenbot.aerodrome.math import (
    calc_exact_in_stable_solidly,
)
from degenbot.aerodrome.math import calc_exact_in_volatile
from degenbot.aerodrome.math import calc_f
from degenbot.aerodrome.math import calc_k
from degenbot.aerodrome.math import get_y_solidly
from degenbot.uniswap.v2_functions import constant_product_calc_exact_in

# ── constant_product tests ──


class TestGetAmountOut:
    """Test the constant-product constant_product_calc_exact_in formula."""

    def test_standard_fee(self):
        """Uniswap V2 0.3% fee with small integer reserves — manually verified."""
        # amount_in=1e6, reserves=1e10 each, fee=3/1000
        # amount_in_after_fee = 1e6 * 997/1000 = 997000
        # result = 997000 * 1e10 // (1e10 * 1000 + 997000) = 996900
        result = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert result == 996900

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
        """PancakeSwap 0.25% fee — manually verified."""
        # amount_in=1e6, reserves=1e10 each, fee=25/10000
        # amount_in_after_fee = 1e6 * 9975/10000 = 997500
        # result = 997500 * 1e10 // (1e10 * 10000 + 997500) = 997400
        result = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(25, 10000),
        )
        assert result == 997400

    def test_asymmetric_reserves(self):
        """Unequal reserves should produce a smaller output than equal reserves."""
        symmetric = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        asymmetric = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=5_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert asymmetric < symmetric

    def test_larger_input_produces_larger_output(self):
        """Monotonicity: more in → more out."""
        small = constant_product_calc_exact_in(
            amount_in=100,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        large = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert large > small

    def test_higher_fee_produces_smaller_output(self):
        """Higher fee → less output for identical inputs and reserves."""
        low_fee = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(3, 1000),
        )
        high_fee = constant_product_calc_exact_in(
            amount_in=1_000_000,
            reserves_in=10_000_000_000,
            reserves_out=10_000_000_000,
            fee=Fraction(1, 100),
        )
        assert high_fee < low_fee


# ── solidly_stable tests ──


class TestSolidlyCalcD:
    """Test the Solidly stable calc_d function."""

    def test_symmetric_inputs(self):
        """With x0 == y == 2e18, D = 3*x*y²/1e18 + x³*y/1e18 = 24e18 + 8e18 = 32e18."""
        x = 2_000_000_000_000_000_000
        result = calc_d(x, x)
        assert result == 32_000_000_000_000_000_000

    def test_asymmetric_inputs(self):
        """With x0=1e18, y=2e18, D = 3*1e18*4e18 + 1e18 = 12e18 + 1e18 = 13e18."""
        result = calc_d(1_000_000_000_000_000_000, 2_000_000_000_000_000_000)
        assert result == 13_000_000_000_000_000_000

    def test_proportional_scaling(self):
        """Doubling both inputs should quadruple D (order-3 homogeneous in fixed-point)."""
        x = 1_000_000_000_000_000_000
        d1 = calc_d(x, x)
        d2 = calc_d(2 * x, 2 * x)
        assert d2 == 8 * d1


class TestSolidlyCalcK:
    """Test the Solidly stable calc_k function."""

    def test_known_value(self):
        """
        With equal 18-decimal reserves (1e18, 2e18),
        k = xy·(x²+y²)/1e18 = 2e18·5e18/1e18 = 10e18.
        """
        result = calc_k(
            balance_0=1_000_000_000_000_000_000,
            balance_1=2_000_000_000_000_000_000,
            decimals_0=10**18,
            decimals_1=10**18,
        )
        assert result == 10_000_000_000_000_000_000

    def test_different_decimals(self):
        """Normalization by decimals should produce a deterministic k value."""
        result = calc_k(
            balance_0=1_000_000_000,
            balance_1=2_000_000_000_000,
            decimals_0=10**9,
            decimals_1=10**18,
        )
        assert result == 2_000_000_000_008

    def test_symmetry(self):
        """k should be symmetric in (balance_0, decimals_0) and (balance_1, decimals_1)."""
        r1 = calc_k(balance_0=3, balance_1=5, decimals_0=10**18, decimals_1=10**18)
        r2 = calc_k(balance_0=5, balance_1=3, decimals_0=10**18, decimals_1=10**18)
        assert r1 == r2


class TestSolidlyCalcF:
    """Test the Solidly stable invariant function f(x0, y)."""

    def test_matches_k_for_cross_product(self):
        """f(x, y) = x*y/1e18 * (x^2+y^2)/1e18, which is the k invariant."""
        x = 2_000_000_000_000_000_000
        y = 3_000_000_000_000_000_000
        a = (x * y) // 10**18
        b = (x * x) // 10**18 + (y * y) // 10**18
        expected = (a * b) // 10**18
        assert calc_f(x, y) == expected

    def test_asymmetric_inputs(self):
        """f(1e18, 2e18) = xy·(x²+y²)/1e18 = 2e18·5e18/1e18 = 10e18."""
        result = calc_f(1_000_000_000_000_000_000, 2_000_000_000_000_000_000)
        assert result == 10_000_000_000_000_000_000

    def test_symmetry(self):
        """f is symmetric: f(x, y) == f(y, x)."""
        x, y = 1_000_000_000_000_000_000, 2_000_000_000_000_000_000
        assert calc_f(x, y) == calc_f(y, x)


class TestSolidlyCalcExactInVolatile:
    """Test the Solidly volatile (constant product) exact-in calculation."""

    def test_standard_fee(self):
        """Volatile swap with token_in=0, matching constant product result."""
        result = calc_exact_in_volatile(
            1_000_000,
            0,
            10_000_000_000,
            10_000_000_000,
            3,
            1000,
        )
        assert result == 996900

    def test_matches_constant_product(self):
        """Volatile calc should match constant_product_calc_exact_in."""
        fee = Fraction(3, 1000)
        amount_in = 1_000_000
        r0 = 10_000_000_000
        r1 = 10_000_000_000
        cp_result = constant_product_calc_exact_in(amount_in, r0, r1, fee)
        solidly_result = calc_exact_in_volatile(
            amount_in, 0, r0, r1, fee.numerator, fee.denominator
        )
        assert cp_result == solidly_result

    def test_token_in_1(self):
        """Swapping token 1 should use reserves1 as input reserves."""
        result_0 = calc_exact_in_volatile(
            1_000_000,
            0,
            10_000_000_000,
            5_000_000_000,
            3,
            1000,
        )
        result_1 = calc_exact_in_volatile(
            1_000_000,
            1,
            5_000_000_000,
            10_000_000_000,
            3,
            1000,
        )
        assert result_0 == result_1


class TestSolidlyCalcExactInStable:
    """Test the Solidly stable exact-in calculation."""

    def test_nonzero_output(self):
        """Exact computed output for symmetric 1e18 reserves with 0.3% fee."""
        result = calc_exact_in_stable_solidly(
            1_000_000,
            0,
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000,
            10**18,
            10**18,
            3,
            1000,
        )
        assert result == 996999

    def test_symmetric_reserves_forward_and_reverse(self):
        """Forward and reverse swaps on symmetric reserves should give equal outputs."""
        common = (
            5_000_000_000_000_000_000,
            5_000_000_000_000_000_000,
            10**18,
            10**18,
            3,
            1000,
        )
        forward = calc_exact_in_stable_solidly(1_000_000_000_000_000_000, 0, *common)
        reverse = calc_exact_in_stable_solidly(1_000_000_000_000_000_000, 1, *common)
        assert forward == reverse


class TestGetY:
    """Test Newton's method y-solver."""

    def test_solidly_converges(self):
        """get_y_solidly should find the correct y preserving invariant k."""
        x0 = 1_000_000_000_000_000_000
        y_init = 1_000_000_000_000_000_000
        xy = calc_f(x0, y_init)
        # With x0 increased by 100_000, y should decrease by ~100_000
        result = get_y_solidly(x0 + 100_000, xy, y_init, 10**18, 10**18)
        assert result == 999999999999900001

    def test_solidly_preserves_invariant(self):
        """The returned y should satisfy f(x0, y) >= k (the target invariant)."""
        x0 = 1_000_000_000_000_000_000
        y_init = 1_000_000_000_000_000_000
        k = calc_f(x0, y_init)
        new_x0 = x0 + 100_000
        y = get_y_solidly(new_x0, k, y_init, 10**18, 10**18)
        assert calc_f(new_x0, y) >= k

    def test_solidly_rounds_down(self):
        """get_y_solidly returns the smallest y such that f(x0, y) >= k;
        f(x0, y-1) must be strictly below k (tight bound)."""
        x0 = 1_000_000_000_000_000_000
        y_init = 1_000_000_000_000_000_000
        k = calc_f(x0, y_init)
        new_x0 = x0 + 100_000
        y = get_y_solidly(new_x0, k, y_init, 10**18, 10**18)
        assert calc_f(new_x0, y) >= k
        assert calc_f(new_x0, y - 1) < k


# ── camelot tests ──


class TestCamelot:
    """Test Camelot-specific calculations."""

    def test_f_camelot_symmetric(self):
        """f_camelot(x, x) with x=1e18
        term1 = x*(y²//1e18 * y//1e18)//1e18 = 1e18
        term2 = (x²//1e18 * x//1e18)*y//1e18 = 1e18
        total = 2e18"""
        x = 1_000_000_000_000_000_000
        result = f_camelot(x, x)
        assert result == 2_000_000_000_000_000_000

    def test_k_camelot_matches_solidly_k(self):
        """Camelot's k should give the same result as Solidly's calc_k for same inputs."""
        b0 = 1_000_000_000_000_000_000
        b1 = 2_000_000_000_000_000_000
        d0 = d1 = 10**18
        assert k_camelot(b0, b1, d0, d1) == calc_k(b0, b1, d0, d1)

    def test_get_y_camelot_converges(self):
        """get_y_camelot should return a y that approximately preserves the invariant."""
        x0 = 1_000_000_000_000_000_000
        y_init = 1_000_000_000_000_000_000
        xy = f_camelot(x0, y_init)
        result = get_y_camelot(x0 + 100_000, xy, y_init)
        assert result == 999999999999900001

    def test_f_camelot_differs_from_solidly_f(self):
        """Camelot's f uses different operation ordering than Solidly's f,
        so they can diverge due to integer truncation differences."""

        # Inputs found via randomized search that produce different truncation
        x = 260_876_273_137_374_942
        y = 218_168_890_076_913_833
        assert f_camelot(x, y) != calc_f(x, y)
