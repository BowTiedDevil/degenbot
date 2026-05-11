"""Tests for Curve pool crypto parameter detection."""

import eth_abi.abi
import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.crypto_detector import detect_crypto_params
from degenbot.functions import encode_function_calldata

POOL_ADDR = get_checksum_address("0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5")

# Selectors
FEE_GAMMA = encode_function_calldata("fee_gamma()", [])[:4]
MID_FEE = encode_function_calldata("mid_fee()", [])[:4]
OUT_FEE = encode_function_calldata("out_fee()", [])[:4]
GAMMA = encode_function_calldata("gamma()", [])[:4]
OFFPEG_FEE_MULTIPLIER = encode_function_calldata("offpeg_fee_multiplier()", [])[:4]


def _encode_uint256(val: int) -> bytes:
    return eth_abi.abi.encode(["uint256"], [val])


class TestDetectCryptoParams:
    def testNotCryptoPool(self):
        """Pool where fee_gamma() reverts is not a crypto pool."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        w3 = FakeCurveW3({})

        result = detect_crypto_params(w3, POOL_ADDR, block_identifier=18_000_000)
        assert not result.is_crypto
        assert result.fee_gamma is None
        assert result.mid_fee is None
        assert result.out_fee is None
        assert result.gamma is None
        assert result.offpeg_fee_multiplier is None

    def testFeeGammaZeroIsNotCrypto(self):
        """Pool where fee_gamma() returns 0 is not a crypto pool."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        w3 = FakeCurveW3({
            FEE_GAMMA: _encode_uint256(0),
        })

        result = detect_crypto_params(w3, POOL_ADDR, block_identifier=18_000_000)
        assert not result.is_crypto

    def testCryptoPoolWithAllParams(self):
        """Crypto pool with fee_gamma > 0 fetches all related parameters."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        w3 = FakeCurveW3({
            FEE_GAMMA: _encode_uint256(5_000_000_000_000_000),
            MID_FEE: _encode_uint256(4_000_000),
            OUT_FEE: _encode_uint256(4_000_000),
            GAMMA: _encode_uint256(100_000_000_000_000),
            OFFPEG_FEE_MULTIPLIER: _encode_uint256(5_000_000_000_000_000),
        })

        result = detect_crypto_params(w3, POOL_ADDR, block_identifier=18_000_000)
        assert result.is_crypto
        assert result.fee_gamma == 5_000_000_000_000_000
        assert result.mid_fee == 4_000_000
        assert result.out_fee == 4_000_000
        assert result.gamma == 100_000_000_000_000
        assert result.offpeg_fee_multiplier == 5_000_000_000_000_000

    def testCryptoPoolWithMissingMidFee(self):
        """Crypto pool where mid_fee() reverts still reports is_crypto=True."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        w3 = FakeCurveW3({
            FEE_GAMMA: _encode_uint256(5_000_000_000_000_000),
            # MID_FEE not provided — will revert
            OUT_FEE: _encode_uint256(4_000_000),
            GAMMA: _encode_uint256(100_000_000_000_000),
        })

        result = detect_crypto_params(w3, POOL_ADDR, block_identifier=18_000_000)
        assert result.is_crypto
        assert result.mid_fee is None
        assert result.out_fee == 4_000_000
        assert result.gamma == 100_000_000_000_000

    def testCryptoPoolWithoutOffpegFee(self):
        """Crypto pool where offpeg_fee_multiplier() reverts returns None for it."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        w3 = FakeCurveW3({
            FEE_GAMMA: _encode_uint256(5_000_000_000_000_000),
            MID_FEE: _encode_uint256(4_000_000),
            OUT_FEE: _encode_uint256(4_000_000),
            GAMMA: _encode_uint256(100_000_000_000_000),
            # OFFPEG_FEE_MULTIPLIER not provided
        })

        result = detect_crypto_params(w3, POOL_ADDR, block_identifier=18_000_000)
        assert result.is_crypto
        assert result.offpeg_fee_multiplier is None
