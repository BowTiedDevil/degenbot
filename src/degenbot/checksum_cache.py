import functools
from typing import cast

from eth_typing import ChecksumAddress, HexAddress

try:
    from degenbot.degenbot_rs import to_checksum_address
except ImportError:
    to_checksum_address = None  # type: ignore[assignment]


@functools.lru_cache(maxsize=512)
def get_checksum_address(address: HexAddress | bytes) -> ChecksumAddress:
    if isinstance(address, str) and len(address) >= 2 and address[:2] == "0X":
        address = "0x" + address[2:]
    return cast("ChecksumAddress", to_checksum_address(address))
