"""Crypto pool DyCalculator.

CRYPTO: Newton's method, dynamic fee, price_scale.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.curve.types import SwapStyle


@dataclass(frozen=True, slots=True)
class CryptoDyCalculator:
    """CRYPTO: Newton's method for y, dynamic fee, price_scale.

    Used by Curve tricrypto and volatile pools.
    """

    swap_style: SwapStyle = SwapStyle.CRYPTO

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
        from degenbot.calculations.stableswap import stableswap_reduction_coefficient
        from degenbot.exceptions.pool import MissingCurveData

        pool_balances = override_state.balances if override_state is not None else pool.balances

        # Fetch cached or on-chain D
        if pool._data_provider is None:
            raise MissingCurveData(
                pool.address,
                "data_provider",
                "Crypto pool requires a data_provider for D, gamma, price_scale.",
            )

        try:
            d = pool._cached_contract_D[block_number]
        except KeyError:
            d = pool._data_provider.D(block_number)
            pool._cached_contract_D[block_number] = d

        try:
            gamma_val = pool._cached_gamma[block_number]
        except KeyError:
            gamma_val = pool._data_provider.gamma(block_number)
            pool._cached_gamma[block_number] = gamma_val

        try:
            price_scale = pool._cached_price_scale[block_number]
        except KeyError:
            price_scale = pool._data_provider.price_scale(block_number)
            pool._cached_price_scale[block_number] = price_scale

        n_coins = len(pool._tokens)

        assert i != j, "coin index out of range"
        assert i < n_coins, "coin index out of range"
        assert j < n_coins, "coin index out of range"
        assert dx > 0, "do not exchange 0 coins"

        # Tricrypto precisions (hard-coded in the contract)
        precisions = [
            10**12,  # USDT
            10**10,  # WBTC
            1,  # WETH
        ]

        xp_ = list(pool_balances)
        xp_[i] += dx
        xp_[0] *= precisions[0]

        for k in range(n_coins - 1):
            xp_[k + 1] = xp_[k + 1] * price_scale[k] * precisions[k + 1] // pool.PRECISION

        amp = pool._a(timestamp=pool._block_timestamps[block_number])
        y = pool._newton_y(amp, gamma_val, xp_, d, j)
        dy = xp_[j] - y - 1

        xp_[j] = y
        if j > 0:
            dy = dy * pool.PRECISION // price_scale[j - 1]
        dy //= precisions[j]

        f = stableswap_reduction_coefficient(xp_, pool.fee_gamma, n_coins)
        fee_calc = (pool.mid_fee * f + pool.out_fee * (10**18 - f)) // 10**18

        dy -= fee_calc * dy // 10**10
        return dy
