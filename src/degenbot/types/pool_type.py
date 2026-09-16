"""PoolFamily and kind enums for pool type resolution."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum, IntEnum
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


class PoolProbe(IntEnum):
    """The on-chain probe outcome BotIo.probe_pool_type returns.

    The Rust probe (pool_builder::builder) classifies a contract by which
    read-only call answers. The enum crosses the PyO3 seam as a 1-based u8
    (auto() values, the curve_math try_from_u8 crossing convention); an
    unrecognized code is seam drift and must raise, never silently classify.

    Member order is the wire contract - keep in lock-step with the
    PoolFamily match in py_bot_io.rs::probe_pool_type (a new Rust variant
    fails that match to compile until a code is assigned here).
    """

    V2 = 1
    V3 = 2
    BALANCER_WEIGHTED = 3
    BALANCER_STABLE = 4
    STABLESWAP = 5


class PoolFamily(Enum):
    """The mathematical family governing swap pricing.

    Used for pool type identification (build_pool dispatch, DB kind derivation).
    Each DEX pool class belongs to exactly one family.
    """

    CONSTANT_PRODUCT = "constant_product"  # V2-family
    CONCENTRATED_LIQUIDITY = "concentrated_liquidity"  # V3-family
    STABLESWAP = "stableswap"  # Curve V1-family
    WEIGHTED = "weighted"  # Balancer (future)


@dataclass(frozen=True)
class PoolTypeDescriptor:
    """Describes the resolved type of a pool.

    Produced by the type resolver, consumed by the dispatch in build_pool.
    """

    family: PoolFamily
    variant: str | None  # "sushiswap", "camelot", "aerodrome", etc.
    # None = canonical Uniswap variant
    kind: str  # DB polymorphic type: "uniswap_v2", "sushiswap_v3", etc.
    factory: ChecksummedAddress | None


def derive_kind(family: PoolFamily, variant: str | None) -> str:
    """Derive the DB kind string from family + variant.

    Examples:
        CONSTANT_PRODUCT + None → "uniswap_v2"
        CONCENTRATED_LIQUIDITY + "sushiswap" → "sushiswap_v3"
        CONSTANT_PRODUCT + "camelot" → "camelot_v2"
        STABLESWAP + None → "stableswap"

    Returns:
        The computed value.

    """
    if family == PoolFamily.STABLESWAP:
        return variant if variant is not None else "stableswap"
    if family == PoolFamily.WEIGHTED:
        return variant if variant is not None else "weighted"
    suffix = "v2" if family == PoolFamily.CONSTANT_PRODUCT else "v3"
    if variant is None:
        return f"uniswap_{suffix}"
    return f"{variant}_{suffix}"
