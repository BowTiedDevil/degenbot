"""Live-admin DyCalculator variants.

LIVE_ADMIN: live balances minus admin, fee, rate convert
LIVE_ADMIN_DYNAMIC: live balances minus admin, offpeg dynamic fee
LIVE_ADMIN_DYNAMIC_PRECISION: precision multipliers for xp, dynamic offpeg fee
LIVE_ADMIN_ORACLE: live balances minus admin, oracle rates, fee, rate convert
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.curve.types import DyCalculationInputs, SwapStyle


def _dynamic_fee(xpi: int, xpj: int, _fee: int, _feemul: int, fee_denominator: int) -> int:
    """Compute dynamic fee for offpeg pools."""
    if _feemul <= fee_denominator:
        return _fee
    xps2 = (xpi + xpj) ** 2
    return (_feemul * _fee) // (
        (_feemul - fee_denominator) * 4 * xpi * xpj // xps2 + fee_denominator
    )


@dataclass(frozen=True, slots=True)
class LiveAdminDyCalculator:
    """LIVE_ADMIN: live balances minus admin, dy = xp[j] - y - 1, fee, rate convert."""

    swap_style: SwapStyle = SwapStyle.LIVE_ADMIN

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        rates = inputs.rate_multipliers
        xp = inputs.xp
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
        y = inputs.get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return (dy - fee) * inputs.PRECISION // rates[j]


@dataclass(frozen=True, slots=True)
class LiveAdminDynamicDyCalculator:
    """LIVE_ADMIN_DYNAMIC: live balances minus admin, dynamic offpeg fee."""

    swap_style: SwapStyle = SwapStyle.LIVE_ADMIN_DYNAMIC

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        assert inputs.effective_balances is not None
        xp_ = list(inputs.effective_balances)
        x = xp_[i] + dx
        y = inputs.get_y(i, j, x, xp_)
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


@dataclass(frozen=True, slots=True)
class LiveAdminDynamicPrecisionDyCalculator:
    """LIVE_ADMIN_DYNAMIC_PRECISION: precision multipliers for xp, dynamic offpeg fee."""

    swap_style: SwapStyle = SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        assert inputs.effective_balances is not None
        balances = inputs.effective_balances
        xp_ = [
            balance * rate
            for balance, rate in zip(balances, inputs.precision_multipliers, strict=True)
        ]

        x = xp_[i] + dx * inputs.precision_multipliers[i]
        y = inputs.get_y(i, j, x, xp_)
        dy = (xp_[j] - y) // inputs.precision_multipliers[j]

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


@dataclass(frozen=True, slots=True)
class LiveAdminOracleDyCalculator:
    """LIVE_ADMIN_ORACLE: live balances minus admin, oracle rates, fee, rate convert."""

    swap_style: SwapStyle = SwapStyle.LIVE_ADMIN_ORACLE

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
