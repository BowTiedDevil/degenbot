"""ABI encode/decode — the stable mirror home for ``_ffi.abi``.

Provides public functions (``encode``, ``encode_packed``, ``encode_single``,
``decode``, ``decode_single``) that delegate to the Rust ``degenbot-abi`` core. Addresses are EIP-55
checksummed on decode.
"""

from collections.abc import Sequence
from typing import Any

from degenbot._ffi.abi import decode as rs_decode
from degenbot._ffi.abi import decode_single as rs_decode_single
from degenbot._ffi.abi import encode as rs_encode
from degenbot._ffi.abi import encode_packed as rs_encode_packed
from degenbot._ffi.abi import encode_single as rs_encode_single
from degenbot.exceptions.base import DegenbotError
from degenbot.utils.bytes import to_bytes

# Re-exported for consumers that still reference the bytes-like alias.
type BytesLike = bytes

__all__ = (
    "AbiDecodeError",
    "AbiEncodeError",
    "BytesLike",
    "canonical_type",
    "decode",
    "decode_single",
    "encode",
    "encode_packed",
    "encode_single",
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


def encode_single(abi_type: str, value: Any) -> bytes:  # ruff:ignore[any-type] - value depends on abi_type
    """Encode a single value into ABI-encoded bytes.

    Args:
        abi_type: ABI type string (e.g., ``"uint256"``).
        value: The value to encode.

    Returns:
        The ABI-encoded bytes.

    Raises:
        AbiEncodeError: If encoding fails.

    """
    try:
        return rs_encode_single(abi_type=abi_type, value=value)
    except (ValueError, NotImplementedError) as e:
        raise AbiEncodeError(message=f"ABI encoding failed: {e}") from e


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


def canonical_type(abi_input: dict[str, Any]) -> str:
    """Return the canonical Solidity type text of one ABI parameter.

    ABI JSON spells tuple types bare (``tuple``, ``tuple[2]``) with the members
    under ``components``; selectors and the encoder need the expanded canonical
    form (``((address,uint256),bytes)[2]``). Mirrors the eth-abi
    ``collapse_if_tuple`` idiom: components collapse to their parenthesized
    canonical text and the array suffix is appended unchanged.

    Args:
        abi_input: One ABI parameter dict (``type``, plus ``components`` for
            tuple types).

    Returns:
        The canonical type string, e.g. ``"((address,uint256),bytes)[2]"``.

    """
    typ = abi_input["type"]
    if not typ.startswith("tuple"):
        return typ
    members = ",".join(canonical_type(component) for component in abi_input.get("components") or [])
    return f"({members}){typ[len('tuple') :]}"
