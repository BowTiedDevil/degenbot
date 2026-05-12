"""Tests for Curve pool detection result types."""

from dataclasses import FrozenInstanceError

import pytest

from degenbot.curve.detection.types import (
    ARampingResult,
    CoinDiscoveryResult,
    CryptoDetectionResult,
    LendingDetectionResult,
    MetapoolDetectionResult,
)


class TestCoinDiscoveryResult:
    """CoinDiscoveryResult is a frozen dataclass holding coin enumeration output."""

    def testConstruction(self):
        result = CoinDiscoveryResult(
            token_addresses=(
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            ),
            balances=(1000000, 2000000),
            coin_prototype="coins(uint256)",
            balance_prototype="balances(uint256)",
        )
        assert len(result.token_addresses) == 2
        assert result.balances == (1000000, 2000000)
        assert result.coin_prototype == "coins(uint256)"
        assert result.balance_prototype == "balances(uint256)"

    def testFrozen(self):
        result = CoinDiscoveryResult(
            token_addresses=(),
            balances=(),
            coin_prototype="coins(uint256)",
            balance_prototype="balances(uint256)",
        )
        with pytest.raises(FrozenInstanceError):
            result.coin_prototype = "coins(int128)"  # type: ignore[misc]


class TestLendingDetectionResult:
    """LendingDetectionResult holds lending token detection output."""

    def testNoLending(self):
        result = LendingDetectionResult(
            use_lending=(False, False),
            precision_multipliers=None,
        )
        assert result.use_lending == (False, False)
        assert result.precision_multipliers is None

    def testWithLendingOverrides(self):
        result = LendingDetectionResult(
            use_lending=(True, False),
            precision_multipliers=(10**2, 10**12),
        )
        assert result.use_lending == (True, False)
        assert result.precision_multipliers == (100, 10**12)

    def testFrozen(self):
        result = LendingDetectionResult(
            use_lending=(False,),
            precision_multipliers=None,
        )
        with pytest.raises(FrozenInstanceError):
            result.use_lending = (True,)  # type: ignore[misc]


class TestMetapoolDetectionResult:
    """MetapoolDetectionResult holds metapool detection output."""

    def testNotMetapool(self):
        result = MetapoolDetectionResult(
            is_meta=False,
            base_pool_address=None,
            tokens_underlying=None,
        )
        assert not result.is_meta
        assert result.base_pool_address is None
        assert result.tokens_underlying is None

    def testMetapool(self):
        result = MetapoolDetectionResult(
            is_meta=True,
            base_pool_address="0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
            tokens_underlying=(
                "0x6B175474E89094C44Da98b954EedeAC495271d0F",
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            ),
        )
        assert result.is_meta
        assert result.base_pool_address is not None
        assert len(result.tokens_underlying) == 3

    def testFrozen(self):
        result = MetapoolDetectionResult(
            is_meta=False,
            base_pool_address=None,
            tokens_underlying=None,
        )
        with pytest.raises(FrozenInstanceError):
            result.is_meta = True  # type: ignore[misc]


class TestCryptoDetectionResult:
    """CryptoDetectionResult holds crypto pool parameter detection output."""

    def testNotCrypto(self):
        result = CryptoDetectionResult(
            is_crypto=False,
            fee_gamma=None,
            mid_fee=None,
            out_fee=None,
            gamma=None,
            offpeg_fee_multiplier=None,
        )
        assert not result.is_crypto
        assert result.fee_gamma is None

    def testCryptoPool(self):
        result = CryptoDetectionResult(
            is_crypto=True,
            fee_gamma=5000000000000000,
            mid_fee=4000000,
            out_fee=4000000,
            gamma=100000000000000,
            offpeg_fee_multiplier=None,
        )
        assert result.is_crypto
        assert result.fee_gamma == 5000000000000000
        assert result.mid_fee == 4000000

    def testFrozen(self):
        result = CryptoDetectionResult(
            is_crypto=False,
            fee_gamma=None,
            mid_fee=None,
            out_fee=None,
            gamma=None,
            offpeg_fee_multiplier=None,
        )
        with pytest.raises(FrozenInstanceError):
            result.is_crypto = True  # type: ignore[misc]


class TestARampingResult:
    """ARampingResult holds A ramping parameter detection output."""

    def testNoRamping(self):
        result = ARampingResult(
            initial_a=None,
            initial_a_time=None,
            future_a=None,
            future_a_time=None,
            has_ramping=False,
        )
        assert not result.has_ramping
        assert result.initial_a is None

    def testWithRamping(self):
        result = ARampingResult(
            initial_a=1000,
            initial_a_time=1700000000,
            future_a=2000,
            future_a_time=1700086400,
            has_ramping=True,
        )
        assert result.has_ramping
        assert result.initial_a == 1000
        assert result.future_a == 2000

    def testFrozen(self):
        result = ARampingResult(
            initial_a=None,
            initial_a_time=None,
            future_a=None,
            future_a_time=None,
            has_ramping=False,
        )
        with pytest.raises(FrozenInstanceError):
            result.has_ramping = True  # type: ignore[misc]
