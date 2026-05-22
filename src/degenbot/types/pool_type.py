"""PoolFamily and kind enums for pool type resolution."""
from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


class PoolFamily(Enum):
    """
    The mathematical family governing swap pricing.

    Used for pool type identification (build_pool dispatch, DB kind derivation).
    Each DEX pool class belongs to exactly one family.
    """

    CONSTANT_PRODUCT = "constant_product"  # V2-family
    CONCENTRATED_LIQUIDITY = "concentrated_liquidity"  # V3-family
    STABLESWAP = "stableswap"  # Curve V1-family
    WEIGHTED = "weighted"  # Balancer (future)


@dataclass(frozen=True)
class PoolTypeDescriptor:
    """
    Describes the resolved type of a pool.

    Produced by the type resolver, consumed by the dispatch in build_pool.
    """

    family: PoolFamily
    variant: str | None  # "sushiswap", "camelot", "aerodrome", etc.
    # None = canonical Uniswap variant
    kind: str  # DB polymorphic type: "uniswap_v2", "sushiswap_v3", etc.
    factory: ChecksumAddress | None


def derive_kind(family: PoolFamily, variant: str | None) -> str:
    """
    Derive the DB kind string from family + variant.

    Examples:
        CONSTANT_PRODUCT + None → "uniswap_v2"
        CONCENTRATED_LIQUIDITY + "sushiswap" → "sushiswap_v3"
        CONSTANT_PRODUCT + "camelot" → "camelot_v2"
        STABLESWAP + None → "stableswap"

    """
    if family == PoolFamily.STABLESWAP:
        return variant if variant is not None else "stableswap"
    if family == PoolFamily.WEIGHTED:
        return variant if variant is not None else "weighted"
    suffix = "v2" if family == PoolFamily.CONSTANT_PRODUCT else "v3"
    if variant is None:
        return f"uniswap_{suffix}"
    return f"{variant}_{suffix}"
