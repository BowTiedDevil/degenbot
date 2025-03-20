from decimal import Decimal

SCALING_FACTOR = Decimal(1 * 10**18)


def bn(x: int | Decimal) -> int:
    return int(x)


def fp(x: int | Decimal) -> int:
    return bn(to_fp(x))


def to_fp(x: int | Decimal) -> Decimal:
    return Decimal(x) * SCALING_FACTOR


def from_fp(x: int | Decimal) -> Decimal:
    return Decimal(x) / SCALING_FACTOR
