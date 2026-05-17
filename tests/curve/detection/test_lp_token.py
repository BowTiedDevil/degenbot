from web3.exceptions import Web3Exception

from tests.curve.detection.fake_provider import make_fake_curve_provider

"""Tests for Curve pool LP token discovery."""

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.lp_token import find_lp_token
from degenbot.provider.call_helpers import encode_function_calldata

POOL_ADDR = get_checksum_address("0xbEbc44782C7DB0a1A60Cb6fe97d0b483032FF1C7")
THREE_CRV_LP = get_checksum_address("0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490")
ZERO_ADDR = "0x0000000000000000000000000000000000000000"

CURVE_V1_REGISTRY_ADDRESS = get_checksum_address("0x90E00ACe148ca3b23Ac1bC8C240C2a7Dd9c2d7f5")
CURVE_V1_FACTORY_ADDRESS = get_checksum_address("0x127db66E7F0b16470Bec194d0f496F9Fa065d0A9")

# Selector for get_lp_token(address)
GET_LP_TOKEN = encode_function_calldata("get_lp_token(address)", [POOL_ADDR])[:4]


def _encode_address(addr: str) -> bytes:
    return eth_abi.abi.encode(["address"], [addr])


class TestFindLpToken:
    def testLpTokenFromFirstRegistry(self):
        """LP token found via the first registry."""

        def handle_get_lp_token(to: str, data: bytes, block: int) -> bytes:
            return _encode_address(THREE_CRV_LP)

        provider = make_fake_curve_provider({
            GET_LP_TOKEN: handle_get_lp_token,
        })

        result = find_lp_token(
            provider,
            POOL_ADDR,
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS),
            block_identifier=18_000_000,
        )
        assert result == THREE_CRV_LP

    def testLpTokenFromSecondRegistry(self):
        """First registry returns zero address, second registry finds the LP token."""

        call_count = {"count": 0}

        def handle_get_lp_token(to: str, data: bytes, block: int) -> bytes:
            call_count["count"] += 1
            if to == CURVE_V1_REGISTRY_ADDRESS:
                return _encode_address(ZERO_ADDR)
            return _encode_address(THREE_CRV_LP)

        provider = make_fake_curve_provider({
            GET_LP_TOKEN: handle_get_lp_token,
        })

        result = find_lp_token(
            provider,
            POOL_ADDR,
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS),
            block_identifier=18_000_000,
        )
        assert result == THREE_CRV_LP

    def testNoLpTokenFound(self):
        """Both registries revert or return zero — no LP token found."""

        provider = make_fake_curve_provider({})  # All calls revert

        result = find_lp_token(
            provider,
            POOL_ADDR,
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS),
            block_identifier=18_000_000,
        )
        assert result is None

    def testFirstRegistryReverts(self):
        """First registry reverts, second registry finds LP token."""

        def handle_get_lp_token(to: str, data: bytes, block: int) -> bytes:
            if to == CURVE_V1_REGISTRY_ADDRESS:
                raise Web3Exception("revert")
            return _encode_address(THREE_CRV_LP)

        provider = make_fake_curve_provider({
            GET_LP_TOKEN: handle_get_lp_token,
        })

        result = find_lp_token(
            provider,
            POOL_ADDR,
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS),
            block_identifier=18_000_000,
        )
        assert result == THREE_CRV_LP
