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
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.curve.types import SwapStyle


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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool._resolve_rates(
            rates=pool.rate_multipliers,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]


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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool._resolve_rates(
            rates=pool.rate_multipliers,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = (xp[j] - y - 1) * pool.PRECISION // rates[j]
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool._resolve_rates(
            rates=pool.rate_multipliers,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = (xp[j] - y) * pool.PRECISION // rates[j]
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        xp = tuple(pool_balances)
        x = xp[i] + dx
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool._resolve_rates(
            rates=pool.rate_multipliers,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]


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
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else pool.balances
        rates = pool._resolve_rates(
            rates=pool.rate_multipliers,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        return (dy - (pool.fee * dy // pool.FEE_DENOMINATOR)) * pool.PRECISION // rates[j]
