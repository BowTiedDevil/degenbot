"""
Metapool DyCalculator variants.

Metapool get_dy has two dispatch axes:
1. MetapoolRateStyle — used in get_dy() when base_pool is not None
2. MetapoolUnderlyingStyle — used in _get_dy_underlying()

Both axes are served by parameterized dataclasses whose
``calculate`` methods dispatch on their style enum — the same pattern
used by ``StandardDyCalculator`` for the non-metapool swap styles.

``MetapoolUnderlyingDyCalculator`` is parameterized by
``MetapoolUnderlyingStyle``. The variants share a common skeleton
(rates → xp → index mapping → x computation → invariant solve →
dy + fee → rate conversion), differing only in how rates are
constructed, which input-conversion factor applies, and how the
output is denominated. A ``match`` on ``underlying_style`` handles
each divergence point.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.calculations.stableswap import stableswap_get_y
from degenbot.curve.types import DyCalculationInputs, MetapoolRateStyle, MetapoolUnderlyingStyle
from degenbot.exceptions.pool import EVMRevertError

if TYPE_CHECKING:
    from degenbot.curve.types import CurveStableswapPoolState


@dataclass(frozen=True, slots=True)
class MetapoolDyCalculator:
    """
    Metapool dy calculator for get_dy() metapool fast-path.

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
        """Calculate."""
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


@dataclass(frozen=True, slots=True)
class MetapoolUnderlyingDyCalculator:
    """
    Metapool dy calculator for _get_dy_underlying().

    Parameterized by ``underlying_style`` which determines how rates are
    constructed, which input-conversion factor applies, and how the
    output dy is denominated:

    - ``REDEMPTION``: rates = (scaled_redemption_price, virtual_price);
      output dy is rate-converted for the redemption coin
    - ``PRECISION_VP``: rates = (PRECISION, virtual_price);
      output dy is divided by precision for the first coin
    - ``STANDARD``: rates from rate_multipliers (VP for base-pool LP
      token); output dy is divided by precision_multipliers for the
      meta coin

    The three variants share a common skeleton: rates → xp → index
    mapping → x computation → invariant solve → dy + fee → rate
    conversion. ``match`` on ``underlying_style`` handles each
    divergence point.
    """

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
        """Calculate."""
        assert inputs.base_pool is not None
        assert inputs.virtual_price is not None
        pool_balances = override_state.balances if override_state is not None else inputs.balances

        base_n_coins = len(inputs.base_pool.tokens)
        max_coin = inputs.n_coins - 1
        vp_rate = inputs.virtual_price

        # ── Rate construction (per-variant) ──
        match self.underlying_style:
            case MetapoolUnderlyingStyle.REDEMPTION:
                assert inputs.scaled_redemption_price is not None
                rates = (inputs.scaled_redemption_price, vp_rate)
            case MetapoolUnderlyingStyle.PRECISION_VP:
                rates = (inputs.PRECISION, vp_rate)
            case MetapoolUnderlyingStyle.STANDARD:
                rates = (inputs.rate_multipliers[0], vp_rate)

        xp = tuple(
            rate * balance // inputs.PRECISION
            for rate, balance in zip(rates, pool_balances, strict=True)
        )

        # ── Index mapping ──
        base_i = i - max_coin
        base_j = j - max_coin
        meta_i = max_coin
        meta_j = max_coin
        if base_i < 0:
            meta_i = i
        if base_j < 0:
            meta_j = j

        # ── x computation ──
        if base_i < 0:
            match self.underlying_style:
                case MetapoolUnderlyingStyle.REDEMPTION:
                    assert inputs.scaled_redemption_price is not None
                    x = xp[i] + (dx * inputs.scaled_redemption_price // inputs.PRECISION)
                case MetapoolUnderlyingStyle.PRECISION_VP:
                    x = xp[i] + dx  # PRECISION // 10**18 == 1
                case MetapoolUnderlyingStyle.STANDARD:
                    x = xp[i] + dx * inputs.precision_multipliers[i]
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

        # ── Invariant solve ──
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

        # ── Rate conversion / withdrawal (per-variant) ──
        match self.underlying_style:
            case MetapoolUnderlyingStyle.REDEMPTION:
                if j == 0:
                    assert inputs.scaled_redemption_price is not None
                    dy = (dy * inputs.PRECISION) // inputs.scaled_redemption_price
                if base_j >= 0:
                    dy, *_ = inputs.base_pool.calc_withdraw_one_coin(
                        _token_amount=dy * inputs.PRECISION // vp_rate,
                        i=base_j,
                        block_identifier=inputs.block_number,
                    )
            case MetapoolUnderlyingStyle.PRECISION_VP:
                if j == 0:
                    pass  # PRECISION // 10**18 == 1, no conversion needed
                else:
                    dy, *_ = inputs.base_pool.calc_withdraw_one_coin(
                        _token_amount=dy * inputs.PRECISION // rates[1],
                        i=base_j,
                        block_identifier=inputs.block_number,
                    )
            case MetapoolUnderlyingStyle.STANDARD:
                if base_j < 0:
                    dy //= inputs.precision_multipliers[meta_j]
                else:
                    dy, *_ = inputs.base_pool.calc_withdraw_one_coin(
                        _token_amount=dy * inputs.PRECISION // vp_rate,
                        i=base_j,
                        block_identifier=inputs.block_number,
                    )

        return dy
