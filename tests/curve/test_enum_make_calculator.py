"""Tests for make_calculator() factory methods on Curve strategy enums."""

from degenbot.curve.calculators.crypto import CryptoDyCalculator
from degenbot.curve.calculators.live_admin import (
    LiveAdminDyCalculator,
    LiveAdminDynamicDyCalculator,
    LiveAdminDynamicPrecisionDyCalculator,
    LiveAdminOracleDyCalculator,
)
from degenbot.curve.calculators.metapool import (
    MetapoolPrecisionVpDyCalculator,
    MetapoolRedemptionVpDyCalculator,
    MetapoolStandardDyCalculator,
    MetapoolUnderlyingPrecisionVpDyCalculator,
    MetapoolUnderlyingRedemptionDyCalculator,
    MetapoolUnderlyingStandardDyCalculator,
)
from degenbot.curve.calculators.standard import (
    BalanceSource,
    ConversionStyle,
    StandardDyCalculator,
)
from degenbot.curve.types import MetapoolRateStyle, MetapoolUnderlyingStyle, SwapStyle


class TestSwapStyleMakeCalculator:
    """SwapStyle.make_calculator() returns correctly parameterized StandardDyCalculator."""

    def test_standard(self):
        calc = SwapStyle.STANDARD.make_calculator()
        assert isinstance(calc, StandardDyCalculator)
        assert calc.swap_style == SwapStyle.STANDARD
        assert calc.balance_source == BalanceSource.RATE_ADJUSTED_XP
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
        assert isinstance(SwapStyle.LIVE_ADMIN.make_calculator(), LiveAdminDyCalculator)

    def test_live_admin_dynamic(self):
        assert isinstance(
            SwapStyle.LIVE_ADMIN_DYNAMIC.make_calculator(), LiveAdminDynamicDyCalculator
        )

    def test_live_admin_dynamic_precision(self):
        assert isinstance(
            SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION.make_calculator(),
            LiveAdminDynamicPrecisionDyCalculator,
        )

    def test_live_admin_oracle(self):
        assert isinstance(
            SwapStyle.LIVE_ADMIN_ORACLE.make_calculator(), LiveAdminOracleDyCalculator
        )

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
    """MetapoolRateStyle.make_calculator() returns the correct DyCalculator subtype."""

    def test_standard(self):
        assert isinstance(MetapoolRateStyle.STANDARD.make_calculator(), MetapoolStandardDyCalculator)

    def test_precision_vp(self):
        assert isinstance(
            MetapoolRateStyle.PRECISION_VP.make_calculator(), MetapoolPrecisionVpDyCalculator
        )

    def test_redemption_vp(self):
        assert isinstance(
            MetapoolRateStyle.REDEMPTION_VP.make_calculator(), MetapoolRedemptionVpDyCalculator
        )


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
