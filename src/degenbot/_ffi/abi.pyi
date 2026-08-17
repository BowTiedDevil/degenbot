from typing import Any, Literal, overload

def decode(
    types: list[str],
    data: bytes,
    checksum: bool = True,
) -> list[str | bool | int | bytes | list[Any]]:
    """Decode ABI-encoded data for multiple types.

    Args:
        types: List of ABI type strings
        data: Raw ABI-encoded bytes
        checksum: If True (default), returns checksummed addresses

    Returns:
        A list of decoded Python values

    Raises:
        ValueError: If data is invalid or insufficient
        NotImplementedError: For unsupported types (e.g., fixed-point)

    """

@overload
def decode_single(
    abi_type: Literal["address"],
    data: bytes,
    checksum: bool = True,
) -> str: ...
@overload
def decode_single(
    abi_type: Literal["bool"],
    data: bytes,
    checksum: bool = True,
) -> bool: ...
@overload
def decode_single(
    abi_type: Literal["uint8", "uint16", "uint32", "uint64", "uint128", "uint256"],
    data: bytes,
    checksum: bool = True,
) -> int: ...
@overload
def decode_single(
    abi_type: Literal["int8", "int16", "int32", "int64", "int128", "int256"],
    data: bytes,
    checksum: bool = True,
) -> int: ...
@overload
def decode_single(
    abi_type: Literal["bytes"],
    data: bytes,
    checksum: bool = True,
) -> bytes: ...
@overload
def decode_single(
    abi_type: Literal["string"],
    data: bytes,
    checksum: bool = True,
) -> str: ...
@overload
def decode_single(
    abi_type: str,
    data: bytes,
    checksum: bool = True,
) -> str | bool | int | bytes: ...
def encode(
    types: list[str],
    values: list[str | bool | int | bytes],
) -> bytes:
    """Encode multiple ABI values.

    Args:
        types: List of ABI type strings
        values: List of Python values to encode

    Returns:
        The ABI-encoded bytes.

    Raises:
        ValueError: If values cannot be encoded or type/value counts differ

    """

def encode_single(abi_type: str, value: str | bool | int | bytes) -> bytes:
    """Encode a single ABI value.

    Args:
        abi_type: ABI type string (e.g., "uint256", "address", "bytes")
        value: Python value to encode

    Returns:
        The ABI-encoded bytes.

    Raises:
        ValueError: If the value cannot be encoded for the given type

    """

def encode_packed(
    types: list[str],
    values: list[str | bool | int | bytes],
) -> bytes:
    """Pack-encode multiple ABI values (Solidity `abi.encodePacked`).

    Each value is encoded tightly with no 32-byte word padding and no
    length prefix for dynamic types — the values are simply concatenated
    in their packed forms. Tuples are packed element-by-element.

    Args:
        types: List of ABI type strings (e.g., ["address", "address", "bool"])
        values: List of Python values to encode

    Returns:
        The packed-encoded bytes.

    Raises:
        ValueError: If values cannot be encoded or type/value counts differ
        NotImplementedError: For unsupported types (e.g., fixed-point)

    """

__all__ = ["decode", "decode_single", "encode", "encode_packed", "encode_single"]
