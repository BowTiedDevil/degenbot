"""
High-performance ABI decoder using Rust backend.

This module provides a faster alternative to eth_abi.abi.decode using
a Rust implementation via PyO3 bindings.

Example:
    >>> from degenbot.abi_decoder import decode
    >>> data = bytes.fromhex(
    ...     "0000000000000000000000000000000000000000000000000000000000000064"
    ...     "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
    ... )
    >>> decode(["uint256", "address"], data)
    [100, '0xd3cda913deb6f67967b99d67acdfa1712c293601']
"""

from collections.abc import Sequence
from typing import Any

from degenbot_rs import decode as _decode
from degenbot_rs import decode_single as _decode_single

__all__ = ["decode", "decode_single"]


def decode(
    types: Sequence[str],
    data: bytes,
    *,
    strict: bool = True,
    checksum: bool = True,
) -> list[Any]:
    """
    Decode ABI-encoded data.

    This function decodes ABI-encoded bytes according to the provided type strings.
    It supports all standard Solidity types including static and dynamic types,
    arrays, and tuples.

    Args:
        types: List of ABI type strings (e.g., ["uint256", "address", "bytes"])
        data: Raw ABI-encoded bytes to decode
        strict: If True (default), performs strict validation. If False, uses
            lenient validation (not yet implemented - will raise NotImplementedError)
        checksum: If True (default), returns checksummed addresses. If False,
            returns lowercase hex addresses.

    Returns:
        A list of decoded Python values. Types are converted as follows:
        - ``uint/int`` → Python ``int``
        - ``address`` → Python ``str`` (checksummed by default)
        - ``bool`` → Python ``bool``
        - ``bytes`` → Python ``bytes``
        - ``string`` → Python ``str``
        - Arrays (e.g., ``uint256[]``) → Python ``list``
        - Tuples (e.g., ``(uint256,address)``) → Python ``list``

    Raises:
        ValueError: If type strings are invalid, data is malformed, or decoding fails.
        NotImplementedError: If strict=False (non-strict mode not yet implemented)
            or if fixed-point types are used.

    Example:
        >>> from degenbot.abi_decoder import decode
        >>> # Decode a uint256 and address
        >>> data = bytes.fromhex(
        ...     "0000000000000000000000000000000000000000000000000000000000000064"
        ...     "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        ... )
        >>> decode(["uint256", "address"], data)
        [100, '0xd3cda913deb6f67967b99d67acdfa1712c293601']

        >>> # Decode dynamic types
        >>> data = bytes.fromhex(
        ...     "0000000000000000000000000000000000000000000000000000000000000020"
        ...     "0000000000000000000000000000000000000000000000000000000000000004"
        ...     "7465737400000000000000000000000000000000000000000000000000000000"
        ... )
        >>> decode(["string"], data)
        ['test']

        >>> # Decode arrays
        >>> data = bytes.fromhex(
        ...     "0000000000000000000000000000000000000000000000000000000000000020"
        ...     "0000000000000000000000000000000000000000000000000000000000000002"
        ...     "0000000000000000000000000000000000000000000000000000000000000001"
        ...     "0000000000000000000000000000000000000000000000000000000000000002"
        ... )
        >>> decode(["uint256[]"], data)
        [[1, 2]]

        >>> # Decode with non-checksummed addresses
        >>> decode(["address"], data, checksum=False)
        ['0xd3cda913deb6f67967b99d67acdfa1712c293601']

    Note:
        This implementation uses the Rust ``alloy-sol-types`` crate for
        high-performance decoding. Type aliases are supported:
        - ``"uint"`` is equivalent to ``"uint256"``
        - ``"int"`` is equivalent to ``"int256"``
        - ``"function"`` is equivalent to ``"bytes24"``
    """
    return _decode(list(types), data, strict, checksum)


def decode_single(
    type_: str,
    data: bytes,
    *,
    strict: bool = True,
    checksum: bool = True,
) -> Any:
    """
    Decode a single ABI value.

    Convenience function for decoding a single value without wrapping the type
    in a list. This is useful when you know you're decoding exactly one value.

    Args:
        type_: ABI type string (e.g., "uint256", "address", "bytes")
        data: Raw ABI-encoded bytes to decode
        strict: If True (default), performs strict validation. If False, uses
            lenient validation (not yet implemented - will raise NotImplementedError)
        checksum: If True (default), returns checksummed addresses. If False,
            returns lowercase hex addresses.

    Returns:
        The decoded Python value. See ``decode()`` for type mappings.

    Raises:
        ValueError: If the type string is invalid, data is malformed, or decoding fails.
        NotImplementedError: If strict=False (non-strict mode not yet implemented)
            or if fixed-point types are used.

    Example:
        >>> from degenbot.abi_decoder import decode_single
        >>> # Decode a single uint256
        >>> data = bytes.fromhex(
        ...     "0000000000000000000000000000000000000000000000000000000000000064"
        ... )
        >>> decode_single("uint256", data)
        100

        >>> # Decode a single address
        >>> data = bytes.fromhex(
        ...     "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        ... )
        >>> decode_single("address", data)
        '0xd3cda913deb6f67967b99d67acdfa1712c293601'

        >>> # Decode with non-checksummed address
        >>> decode_single("address", data, checksum=False)
        '0xd3cda913deb6f67967b99d67acdfa1712c293601'

        >>> # Decode a string
        >>> data = bytes.fromhex(
        ...     "0000000000000000000000000000000000000000000000000000000000000020"
        ...     "0000000000000000000000000000000000000000000000000000000000000004"
        ...     "7465737400000000000000000000000000000000000000000000000000000000"
        ... )
        >>> decode_single("string", data)
        'test'

    Note:
        This function is equivalent to calling ``decode([type_], data)[0]``,
        but is provided for convenience and clarity when working with single values.
    """
    return _decode_single(
        ty=type_,
        data=data,
        strict=strict,
        checksum=checksum,
    )
