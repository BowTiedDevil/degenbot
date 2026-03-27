from collections.abc import Sequence
from typing import Any

from eth_typing import ChecksumAddress

__all__ = [
    "decode",
    "decode_single",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "to_checksum_address",
]

def decode(
    types: Sequence[str],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> list[Any]:
    """
    Decode ABI-encoded data for multiple types.

    Args:
        types: List of ABI type strings (e.g., ["uint256", "address", "bytes"])
        data: Raw ABI-encoded bytes
        strict: If True (default), performs strict validation
        checksum: If True (default), returns EIP-55 checksummed addresses

    Returns:
        A list of decoded Python values

    Raises:
        ValueError: If type strings are invalid, data is malformed, or decoding fails
        NotImplementedError: If strict=False or if fixed-point types are used
    """
    ...

def decode_single(
    abi_type: str,
    data: bytes,
    *,
    strict: bool = True,
    checksum: bool = True,
) -> Any:
    """
    Decode a single ABI value.

    Convenience function for decoding a single value without wrapping the type
    in a list.

    Args:
        abi_type: ABI type string (e.g., "uint256", "address")
        data: Raw ABI-encoded bytes to decode
        strict: If True (default), performs strict validation
        checksum: If True (default), returns EIP-55 checksummed addresses

    Returns:
        The decoded Python value

    Raises:
        ValueError: If the type string is invalid, data is malformed, or decoding fails
        NotImplementedError: If strict=False or if fixed-point types are used
    """
    ...

def get_sqrt_ratio_at_tick(tick: int) -> int:
    """
    Convert a tick value to its corresponding sqrt price (X96 format).

    Args:
        tick: The tick value in range [-887272, 887272]

    Returns:
        The sqrt price X96 value as an integer

    Raises:
        ValueError: If the tick value is outside the valid range
    """
    ...

def get_tick_at_sqrt_ratio(sqrt_price_x96: int | bytes) -> int:
    """
    Convert a sqrt price (X96 format) to its corresponding tick value.

    Args:
        sqrt_price_x96: The sqrt price X96 value as an integer or bytes (max 20 bytes)

    Returns:
        The tick value corresponding to the given sqrt price

    Raises:
        ValueError: If the sqrt price is too large (exceeds 20 bytes) or
            outside the valid [MIN_SQRT_RATIO, MAX_SQRT_RATIO) range
        TypeError: If the input is not an int or bytes
    """
    ...

def to_checksum_address(address: str | bytes) -> ChecksumAddress:
    """
    Generate an EIP-55 checksummed address from the input.

    Accepts either a hex string or a 20-byte sequence and returns
    a checksummed Ethereum address.

    Args:
        address: A hex string (with or without '0x' prefix) or 20-byte
            sequence representing an address

    Returns:
        A checksummed Ethereum address string with proper uppercase/lowercase

    Raises:
        ValueError: If the string is not a valid hex address, or if bytes
            are not exactly 20 bytes long
        TypeError: If the input is not a string or bytes

    Example:
        >>> to_checksum_address("0x66f9664f97f2b50f62d13ea064982f936de76657")
        '0x66F9664f97f2B50f62d13Ea064982f936de76657'
    """
    ...
