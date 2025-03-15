__all__ = (
    "MAX_INT16",
    "MAX_INT24",
    "MAX_INT128",
    "MAX_INT256",
    "MAX_UINT8",
    "MAX_UINT128",
    "MAX_UINT160",
    "MAX_UINT256",
    "MIN_INT16",
    "MIN_INT24",
    "MIN_INT128",
    "MIN_INT256",
    "MIN_UINT8",
    "MIN_UINT128",
    "MIN_UINT160",
    "MIN_UINT256",
    "WRAPPED_NATIVE_TOKENS",
    "ZERO_ADDRESS",
)

import typing

from eth_typing import ChainId, ChecksumAddress

from degenbot.cache import get_checksum_address


def _min_uint(_: int) -> int:
    return 0


def _max_uint(bits: int) -> int:
    return typing.cast("int", 2**bits - 1)


def _min_int(bits: int) -> int:
    return typing.cast("int", -(2 ** (bits - 1)))


def _max_int(bits: int) -> int:
    return typing.cast("int", (2 ** (bits - 1)) - 1)


MIN_INT16 = _min_int(16)
MAX_INT16 = _max_int(16)

MIN_INT24 = _min_int(24)
MAX_INT24 = _max_int(24)

MIN_INT128 = _min_int(128)
MAX_INT128 = _max_int(128)

MIN_INT256 = _min_int(256)
MAX_INT256 = _max_int(256)

MIN_UINT8 = _min_uint(8)
MAX_UINT8 = _max_uint(8)

MIN_UINT24 = _min_uint(24)
MAX_UINT24 = _max_uint(24)

MIN_UINT128 = _min_uint(128)
MAX_UINT128 = _max_uint(128)

MIN_UINT160 = _min_uint(160)
MAX_UINT160 = _max_uint(160)

MIN_UINT256 = _min_uint(256)
MAX_UINT256 = _max_uint(256)

ZERO_ADDRESS: ChecksumAddress = get_checksum_address("0x0000000000000000000000000000000000000000")


# Contract addresses for the wrapped native token, keyed by chain ID
WRAPPED_NATIVE_TOKENS: dict[int, ChecksumAddress] = {
    ChainId.ETH: get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
    ChainId.BASE: get_checksum_address("0x4200000000000000000000000000000000000006"),
    ChainId.FTM: get_checksum_address("0x21be370D5312f44cB42ce377BC9b8a0cEF1A4C83"),
    ChainId.ARB1: get_checksum_address("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
    ChainId.AVAX: get_checksum_address("0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7"),
}
