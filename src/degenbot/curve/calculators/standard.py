"""Standard and simple DyCalculator variants.

These calculators cover the most common swap paths:
- STANDARD: dy = xp[j] - y - 1, fee, then rate convert
- RATE_ADJUSTED: dy converted to target units before fee
- RATE_ADJUSTED_NO_ONE: like RATE_ADJUSTED but without the -1 subtraction
- RAW_BALANCE: no rate conversion, fee applied directly
- NO_ONE_FEE_RATE: dy = xp[j] - y (no -1), fee, then rate convert
- CYTOKEN: fee inside rate conversion
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.curve.types import DyCalculationInputs, SwapStyle


@dataclass(frozen=True, slots=True)
class StandardDyCalculator:
    """STANDARD: dy = xp[j] - y - 1, fee, then rate convert.

    Used by plain StableSwap pools without lending tokens.
    """

    swap_style: SwapStyle = SwapStyle.STANDARD

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        rates = inputs.resolved_rates
        xp = inputs.xp
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
        y = inputs.get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return (dy - fee) * inputs.PRECISION // rates[j]


@dataclass(frozen=True, slots=True)
class RateAdjustedDyCalculator:
    """RATE_ADJUSTED: dy converted to target units before fee.

    Used by 3pool, Compound, PAX, etc.
    """

    swap_style: SwapStyle = SwapStyle.RATE_ADJUSTED

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        rates = inputs.resolved_rates
        xp = inputs.xp
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
        y = inputs.get_y(i, j, x, xp)
        dy = (xp[j] - y - 1) * inputs.PRECISION // rates[j]
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return dy - fee


@dataclass(frozen=True, slots=True)
class RateAdjustedNoOneDyCalculator:
    """RATE_ADJUSTED_NO_ONE: like RATE_ADJUSTED but without the -1 subtraction.

    Used by some ytoken pools.
    """

    swap_style: SwapStyle = SwapStyle.RATE_ADJUSTED_NO_ONE

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        rates = inputs.resolved_rates
        xp = inputs.xp
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
        y = inputs.get_y(i, j, x, xp)
        dy = (xp[j] - y) * inputs.PRECISION // rates[j]
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return dy - fee


@dataclass(frozen=True, slots=True)
class RawBalanceDyCalculator:
    """RAW_BALANCE: no rate conversion, fee applied directly.

    Used by pools that don't need precision adjustment.
    """

    swap_style: SwapStyle = SwapStyle.RAW_BALANCE

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        xp = inputs.balances
        x = xp[i] + dx
        y = inputs.get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return dy - fee


@dataclass(frozen=True, slots=True)
class NoOneFeeRateDyCalculator:
    """NO_ONE_FEE_RATE: dy = xp[j] - y (no -1), fee, then rate convert.

    Used by AETH/RETH pools.
    """

    swap_style: SwapStyle = SwapStyle.NO_ONE_FEE_RATE

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        rates = inputs.resolved_rates
        xp = inputs.xp
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
        y = inputs.get_y(i, j, x, xp)
        dy = xp[j] - y
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return (dy - fee) * inputs.PRECISION // rates[j]


@dataclass(frozen=True, slots=True)
class CytokenDyCalculator:
    """CYTOKEN: fee inside rate conversion.

    dy = xp[j] - y - 1, then (dy - fee) * PRECISION // rates[j]
    """

    swap_style: SwapStyle = SwapStyle.CYTOKEN

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        rates = inputs.resolved_rates
        xp = inputs.xp
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
        y = inputs.get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        return (dy - (inputs.fee * dy // inputs.FEE_DENOMINATOR)) * inputs.PRECISION // rates[j]
