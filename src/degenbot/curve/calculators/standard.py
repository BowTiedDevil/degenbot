"""Parameterized DyCalculator for standard Curve swap paths.

A single ``StandardDyCalculator`` dataclass replaces six former
class-per-variant dataclasses.  Three independent axes encode every
observed combination:

- **balance_source** — whether XP uses rate-adjusted or raw balances
- **subtract_one** — whether ``dy = xp[j] - y - 1`` or ``xp[j] - y``
- **conversion_style** — the order of fee and rate-conversion steps

The ``SwapStyle`` enum maps each variant to the right axis values via
``make_calculator()``.  ``CYTOKEN`` is preserved as a ``SwapStyle``
label (the pool contracts *are* different) but maps to the same axis
configuration as ``STANDARD`` because the arithmetic is identical.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState, DyCalculationInputs, SwapStyle

from degenbot.calculations.stableswap import stableswap_get_y
from degenbot.exceptions.pool import EVMRevertError


class BalanceSource(Enum):
    """Which balance source the calculator uses for XP construction."""

    RATE_ADJUSTED_XP = auto()  # inputs.xp + inputs.resolved_rates
    RAW_BALANCES = auto()  # inputs.balances, no rate adjustment


class ConversionStyle(Enum):
    """Order of fee and rate-conversion steps on raw dy."""

    FEE_THEN_RATE = auto()  # fee on raw dy, then rate convert
    RATE_THEN_FEE = auto()  # rate convert first, then fee on converted dy
    FEE_ONLY = auto()  # fee only, no rate conversion


@dataclass(frozen=True, slots=True)
class StandardDyCalculator:
    """Parameterized dy calculator for standard Curve swap paths.

    Encodes three independent variation axes that previously required
    six separate dataclasses:

    - balance_source: xp-based (rate-adjusted) or raw balances
    - subtract_one: whether dy = xp[j] - y - 1 or xp[j] - y
    - conversion_style: fee→rate, rate→fee, or fee-only ordering

    STANDARD and CYTOKEN produce identical arithmetic (the only
    difference is the ``swap_style`` label carried for logging).
    """

    swap_style: SwapStyle
    balance_source: BalanceSource = BalanceSource.RATE_ADJUSTED_XP
    subtract_one: bool = True
    conversion_style: ConversionStyle = ConversionStyle.FEE_THEN_RATE

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        # ── Balance source ──
        if self.balance_source is BalanceSource.RAW_BALANCES:
            xp = inputs.balances
            x = xp[i] + dx
            rates = (inputs.PRECISION,) * inputs.n_coins  # identity rates
        else:
            xp = inputs.xp
            rates = inputs.resolved_rates
            x = xp[i] + (dx * rates[i] // inputs.PRECISION)

        # ── Solve invariant ──
        try:
            y = stableswap_get_y(
                i,
                j,
                x=x,
                xp=xp,
                amp=inputs.amp,
                n_coins=inputs.n_coins,
                a_precision=inputs.a_precision,
                y_variant=inputs.y_variant,
                d_variant=inputs.d_variant,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

        # ── Raw dy ──
        dy = xp[j] - y - (1 if self.subtract_one else 0)

        # ── Fee & rate conversion ──
        match self.conversion_style:
            case ConversionStyle.FEE_THEN_RATE:
                fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
                return (dy - fee) * inputs.PRECISION // rates[j]
            case ConversionStyle.RATE_THEN_FEE:
                dy_converted = dy * inputs.PRECISION // rates[j]
                fee = inputs.fee * dy_converted // inputs.FEE_DENOMINATOR
                return dy_converted - fee
            case ConversionStyle.FEE_ONLY:
                fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
                return dy - fee
