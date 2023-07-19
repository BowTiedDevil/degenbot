from typing import Dict

import web3
from eth_typing import ChecksumAddress

MIN_INT16: int = -(2 ** (16 - 1))
MAX_INT16: int = (2 ** (16 - 1)) - 1

MIN_INT128: int = -(2 ** (128 - 1))
MAX_INT128: int = (2 ** (128 - 1)) - 1

MIN_INT256: int = -(2 ** (256 - 1))
MAX_INT256: int = (2 ** (256 - 1)) - 1

MIN_UINT8: int = 0
MAX_UINT8: int = 2**8 - 1

MIN_UINT128: int = 0
MAX_UINT128: int = 2**128 - 1

MIN_UINT160: int = 0
MAX_UINT160: int = 2**160 - 1

MIN_UINT256: int = 0
MAX_UINT256: int = 2**256 - 1

ZERO_ADDRESS: ChecksumAddress = web3.Web3.toChecksumAddress(
    "0x0000000000000000000000000000000000000000"
)


# Contract addresses for the native blockchain token, keyed by chain ID
WRAPPED_NATIVE_TOKENS: Dict[int, ChecksumAddress] = {
    # Ethereum (ETH)
    1: web3.Web3.toChecksumAddress(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    ),
    # Fantom (WFTM)
    250: web3.Web3.toChecksumAddress(
        "0x21be370D5312f44cB42ce377BC9b8a0cEF1A4C83"
    ),
    # Arbitrum (AETH)
    42161: web3.Web3.toChecksumAddress(
        "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
    ),
    # Avalanche (WAVAX)
    43114: web3.Web3.toChecksumAddress(
        "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7"
    ),
}
