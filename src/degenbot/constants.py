from typing import Dict

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address


def _min_uint(_: int) -> int:
    return 0


def _max_uint(bits: int) -> int:
    result: int = 2**bits - 1
    return result


def _min_int(bits: int) -> int:
    result: int = -(2 ** (bits - 1))
    return result


def _max_int(bits: int) -> int:
    result: int = (2 ** (bits - 1)) - 1
    return result


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

MIN_UINT128 = _min_uint(128)
MAX_UINT128 = _max_uint(128)

MIN_UINT160 = _min_uint(160)
MAX_UINT160 = _max_uint(160)

MIN_UINT256 = _min_uint(256)
MAX_UINT256 = _max_uint(256)

ZERO_ADDRESS: ChecksumAddress = to_checksum_address("0x0000000000000000000000000000000000000000")


# Contract addresses for the native blockchain token, keyed by chain ID
WRAPPED_NATIVE_TOKENS: Dict[int, ChecksumAddress] = {
    # Ethereum (WETH)
    1: to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
    # Fantom (WFTM)
    250: to_checksum_address("0x21be370D5312f44cB42ce377BC9b8a0cEF1A4C83"),
    # Arbitrum (AETH)
    42161: to_checksum_address("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
    # Avalanche (WAVAX)
    43114: to_checksum_address("0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7"),
}
