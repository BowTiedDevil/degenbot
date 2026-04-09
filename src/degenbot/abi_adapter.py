"""
ABI encoding/decoding adapter.

Provides a unified interface for ABI operations that can delegate to either:
- The Rust implementation (`_rs` module) for high-performance decoding
- The Python `eth_abi` library for full ABI support including encoding

The Rust decoder is faster but only supports decoding. The `eth_abi` library
supports both encoding and decoding with broader type coverage.

Environment Variables:
    DEGENBOT_USE_RUST_ABI_DECODER: Control the default backend for decoding.
        Set to "0", "false", or "no" to use eth_abi backend.
        Set to "1", "true", or "yes" (or leave unset) to use Rust backend.
        Default: Rust backend.
"""

from collections.abc import Sequence
from enum import Enum, auto
from os import environ
from typing import Any, Final

import eth_abi.abi
from eth_abi.exceptions import DecodingError as EthAbiDecodingError
from eth_abi.exceptions import EncodingError as EthAbiEncodingError
from eth_abi.exceptions import ParseError as EthAbiParseError

from degenbot.degenbot_rs import decode as rs_decode
from degenbot.degenbot_rs import decode_single as rs_decode_single
from degenbot.exceptions.base import DegenbotError
from degenbot.utils.bytes import HexBytesLike, to_bytes

# Type alias for bytes-like data
type BytesLike = HexBytesLike

# Note: _ensure_bytes is aliased to to_bytes from utils.bytes for backwards compatibility
_ensure_bytes = to_bytes


class AbiBackend(Enum):
    """Available backends for ABI operations."""

    RUST = auto()
    ETH_ABI = auto()


def _get_default_backend_from_env() -> AbiBackend:
    """
    Determine the default backend from the DEGENBOT_USE_RUST_ABI_DECODER environment variable.

    Returns:
        AbiBackend.RUST if the envvar is set to a truthy value or unset.
        AbiBackend.ETH_ABI if the envvar is set to a falsy value.
    """
    env_value = environ.get("DEGENBOT_USE_RUST_ABI_DECODER", "true").lower()
    if env_value in {"0", "false", "no", "off"}:
        return AbiBackend.ETH_ABI
    return AbiBackend.RUST


class AbiEncodeError(DegenbotError):
    """Raised when ABI encoding fails."""


class AbiDecodeError(DegenbotError):
    """Raised when ABI decoding fails."""


class AbiUnsupportedOperation(DegenbotError):
    """Raised when an operation is not supported by the selected backend."""


