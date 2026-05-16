"""Tests for Curve pool strategy resolution."""

import pytest

import pickle

from degenbot.curve._pool_strategies import resolve_pool_strategies
from degenbot.curve.calculators.standard import StandardDyCalculator
from degenbot.curve.types import (
    DVariant,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    PoolStrategies,
    SwapStyle,
    YDVariant,
    YVariant,
)


class TestResolvePoolStrategies:
    """Test resolve_pool_strategies() for known pool addresses."""

    def test_default_for_unknown_address(self):

        strategies = resolve_pool_strategies("0x0000000000000000000000000000000000000001")
        # Unknown address gets defaults with StandardDyCalculator
        assert strategies.swap_style == SwapStyle.STANDARD
        assert strategies.lending_rate_style == LendingRateStyle.NONE
        assert isinstance(strategies.dy_calculator, StandardDyCalculator)

    def test_tripool(self):
        # 3pool uses RATE_ADJUSTED
        strategies = resolve_pool_strategies("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
        assert strategies.swap_style == SwapStyle.RATE_ADJUSTED
        assert strategies.lending_rate_style == LendingRateStyle.NONE

    def test_compound_ctoken(self):
        # Compound pool uses CTOKEN rates + RATE_ADJUSTED_NO_ONE
        strategies = resolve_pool_strategies("0xA2B47E3D5c44877cca798226B7B8118F9BFb7A56")
        assert strategies.swap_style == SwapStyle.RATE_ADJUSTED_NO_ONE
        assert strategies.lending_rate_style == LendingRateStyle.CTOKEN

    def test_y_pool_ytoken(self):
        # Y pool uses YTOKEN rates + RATE_ADJUSTED
        strategies = resolve_pool_strategies("0x45F783CCE6B7FF23B2ab2D70e416cdb7D6055f51")
        assert strategies.swap_style == SwapStyle.RATE_ADJUSTED_NO_ONE
        assert strategies.lending_rate_style == LendingRateStyle.YTOKEN

    def test_y_pool_ytoken_v1(self):
        # 0x06364f10 uses RATE_ADJUSTED (with -1)
        strategies = resolve_pool_strategies("0x06364f10B501e868329afBc005b3492902d6C763")
        assert strategies.swap_style == SwapStyle.RATE_ADJUSTED
        assert strategies.lending_rate_style == LendingRateStyle.YTOKEN

    def test_aeth_pool(self):
        strategies = resolve_pool_strategies("0xA96A65c051bF88B4095Ee1f2451C2A9d43F53Ae2")
        assert strategies.swap_style == SwapStyle.NO_ONE_FEE_RATE
        assert strategies.lending_rate_style == LendingRateStyle.AETH

    def test_reth_pool(self):
        strategies = resolve_pool_strategies("0xF9440930043eb3997fc70e1339dBb11F341de7A8")
        assert strategies.swap_style == SwapStyle.NO_ONE_FEE_RATE
        assert strategies.lending_rate_style == LendingRateStyle.RETH

    def test_cytoken_pool(self):
        strategies = resolve_pool_strategies("0x2dded6Da1BF5DBdF597C45fcFaa3194e53EcfeAF")
        assert strategies.swap_style == SwapStyle.CYTOKEN
        assert strategies.lending_rate_style == LendingRateStyle.CYTOKEN

    def test_crypto_pool(self):
        strategies = resolve_pool_strategies("0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5")
        assert strategies.swap_style == SwapStyle.CRYPTO

    def test_live_admin_pool(self):
        strategies = resolve_pool_strategies("0x4e0915C88bC70750D68C481540F081fEFaF22273")
        assert strategies.swap_style == SwapStyle.LIVE_ADMIN

    def test_live_admin_oracle_pool(self):
        strategies = resolve_pool_strategies("0x59Ab5a5b5d617E478a2479B0cAD80DA7e2831492")
        assert strategies.swap_style == SwapStyle.LIVE_ADMIN_ORACLE
        assert strategies.lending_rate_style == LendingRateStyle.ORACLE

    def test_live_admin_dynamic_pool(self):
        strategies = resolve_pool_strategies("0xEB16Ae0052ed37f479f7fe63849198Df1765a733")
        assert strategies.swap_style == SwapStyle.LIVE_ADMIN_DYNAMIC

    def test_live_admin_dynamic_precision_pool(self):
        strategies = resolve_pool_strategies("0xDeBF20617708857ebe4F679508E7b7863a8A8EeE")
        assert strategies.swap_style == SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION

    def test_metapool_precision_vp(self):
        strategies = resolve_pool_strategies("0xC61557C5d177bd7DC889A3b621eEC333e168f68A")
        assert strategies.metapool_rate_style == MetapoolRateStyle.PRECISION_VP
        assert strategies.metapool_underlying_style == MetapoolUnderlyingStyle.PRECISION_VP

    def test_metapool_redemption(self):
        strategies = resolve_pool_strategies("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d")
        assert strategies.metapool_rate_style == MetapoolRateStyle.REDEMPTION_VP
        assert strategies.metapool_underlying_style == MetapoolUnderlyingStyle.REDEMPTION

    def test_raw_balance_pool(self):
        strategies = resolve_pool_strategies("0x04c90C198b2eFF55716079bc06d7CCc4aa4d7512")
        assert strategies.swap_style == SwapStyle.RAW_BALANCE

    def test_standard_pool(self):
        strategies = resolve_pool_strategies("0xDC24316b9AE028F1497c275EB9192a3Ea0f67022")
        assert strategies.swap_style == SwapStyle.STANDARD

    def test_variant_groups_merged(self):
        """Strategies merge variant group resolution with address mapping."""
        # 0xA5407eAE is in D_VARIANT_GROUP_1 (VARIANT_ALPHA_DP_ALPHA) and Y_VARIANT_GROUP_0+1 (VARIANT_0)
        # It uses RATE_ADJUSTED_NO_ONE swap style but has NO lending tokens
        # (USE_LENDING = [False, False, False, False] in the contract)
        strategies = resolve_pool_strategies("0xA5407eAE9Ba41422680e2e00537571bcC53efBfD")
        assert strategies.d_variant == DVariant.VARIANT_ALPHA_DP_ALPHA
        assert strategies.y_variant == YVariant.VARIANT_0
        assert strategies.swap_style == SwapStyle.RATE_ADJUSTED_NO_ONE
        assert strategies.lending_rate_style == LendingRateStyle.NONE

    def test_unknown_address_has_standard_variants(self):
        strategies = resolve_pool_strategies("0x0000000000000000000000000000000000000001")
        assert strategies.d_variant == DVariant.STANDARD
        assert strategies.y_variant == YVariant.STANDARD
        assert strategies.yd_variant == YDVariant.STANDARD


class TestPoolStrategiesDataclass:
    """Test PoolStrategies frozen dataclass behavior."""

    def test_defaults(self):
        s = PoolStrategies()
        assert s.d_variant == DVariant.STANDARD
        assert s.y_variant == YVariant.STANDARD
        assert s.yd_variant == YDVariant.STANDARD
        assert s.swap_style == SwapStyle.STANDARD
        assert s.metapool_rate_style == MetapoolRateStyle.STANDARD
        assert s.metapool_underlying_style == MetapoolUnderlyingStyle.STANDARD
        assert s.lending_rate_style == LendingRateStyle.NONE

    def test_frozen(self):
        s = PoolStrategies()
        with pytest.raises(AttributeError):
            s.swap_style = SwapStyle.CRYPTO

    def test_equality(self):
        s1 = PoolStrategies(swap_style=SwapStyle.CRYPTO)
        s2 = PoolStrategies(swap_style=SwapStyle.CRYPTO)
        assert s1 == s2

    def test_inequality(self):
        s1 = PoolStrategies(swap_style=SwapStyle.CRYPTO)
        s2 = PoolStrategies(swap_style=SwapStyle.STANDARD)
        assert s1 != s2

    def test_picklable(self):

        s = PoolStrategies(
            d_variant=DVariant.VARIANT_ALPHA,
            swap_style=SwapStyle.RATE_ADJUSTED,
            lending_rate_style=LendingRateStyle.CTOKEN,
        )
        roundtripped = pickle.loads(pickle.dumps(s))
        assert roundtripped == s
