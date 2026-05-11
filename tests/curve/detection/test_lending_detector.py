"""Tests for Curve pool lending token detection."""

import eth_abi.abi
import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.lending_detector import detect_lending_tokens
from degenbot.functions import encode_function_calldata

# Well-known addresses
CDAI = get_checksum_address("0x5d3a536E4D6DbD6114cc1Ead35777bAB948E3643")
CUSDC = get_checksum_address("0x39AA39c021dfbaE8faC545936693aC917d5E7563")
DAI = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
USDC = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
USDT = get_checksum_address("0xdAC17F958D2ee523a2206206994597C13D831ec7")
YDAI = get_checksum_address("0xC2Cb1040220768554cf699b0d863A3cd4324ce32")
ZERO_ADDR = "0x0000000000000000000000000000000000000000"
POOL_ADDR = get_checksum_address("0xbEbc44782C7DB0a1A60Cb6fe97d0b483032FF1C7")

# Selectors
IS_CTOKEN = encode_function_calldata("isCToken()", [])[:4]
UNDERLYING = encode_function_calldata("underlying()", [])[:4]
DECIMALS = encode_function_calldata("decimals()", [])[:4]
TOKEN = encode_function_calldata("token()", [])[:4]


def _encode_uint256(val: int) -> bytes:
    return eth_abi.abi.encode(["uint256"], [val])


def _encode_address(addr: str) -> bytes:
    return eth_abi.abi.encode(["address"], [addr])


def _encode_bool(val: bool) -> bytes:
    return eth_abi.abi.encode(["bool"], [val])


def _encode_uint8(val: int) -> bytes:
    return eth_abi.abi.encode(["uint8"], [val])


class FakeErc20Token:
    """Minimal fake ERC-20 token with just address and decimals."""

    def __init__(self, address: str, decimals: int) -> None:
        self.address = address
        self.decimals = decimals


class TestDetectLendingTokens:
    def testNoLendingTokens(self):
        """Plain pool with no cTokens or yTokens."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        token_addresses = (DAI, USDC, USDT)
        tokens = tuple(
            FakeErc20Token(addr, d)
            for addr, d in [
                (DAI, 18),
                (USDC, 6),
                (USDT, 6),
            ]
        )

        # isCToken() returns False for all, token() reverts for all
        w3 = FakeCurveW3({
            IS_CTOKEN: _encode_bool(False),
        })

        result = detect_lending_tokens(
            w3,
            POOL_ADDR,
            token_addresses,
            tokens,
            block_identifier=18_000_000,
        )
        assert result.use_lending == (False, False, False)
        assert result.precision_multipliers is None

    def testCTokenDetection(self):
        """cToken detection via isCToken(), with underlying decimals for precision."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        # Pool with cDAI (8 decimals) and USDC (6 decimals)
        token_addresses = (CDAI, USDC)
        tokens = tuple(
            FakeErc20Token(addr, d)
            for addr, d in [
                (CDAI, 8),
                (USDC, 6),
            ]
        )

        call_count = {"is_ctoken": 0}

        def handle_is_ctoken(to: str, data: bytes, block: int) -> bytes:
            call_count["is_ctoken"] += 1
            # First call for cDAI → True, second call for USDC → False
            if to == CDAI:
                return _encode_bool(True)
            return _encode_bool(False)

        def handle_underlying(to: str, data: bytes, block: int) -> bytes:
            return _encode_address(DAI)

        def handle_decimals(to: str, data: bytes, block: int) -> bytes:
            # DAI has 18 decimals
            return _encode_uint8(18)

        w3 = FakeCurveW3({
            IS_CTOKEN: handle_is_ctoken,
            UNDERLYING: handle_underlying,
            DECIMALS: handle_decimals,
        })

        result = detect_lending_tokens(
            w3,
            POOL_ADDR,
            token_addresses,
            tokens,
            block_identifier=18_000_000,
        )
        assert result.use_lending == (True, False)
        # Precision multiplier for cDAI: 10^(18-18) = 1
        # USDC gets default: 10^(18-6) = 10^12
        assert result.precision_multipliers is not None
        assert result.precision_multipliers[0] == 1  # 10^(18-18) for DAI underlying
        assert result.precision_multipliers[1] == 10**12  # 10^(18-6) for USDC

    def testCTokenWithDifferentUnderlyingDecimals(self):
        """cUSDC has 8 decimals, USDC underlying has 6 → precision = 10^12."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        token_addresses = (CUSDC, DAI)
        tokens = tuple(
            FakeErc20Token(addr, d)
            for addr, d in [
                (CUSDC, 8),
                (DAI, 18),
            ]
        )

        def handle_is_ctoken(to: str, data: bytes, block: int) -> bytes:
            if to == CUSDC:
                return _encode_bool(True)
            return _encode_bool(False)

        def handle_underlying(to: str, data: bytes, block: int) -> bytes:
            return _encode_address(USDC)

        def handle_decimals(to: str, data: bytes, block: int) -> bytes:
            if to == USDC:
                return _encode_uint8(6)
            return _encode_uint8(18)

        w3 = FakeCurveW3({
            IS_CTOKEN: handle_is_ctoken,
            UNDERLYING: handle_underlying,
            DECIMALS: handle_decimals,
        })

        result = detect_lending_tokens(
            w3,
            POOL_ADDR,
            token_addresses,
            tokens,
            block_identifier=18_000_000,
        )
        assert result.use_lending == (True, False)
        assert result.precision_multipliers is not None
        # cUSDC: 10^(18-6) = 10^12
        assert result.precision_multipliers[0] == 10**12

    def testYTokenDetection(self):
        """yToken detection via token() returning a non-zero address."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        token_addresses = (YDAI, USDC)
        tokens = tuple(
            FakeErc20Token(addr, d)
            for addr, d in [
                (YDAI, 18),
                (USDC, 6),
            ]
        )

        def handle_is_ctoken(to: str, data: bytes, block: int) -> bytes:
            # yTokens don't respond to isCToken()
            raise Exception("revert")

        def handle_token(to: str, data: bytes, block: int) -> bytes:
            if to == YDAI:
                return _encode_address(DAI)
            return _encode_address(ZERO_ADDR)

        w3 = FakeCurveW3({
            IS_CTOKEN: handle_is_ctoken,
            TOKEN: handle_token,
        })

        result = detect_lending_tokens(
            w3,
            POOL_ADDR,
            token_addresses,
            tokens,
            block_identifier=18_000_000,
        )
        assert result.use_lending == (True, False)
        # yToken: no override needed (same decimals as underlying)
        # But precision_multipliers should still be returned since we have lending
        assert result.precision_multipliers is not None

    def testYTokenWithZeroAddressNotLending(self):
        """yToken where token() returns zero address is not treated as lending."""
        from tests.curve.detection.fake_w3 import FakeCurveW3

        token_addresses = (DAI, USDC)
        tokens = tuple(
            FakeErc20Token(addr, d)
            for addr, d in [
                (DAI, 18),
                (USDC, 6),
            ]
        )

        def handle_is_ctoken(to: str, data: bytes, block: int) -> bytes:
            raise Exception("revert")

        def handle_token(to: str, data: bytes, block: int) -> bytes:
            # Returns zero address — not a yToken
            return _encode_address(ZERO_ADDR)

        w3 = FakeCurveW3({
            IS_CTOKEN: handle_is_ctoken,
            TOKEN: handle_token,
        })

        result = detect_lending_tokens(
            w3,
            POOL_ADDR,
            token_addresses,
            tokens,
            block_identifier=18_000_000,
        )
        assert result.use_lending == (False, False)
        assert result.precision_multipliers is None
