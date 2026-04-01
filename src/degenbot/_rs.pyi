"""
Type stubs for the degenbot Rust extension module (_rs).

This module provides high-performance implementations of common operations
used by the degenbot Python package.
"""

from collections.abc import Coroutine, Iterator
from typing import Any, Literal, overload

class FastHexBytes:
    """
    High-performance hex/bytes type with pre-computed hex representation.

    Stores both bytes and pre-computed "0x"-prefixed hex string for zero-cost
    `hex()` calls. Implements Python buffer protocol for bytes compatibility.

    Performance Tips:
        - Use `memoryview(obj)` for zero-copy buffer access
        - Use `obj.raw` property for direct bytes access without allocation
        - Avoid `bytes(obj)` in hot paths - it creates a new Python object
        - Slicing returns `FastHexBytes` with pre-computed hex for zero-cost `.hex()`

    Accepts: hex string (with/without 0x), bytes, bytearray, memoryview,
             int, bool, or another `FastHexBytes`.
    """

    def __init__(
        self, value: str | bytes | bytearray | memoryview | int | bool | FastHexBytes
    ) -> None: ...
    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[int]: ...
    def __reversed__(self) -> Iterator[int]: ...
    def __contains__(self, item: int) -> bool: ...
    def __bytes__(self) -> bytes: ...
    def __bool__(self) -> bool: ...
    def __hash__(self) -> int: ...
    def __eq__(self, other: object) -> bool: ...
    def __ne__(self, other: object) -> bool: ...
    def __add__(self, other: str | bytes | FastHexBytes) -> FastHexBytes: ...
    def __radd__(self, other: bytes) -> FastHexBytes: ...
    def __mul__(self, n: int) -> FastHexBytes: ...
    def __rmul__(self, n: int) -> FastHexBytes: ...
    @overload
    def __getitem__(self, index: int) -> int: ...
    @overload
    def __getitem__(self, index: slice) -> FastHexBytes: ...
    def hex(self) -> str:
        """Return pre-computed hex string with 0x prefix (zero cost)."""

    def to_0x_hex(self) -> str:
        """Return hex string with 0x prefix (same as `hex()`)."""

    @property
    def hex_property(self) -> str:
        """Getter for hex property (alias for hex())."""

    @property
    def raw(self) -> bytes:
        """Getter for raw property (bytes content)."""

    def __reduce__(self) -> tuple[type, tuple[bytes]]: ...

def get_sqrt_ratio_at_tick(tick: int) -> int:
    """
    Convert a tick value to its corresponding sqrt price (X96 format).

    Args:
        tick: The tick value in range [-887272, 887272]

    Returns:
        A Python int representing the sqrt price X96 value

    Raises:
        ValueError: If the tick value is invalid (out of range)
    """

