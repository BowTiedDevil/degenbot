import functools

from cchecksum import to_checksum_address
from eth_typing import ChecksumAddress, HexAddress


@functools.lru_cache(maxsize=512)
def get_checksum_address(address: HexAddress | bytes) -> ChecksumAddress:
    return to_checksum_address(address)