class AbiAdapter:
    """
    Adapter for ABI encoding/decoding operations.

    Allows switching between Rust (fast decoding) and eth_abi (full support)
    implementations.

    Example:
        >>> adapter = AbiAdapter(backend=AbiBackend.RUST)
        >>> # Decode with Rust backend
        >>> data = eth_abi.abi.encode(["uint256", "address"], [100, "0x" + "00" * 20])
        >>> values = adapter.decode(["uint256", "address"], data)
        >>> # Switch to eth_abi for encoding
        >>> adapter.backend = AbiBackend.ETH_ABI
        >>> encoded = adapter.encode(["uint256"], [42])
    """

    __slots__ = ("_backend",)

    def __init__(self, backend: AbiBackend | None = None) -> None:
        """
        Initialize the adapter with the specified backend.

        Args:
            backend: The backend to use for ABI operations. If None, uses the
                     default from DEGENBOT_USE_RUST_ABI_DECODER environment variable.
        """
        self._backend = backend if backend is not None else _get_default_backend_from_env()

    @property
    def backend(self) -> AbiBackend:
        """Get the current backend."""
        return self._backend

    @backend.setter
    def backend(self, value: AbiBackend) -> None:
        """Set the backend."""
        self._backend = value

    def encode(self, types: Sequence[str], args: Sequence[Any]) -> bytes:
        """
        Encode values into ABI-encoded bytes.

        Args:
            types: ABI type strings (e.g., ["uint256", "address"])
            args: Values to encode

        Returns:
            ABI-encoded bytes

        Raises:
            AbiEncodeError: If encoding fails
            AbiUnsupportedOperation: If using Rust backend (encoding not supported)
        """
        if self._backend == AbiBackend.RUST:
            raise AbiUnsupportedOperation(
                message="Encoding is not supported by the Rust backend. "
                "Switch to AbiBackend.ETH_ABI for encoding operations."
            )

        try:
            return eth_abi.abi.encode(types=list(types), args=list(args))
        except (EthAbiEncodingError, EthAbiParseError) as e:
            raise AbiEncodeError(message=f"ABI encoding failed: {e}") from e

    def decode(
        self,
        types: Sequence[str],
        data: BytesLike,
        *,
        checksum: bool = True,
    ) -> tuple[Any, ...]:
        """
        Decode ABI-encoded bytes into Python values.

        Args:
            types: ABI type strings (e.g., ["uint256", "address"])
            data: ABI-encoded bytes or HexBytes to decode
            checksum: If True (default), return checksummed addresses.
                      Only applies to Rust backend.

        Returns:
            Tuple of decoded values

        Raises:
            AbiDecodeError: If decoding fails
        """
        if self._backend == AbiBackend.RUST:
            return self._decode_rust(types, data, checksum)
        return self._decode_eth_abi(types, data)

    def _decode_rust(
        self,
        types: Sequence[str],
        data: BytesLike,
        checksum: bool,  # noqa: FBT001 - part of internal interface
    ) -> tuple[Any, ...]:
        """Decode using the Rust backend."""
        # Convert HexBytes to bytes if needed
        data_bytes = _ensure_bytes(data)
        try:
            result = rs_decode(types=list(types), data=data_bytes, checksum=checksum)
        except ValueError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        except NotImplementedError:
            # Fall back to eth_abi for unsupported types (e.g., fixed-point)
            return self._decode_eth_abi(types, data)
        else:
            return tuple(result)

    @staticmethod
    def _decode_eth_abi(types: Sequence[str], data: BytesLike) -> tuple[Any, ...]:
        """Decode using the eth_abi backend."""
        # eth_abi requires plain bytes
        data_bytes = _ensure_bytes(data)
        try:
            return eth_abi.abi.decode(types=list(types), data=data_bytes)
        except EthAbiDecodingError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e

    def decode_single(
        self,
        abi_type: str,
        data: BytesLike,
        *,
        checksum: bool = True,
    ) -> Any:  # noqa: ANN401 - return type depends on abi_type
        """
        Decode a single ABI value.

        Convenience method for decoding a single value.

        Args:
            abi_type: ABI type string (e.g., "uint256")
            data: ABI-encoded bytes or HexBytes
            checksum: If True (default), return checksummed addresses.
                      Only applies to Rust backend.

        Returns:
            The decoded value

        Raises:
            AbiDecodeError: If decoding fails
        """
        if self._backend == AbiBackend.RUST:
            return self._decode_single_rust(abi_type, data, checksum)
        return self._decode_single_eth_abi(abi_type, data)

    def _decode_single_rust(
        self,
        abi_type: str,
        data: BytesLike,
        checksum: bool,  # noqa: FBT001 - part of internal interface
    ) -> Any:  # noqa: ANN401 - return type depends on abi_type
        """Decode a single value using the Rust backend."""
        # Convert HexBytes to bytes if needed
        data_bytes = _ensure_bytes(data)
        try:
            return rs_decode_single(abi_type=abi_type, data=data_bytes, checksum=checksum)
        except ValueError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        except NotImplementedError:
            # Fall back to eth_abi for unsupported types
            return self._decode_single_eth_abi(abi_type, data)

    @staticmethod
    def _decode_single_eth_abi(abi_type: str, data: BytesLike) -> Any:  # noqa: ANN401
        """Decode a single value using the eth_abi backend."""
        # eth_abi requires plain bytes
        data_bytes = _ensure_bytes(data)
        try:
            (result,) = eth_abi.abi.decode(types=[abi_type], data=data_bytes)
        except EthAbiDecodingError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        else:
            return result

    def supports_encoding(self) -> bool:
        """Check if the current backend supports encoding."""
        return self._backend == AbiBackend.ETH_ABI

    def supports_type(self, abi_type: str, operation: str = "decode") -> bool:
        """
        Check if the current backend supports a specific ABI type.

        Args:
            abi_type: ABI type string to check
            operation: Either "encode" or "decode"

        Returns:
            True if the type is supported, False otherwise
        """
        if operation == "encode":
            return self._backend == AbiBackend.ETH_ABI

        # Rust decoder doesn't support fixed-point types
        if "fixed" in abi_type.lower():
            return self._backend == AbiBackend.ETH_ABI

        return True