@overload
def get_tick_at_sqrt_ratio(sqrt_price_x96: int) -> int: ...
@overload
def get_tick_at_sqrt_ratio(sqrt_price_x96: bytes) -> int: ...
@overload
def to_checksum_address(address: str) -> str: ...
@overload
def to_checksum_address(address: bytes) -> str: ...
def decode(
    types: list[str],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> list[Any]:
    """
    Decode ABI-encoded data for multiple types.

    Args:
        types: List of ABI type strings
        data: Raw ABI-encoded bytes
        strict: If True (default), performs strict validation
        checksum: If True (default), returns checksummed addresses

    Returns:
        A list of decoded Python values

    Raises:
        ValueError: If data is invalid or insufficient
        NotImplementedError: If strict=False or for unsupported types
    """

@overload
def decode_single(
    abi_type: Literal["address"],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> str: ...
@overload
def decode_single(
    abi_type: Literal["bool"],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> bool: ...
@overload
def decode_single(
    abi_type: Literal["string"],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> str: ...
@overload
def decode_single(
    abi_type: Literal[
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "uint128",
        "uint256",
    ],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> int: ...
@overload
def decode_single(
    abi_type: Literal[
        "int8",
        "int16",
        "int32",
        "int64",
        "int128",
        "int256",
    ],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> int: ...
@overload
def decode_single(
    abi_type: Literal["bytes"],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> bytes: ...
@overload
def decode_single(
    abi_type: Literal[
        "bytes1",
        "bytes2",
        "bytes3",
        "bytes4",
        "bytes5",
        "bytes6",
        "bytes7",
        "bytes8",
        "bytes9",
        "bytes10",
        "bytes11",
        "bytes12",
        "bytes13",
        "bytes14",
        "bytes15",
        "bytes16",
        "bytes17",
        "bytes18",
        "bytes19",
        "bytes20",
        "bytes21",
        "bytes22",
        "bytes23",
        "bytes24",
        "bytes25",
        "bytes26",
        "bytes27",
        "bytes28",
        "bytes29",
        "bytes30",
        "bytes31",
        "bytes32",
    ],
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> bytes: ...
@overload
def decode_single(
    abi_type: str,
    data: bytes,
    strict: bool = True,
    checksum: bool = True,
) -> str | bool | int | bytes:
    """
    Decode a single ABI value.

    Args:
        abi_type: ABI type string
        data: Raw ABI-encoded bytes
        strict: If True (default), performs strict validation
        checksum: If True (default), returns checksummed addresses

    Returns:
        The decoded Python value

    Raises:
        ValueError: If data is invalid or insufficient
        NotImplementedError: If strict=False or for unsupported types
    """

def encode_function_call(function_signature: str, args: list[str]) -> bytes:
    """
    Encode function arguments into calldata.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"
        args: List of arguments as strings

    Returns:
        Encoded calldata as bytes (selector + encoded args)

    Raises:
        ValueError: If the signature or arguments are invalid
    """

def decode_return_data(data: bytes, output_types: list[str]) -> list[str]:
    """
    Decode return data from a contract call.

    Args:
        data: Return data as bytes
        output_types: List of output type strings like ["uint256", "address"]

    Returns:
        List of decoded values as strings

    Raises:
        ValueError: If data is invalid or cannot be decoded
    """

def get_function_selector(function_signature: str) -> str:
    """
    Parse a function signature and return its selector.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"

    Returns:
        4-byte function selector as hex string (e.g., "0xa9059cbb")

    Raises:
        ValueError: If the function signature is invalid
    """

class Contract:
    """
    Synchronous wrapper for smart contract interactions.
    """

    def __init__(self, address: str, provider_url: str | None = None) -> None: ...
    @property
    def address(self) -> str: ...
    def call(
        self,
        function_signature: str,
        args: list[str],
        block_number: int | None = None,
    ) -> list[Any]:
        """
        Execute a contract call.

        Args:
            function_signature: Function signature like "balanceOf(address)"
            args: List of arguments as strings
            block_number: Optional block number to query

        Returns:
            List of decoded return values
        """

class LogFilter:
    """
    Filter for log queries.
    """

    def __init__(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> None: ...
    @property
    def from_block(self) -> int | None: ...
    @property
    def to_block(self) -> int | None: ...
    @property
    def addresses(self) -> list[str]: ...
    @property
    def topics(self) -> list[list[str]]: ...

class AlloyProvider:
    """
    Synchronous Ethereum RPC provider.

    Automatically detects connection type from URL:
    - HTTP/HTTPS URLs use HTTP transport with connection pooling
    - File paths (Unix: /path, Windows: \\\\.\\pipe\\...) use IPC transport
    """

    def __init__(
        self,
        rpc_url: str,
        max_connections: int = 10,
        timeout: float = 30.0,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> int: ...
    def get_chain_id(self) -> int: ...
    def get_gas_price(self) -> str: ...
    def get_block(self, block_number: int) -> dict[str, Any] | None: ...
    def get_transaction(self, tx_hash: str) -> dict[str, Any] | None: ...
    def get_transaction_receipt(self, tx_hash: str) -> dict[str, Any] | None: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> FastHexBytes: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> FastHexBytes: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> int: ...
    def close(self) -> None: ...

class AsyncAlloyProvider:
    """
    Async wrapper for AlloyProvider operations.
    """

    def __init__(self, sync_provider: AlloyProvider) -> None: ...
    @staticmethod
    def create(
        rpc_url: str,
        max_connections: int = 10,
        timeout: float = 30.0,
        max_retries: int = 10,
    ) -> Coroutine[Any, Any, AsyncAlloyProvider]: ...

class AsyncContract:
    """
    Async wrapper for contract interactions.
    """

    def __init__(self, address: str, provider_url: str) -> None: ...
    def call(
        self,
        function_signature: str,
        args: list[str],
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, list[Any]]: ...
    def batch_call(
        self,
        calls: list[tuple[str, list[str]]],
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, list[list[Any]]]: ...

__all__ = [
    "AlloyProvider",
    "AsyncAlloyProvider",
    "AsyncContract",
    "Contract",
    "FastHexBytes",
    "LogFilter",
    "decode",
    "decode_return_data",
    "decode_single",
    "encode_function_call",
    "get_function_selector",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "to_checksum_address",
]
