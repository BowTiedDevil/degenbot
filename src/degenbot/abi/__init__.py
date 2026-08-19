"""ABI encode/decode — the stable mirror home for ``_ffi.abi``.

Provides three public functions (``encode``, ``decode``, ``decode_single``)
that delegate to the Rust ``degenbot-abi`` core. Addresses are EIP-55
checksummed on decode.
"""

from collections.abc import Sequence
from typing import Any

from degenbot._ffi.abi import decode as rs_decode
from degenbot._ffi.abi import decode_single as rs_decode_single
from degenbot._ffi.abi import encode as rs_encode
from degenbot._ffi.abi import encode_packed as rs_encode_packed
from degenbot.exceptions.base import DegenbotError
from degenbot.utils.bytes import to_bytes

# Re-exported for consumers that still reference the bytes-like alias.
type BytesLike = bytes

__all__ = (
    "AbiDecodeError",
    "AbiEncodeError",
    "BytesLike",
    "decode",
    "decode_single",
    "encode",
    "encode_packed",
)


class AbiEncodeError(DegenbotError):
    """Raised when ABI encoding fails."""


class AbiDecodeError(DegenbotError):
    """Raised when ABI decoding fails."""


def encode(types: Sequence[str], args: Sequence[Any]) -> bytes:
    """Encode values into ABI-encoded bytes.

    Args:
        types: ABI type strings (e.g., ``["uint256", "address"]``).
        args: Values to encode.

    Returns:
        ABI-encoded bytes.

    Raises:
        AbiEncodeError: If encoding fails.

    """
    try:
        return rs_encode(types=list(types), values=list(args))
    except (ValueError, NotImplementedError) as e:
        raise AbiEncodeError(message=f"ABI encoding failed: {e}") from e


def encode_packed(types: Sequence[str], args: Sequence[Any]) -> bytes:
    """Pack-encode values into Solidity ``abi.encodePacked`` bytes.

    Each value is encoded tightly with no 32-byte word padding and no
    length prefix for dynamic types — the values are simply concatenated
    in their packed forms. Tuples are packed element-by-element.

    Args:
        types: ABI type strings (e.g., ``["address", "address", "bool"]``).
        args: Values to encode.

    Returns:
        The packed-encoded bytes.

    Raises:
        AbiEncodeError: If encoding fails.

    """
    try:
        return rs_encode_packed(types=list(types), values=list(args))
    except (ValueError, NotImplementedError) as e:
        raise AbiEncodeError(message=f"ABI packed encoding failed: {e}") from e


def decode(types: Sequence[str], data: BytesLike) -> tuple[Any, ...]:
    """Decode ABI-encoded bytes into Python values.

    Args:
        types: ABI type strings (e.g., ``["uint256", "address"]``).
        data: ABI-encoded bytes.

    Returns:
        Tuple of decoded values.

    Raises:
        AbiDecodeError: If decoding fails.

    """
    data_bytes = to_bytes(data)
    try:
        result = rs_decode(types=list(types), data=data_bytes, checksum=True)
    except (ValueError, NotImplementedError) as e:
        raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
    return tuple(result)


def decode_single(abi_type: str, data: BytesLike) -> Any:  # ruff:ignore[any-type] - return depends on abi_type
    """Decode a single ABI value.

    Args:
        abi_type: ABI type string (e.g., ``"uint256"``).
        data: ABI-encoded bytes.

    Returns:
        The decoded value.

    Raises:
        AbiDecodeError: If decoding fails.

    """
    data_bytes = to_bytes(data)
    try:
        return rs_decode_single(abi_type=abi_type, data=data_bytes, checksum=True)
    except (ValueError, NotImplementedError) as e:
        raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
