"""Metapool DyCalculator variants.

Metapool get_dy has two dispatch axes:
1. MetapoolRateStyle — used in get_dy() when base_pool is not None
2. MetapoolUnderlyingStyle — used in _get_dy_underlying()

Each style gets its own calculator dataclass.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.curve.types import CurveStableswapPoolState

from degenbot.curve.types import MetapoolRateStyle, MetapoolUnderlyingStyle

# ── Metapool rate-style calculators (for get_dy metapool fast-path) ──


@dataclass(frozen=True, slots=True)
class MetapoolPrecisionVpDyCalculator:
    """PRECISION_VP: rates = (PRECISION, virtual_price)."""

    rate_style: MetapoolRateStyle = MetapoolRateStyle.PRECISION_VP

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
        rates = (
            pool.PRECISION,
            pool._get_virtual_price(block_number=block_number),
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]


@dataclass(frozen=True, slots=True)
class MetapoolRedemptionVpDyCalculator:
    """REDEMPTION_VP: rates = (redemption_price, virtual_price)."""

    rate_style: MetapoolRateStyle = MetapoolRateStyle.REDEMPTION_VP

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
        rates = (
            pool._get_scaled_redemption_price(block_number=block_number),
            pool._get_virtual_price(block_number=block_number),
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]


@dataclass(frozen=True, slots=True)
class MetapoolStandardDyCalculator:
    """STANDARD metapool: rates = (rate_multipliers[0], virtual_price)."""

    rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD

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
        rates = (
            pool.rate_multipliers[0],
            pool._get_virtual_price(block_number=block_number),
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        y = pool._get_y(i, j, x, xp)
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]


# ── Metapool underlying-style calculators (for _get_dy_underlying) ──


@dataclass(frozen=True, slots=True)
class MetapoolUnderlyingRedemptionDyCalculator:
    """REDEMPTION underlying: redemption_price for first coin, VP for second."""

    underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.REDEMPTION

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
        # Re-implemented from _get_dy_underlying REDEMPTION branch
        assert pool.base_pool is not None
        pool_balances = override_state.balances if override_state is not None else pool.balances

        base_n_coins = len(pool.base_pool._tokens)
        max_coin = len(pool._tokens) - 1
        redemption_coin = 0

        rates = (
            pool._get_scaled_redemption_price(block_number=block_number),
            (vp_rate := pool._get_virtual_price(block_number=block_number)),
        )
        xp = pool._xp(rates=rates, balances=pool_balances)

        base_i = i - max_coin
        base_j = j - max_coin
        meta_i = max_coin
        meta_j = max_coin
        if base_i < 0:
            meta_i = i
        if base_j < 0:
            meta_j = j

        if base_i < 0:
            x = xp[i] + (
                dx
                * pool._get_scaled_redemption_price(block_number=block_number)
                // pool.PRECISION
            )
        elif base_j < 0:
            base_inputs = [0] * base_n_coins
            base_inputs[base_i] = dx
            x = (
                pool.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=block_number,
                    override_state=(
                        override_state.base if override_state is not None else None
                    ),
                )
                * vp_rate
                // pool.PRECISION
            )
            x -= x * pool.base_pool.fee // (2 * pool.FEE_DENOMINATOR)
            x += xp[max_coin]
        else:
            return cast("int", pool.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=block_number,
                override_state=(override_state.base if override_state is not None else None),
            ))

        y = pool._get_y(meta_i, meta_j, x, xp)
        dy = xp[meta_j] - y - 1
        dy -= pool.fee * dy // pool.FEE_DENOMINATOR
        if j == redemption_coin:
            dy = (dy * pool.PRECISION) // pool._get_scaled_redemption_price(
                block_number=block_number
            )

        if base_j >= 0:
            dy, *_ = pool.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * pool.PRECISION // vp_rate,
                i=base_j,
                block_identifier=block_number,
            )

        return dy


@dataclass(frozen=True, slots=True)
class MetapoolUnderlyingPrecisionVpDyCalculator:
    """PRECISION_VP underlying: rates = (PRECISION, virtual_price)."""

    underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.PRECISION_VP

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
        assert pool.base_pool is not None
        pool_balances = override_state.balances if override_state is not None else pool.balances

        base_n_coins = len(pool.base_pool._tokens)
        max_coin = len(pool._tokens) - 1

        rates = (pool.PRECISION, pool._get_virtual_price(block_number=block_number))
        xp = pool._xp(rates=rates, balances=pool_balances)

        base_i: int = 0
        base_j: int = 0
        meta_i: int = 0
        meta_j: int = 0

        if i != 0:
            base_i = i - max_coin
            meta_i = 1
        if j != 0:
            base_j = j - max_coin
            meta_j = 1

        if i == 0:
            x = xp[i] + dx * (rates[0] // 10**18)
        elif j == 0:
            base_inputs = [0] * base_n_coins
            base_inputs[base_i] = dx
            x = (
                pool.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=block_number,
                    override_state=(
                        override_state.base if override_state is not None else None
                    ),
                )
                * rates[1]
                // pool.PRECISION
            )
            x -= x * pool.base_pool.fee // (2 * pool.FEE_DENOMINATOR)
            x += xp[max_coin]
        else:
            return cast("int", pool.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=block_number,
                override_state=(override_state.base if override_state is not None else None),
            ))

        y = pool._get_y(meta_i, meta_j, x, xp)
        dy = xp[meta_j] - y - 1
        dy -= pool.fee * dy // pool.FEE_DENOMINATOR

        if j == 0:
            dy //= rates[0] // 10**18
        else:
            dy, *_ = pool.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * pool.PRECISION // rates[1],
                i=base_j,
                block_identifier=block_number,
            )

        return dy


@dataclass(frozen=True, slots=True)
class MetapoolUnderlyingStandardDyCalculator:
    """STANDARD underlying: rate_multipliers with VP for base pool LP token."""

    underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD

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
        assert pool.base_pool is not None
        pool_balances = override_state.balances if override_state is not None else pool.balances

        working_rates = list(pool.rate_multipliers)
        vp_rate = pool._get_virtual_price(block_number=block_number)
        working_rates[-1] = vp_rate

        xp = pool._xp(rates=tuple(working_rates), balances=pool_balances)
        precisions = pool.precision_multipliers

        base_n_coins = len(pool.base_pool._tokens)
        max_coin = len(pool._tokens) - 1

        base_i = i - max_coin
        base_j = j - max_coin
        meta_i = max_coin
        meta_j = max_coin
        if base_i < 0:
            meta_i = i
        if base_j < 0:
            meta_j = j

        if base_i < 0:
            x = xp[i] + dx * precisions[i]
        elif base_j < 0:
            base_inputs = [0] * base_n_coins
            base_inputs[base_i] = dx
            x = (
                pool.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )
                * vp_rate
                // pool.PRECISION
            )
            x -= x * pool.base_pool.fee // (2 * pool.FEE_DENOMINATOR)
            x += xp[max_coin]
        else:
            return cast("int", pool.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=block_number,
                override_state=(override_state.base if override_state is not None else None),
            ))

        y = pool._get_y(meta_i, meta_j, x, xp)
        dy = xp[meta_j] - y - 1
        dy -= pool.fee * dy // pool.FEE_DENOMINATOR

        if base_j < 0:
            dy //= precisions[meta_j]
        else:
            dy, *_ = pool.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * pool.PRECISION // vp_rate,
                i=base_j,
                block_identifier=block_number,
            )

        return dy
