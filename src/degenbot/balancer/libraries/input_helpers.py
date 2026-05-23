"""Balancer V2 query helpers that format and validate RPC inputs."""
from eth_typing import ChecksumAddress

from degenbot.exceptions.pool import EVMRevertError


def ensure_input_length_match(*nums: int) -> None:
    """Perform ensure input length match.

    Raises:
        EVMRevertError: See function documentation.

    """
    if len(set(nums)) != 1:
        raise EVMRevertError(error="INPUT_LENGTH_MISMATCH")


def ensure_array_is_sorted(array: list[ChecksumAddress]) -> None:
    """Perform ensure array is sorted.

    Raises:
        EVMRevertError: See function documentation.

    """
    if len(array) <= 1:
        return

    if sorted(array) != array:
        raise EVMRevertError(error="UNSORTED_ARRAY")
