from decimal import Decimal

SCALING_FACTOR = 1 * 10**18


def bn(x: int | Decimal) -> int:
    return int(x)


def fp(x: int | Decimal) -> int:
    return bn(toFp(x))


def toFp(x: int | Decimal) -> Decimal:
    return Decimal(x) * Decimal(SCALING_FACTOR)


def fromFp(x: int | Decimal) -> Decimal:
    return Decimal(x) / Decimal(SCALING_FACTOR)
