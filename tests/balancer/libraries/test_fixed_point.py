"""
Tests for Balancer FixedPoint library, ported from the Balancer v2 monorepo Solidity tests at
https://github.com/balancer/balancer-v2-monorepo/blob/master/pkg/solidity-utils/test/FixedPoint.test.ts

These tests verify the 18-decimal fixed-point arithmetic primitives: mul_down, mul_up, div_down,
div_up, pow_down, pow_up, and complement.
"""

from decimal import Decimal

import pytest

from degenbot.balancer.libraries import fixed_point
from degenbot.balancer.libraries.constants import FOUR, ONE, TWO
from degenbot.balancer.libraries.helpers import fp
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.pool import EVMRevertError

EXPECTED_RELATIVE_ERROR = Decimal("1e-14")

ONE_DEC = Decimal(10) ** 18


# ---------- add / sub ----------


class TestAdd:
    def test_basic(self):
        assert fixed_point.add(3 * ONE, 5 * ONE) == 8 * ONE

    def test_overflow(self):
        with pytest.raises(EVMRevertError, match="ADD_OVERFLOW"):
            fixed_point.add(MAX_UINT256, 1)


class TestSub:
    def test_basic(self):
        assert fixed_point.sub(5 * ONE, 3 * ONE) == 2 * ONE

    def test_underflow(self):
        with pytest.raises(EVMRevertError, match="SUB_OVERFLOW"):
            fixed_point.sub(3 * ONE, 5 * ONE)


# ---------- mul_down / mul_up ----------


class TestMulDown:
    def test_identity(self):
        """Multiplying by ONE is identity."""
        assert fixed_point.mul_down(5 * ONE, ONE) == 5 * ONE

    def test_zero(self):
        assert fixed_point.mul_down(5 * ONE, 0) == 0

    def test_halves(self):
        """0.5 * 0.5 = 0.25, rounded down."""
        result = fixed_point.mul_down(5 * 10**17, 5 * 10**17)
        # 0.5 * 0.5 = 0.25
        assert result == 25 * 10**16  # exact

    def test_rounds_down(self):
        """mul_down should truncate (round down)."""
        # mul_down(a, b) = (a * b) // ONE
        # With non-fixed-point values, the result is zero because 21 // 1e18 = 0
        assert fixed_point.mul_down(3, 7) == 0

        # Better example with fixed-point:
        result = fixed_point.mul_down(3 * ONE, 7 * ONE)
        assert result == 21 * ONE  # 21e36 // 1e18 = 21e18

        # Example with rounding: 1 * 7 = 7; 7 // 1e18 = 0, but also check with just-off values
        # 1.5 * 1.5 = 2.25 → 2 in FP18 (truncated)
        one_point_five = 15 * 10**17
        result = fixed_point.mul_down(one_point_five, one_point_five)
        expected = 225 * 10**16  # 2.25 in FP18
        assert result == expected


class TestMulUp:
    def test_identity(self):
        assert fixed_point.mul_up(5 * ONE, ONE) == 5 * ONE

    def test_zero(self):
        assert fixed_point.mul_up(5 * ONE, 0) == 0

    def test_rounds_up(self):
        """mul_up should round up when there's a remainder."""
        # 1 * 3 = 3; 3 // 10**18 = 0, but mul_up rounds up → 1
        result = fixed_point.mul_up(1, 3)
        assert result == 1  # ((1*3 - 1) // 10**18) + 1 = 0 + 1 = 1


# ---------- div_down / div_up ----------


class TestDivDown:
    def test_identity(self):
        assert fixed_point.div_down(5 * ONE, ONE) == 5 * ONE

    def test_zero_division(self):
        with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
            fixed_point.div_down(5 * ONE, 0)

    def test_zero_numerator(self):
        assert fixed_point.div_down(0, 5 * ONE) == 0


class TestDivUp:
    def test_identity(self):
        assert fixed_point.div_up(5 * ONE, ONE) == 5 * ONE

    def test_zero_division(self):
        with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
            fixed_point.div_up(5 * ONE, 0)

    def test_zero_numerator(self):
        assert fixed_point.div_up(0, 5 * ONE) == 0


# ---------- pow_down / pow_up ----------


VALUES_POW_4 = [
    Decimal("0.0007"),
    Decimal("0.0022"),
    Decimal("0.093"),
    Decimal("2.9"),
    Decimal("13.3"),
    Decimal("450.8"),
    Decimal("1550.3339"),
    Decimal("69039.11"),
    Decimal("7834839.432"),
    Decimal("83202933.5433"),
    Decimal("9983838318.4"),
    Decimal("15831567871.1"),
]


VALUES_POW_2 = [
    Decimal("8e-9"),
    Decimal("0.0000013"),
    Decimal("0.000043"),
    *VALUES_POW_4,
    Decimal("8382392893832.1"),
    Decimal("38859321075205.1"),
    Decimal("848205610278492.2383"),
    Decimal("371328129389320282.3783289"),
]


