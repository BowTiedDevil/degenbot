"""Metapool DyCalculator variants.

Metapool get_dy has two dispatch axes:
1. MetapoolRateStyle — used in get_dy() when base_pool is not None
2. MetapoolUnderlyingStyle — used in _get_dy_underlying()

The three rate-style calculators that previously each had their own
frozen dataclass are now a single ``MetapoolDyCalculator`` parameterized
by ``MetapoolRateStyle``, since they differ only in how the rates tuple
is constructed.

The three underlying-style calculators remain separate dataclasses
because they diverge structurally in their input/output conversion
paths (precision_multipliers vs rates[0]//10^18 vs
scaled_redemption_price inversion) and forcing them into one method
would obscure more than it saves.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.calculations.stableswap import stableswap_get_y
from degenbot.curve.types import DyCalculationInputs, MetapoolRateStyle, MetapoolUnderlyingStyle
from degenbot.exceptions.pool import EVMRevertError

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState

# ── Metapool rate-style calculator ──


@dataclass(frozen=True, slots=True)
class MetapoolDyCalculator:
    """Metapool dy calculator for get_dy() metapool fast-path.

    Parameterized by ``rate_style`` which determines how the rates tuple
    is constructed:

    - ``STANDARD``: rates = (rate_multipliers[0], virtual_price)
    - ``PRECISION_VP``: rates = (PRECISION, virtual_price)
    - ``REDEMPTION_VP``: rates = (scaled_redemption_price, virtual_price)

    All three share the same computation after rate construction.
    """

    rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        pool_balances = override_state.balances if override_state is not None else inputs.balances

        match self.rate_style:
            case MetapoolRateStyle.STANDARD:
                assert inputs.virtual_price is not None
                rates = (inputs.rate_multipliers[0], inputs.virtual_price)
            case MetapoolRateStyle.PRECISION_VP:
                assert inputs.virtual_price is not None
                rates = (inputs.PRECISION, inputs.virtual_price)
            case MetapoolRateStyle.REDEMPTION_VP:
                assert inputs.scaled_redemption_price is not None
                assert inputs.virtual_price is not None
                rates = (inputs.scaled_redemption_price, inputs.virtual_price)

        xp = tuple(
            rate * balance // inputs.PRECISION
            for rate, balance in zip(rates, pool_balances, strict=True)
        )
        x = xp[i] + (dx * rates[i] // inputs.PRECISION)
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
        dy = xp[j] - y - 1
        fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
        return (dy - fee) * inputs.PRECISION // rates[j]


# ── Metapool underlying-style calculators ──


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
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        assert inputs.base_pool is not None
        pool_balances = override_state.balances if override_state is not None else inputs.balances
        assert inputs.scaled_redemption_price is not None
        assert inputs.virtual_price is not None

        base_n_coins = len(inputs.base_pool.tokens)
        max_coin = inputs.n_coins - 1
        redemption_coin = 0

        rates = (
            inputs.scaled_redemption_price,
            (vp_rate := inputs.virtual_price),
        )
        xp = tuple(
            rate * balance // inputs.PRECISION
            for rate, balance in zip(rates, pool_balances, strict=True)
        )

        base_i = i - max_coin
        base_j = j - max_coin
        meta_i = max_coin
        meta_j = max_coin
        if base_i < 0:
            meta_i = i
        if base_j < 0:
            meta_j = j

        if base_i < 0:
            x = xp[i] + (dx * inputs.scaled_redemption_price // inputs.PRECISION)
        elif base_j < 0:
            base_inputs = [0] * base_n_coins
            base_inputs[base_i] = dx
            x = (
                inputs.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=inputs.block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )
                * vp_rate
                // inputs.PRECISION
            )
            x -= x * inputs.base_pool.fee // (2 * inputs.FEE_DENOMINATOR)
            x += xp[max_coin]
        else:
            return inputs.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=inputs.block_number,
                override_state=(override_state.base if override_state is not None else None),
            )

        try:
            y = stableswap_get_y(
                meta_i,
                meta_j,
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
        dy = xp[meta_j] - y - 1
        dy -= inputs.fee * dy // inputs.FEE_DENOMINATOR
        if j == redemption_coin:
            dy = (dy * inputs.PRECISION) // inputs.scaled_redemption_price

        if base_j >= 0:
            dy, *_ = inputs.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * inputs.PRECISION // vp_rate,
                i=base_j,
                block_identifier=inputs.block_number,
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
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        assert inputs.base_pool is not None
        pool_balances = override_state.balances if override_state is not None else inputs.balances
        assert inputs.virtual_price is not None

        base_n_coins = len(inputs.base_pool.tokens)
        max_coin = inputs.n_coins - 1

        rates = (inputs.PRECISION, inputs.virtual_price)
        xp = tuple(
            rate * balance // inputs.PRECISION
            for rate, balance in zip(rates, pool_balances, strict=True)
        )

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
                inputs.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=inputs.block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )
                * rates[1]
                // inputs.PRECISION
            )
            x -= x * inputs.base_pool.fee // (2 * inputs.FEE_DENOMINATOR)
            x += xp[max_coin]
        else:
            return inputs.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=inputs.block_number,
                override_state=(override_state.base if override_state is not None else None),
            )

        try:
            y = stableswap_get_y(
                meta_i,
                meta_j,
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
        dy = xp[meta_j] - y - 1
        dy -= inputs.fee * dy // inputs.FEE_DENOMINATOR

        if j == 0:
            dy //= rates[0] // 10**18
        else:
            dy, *_ = inputs.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * inputs.PRECISION // rates[1],
                i=base_j,
                block_identifier=inputs.block_number,
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
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        assert inputs.base_pool is not None
        pool_balances = override_state.balances if override_state is not None else inputs.balances
        assert inputs.virtual_price is not None

        working_rates = list(inputs.rate_multipliers)
        vp_rate = inputs.virtual_price
        working_rates[-1] = vp_rate

        xp = tuple(
            rate * balance // inputs.PRECISION
            for rate, balance in zip(working_rates, pool_balances, strict=True)
        )
        precisions = inputs.precision_multipliers

        base_n_coins = len(inputs.base_pool.tokens)
        max_coin = inputs.n_coins - 1

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
                inputs.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=inputs.block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )
                * vp_rate
                // inputs.PRECISION
            )
            x -= x * inputs.base_pool.fee // (2 * inputs.FEE_DENOMINATOR)
            x += xp[max_coin]
        else:
            return inputs.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=inputs.block_number,
                override_state=(override_state.base if override_state is not None else None),
            )

        try:
            y = stableswap_get_y(
                meta_i,
                meta_j,
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
        dy = xp[meta_j] - y - 1
        dy -= inputs.fee * dy // inputs.FEE_DENOMINATOR

        if base_j < 0:
            dy //= precisions[meta_j]
        else:
            dy, *_ = inputs.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * inputs.PRECISION // vp_rate,
                i=base_j,
                block_identifier=inputs.block_number,
            )

        return dy
