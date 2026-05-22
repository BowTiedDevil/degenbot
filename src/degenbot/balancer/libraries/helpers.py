"""Balancer V2 pool helper calculations (token ratios, invariant checks)."""
from decimal import Decimal

SCALING_FACTOR = Decimal(1 * 10**18)


def bn(x: int | Decimal) -> int:
    """Return bn."""
    return int(x)


def fp(x: int | Decimal) -> int:
    """Return fp."""
    return bn(to_fp(x))


def to_fp(x: int | Decimal) -> Decimal:
    """Convert to fp."""
    return Decimal(x) * SCALING_FACTOR


def from_fp(x: int | Decimal) -> Decimal:
    """From fp."""
    return Decimal(x) / SCALING_FACTOR