VALUES_POW_1 = [
    # Note: 1.7e-18 is excluded because fp(1.7e-18) = 1, and LogExpMath.pow(1, ONE) reverts
    # with PRODUCT_OUT_OF_BOUNDS on-chain (ln(1) ~= 0 but the Taylor series approximation error,
    # multiplied by ONE, exceeds MIN_NATURAL_EXPONENT). This is the same behavior as the on-chain
    # Solidity.
    Decimal("1.7e-15"),
    Decimal("1.7e-11"),
    *VALUES_POW_2,
    Decimal("701847104729761867823532.139"),
    Decimal("175915239864219235419349070.947"),
]


def check_pow(x: Decimal, power: int):
    result = fp(x**power)
    # The deployed on-chain FixedPoint.powDown/powUp always go through LogExpMath.pow + error bound,
    # not through fast-path mul operations. The error bound is MAX_POW_RELATIVE_ERROR (1e-14) plus
    # a +1 absolute term that dominates for very small results.
    assert fixed_point.pow_down(fp(x), fp(power)) <= result  # pow_down must round down (or zero)
    assert fixed_point.pow_up(fp(x), fp(power)) >= result  # pow_up must round up
    # Use a larger tolerance for the approximate check since max_error includes a +1 constant
    # that causes large relative error for tiny values.
    assert fixed_point.pow_up(fp(x), fp(power)) == pytest.approx(
        result,
        rel=Decimal("1e-10"),
        abs=2,
    )
    assert fixed_point.pow_down(fp(x), fp(power)) == pytest.approx(
        result,
        rel=Decimal("1e-10"),
        abs=2,
    )


def check_pows(power: int, values: list[Decimal]):
    for value in values:
        check_pow(x=value, power=power)


class TestPowDown:
    def test_y_equals_one(self):
        """x^1 via pow_down should be <= x (rounds down)."""
        x = 5 * ONE
        result = fixed_point.pow_down(x, ONE)
        # The on-chain contract always uses the general path (LogExpMath.pow + error bound),
        # with no y==1 fast path. The error bound makes pow_down round down and pow_up round up.
        assert result <= x
        assert result == pytest.approx(x, rel=1e-10)

    def test_y_equals_two(self):
        """x^2 via pow_down should be <= the exact value (rounds down)."""
        x = 3 * ONE
        result = fixed_point.pow_down(x, TWO)
        # pow_down rounds down; it goes through LogExpMath.pow - max_error,
        # which differs slightly from mul_down(x, x) used in the monorepo fast path.
        # The deployed on-chain contract does NOT have the fast path.
        exact = fixed_point.mul_down(x, x)
        assert result <= exact  # must round down
        assert result == pytest.approx(exact, rel=1e-10)

    def test_y_equals_four(self):
        """x^4 via pow_down should be <= the exact value (rounds down)."""
        x = 3 * ONE
        result = fixed_point.pow_down(x, FOUR)
        square = fixed_point.mul_down(x, x)
        exact = fixed_point.mul_down(square, square)
        assert result <= exact  # must round down
        assert result == pytest.approx(exact, rel=1e-10)

    def test_fractional_exponent(self):
        """4^0.5 = 2, rounded down."""
        result = fixed_point.pow_down(4 * ONE, fp(0.5))
        assert result == pytest.approx(2 * ONE, rel=1e-10)


class TestPowUp:
    def test_y_equals_one(self):
        """x^1 via pow_up should be >= x (rounds up)."""
        x = 5 * ONE
        result = fixed_point.pow_up(x, ONE)
        assert result >= x
        assert result == pytest.approx(x, rel=1e-10)

    def test_y_equals_two(self):
        """x^2 via pow_up should be >= the exact value (rounds up)."""
        x = 3 * ONE
        result = fixed_point.pow_up(x, TWO)
        exact = fixed_point.mul_up(x, x)
        assert result >= exact  # must round up
        assert result == pytest.approx(exact, rel=1e-10)

    def test_y_equals_four(self):
        """x^4 via pow_up should be >= the exact value (rounds up)."""
        x = 3 * ONE
        result = fixed_point.pow_up(x, FOUR)
        square = fixed_point.mul_up(x, x)
        exact = fixed_point.mul_up(square, square)
        assert result >= exact  # must round up
        assert result == pytest.approx(exact, rel=1e-10)


class TestNonFractionalPow:
    def test_pow_1(self):
        check_pows(power=1, values=VALUES_POW_1)

    def test_pow_2(self):
        check_pows(power=2, values=VALUES_POW_2)

    def test_pow_4(self):
        check_pows(power=4, values=VALUES_POW_4)


# ---------- complement ----------


class TestComplement:
    def test_zero(self):
        """complement(0) = 1."""
        assert fixed_point.complement(0) == ONE

    def test_one(self):
        """complement(1) = 0."""
        assert fixed_point.complement(ONE) == 0

    def test_half(self):
        """complement(0.5) = 0.5."""
        assert fixed_point.complement(5 * 10**17) == 5 * 10**17

    def test_greater_than_one(self):
        """complement clamps to 0 for values > ONE."""
        assert fixed_point.complement(2 * ONE) == 0

    def test_small(self):
        """complement(0.01) = 0.99."""
        result = fixed_point.complement(1 * 10**16)
        assert result == 99 * 10**16
