"""Tests for make_calculator() factory methods on Curve strategy enums."""


class TestSwapStyleMakeCalculator:
    """SwapStyle.make_calculator() returns the correct DyCalculator subtype."""

    def test_standard(self):
        from degenbot.curve.calculators.standard import StandardDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.STANDARD.make_calculator(), StandardDyCalculator)

    def test_rate_adjusted(self):
        from degenbot.curve.calculators.standard import RateAdjustedDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.RATE_ADJUSTED.make_calculator(), RateAdjustedDyCalculator)

    def test_rate_adjusted_no_one(self):
        from degenbot.curve.calculators.standard import RateAdjustedNoOneDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(
            SwapStyle.RATE_ADJUSTED_NO_ONE.make_calculator(), RateAdjustedNoOneDyCalculator
        )

    def test_raw_balance(self):
        from degenbot.curve.calculators.standard import RawBalanceDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.RAW_BALANCE.make_calculator(), RawBalanceDyCalculator)

    def test_crypto(self):
        from degenbot.curve.calculators.crypto import CryptoDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.CRYPTO.make_calculator(), CryptoDyCalculator)

    def test_live_admin(self):
        from degenbot.curve.calculators.live_admin import LiveAdminDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.LIVE_ADMIN.make_calculator(), LiveAdminDyCalculator)

    def test_live_admin_dynamic(self):
        from degenbot.curve.calculators.live_admin import LiveAdminDynamicDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(
            SwapStyle.LIVE_ADMIN_DYNAMIC.make_calculator(), LiveAdminDynamicDyCalculator
        )

    def test_live_admin_dynamic_precision(self):
        from degenbot.curve.calculators.live_admin import LiveAdminDynamicPrecisionDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(
            SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION.make_calculator(),
            LiveAdminDynamicPrecisionDyCalculator,
        )

    def test_live_admin_oracle(self):
        from degenbot.curve.calculators.live_admin import LiveAdminOracleDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(
            SwapStyle.LIVE_ADMIN_ORACLE.make_calculator(), LiveAdminOracleDyCalculator
        )

    def test_no_one_fee_rate(self):
        from degenbot.curve.calculators.standard import NoOneFeeRateDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.NO_ONE_FEE_RATE.make_calculator(), NoOneFeeRateDyCalculator)

    def test_cytoken(self):
        from degenbot.curve.calculators.standard import CytokenDyCalculator
        from degenbot.curve.types import SwapStyle

        assert isinstance(SwapStyle.CYTOKEN.make_calculator(), CytokenDyCalculator)


class TestMetapoolRateStyleMakeCalculator:
    """MetapoolRateStyle.make_calculator() returns the correct DyCalculator subtype."""

    def test_standard(self):
        from degenbot.curve.calculators.metapool import MetapoolStandardDyCalculator
        from degenbot.curve.types import MetapoolRateStyle

        assert isinstance(MetapoolRateStyle.STANDARD.make_calculator(), MetapoolStandardDyCalculator)

    def test_precision_vp(self):
        from degenbot.curve.calculators.metapool import MetapoolPrecisionVpDyCalculator
        from degenbot.curve.types import MetapoolRateStyle

        assert isinstance(
            MetapoolRateStyle.PRECISION_VP.make_calculator(), MetapoolPrecisionVpDyCalculator
        )

    def test_redemption_vp(self):
        from degenbot.curve.calculators.metapool import MetapoolRedemptionVpDyCalculator
        from degenbot.curve.types import MetapoolRateStyle

        assert isinstance(
            MetapoolRateStyle.REDEMPTION_VP.make_calculator(), MetapoolRedemptionVpDyCalculator
        )


class TestMetapoolUnderlyingStyleMakeCalculator:
    """MetapoolUnderlyingStyle.make_calculator() returns the correct DyCalculator subtype."""

    def test_standard(self):
        from degenbot.curve.calculators.metapool import MetapoolUnderlyingStandardDyCalculator
        from degenbot.curve.types import MetapoolUnderlyingStyle

        assert isinstance(
            MetapoolUnderlyingStyle.STANDARD.make_calculator(),
            MetapoolUnderlyingStandardDyCalculator,
        )

    def test_redemption(self):
        from degenbot.curve.calculators.metapool import MetapoolUnderlyingRedemptionDyCalculator
        from degenbot.curve.types import MetapoolUnderlyingStyle

        assert isinstance(
            MetapoolUnderlyingStyle.REDEMPTION.make_calculator(),
            MetapoolUnderlyingRedemptionDyCalculator,
        )

    def test_precision_vp(self):
        from degenbot.curve.calculators.metapool import MetapoolUnderlyingPrecisionVpDyCalculator
        from degenbot.curve.types import MetapoolUnderlyingStyle

        assert isinstance(
            MetapoolUnderlyingStyle.PRECISION_VP.make_calculator(),
            MetapoolUnderlyingPrecisionVpDyCalculator,
        )
