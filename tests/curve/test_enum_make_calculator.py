"""Tests for make_calculator() factory methods on Curve strategy enums."""

from degenbot.curve.calculators.crypto import CryptoDyCalculator
from degenbot.curve.calculators.live_admin import LiveAdminDynamicDyCalculator, PrecisionMode
from degenbot.curve.calculators.metapool import (
    MetapoolDyCalculator,
    MetapoolUnderlyingPrecisionVpDyCalculator,
    MetapoolUnderlyingRedemptionDyCalculator,
    MetapoolUnderlyingStandardDyCalculator,
)
from degenbot.curve.calculators.standard import (
    BalanceSource,
    ConversionStyle,
    RateSource,
    StandardDyCalculator,
)
from degenbot.curve.types import MetapoolRateStyle, MetapoolUnderlyingStyle, SwapStyle


class TestSwapStyleMakeCalculator:
    """SwapStyle.make_calculator() returns correctly parameterized calculators."""

    def test_standard(self):
        calc = SwapStyle.STANDARD.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.STANDARD
        assert calc.balance_source == BalanceSource.RATE_ADJUSTED_XP
        assert calc.rate_source == RateSource.RESOLVED_RATES
        assert calc.subtract_one is True
        assert calc.conversion_style == ConversionStyle.FEE_THEN_RATE

    def test_rate_adjusted(self):
        calc = SwapStyle.RATE_ADJUSTED.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.RATE_ADJUSTED
        assert calc.conversion_style == ConversionStyle.RATE_THEN_FEE

    def test_rate_adjusted_no_one(self):
        calc = SwapStyle.RATE_ADJUSTED_NO_ONE.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.RATE_ADJUSTED_NO_ONE
        assert calc.subtract_one is False
        assert calc.conversion_style == ConversionStyle.RATE_THEN_FEE

    def test_raw_balance(self):
        calc = SwapStyle.RAW_BALANCE.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.RAW_BALANCE
        assert calc.balance_source == BalanceSource.RAW_BALANCES
        assert calc.conversion_style == ConversionStyle.FEE_ONLY

    def test_crypto(self):
        assert isinstance(SwapStyle.CRYPTO.make_calculator(), CryptoDyCalculator)

    def test_live_admin(self):
        calc = SwapStyle.LIVE_ADMIN.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN
        assert calc.rate_source == RateSource.RATE_MULTIPLIERS

    def test_live_admin_dynamic(self):
        calc = SwapStyle.LIVE_ADMIN_DYNAMIC.make_calculator()
        assert isinstance(calc, LiveAdminDynamicDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN_DYNAMIC
        assert calc.precision_mode == PrecisionMode.NONE

    def test_live_admin_dynamic_precision(self):
        calc = SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION.make_calculator()
        assert isinstance(calc, LiveAdminDynamicDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION
        assert calc.precision_mode == PrecisionMode.PRECISION_MULTIPLIERS

    def test_live_admin_oracle(self):
        calc = SwapStyle.LIVE_ADMIN_ORACLE.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.LIVE_ADMIN_ORACLE
        assert calc.rate_source == RateSource.RESOLVED_RATES

    def test_no_one_fee_rate(self):
        calc = SwapStyle.NO_ONE_FEE_RATE.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.NO_ONE_FEE_RATE
        assert calc.subtract_one is False
        assert calc.conversion_style == ConversionStyle.FEE_THEN_RATE

    def test_cytoken(self):
        calc = SwapStyle.CYTOKEN.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.CYTOKEN
        # CYTOKEN uses identical arithmetic to STANDARD (same axes)
        assert calc.balance_source == BalanceSource.RATE_ADJUSTED_XP
        assert calc.subtract_one is True
        assert calc.conversion_style == ConversionStyle.FEE_THEN_RATE


class TestMetapoolRateStyleMakeCalculator:
    """MetapoolRateStyle.make_calculator() returns parameterized MetapoolDyCalculator."""

    @staticmethod
    def _make(rate_style: MetapoolRateStyle) -> MetapoolDyCalculator:
        calc = rate_style.make_calculator()
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
    """MetapoolUnderlyingStyle.make_calculator() returns the correct DyCalculator subtype."""

    def test_standard(self):
        assert isinstance(
            MetapoolUnderlyingStyle.STANDARD.make_calculator(),
            MetapoolUnderlyingStandardDyCalculator,
        )

    def test_redemption(self):
        assert isinstance(
            MetapoolUnderlyingStyle.REDEMPTION.make_calculator(),
            MetapoolUnderlyingRedemptionDyCalculator,
        )

    def test_precision_vp(self):
        assert isinstance(
            MetapoolUnderlyingStyle.PRECISION_VP.make_calculator(),
            MetapoolUnderlyingPrecisionVpDyCalculator,
        )
