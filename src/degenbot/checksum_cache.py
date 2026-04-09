import functools
from typing import cast

from eth_typing import ChecksumAddress, HexAddress

from degenbot.degenbot_rs import to_checksum_address


@functools.lru_cache(maxsize=512)
def get_checksum_address(address: HexAddress | bytes) -> ChecksumAddress:
    return cast("ChecksumAddress", to_checksum_address(address))
