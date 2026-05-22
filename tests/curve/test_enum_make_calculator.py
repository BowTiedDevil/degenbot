"""Tests for make_calculator() factory functions on Curve strategy enums."""

from degenbot.curve.calculators.crypto import CryptoDyCalculator
from degenbot.curve.calculators.live_admin import LiveAdminDynamicDyCalculator, PrecisionMode
from degenbot.curve.calculators.metapool import (
    MetapoolDyCalculator,
    MetapoolUnderlyingDyCalculator,
)
from degenbot.curve.calculators.standard import (
    BalanceSource,
    ConversionStyle,
    RateSource,
    StandardDyCalculator,
)
from degenbot.curve.strategies import (
    make_metapool_calculator,
    make_metapool_underlying_calculator,
    make_swap_style_calculator,
)
from degenbot.curve.types import MetapoolRateStyle, MetapoolUnderlyingStyle, SwapStyle


class TestSwapStyleMakeCalculator:
    """make_swap_style_calculator() returns correctly parameterized calculators."""

    def test_standard(self):
        calc = make_swap_style_calculator(SwapStyle.STANDARD)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.STANDARD
        assert calc.balance_source == BalanceSource.RATE_ADJUSTED_XP
        assert calc.rate_source == RateSource.RESOLVED_RATES
        assert calc.subtract_one is True
        assert calc.conversion_style == ConversionStyle.FEE_THEN_RATE

    def test_rate_adjusted(self):
        calc = make_swap_style_calculator(SwapStyle.RATE_ADJUSTED)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.RATE_ADJUSTED
        assert calc.conversion_style == ConversionStyle.RATE_THEN_FEE

    def test_rate_adjusted_no_one(self):
        calc = make_swap_style_calculator(SwapStyle.RATE_ADJUSTED_NO_ONE)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.RATE_ADJUSTED_NO_ONE
        assert calc.subtract_one is False
        assert calc.conversion_style == ConversionStyle.RATE_THEN_FEE

    def test_raw_balance(self):
        calc = make_swap_style_calculator(SwapStyle.RAW_BALANCE)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.RAW_BALANCE
        assert calc.balance_source == BalanceSource.RAW_BALANCES
        assert calc.conversion_style == ConversionStyle.FEE_ONLY

    def test_crypto(self):
        assert isinstance(make_swap_style_calculator(SwapStyle.CRYPTO), CryptoDyCalculator)

    def test_live_admin(self):
        calc = make_swap_style_calculator(SwapStyle.LIVE_ADMIN)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN
        assert calc.rate_source == RateSource.RATE_MULTIPLIERS

    def test_live_admin_dynamic(self):
        calc = make_swap_style_calculator(SwapStyle.LIVE_ADMIN_DYNAMIC)
        assert isinstance(calc, LiveAdminDynamicDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN_DYNAMIC
        assert calc.precision_mode == PrecisionMode.NONE

    def test_live_admin_dynamic_precision(self):
        calc = make_swap_style_calculator(SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION)
        assert isinstance(calc, LiveAdminDynamicDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION
        assert calc.precision_mode == PrecisionMode.PRECISION_MULTIPLIERS

    def test_live_admin_oracle(self):
        calc = make_swap_style_calculator(SwapStyle.LIVE_ADMIN_ORACLE)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN_ORACLE
        assert calc.rate_source == RateSource.RESOLVED_RATES

    def test_no_one_fee_rate(self):
        calc = make_swap_style_calculator(SwapStyle.NO_ONE_FEE_RATE)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.NO_ONE_FEE_RATE
        assert calc.subtract_one is False
        assert calc.conversion_style == ConversionStyle.FEE_THEN_RATE

    def test_cytoken(self):
        calc = make_swap_style_calculator(SwapStyle.CYTOKEN)
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.CYTOKEN
        # CYTOKEN uses identical arithmetic to STANDARD (same axes)
        assert calc.balance_source == BalanceSource.RATE_ADJUSTED_XP
        assert calc.subtract_one is True
        assert calc.conversion_style == ConversionStyle.FEE_THEN_RATE


class TestMetapoolRateStyleMakeCalculator:
    """make_metapool_calculator() returns parameterized MetapoolDyCalculator."""

    @staticmethod
    def _make(rate_style: MetapoolRateStyle) -> MetapoolDyCalculator:
        calc = make_metapool_calculator(rate_style)
        assert isinstance(calc, MetapoolDyCalculator)
        assert calc.rate_style == rate_style
        return calc

    def test_standard(self):
        self._make(MetapoolRateStyle.STANDARD)

    def test_precision_vp(self):
        self._make(MetapoolRateStyle.PRECISION_VP)

    def test_redemption_vp(self):
        self._make(MetapoolRateStyle.REDEMPTION_VP)


class TestMetapoolUnderlyingStyleMakeCalculator:
    """
    make_metapool_underlying_calculator() returns
    parameterized MetapoolUnderlyingDyCalculator.
    """

    def test_standard(self):
        calc = make_metapool_underlying_calculator(MetapoolUnderlyingStyle.STANDARD)
        assert isinstance(calc, MetapoolUnderlyingDyCalculator)
        assert calc.underlying_style == MetapoolUnderlyingStyle.STANDARD

    def test_redemption(self):
        calc = make_metapool_underlying_calculator(MetapoolUnderlyingStyle.REDEMPTION)
        assert isinstance(calc, MetapoolUnderlyingDyCalculator)
        assert calc.underlying_style == MetapoolUnderlyingStyle.REDEMPTION

    def test_precision_vp(self):
        calc = make_metapool_underlying_calculator(MetapoolUnderlyingStyle.PRECISION_VP)
        assert isinstance(calc, MetapoolUnderlyingDyCalculator)
        assert calc.underlying_style == MetapoolUnderlyingStyle.PRECISION_VP
