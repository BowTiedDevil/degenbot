import functools

from eth_typing import ChecksumAddress, HexAddress

from degenbot_rs import to_checksum_address


@functools.lru_cache(maxsize=512)
def get_checksum_address(address: HexAddress | bytes) -> ChecksumAddress:
    return to_checksum_address(address)
