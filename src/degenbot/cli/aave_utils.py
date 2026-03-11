import eth_abi
from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address


def _decode_address(input_: bytes) -> ChecksumAddress:
    """
    Get the checksummed address from the given byte stream.
    """

    (address,) = eth_abi.abi.decode(types=["address"], data=input_)
    return get_checksum_address(address)


def _decode_uint_values(
    event: LogReceipt,
    num_values: int | None = None,
) -> tuple[int, ...]:
    """
    Decode uint256 values from event data.
    """

    if num_values is None:
        num_values = len(event["data"]) // 32
    types = ["uint256"] * num_values
    return eth_abi.abi.decode(types=types, data=event["data"])
