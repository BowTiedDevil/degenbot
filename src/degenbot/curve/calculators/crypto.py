"""Crypto pool DyCalculator.

CRYPTO: Newton's method, dynamic fee, price_scale.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.calculations.stableswap import stableswap_newton_y, stableswap_reduction_coefficient
from degenbot.curve.types import DyCalculationInputs, SwapStyle
from degenbot.exceptions.pool import EVMRevertError


@dataclass(frozen=True, slots=True)
class CryptoDyCalculator:
    """CRYPTO: Newton's method for y, dynamic fee, price_scale.

    Used by Curve tricrypto and volatile pools.
    """

    swap_style: SwapStyle = SwapStyle.CRYPTO

    def calculate(  # noqa: PLR6301
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        assert inputs.d is not None, "Crypto pool requires d in inputs"
        assert inputs.gamma is not None, "Crypto pool requires gamma in inputs"
        assert inputs.price_scale is not None, "Crypto pool requires price_scale in inputs"

        d = inputs.d
        gamma_val = inputs.gamma
        price_scale = inputs.price_scale
        pool_balances = inputs.balances

        n_coins = inputs.n_coins

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
            xp_[k + 1] = xp_[k + 1] * price_scale[k] * precisions[k + 1] // inputs.PRECISION

        amp = inputs.amp
        try:
            y = stableswap_newton_y(
                amp,
                gamma_val,
                xp=xp_,
                d=d,
                token_index=j,
                n_coins=n_coins,
                a_multiplier=inputs.a_precision,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e
        dy = xp_[j] - y - 1

        xp_[j] = y
        if j > 0:
            dy = dy * inputs.PRECISION // price_scale[j - 1]
        dy //= precisions[j]

        f = stableswap_reduction_coefficient(xp_, inputs.fee_gamma, n_coins)
        fee_calc = (inputs.mid_fee * f + inputs.out_fee * (10**18 - f)) // 10**18

        dy -= fee_calc * dy // 10**10
        return dy
