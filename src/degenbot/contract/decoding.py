"""ABI decoding utilities for contract return data."""

from typing import cast

from eth_typing import ChecksumAddress

from degenbot.degenbot_rs import decode_single as rs_decode_single


def decode_address(input_: bytes) -> ChecksumAddress:
    """Get the checksummed address from the given byte stream.

    Routes through the `degenbot-abi` core (`decode_single`) which performs
    EIP-55 checksumming — the same path used by `degenbot.abi_decode_single`.

    Returns:
        The computed value.

    """
    return cast(
        "ChecksumAddress",
        rs_decode_single(abi_type="address", data=input_, checksum=True),
    )
