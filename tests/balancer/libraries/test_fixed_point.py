from decimal import Decimal

import pytest

from degenbot.balancer.libraries import fixed_point
from degenbot.balancer.libraries.helpers import fp

EXPECTED_RELATIVE_ERROR = 1 * 10**-14


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
    Decimal("1.7e-18"),
    Decimal("1.7e-15"),
    Decimal("1.7e-11"),
    *VALUES_POW_2,
    Decimal("701847104729761867823532.139"),
    Decimal("175915239864219235419349070.947"),
]


def check_pow(x: Decimal, power: int):
    result = fp(x**power)

    assert fixed_point.powDown(fp(x), fp(power)) == pytest.approx(
        result,
        rel=EXPECTED_RELATIVE_ERROR,
    )
    assert fixed_point.powUp(fp(x), fp(power)) == pytest.approx(
        result,
        rel=EXPECTED_RELATIVE_ERROR,
    )


def check_pows(power: int, values: list[Decimal | int]):
    for value in values:
        check_pow(value, power)


def test_non_fractional_pow_1():
    check_pows(power=1, values=VALUES_POW_1)


def test_non_fractional_pow_2():
    check_pows(power=2, values=VALUES_POW_2)


def test_non_fractional_pow_4():
    check_pows(power=4, values=VALUES_POW_4)
