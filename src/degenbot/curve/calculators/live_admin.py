"""Live-admin DyCalculator variants.

Two parameterized calculators cover the four live-admin swap styles:

- ``LiveAdminDyCalculator`` — LIVE_ADMIN and LIVE_ADMIN_ORACLE
  variants. These are identical to ``StandardDyCalculator`` with
  different ``rate_source`` values. Kept as a thin re-export for
  ``isinstance`` checks and direct import compatibility.

- ``LiveAdminDynamicDyCalculator`` — handles the two dynamic-fee
  variants (LIVE_ADMIN_DYNAMIC and LIVE_ADMIN_DYNAMIC_PRECISION),
  differing only in whether precision multipliers are applied to the
  effective balances.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING

from degenbot.calculations.stableswap import stableswap_get_y
from degenbot.exceptions.pool import EVMRevertError

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState, DyCalculationInputs, SwapStyle


def _dynamic_fee(xpi: int, xpj: int, _fee: int, _feemul: int, fee_denominator: int) -> int:
    """Compute dynamic fee for offpeg pools.

    Returns:
        The computed integer value.

    """
    if _feemul <= fee_denominator:
        return _fee
    xps2 = (xpi + xpj) ** 2
    return (_feemul * _fee) // (
        (_feemul - fee_denominator) * 4 * xpi * xpj // xps2 + fee_denominator
    )


class PrecisionMode(Enum):
    """Whether precision multipliers are applied to effective balances."""

    NONE = auto()  # raw effective_balances as xp
    PRECISION_MULTIPLIERS = auto()  # effective_balances * precision_multipliers


@dataclass(frozen=True, slots=True)
class LiveAdminDynamicDyCalculator:
    """Live-admin dynamic-fee dy calculator.

    Parameterized by ``precision_mode``:

    - ``NONE``: xp = effective_balances, dy = xp[j] - y (no -1)
    - ``PRECISION_MULTIPLIERS``: xp = effective_balances * precision_multipliers,
      dy = (xp[j] - y) // precision_multipliers[j]

    Both compute a dynamic offpeg fee via ``_dynamic_fee``.
    """

    swap_style: SwapStyle
    precision_mode: PrecisionMode = PrecisionMode.NONE

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,  # noqa: ARG002
    ) -> int:
        """Calculate.

        Returns:
            The computed integer value.

        Raises:
            EVMRevertError: See function documentation.

        """
        assert inputs.effective_balances is not None

        if self.precision_mode is PrecisionMode.PRECISION_MULTIPLIERS:
            xp_ = [
                balance * rate
                for balance, rate in zip(
                    inputs.effective_balances,
                    inputs.precision_multipliers,
                    strict=True,
                )
            ]
            x = xp_[i] + dx * inputs.precision_multipliers[i]
        else:
            xp_ = list(inputs.effective_balances)
            x = xp_[i] + dx

        try:
            y = stableswap_get_y(
                i,
                j,
                x=x,
                xp=xp_,
                amp=inputs.amp,
                n_coins=inputs.n_coins,
                a_precision=inputs.a_precision,
                y_variant=inputs.y_variant,
                d_variant=inputs.d_variant,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

        if self.precision_mode is PrecisionMode.PRECISION_MULTIPLIERS:
            dy = (xp_[j] - y) // inputs.precision_multipliers[j]
        else:
            dy = xp_[j] - y

        fee_ = (
            _dynamic_fee(
                xpi=(xp_[i] + x) // 2,
                xpj=(xp_[j] + y) // 2,
                _fee=inputs.fee,
                _feemul=inputs.offpeg_fee_multiplier,
                fee_denominator=inputs.FEE_DENOMINATOR,
            )
            * dy
            // inputs.FEE_DENOMINATOR
        )
        return dy - fee_
