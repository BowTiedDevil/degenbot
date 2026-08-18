"""Tests for standalone calculation functions in degenbot.calculations."""

from degenbot.aerodrome.math import (
    calc_exact_in_stable_solidly,
    calc_exact_in_volatile,
    calc_f,
    calc_k,
    get_y_solidly,
)
from degenbot.aerodrome.math import camelot_f as f_camelot
from degenbot.aerodrome.math import camelot_get_y_camelot as get_y_camelot
from degenbot.aerodrome.math import camelot_k as k_camelot


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
