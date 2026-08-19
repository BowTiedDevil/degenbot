"""Checksum address conversion with LRU caching."""

from __future__ import annotations

import functools
from typing import TYPE_CHECKING, cast

from degenbot._ffi import to_checksum_address

if TYPE_CHECKING:
    from eth_typing import HexAddress

    from degenbot._ffi import ChecksummedAddress

_HEX_PREFIX_LENGTH = 2


@functools.lru_cache(maxsize=512)
def get_checksum_address(address: HexAddress | bytes) -> ChecksummedAddress:
    """Return checksum address.

    Returns:
        The computed value.

    """
    if isinstance(address, str) and len(address) >= _HEX_PREFIX_LENGTH and address[:2] == "0X":
        address = cast("HexAddress", "0x" + address[2:])
    return to_checksum_address(address)
