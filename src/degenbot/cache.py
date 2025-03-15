import functools

from eth_typing import AnyAddress, ChecksumAddress
from eth_utils.address import to_checksum_address


@functools.cache
def get_checksum_address(address: AnyAddress | str | bytes) -> ChecksumAddress:
    return to_checksum_address(address)
