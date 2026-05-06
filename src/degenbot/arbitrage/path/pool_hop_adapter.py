"""Extract ``to_hop_state`` and ``extract_fee`` from pool classes.

Thin compatibility shim that delegates to each pool's own
``to_hop_state`` and ``extract_fee`` methods.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.types.hop_types import HopType
    from degenbot.types.pool_protocols import ArbitragePathPool


def extract_fee(pool: ArbitragePathPool, *, zero_for_one: bool) -> Fraction:
    """Extract the trading fee for a swap direction from any supported pool."""
    return pool.extract_fee(zero_for_one=zero_for_one)


def to_hop_state(
    pool: ArbitragePathPool,
    *,
    zero_for_one: bool,
    state_override: object | None = None,
) -> HopType:
    """Convert any supported pool to its solver-compatible ``HopType``."""
    return pool.to_hop_state(zero_for_one=zero_for_one, state_override=state_override)
