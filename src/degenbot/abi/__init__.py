"""ABI encode/decode — the stable mirror home for ``_ffi.abi``.

Provides three public functions (``encode``, ``decode``, ``decode_single``)
that delegate to the Rust ``degenbot-abi`` core with a private fallback to
``eth_abi`` for fixed-point types the Rust core does not yet cover.

Environment Variables:
    DEGENBOT_USE_RUST_ABI_DECODER: Control the default backend.
        Set to ``"0"``, ``"false"``, or ``"no"`` to force ``eth_abi``.
        Set to ``"1"``, ``"true"``, or ``"yes"`` (or leave unset) for Rust.
        Default: Rust backend.
"""

from collections.abc import Sequence
from os import environ
from typing import Any

import eth_abi.abi
from eth_abi.exceptions import DecodingError as EthAbiDecodingError
from eth_abi.exceptions import EncodingError as EthAbiEncodingError
from eth_abi.exceptions import ParseError as EthAbiParseError

from degenbot._ffi.abi import decode as rs_decode
from degenbot._ffi.abi import decode_single as rs_decode_single
from degenbot._ffi.abi import encode as rs_encode
from degenbot.exceptions.base import DegenbotError
from degenbot.utils.bytes import HexBytesLike, to_bytes

# Re-exported for consumers that still reference the bytes-like alias.
type BytesLike = HexBytesLike

__all__ = (
    "AbiDecodeError",
    "AbiEncodeError",
    "BytesLike",
    "decode",
    "decode_single",
    "encode",
)


class AbiEncodeError(DegenbotError):
    """Raised when ABI encoding fails."""


class AbiDecodeError(DegenbotError):
    """Raised when ABI decoding fails."""


def _use_rust_backend() -> bool:
    """Check ``DEGENBOT_USE_RUST_ABI_DECODER`` for the default backend.

    Returns:
        True if the Rust backend should be used (default), False for eth_abi.

    """
    env_value = environ.get("DEGENBOT_USE_RUST_ABI_DECODER", "true").lower()
    return env_value not in {"0", "false", "no", "off"}


def encode(types: Sequence[str], args: Sequence[Any]) -> bytes:
    """Encode values into ABI-encoded bytes.

    Delegates to the Rust core; falls back to ``eth_abi`` for types the
    Rust core does not yet support (e.g., fixed-point).

    Args:
        types: ABI type strings (e.g., ``["uint256", "address"]``).
        args: Values to encode.

    Returns:
        ABI-encoded bytes.

    Raises:
        AbiEncodeError: If encoding fails in both backends.

    """
    if _use_rust_backend():
        try:
            return rs_encode(types=list(types), values=list(args))
        except ValueError as e:
            raise AbiEncodeError(message=f"ABI encoding failed: {e}") from e
        except NotImplementedError:
            pass  # fall back to eth_abi for unsupported types

    try:
        return eth_abi.abi.encode(types=list(types), args=list(args))
    except (EthAbiEncodingError, EthAbiParseError) as e:
        raise AbiEncodeError(message=f"ABI encoding failed: {e}") from e


def decode(types: Sequence[str], data: BytesLike) -> tuple[Any, ...]:
    """Decode ABI-encoded bytes into Python values.

    Delegates to the Rust core (with EIP-55 checksummed addresses); falls
    back to ``eth_abi`` for types the Rust core does not yet support
    (e.g., fixed-point).

    Args:
        types: ABI type strings (e.g., ``["uint256", "address"]``).
        data: ABI-encoded bytes or ``HexBytes``.

    Returns:
        Tuple of decoded values.

    Raises:
        AbiDecodeError: If decoding fails in both backends.

    """
    if _use_rust_backend():
        data_bytes = to_bytes(data)
        try:
            result = rs_decode(types=list(types), data=data_bytes, checksum=True)
        except ValueError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        except NotImplementedError:
            pass  # fall back to eth_abi for unsupported types
        else:
            return tuple(result)

    data_bytes = to_bytes(data)
    try:
        return eth_abi.abi.decode(types=list(types), data=data_bytes)
    except EthAbiDecodingError as e:
        raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e


def decode_single(abi_type: str, data: BytesLike) -> Any:  # noqa: ANN401 - return depends on abi_type
    """Decode a single ABI value.

    Convenience wrapper around :func:`decode` for single-value decodes;
    delegates to the Rust core with EIP-55 checksumming for addresses.

    Args:
        abi_type: ABI type string (e.g., ``"uint256"``).
        data: ABI-encoded bytes or ``HexBytes``.

    Returns:
        The decoded value.

    Raises:
        AbiDecodeError: If decoding fails in both backends.

    """
    if _use_rust_backend():
        data_bytes = to_bytes(data)
        try:
            return rs_decode_single(abi_type=abi_type, data=data_bytes, checksum=True)
        except ValueError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        except NotImplementedError:
            pass  # fall back to eth_abi for unsupported types

    data_bytes = to_bytes(data)
    try:
        (result,) = eth_abi.abi.decode(types=[abi_type], data=data_bytes)
    except EthAbiDecodingError as e:
        raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
    return result
