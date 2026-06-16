"""Balancer V2 pool helper calculations (token ratios, invariant checks)."""

from decimal import Decimal

SCALING_FACTOR = Decimal(1 * 10**18)


def bn(x: int | Decimal) -> int:
    """Return bn.

    Returns:
        The computed integer value.

    """
    return int(x)


def fp(x: int | Decimal) -> int:
    """Return fp.

    Returns:
        The computed integer value.

    """
    return bn(to_fp(x))


def to_fp(x: int | Decimal) -> Decimal:
    """Convert to fp.

    Returns:
        The computed value.

    """
    return Decimal(x) * SCALING_FACTOR


def from_fp(x: int | Decimal) -> Decimal:
    """From fp.

    Returns:
        An instance wrapping the given fp.

    """
    return Decimal(x) / SCALING_FACTOR
