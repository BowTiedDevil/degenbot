import math
import operator
import random
from decimal import Decimal

from degenbot.types import EvmInt

NUMERIC_OPERATIONS = [
    operator.add,
    operator.floordiv,
    operator.lshift,
    operator.mod,
    operator.mul,
    operator.rshift,
    operator.sub,
    operator.truediv,
    operator.pow,
    operator.and_,
    operator.or_,
]


def test_evm_int_comparions():
    x = EvmInt(1)

    assert x > 0
    assert x <= 1
    assert x == 1
    assert x >= 1
    assert x < 2

    assert x <= "1"
    assert x == "1"
    assert x >= "1"


def test_evm_int_bit_operations():
    x = EvmInt(1)

    assert x >> 2 == 0
    assert x >> 3 == 0
    assert x >> 4 == 0

    assert x << 2 == 4
    assert x << 3 == 8
    assert x << 4 == 16

    assert EvmInt(1) & EvmInt(1) == 1
    assert EvmInt(1) & EvmInt(2) == 0
    assert EvmInt(2) & EvmInt(1) == 0

    assert EvmInt(1) | EvmInt(1) == 1
    assert EvmInt(1) | EvmInt(2) == 3
    assert EvmInt(2) | EvmInt(1) == 3

    assert ~EvmInt(1) == -2


def test_evm_int_div():
    # Default Python floor division rounds to negative infinity
    assert 5 // 2 == 2
    assert -5 // 2 == -3

    # EvmInt reproduces the EVM floor rounding behavior, which rounds to zero
    assert EvmInt(5) // EvmInt(2) == EvmInt(2) == 2
    assert EvmInt(-5) // EvmInt(2) == EvmInt(-2) == -2
    assert 5 // EvmInt(2) == EvmInt(2) == 2
    assert -5 // EvmInt(2) == EvmInt(-2) == -2

    # EVM does not do floating point division, so / operator should defer to //
    assert EvmInt(5) / EvmInt(2) == EvmInt(2) == 2
    assert EvmInt(-5) / EvmInt(2) == EvmInt(-2) == -2
    assert 5 / EvmInt(2) == EvmInt(2) == 2
    assert -5 / EvmInt(2) == EvmInt(-2) == -2


def test_evm_int_div_fuzzed():
    for _ in range(10_000):
        x = random.randint(-10, 10)
        y = random.randint(-10, 10)

        assert EvmInt(x) / EvmInt(y) == EvmInt(x) // EvmInt(y)

        if x == 0 or y == 0:
            assert EvmInt(x) // EvmInt(y) == 0
        elif x > 0 and y > 0:
            assert EvmInt(x) // EvmInt(y) == Decimal(x) // Decimal(y) == x // y
        elif x < 0 and y < 0:
            assert EvmInt(x) // EvmInt(y) == Decimal(x) // Decimal(y) == abs(x) // abs(y)
        else:
            assert EvmInt(x) // EvmInt(y) == Decimal(x) // Decimal(y) == -(abs(x) // abs(y))


def test_evm_int_operations_return_same_type():
    x = EvmInt(1)
    y = EvmInt(2)

    for operation in NUMERIC_OPERATIONS:
        assert isinstance(operation(x, y), EvmInt)


def test_evm_int_operations_on_differing_types():
    x = EvmInt(1)

    y_values = [
        2,
        2.0,
        "2",
        Decimal(2),
    ]

    for operation in NUMERIC_OPERATIONS:
        print(f"{operation=}")
        for y in y_values:
            assert isinstance(operation(x, y), EvmInt)

            # str type implements a modulo method for formatting, which will raise a TypeError when
            # used numerically
            if not isinstance(y, str):
                assert isinstance(operation(y, x), EvmInt)

    for operation in [
        math.ceil,
        math.floor,
        math.trunc,
        operator.pos,
        operator.neg,
    ]:
        print(f"{operation=}")
        assert isinstance(operation(x), EvmInt)

    for operation in [
        operator.lt,
        operator.le,
        operator.eq,
        operator.gt,
        operator.ge,
    ]:
        print(f"{operation=}")
        for y in y_values:
            operation(x, y)
