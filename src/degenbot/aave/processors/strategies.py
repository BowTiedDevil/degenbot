"""Rounding strategies for Aave token processors."""

from dataclasses import dataclass
from enum import Enum


class RoundingMode(Enum):
    """Rounding modes for ray math operations."""

    HALF_UP = "half_up"
    FLOOR = "floor"
    CEIL = "ceil"


@dataclass(frozen=True, slots=True)
class RoundingStrategy:
    """
    Determines ray_div rounding for each operation type.

    The rounding mode applies to the ray_div operation that converts
    raw amounts to scaled amounts.
    """

    mint_rounding: RoundingMode
    burn_rounding: RoundingMode


@dataclass(frozen=True, slots=True)
class DiscountStrategy:
    """
    GHO-specific discount behavior.

    Discount is a mechanism where GHO borrowers with staked AAVE
    receive a discount on their interest rate.
    """

    supports_discount: bool
    has_discounted_balance_method: bool  # V2+ has getDiscountedBalance


# Collateral (aToken) rounding strategies by revision
# V1/V3: HALF_UP for all operations
# V4/V5: HALF_UP for mint, CEIL for burn
COLLATERAL_STRATEGIES: dict[int, RoundingStrategy] = {
    1: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    2: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    3: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    4: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.CEIL,
    ),
    5: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.CEIL,
    ),
}

# Debt (vToken) rounding strategies by revision
# V1/V3: HALF_UP for all operations
# V4/V5: CEIL for mint (borrow), FLOOR for burn (repay)
DEBT_STRATEGIES: dict[int, RoundingStrategy] = {
    1: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    2: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    3: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    4: RoundingStrategy(
        mint_rounding=RoundingMode.CEIL,
        burn_rounding=RoundingMode.FLOOR,
    ),
    5: RoundingStrategy(
        mint_rounding=RoundingMode.CEIL,
        burn_rounding=RoundingMode.FLOOR,
    ),
}

# GHO discount strategies by revision
# V1: Supports discount, no getDiscountedBalance method
# V2/V3: Supports discount with getDiscountedBalance method
# V4+: Discount deprecated
GHO_DISCOUNT_STRATEGIES: dict[int, DiscountStrategy] = {
    1: DiscountStrategy(
        supports_discount=True,
        has_discounted_balance_method=False,
    ),
    2: DiscountStrategy(
        supports_discount=True,
        has_discounted_balance_method=True,
    ),
    3: DiscountStrategy(
        supports_discount=True,
        has_discounted_balance_method=True,
    ),
    4: DiscountStrategy(
        supports_discount=False,
        has_discounted_balance_method=False,
    ),
    5: DiscountStrategy(
        supports_discount=False,
        has_discounted_balance_method=False,
    ),
    6: DiscountStrategy(
        supports_discount=False,
        has_discounted_balance_method=False,
    ),
}

# GHO rounding strategies by revision
# V1/V2/V3: HALF_UP for all operations (discount takes precedence)
# V4: HALF_UP for all operations
# V5+: CEIL for mint, FLOOR for burn
GHO_STRATEGIES: dict[int, RoundingStrategy] = {
    1: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    2: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    3: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    4: RoundingStrategy(
        mint_rounding=RoundingMode.HALF_UP,
        burn_rounding=RoundingMode.HALF_UP,
    ),
    5: RoundingStrategy(
        mint_rounding=RoundingMode.CEIL,
        burn_rounding=RoundingMode.FLOOR,
    ),
    6: RoundingStrategy(
        mint_rounding=RoundingMode.CEIL,
        burn_rounding=RoundingMode.FLOOR,
    ),
}


__all__ = [
    "COLLATERAL_STRATEGIES",
    "DEBT_STRATEGIES",
    "GHO_DISCOUNT_STRATEGIES",
    "GHO_STRATEGIES",
    "DiscountStrategy",
    "RoundingMode",
    "RoundingStrategy",
]
