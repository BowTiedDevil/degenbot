from eth_typing import ChecksumAddress
from web3 import Web3

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

ZERO_ADDRESS: ChecksumAddress = Web3.toChecksumAddress(
    "0x0000000000000000000000000000000000000000"
)