# Module-level convenience functions using the default adapter
_default_adapter: Final[AbiAdapter] = AbiAdapter()


def get_default_adapter() -> AbiAdapter:
    """
    Get the default ABI adapter instance.

    Returns:
        The module-level default adapter. The backend is determined by
        the DEGENBOT_USE_RUST_ABI_DECODER environment variable.
    """
    return _default_adapter


def get_default_backend() -> AbiBackend:
    """
    Get the default backend determined by the environment variable.

    Returns:
        The default AbiBackend based on DEGENBOT_USE_RUST_ABI_DECODER.
    """
    return _get_default_backend_from_env()


def encode(types: Sequence[str], args: Sequence[Any]) -> bytes:
    """
    Encode values into ABI-encoded bytes using eth_abi.

    Note: This always uses eth_abi since the Rust backend
    does not support encoding.

    Args:
        types: ABI type strings
        args: Values to encode

    Returns:
        ABI-encoded bytes
    """
    try:
        return eth_abi.abi.encode(types=list(types), args=list(args))
    except (EthAbiEncodingError, EthAbiParseError) as e:
        raise AbiEncodeError(message=f"ABI encoding failed: {e}") from e


def decode(
    types: Sequence[str],
    data: BytesLike,
    *,
    backend: AbiBackend | None = None,
    checksum: bool = True,
) -> tuple[Any, ...]:
    """
    Decode ABI-encoded bytes into Python values.

    Args:
        types: ABI type strings
        data: ABI-encoded bytes or HexBytes
        backend: Backend to use. If None, uses the default from
                 DEGENBOT_USE_RUST_ABI_DECODER environment variable.
        checksum: If True, return checksummed addresses (Rust backend only)

    Returns:
        Tuple of decoded values
    """
    if backend is None:
        backend = _get_default_backend_from_env()

    if backend == AbiBackend.RUST:
        # Convert HexBytes to bytes if needed
        data_bytes = _ensure_bytes(data)
        try:
            result = rs_decode(types=list(types), data=data_bytes, checksum=checksum)
        except ValueError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        except NotImplementedError:
            # Fall back to eth_abi for unsupported types
            backend = AbiBackend.ETH_ABI
        else:
            return tuple(result)

    if backend == AbiBackend.ETH_ABI:
        # eth_abi requires plain bytes
        data_bytes = _ensure_bytes(data)
        try:
            return eth_abi.abi.decode(types=list(types), data=data_bytes)
        except EthAbiDecodingError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e

    raise AbiDecodeError(message=f"Unknown backend: {backend}")


def decode_single(
    abi_type: str,
    data: BytesLike,
    *,
    backend: AbiBackend | None = None,
    checksum: bool = True,
) -> Any:  # noqa: ANN401 - return type depends on abi_type
    """
    Decode a single ABI value.

    Args:
        abi_type: ABI type string
        data: ABI-encoded bytes or HexBytes
        backend: Backend to use. If None, uses the default from
                 DEGENBOT_USE_RUST_ABI_DECODER environment variable.
        checksum: If True, return checksummed addresses (Rust backend only)

    Returns:
        The decoded value
    """
    if backend is None:
        backend = _get_default_backend_from_env()

    if backend == AbiBackend.RUST:
        # Convert HexBytes to bytes if needed
        data_bytes = _ensure_bytes(data)
        try:
            return rs_decode_single(abi_type=abi_type, data=data_bytes, checksum=checksum)
        except ValueError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        except NotImplementedError:
            # Fall back to eth_abi for unsupported types
            backend = AbiBackend.ETH_ABI

    if backend == AbiBackend.ETH_ABI:
        # eth_abi requires plain bytes
        data_bytes = _ensure_bytes(data)
        try:
            (result,) = eth_abi.abi.decode(types=[abi_type], data=data_bytes)
        except EthAbiDecodingError as e:
            raise AbiDecodeError(message=f"ABI decoding failed: {e}") from e
        else:
            return result

    raise AbiDecodeError(message=f"Unknown backend: {backend}")


__all__ = [
    "AbiAdapter",
    "AbiBackend",
    "AbiDecodeError",
    "AbiEncodeError",
    "AbiUnsupportedOperation",
    "BytesLike",
    "decode",
    "decode_single",
    "encode",
    "get_default_adapter",
    "get_default_backend",
]
