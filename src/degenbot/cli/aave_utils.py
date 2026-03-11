import eth_abi.abi
from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address


def decode_address(input_: bytes) -> ChecksumAddress:
    """
    Get the checksummed address from the given byte stream.
    """

    (address,) = eth_abi.abi.decode(types=["address"], data=input_)
    return get_checksum_address(address)
