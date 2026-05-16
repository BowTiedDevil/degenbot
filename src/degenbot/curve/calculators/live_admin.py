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
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.curve.types import SwapStyle


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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool.rate_multipliers

        live_balances = [
            pool._fetch_token_balance(token, pool.address, block_identifier=block_number)
            for token in pool._tokens
        ]
        admin_balances = pool._get_admin_balances(block_number=block_number)

        balances = [
            pool_balance - admin_balance
            for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
        ]

        xp = pool._xp(rates=rates, balances=balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]


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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        live_balances = [
            pool._fetch_token_balance(token, pool.address, block_identifier=block_number)
            for token in pool._tokens
        ]
        admin_balances = pool._get_admin_balances(block_number=block_number)

        xp_ = [
            pool_balance - admin_balance
            for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
        ]
        x = xp_[i] + dx
        y = pool._get_y(i, j, x, xp_)
        dy = xp_[j] - y
        fee_ = (
            _dynamic_fee(
                xpi=(xp_[i] + x) // 2,
                xpj=(xp_[j] + y) // 2,
                _fee=pool.fee,
                _feemul=pool.offpeg_fee_multiplier,
                fee_denominator=pool.FEE_DENOMINATOR,
            )
            * dy
            // pool.FEE_DENOMINATOR
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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        live_balances = [
            pool._fetch_token_balance(token, pool.address, block_identifier=block_number)
            for token in pool._tokens
        ]
        admin_balances = pool._get_admin_balances(block_number=block_number)
        balances = [
            pool_balance - admin_balance
            for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
        ]

        xp_ = [
            balance * rate
            for balance, rate in zip(balances, pool.precision_multipliers, strict=True)
        ]

        x = xp_[i] + dx * pool.precision_multipliers[i]
        y = pool._get_y(i, j, x, xp_)
        dy = (xp_[j] - y) // pool.precision_multipliers[j]

        fee_ = (
            _dynamic_fee(
                xpi=(xp_[i] + x) // 2,
                xpj=(xp_[j] + y) // 2,
                _fee=pool.fee,
                _feemul=pool.offpeg_fee_multiplier,
                fee_denominator=pool.FEE_DENOMINATOR,
            )
            * dy
            // pool.FEE_DENOMINATOR
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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool.rate_multipliers

        live_balances = [
            pool._fetch_token_balance(token, pool.address, block_identifier=block_number)
            for token in pool._tokens
        ]
        admin_balances = pool._get_admin_balances(block_number=block_number)
        balances = [
            pool_balance - admin_balance
            for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
        ]
        rates = pool._resolve_rates(
            rates=rates,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]
