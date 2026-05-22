"""Calculator construction for Curve StableSwap pool strategies.

This module breaks the circular dependency between degenbot.curve.types
and the calculator modules in degenbot.curve.calculators. Calculators
import from types (DyCalculationInputs, enums); this module imports from
both types and calculators, so neither direction creates a cycle.

PoolStrategies.__post_init__ calls the factory functions here to
auto-construct calculator instances from enum values.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Protocol

from degenbot.curve.calculators.crypto import CryptoDyCalculator
from degenbot.curve.calculators.live_admin import (
    LiveAdminDynamicDyCalculator,
    PrecisionMode,
)
from degenbot.curve.calculators.metapool import (
    MetapoolDyCalculator,
    MetapoolUnderlyingDyCalculator,
)
from degenbot.curve.calculators.standard import (
    BalanceSource,
    ConversionStyle,
    RateSource,
    StandardDyCalculator,
)
from degenbot.curve.types import (
    DVariant,
    DyCalculationInputs,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    SwapStyle,
    YDVariant,
    YVariant,
)

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState


class DyCalculator(Protocol):
    """Calculates dy (output amount) for a Curve StableSwap swap.

    Each SwapStyle variant maps to a frozen dataclass implementing this
    protocol. The pool's get_dy() delegates to the injected calculator
    via PoolStrategies.dy_calculator.

    Calculators receive a DyCalculationInputs object carrying all
    pre-resolved data (balances, rates, xp, variant enums for invariant
    solving). All I/O and cache lookups happen before the calculator
    is called — the calculator is pure math.
    """

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int: ...


def make_swap_style_calculator(swap_style: SwapStyle) -> DyCalculator:
    """Construct the appropriate DyCalculator for a SwapStyle value."""
    match swap_style:
        case SwapStyle.STANDARD | SwapStyle.CYTOKEN:
            return StandardDyCalculator(swap_style=swap_style)
        case SwapStyle.RATE_ADJUSTED:
            return StandardDyCalculator(
                swap_style=swap_style,
                conversion_style=ConversionStyle.RATE_THEN_FEE,
            )
        case SwapStyle.RATE_ADJUSTED_NO_ONE:
            return StandardDyCalculator(
                swap_style=swap_style,
                subtract_one=False,
                conversion_style=ConversionStyle.RATE_THEN_FEE,
            )
        case SwapStyle.RAW_BALANCE:
            return StandardDyCalculator(
                swap_style=swap_style,
                balance_source=BalanceSource.RAW_BALANCES,
                conversion_style=ConversionStyle.FEE_ONLY,
            )
        case SwapStyle.NO_ONE_FEE_RATE:
            return StandardDyCalculator(
                swap_style=swap_style,
                subtract_one=False,
            )
        case SwapStyle.LIVE_ADMIN:
            return StandardDyCalculator(
                swap_style=swap_style,
                rate_source=RateSource.RATE_MULTIPLIERS,
            )
        case SwapStyle.LIVE_ADMIN_ORACLE:
            return StandardDyCalculator(swap_style=swap_style)
        case SwapStyle.LIVE_ADMIN_DYNAMIC:
            return LiveAdminDynamicDyCalculator(
                swap_style=swap_style,
                precision_mode=PrecisionMode.NONE,
            )
        case SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION:
            return LiveAdminDynamicDyCalculator(
                swap_style=swap_style,
                precision_mode=PrecisionMode.PRECISION_MULTIPLIERS,
            )
        case SwapStyle.CRYPTO:
            return CryptoDyCalculator()


def make_metapool_calculator(rate_style: MetapoolRateStyle) -> DyCalculator:
    """Construct the appropriate metapool DyCalculator for a MetapoolRateStyle value."""
    return MetapoolDyCalculator(rate_style=rate_style)


def make_metapool_underlying_calculator(
    underlying_style: MetapoolUnderlyingStyle,
) -> DyCalculator:
    """Construct the appropriate metapool underlying DyCalculator."""
    return MetapoolUnderlyingDyCalculator(underlying_style=underlying_style)


@dataclasses.dataclass(slots=True, frozen=True)
class PoolStrategies:
    """Resolved calculation strategies for a Curve pool instance.

    Set at construction time by the builder from the pool address.
    The pool class is address-agnostic — it only reads these strategy values.

    Calculator instances are auto-constructed from the enum values in
    __post_init__ if not explicitly provided. Callers may pass explicit
    calculator instances to override the default factory (e.g., in tests).
    """

    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    swap_style: SwapStyle = SwapStyle.STANDARD
    metapool_rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD
    metapool_underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD
    lending_rate_style: LendingRateStyle = LendingRateStyle.NONE

    # Calculator instances — auto-constructed from enum values if not provided.
    # Enum values remain for introspection (e.g., logging).
    dy_calculator: DyCalculator | None = dataclasses.field(default=None)
    metapool_dy_calculator: DyCalculator | None = dataclasses.field(default=None)
    metapool_underlying_dy_calculator: DyCalculator | None = dataclasses.field(default=None)

    def __post_init__(self) -> None:
        # Auto-construct calculators from enum values when not explicitly set.
        # Uses object.__setattr__ because the dataclass is frozen.
        if self.dy_calculator is None:
            object.__setattr__(self, "dy_calculator", make_swap_style_calculator(self.swap_style))
        if self.metapool_dy_calculator is None:
            object.__setattr__(
                self,
                "metapool_dy_calculator",
                make_metapool_calculator(self.metapool_rate_style),
            )
        if self.metapool_underlying_dy_calculator is None:
            object.__setattr__(
                self,
                "metapool_underlying_dy_calculator",
                make_metapool_underlying_calculator(self.metapool_underlying_style),
            )
