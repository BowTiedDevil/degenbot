from web3.exceptions import Web3Exception
from tests.curve.detection.fake_provider import make_fake_curve_provider

"""Tests for Curve pool metapool detection."""

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.metapool_detector import detect_metapool
from degenbot.provider.call_helpers import encode_function_calldata

POOL_ADDR = get_checksum_address("0xbEbc44782C7DB0a1A60Cb6fe97d0b483032FF1C7")
TRIPOOL_ADDR = get_checksum_address("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
THREE_CRV_LP = get_checksum_address("0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490")
DAI = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
USDC = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
USDT = get_checksum_address("0xdAC17F958D2ee523a2206206994597C13D831ec7")
ZERO_ADDR = "0x0000000000000000000000000000000000000000"

CURVE_V1_REGISTRY_ADDRESS = get_checksum_address("0x90E00ACe148ca3b23Ac1bC8C240C2a7Dd9c2d7f5")
CURVE_V1_FACTORY_ADDRESS = get_checksum_address("0x127db66E7F0b16470Bec194d0f496F9Fa065d0A9")

# Selectors
IS_META = encode_function_calldata("is_meta(address)", [POOL_ADDR])[:4]
BASE_POOL = encode_function_calldata("base_pool()", [])[:4]
GET_BASE_POOL = encode_function_calldata("get_base_pool(address)", [POOL_ADDR])[:4]
GET_UNDERLYING_COINS = encode_function_calldata("get_underlying_coins(address)", [POOL_ADDR])[:4]


def _encode_uint256(val: int) -> bytes:
    return eth_abi.abi.encode(["uint256"], [val])


def _encode_address(addr: str) -> bytes:
    return eth_abi.abi.encode(["address"], [addr])


def _encode_bool(val: bool) -> bytes:
    return eth_abi.abi.encode(["bool"], [val])


def _encode_address_array8(addrs: list[str]) -> bytes:
    """Encode address[8] with zero-padded tail."""
    padded = addrs + [ZERO_ADDR] * (8 - len(addrs))
    return eth_abi.abi.encode(["address[8]"], [padded])


class TestDetectMetapool:
    def testNotMetapool(self):
        """Pool where is_meta() returns False for all registries."""

        def handle_is_meta(to: str, data: bytes, block: int) -> bytes:
            return _encode_bool(False)

        provider = make_fake_curve_provider({
            IS_META: handle_is_meta,
        })

        result = detect_metapool(
            provider,
            POOL_ADDR,
            (DAI, USDC),
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS,),
            block_identifier=18_000_000,
        )
        assert not result.is_meta
        assert result.base_pool_address is None
        assert result.tokens_underlying is None

    def testMetapoolDetectedFromRegistry(self):
        """Metapool detected via is_meta() on the first registry."""

        base_pool_addr = TRIPOOL_ADDR
        underlying_coins = [DAI, USDC, USDT]

        def handle_call(to: str, data: bytes, block: int) -> bytes:
            selector = data[:4]
            if selector == IS_META:
                return _encode_bool(True)
            if selector == BASE_POOL:
                return _encode_address(base_pool_addr)
            if selector == GET_UNDERLYING_COINS:
                return _encode_address_array8(underlying_coins)
            msg = f"Unexpected selector: {selector.hex()}"
            raise Web3Exception(msg)

        provider = make_fake_curve_provider({
            IS_META: handle_call,
            BASE_POOL: handle_call,
            GET_UNDERLYING_COINS: handle_call,
        })

        result = detect_metapool(
            provider,
            POOL_ADDR,
            (DAI, THREE_CRV_LP),
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS,),
            block_identifier=18_000_000,
        )
        assert result.is_meta
        assert result.base_pool_address == TRIPOOL_ADDR
        assert result.tokens_underlying is not None
        assert len(result.tokens_underlying) == 3

    def testMetapoolFallbackToGetBasePool(self):
        """If base_pool() reverts, falls back to get_base_pool() on the registry."""

        base_pool_addr = TRIPOOL_ADDR
        underlying_coins = [DAI, USDC, USDT]

        def handle_call(to: str, data: bytes, block: int) -> bytes:
            selector = data[:4]
            if selector == IS_META:
                return _encode_bool(True)
            if selector == BASE_POOL:
                msg = "base_pool() not supported"
                raise Web3Exception(msg)
            if selector == GET_BASE_POOL:
                return _encode_address(base_pool_addr)
            if selector == GET_UNDERLYING_COINS:
                return _encode_address_array8(underlying_coins)
            msg = f"Unexpected selector: {selector.hex()}"
            raise Web3Exception(msg)

        provider = make_fake_curve_provider({
            IS_META: handle_call,
            BASE_POOL: handle_call,
            GET_BASE_POOL: handle_call,
            GET_UNDERLYING_COINS: handle_call,
        })

        result = detect_metapool(
            provider,
            POOL_ADDR,
            (DAI, THREE_CRV_LP),
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS,),
            block_identifier=18_000_000,
        )
        assert result.is_meta
        assert result.base_pool_address == TRIPOOL_ADDR

    def testMetapoolFallbackTo3CrvHardcode(self):
        """If base_pool() and get_base_pool() both revert but second token is 3Crv LP."""

        underlying_coins = [DAI, USDC, USDT]

        def handle_call(to: str, data: bytes, block: int) -> bytes:
            selector = data[:4]
            if selector == IS_META:
                return _encode_bool(True)
            if selector == BASE_POOL:
                raise Web3Exception("revert")
            if selector == GET_BASE_POOL:
                raise Web3Exception("revert")
            if selector == GET_UNDERLYING_COINS:
                return _encode_address_array8(underlying_coins)
            msg = f"Unexpected selector: {selector.hex()}"
            raise Web3Exception(msg)

        provider = make_fake_curve_provider({
            IS_META: handle_call,
            BASE_POOL: handle_call,
            GET_BASE_POOL: handle_call,
            GET_UNDERLYING_COINS: handle_call,
        })

        result = detect_metapool(
            provider,
            POOL_ADDR,
            (DAI, THREE_CRV_LP),
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS,),
            block_identifier=18_000_000,
        )
        assert result.is_meta
        # Falls back to tripool hardcode
        assert result.base_pool_address == TRIPOOL_ADDR

    def testBothRegistriesTried(self):
        """If first registry reverts on is_meta(), second registry is tried."""

        first_registry_tried = {"value": False}

        def handle_call(to: str, data: bytes, block: int) -> bytes:
            selector = data[:4]
            if selector == IS_META:
                if to == CURVE_V1_REGISTRY_ADDRESS:
                    first_registry_tried["value"] = True
                    raise Web3Exception("revert")
                # Second registry says yes
                return _encode_bool(True)
            if selector == BASE_POOL:
                return _encode_address(TRIPOOL_ADDR)
            if selector == GET_UNDERLYING_COINS:
                return _encode_address_array8([DAI, USDC, USDT])
            msg = f"Unexpected selector: {selector.hex()}"
            raise Web3Exception(msg)

        provider = make_fake_curve_provider({
            IS_META: handle_call,
            BASE_POOL: handle_call,
            GET_UNDERLYING_COINS: handle_call,
        })

        result = detect_metapool(
            provider,
            POOL_ADDR,
            (DAI, THREE_CRV_LP),
            registry_addresses=(CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS),
            block_identifier=18_000_000,
        )
        assert result.is_meta
        assert first_registry_tried["value"]
