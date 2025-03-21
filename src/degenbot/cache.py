import functools

from eth_typing import ChecksumAddress, HexStr
from eth_utils.address import to_checksum_address


@functools.lru_cache
def get_checksum_address(address: HexStr | bytes) -> ChecksumAddress:
    return to_checksum_address(address)
