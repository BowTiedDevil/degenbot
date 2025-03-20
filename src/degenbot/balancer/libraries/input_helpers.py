from eth_typing import ChecksumAddress

from degenbot.exceptions import EVMRevertError


def ensure_input_length_match(*nums: int) -> None:
    if len(set(nums)) != 1:
        raise EVMRevertError(error="INPUT_LENGTH_MISMATCH")


def ensure_array_is_sorted(array: list[ChecksumAddress]) -> None:
    if len(array) <= 1:
        return

    if sorted(array) != array:
        raise EVMRevertError(error="UNSORTED_ARRAY")
