"""Type stubs for the degenbot Rust extension module (degenbot_rs).

This module provides high-performance implementations of common operations
used by the degenbot Python package.
"""

from collections.abc import Coroutine
from typing import Any, Literal, overload

from hexbytes import HexBytes

from degenbot.types.rpc_types import (
    BlockData,
    LogData,
    TransactionData,
    TransactionReceiptData,
)

# ------------------------------------------------------------------
# ABI encoding / decoding
# ------------------------------------------------------------------

# ------------------------------------------------------------------
# Concentrated-liquidity math (cl_*)
# ------------------------------------------------------------------

def cl_most_significant_bit(x: int) -> int:
    """Find the index of the most significant bit set in x.

    Args:
        x: A non-negative integer

    Returns:
        The index (0-255) of the highest set bit

    Raises:
        ValueError: If x is zero

    """

def cl_least_significant_bit(x: int) -> int:
    """Find the index of the least significant bit set in x.

    Args:
        x: A non-negative integer

    Returns:
        The index (0-255) of the lowest set bit

    Raises:
        ValueError: If x is zero

    """

def cl_muldiv(a: int, b: int, denominator: int) -> int:
    """Compute floor(a * b / denominator) with full 512-bit precision.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The floored result as a Python int

    Raises:
        ValueError: On division by zero or overflow

    """

def cl_muldiv_rounding_up(a: int, b: int, denominator: int) -> int:
    """Compute ceil(a * b / denominator) with full 512-bit precision.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The ceiling result as a Python int

    Raises:
        ValueError: On division by zero or overflow

    """

def cl_div_rounding_up(x: int, y: int) -> int:
    """Compute ceil(x / y) without overflow checking.

    Args:
        x: Dividend
        y: Divisor

    Returns:
        The ceiling result as a Python int

    Raises:
        ValueError: If y is zero

    """

def cl_simple_mul_div(a: int, b: int, denominator: int) -> int:
    """Compute (a * b) / denominator without overflow checking.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The result as a Python int

    Raises:
        ValueError: If denominator is zero

    """

def cl_add_delta(x: int, y: int) -> int:
    """Add a signed delta y to x, checking that the result fits in uint128.

    Args:
        x: Base value (must fit in uint128)
        y: Signed delta (must fit in int128)

    Returns:
        The result as a Python int

    Raises:
        ValueError: If the result overflows or inputs are out of range

    """

def cl_get_amount0_delta(
    sqrt_price_a: int,
    sqrt_price_b: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount0 delta between two prices for a given liquidity.

    Args:
        sqrt_price_a: First sqrt price (X96)
        sqrt_price_b: Second sqrt price (X96)
        liquidity: Liquidity value
        round_up: Whether to round up

    Returns:
        The token0 amount delta as a Python int

    Raises:
        ValueError: On invalid input (zero price, overflow, etc.)

    """

def cl_get_amount1_delta(
    sqrt_price_a: int,
    sqrt_price_b: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount1 delta between two prices for a given liquidity.

    Args:
        sqrt_price_a: First sqrt price (X96)
        sqrt_price_b: Second sqrt price (X96)
        liquidity: Liquidity value
        round_up: Whether to round up

    Returns:
        The token1 amount delta as a Python int

    Raises:
        ValueError: On invalid input (negative liquidity, overflow, etc.)

    """

def cl_get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of token0, rounding up.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount: Token0 amount
        add: Whether to add (True) or remove (False)

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On overflow or insufficient liquidity

    """

def cl_get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of token1, rounding down.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount: Token1 amount
        add: Whether to add (True) or remove (False)

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On overflow or insufficient liquidity

    """

def cl_get_next_sqrt_price_from_input(
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an input amount.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount_in: Input amount
        zero_for_one: Direction flag

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On invalid price/liquidity or overflow

    """

def cl_get_next_sqrt_price_from_output(
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an output amount.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount_out: Output amount
        zero_for_one: Direction flag

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On invalid price/liquidity or overflow

    """

def cl_compute_swap_step_v3(
    sqrt_price_current: int,
    sqrt_price_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute a V3-style swap step.

    Args:
        sqrt_price_current: Current sqrt price (X96)
        sqrt_price_target: Target sqrt price (X96)
        liquidity: Liquidity value
        amount_remaining: Remaining amount (signed)
        fee_pips: Fee in pips

    Returns:
        Tuple of (sqrt_price_next, amount_in, amount_out, fee_amount)

    Raises:
        ValueError: On invalid input, overflow, or if liquidity exceeds int128

    """

def cl_compute_swap_step_v4(
    sqrt_price_current: int,
    sqrt_price_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute a V4-style swap step.

    Args:
        sqrt_price_current: Current sqrt price (X96)
        sqrt_price_target: Target sqrt price (X96)
        liquidity: Liquidity value
        amount_remaining: Remaining amount (signed)
        fee_pips: Fee in pips

    Returns:
        Tuple of (sqrt_price_next, amount_in, amount_out, fee_amount)

    Raises:
        ValueError: On invalid input, overflow, or if liquidity exceeds int128

    """

def cl_max_usable_tick(tick_spacing: int) -> int:
    """Compute the maximum usable tick for a given tick spacing.

    Args:
        tick_spacing: The tick spacing value

    Returns:
        The maximum usable tick as an int

    """

def cl_min_usable_tick(tick_spacing: int) -> int:
    """Compute the minimum usable tick for a given tick spacing.

    Args:
        tick_spacing: The tick spacing value

    Returns:
        The minimum usable tick as an int

    """

def get_sqrt_ratio_at_tick(tick: int) -> int:
    """Convert a tick value to its corresponding sqrt price (X96 format).

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
    abi_type: Literal["string"],
    data: bytes,
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
    checksum: bool = True,
) -> bytes: ...
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

def encode_function_call(function_signature: str, args: list[str]) -> bytes:
    """Encode function arguments into calldata.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"
        args: List of arguments as strings

    Returns:
        Encoded calldata as bytes (selector + encoded args)

    Raises:
        ValueError: If the signature or arguments are invalid

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

def decode_return_data(data: bytes, output_types: list[str]) -> list[str]:
    """Decode return data from a contract call.

    Args:
        data: Return data as bytes
        output_types: List of output type strings like ["uint256", "address"]

    Returns:
        List of decoded values as strings

    Raises:
        ValueError: If data is invalid or cannot be decoded

    """

def get_function_selector(function_signature: str) -> str:
    """Parse a function signature and return its selector.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"

    Returns:
        4-byte function selector as hex string (e.g., "0xa9059cbb")

    Raises:
        ValueError: If the function signature is invalid

    """

class Contract:
    """Synchronous wrapper for smart contract interactions."""

    def __init__(self, address: str, provider_url: str | None = None) -> None: ...
    @staticmethod
    def from_provider(address: str, provider: AlloyProvider) -> Contract: ...
    @property
    def address(self) -> str: ...
    def call(
        self,
        function_signature: str,
        args: list[str],
        block_number: int | None = None,
    ) -> list[str]:
        """Execute a contract call.

        Args:
            function_signature: Function signature like "balanceOf(address)"
            args: List of arguments as strings
            block_number: Optional block number to query

        Returns:
            List of decoded return values as strings

        """

class LogFilter:
    """Filter for log queries."""

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
    r"""Synchronous Ethereum RPC provider.

    Automatically detects connection type from URL:
    - HTTP/HTTPS URLs use HTTP transport with connection pooling
    - WS/WSS URLs use WebSocket transport
    - File paths (Unix: /path, Windows: \\.\pipe\...) use IPC transport

    Rate limiting is opt-in: pass ``requests_per_second`` and ``burst``
    together to enable transport-level throttling on HTTP connections.
    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> int: ...
    def get_chain_id(self) -> int: ...
    def get_gas_price(self) -> int: ...
    def get_block(self, block_number: int) -> BlockData | None: ...
    def get_transaction(self, tx_hash: str) -> TransactionData | None: ...
    def get_transaction_receipt(self, tx_hash: str) -> TransactionReceiptData | None: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogData]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> HexBytes: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> HexBytes: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> int: ...
    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> HexBytes: ...
    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int: ...
    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int: ...
    def make_request(
        self,
        method: str,
        params: list[str | bool | int | bytes | None],
    ) -> str | bool | int | bytes | list[Any] | dict[str, Any] | None: ...
    def close(self) -> None: ...
    def subscribe_blocks(self) -> AlloySubscription: ...
    def subscribe_full_blocks(self) -> AlloySubscription: ...
    def subscribe_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_full_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> AlloySubscription: ...

class AsyncAlloyProvider:
    """Async wrapper for AlloyProvider operations."""

    def __init__(self, sync_provider: AlloyProvider) -> None: ...
    @staticmethod
    def create(
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> Coroutine[Any, Any, AsyncAlloyProvider]: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> Coroutine[Any, Any, int]: ...
    def get_chain_id(self) -> Coroutine[Any, Any, int]: ...
    def get_gas_price(self) -> Coroutine[Any, Any, int]: ...
    def get_block(self, block_number: int) -> Coroutine[Any, Any, BlockData | None]: ...
    def get_transaction(self, tx_hash: str) -> Coroutine[Any, Any, TransactionData | None]: ...
    def get_transaction_receipt(
        self, tx_hash: str
    ) -> Coroutine[Any, Any, TransactionReceiptData | None]: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Coroutine[Any, Any, list[LogData]]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, HexBytes]: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, HexBytes]: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, HexBytes]: ...
    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def make_request(
        self,
        method: str,
        params: list[str | bool | int | bytes | None],
    ) -> Coroutine[Any, Any, str | bool | int | bytes | list[Any] | dict[str, Any] | None]: ...
    def close(self) -> None: ...
    def subscribe_blocks(self) -> AlloySubscription: ...
    def subscribe_full_blocks(self) -> AlloySubscription: ...
    def subscribe_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_full_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> AlloySubscription: ...

class AlloySubscription:
    """Python wrapper for a subscription from the Rust layer."""

    def __aiter__(self) -> AlloySubscription: ...
    def __anext__(self) -> Coroutine[Any, Any, BlockData | LogData | str]: ...
    def drain(self) -> list[BlockData | LogData | str]: ...
    async def started(self) -> None: ...
    def unsubscribe(self) -> None: ...

class AsyncContract:
    """Async wrapper for contract interactions."""

    @staticmethod
    def create(
        address: str,
        provider_url: str,
        max_retries: int | None = None,
    ) -> Coroutine[Any, Any, AsyncContract]: ...
    @staticmethod
    def from_provider(address: str, provider: AlloyProvider) -> AsyncContract: ...
    @property
    def address(self) -> str: ...
    def call(
        self,
        function_signature: str,
        args: list[str],
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, list[str]]: ...
    def batch_call(
        self,
        calls: list[tuple[str, list[str]]],
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, list[list[str]]]: ...

class UniswapArbEngine:
    """Rust-side engine for Uniswap arbitrage path solving."""

    def __init__(self) -> None: ...
    def load_v3_snapshot_from_py(self, py_data: dict[str, dict[int, tuple[int, int]]]) -> None: ...
    def begin_v3_snapshot_stream(self) -> None: ...
    def insert_v3_pool_snapshot(
        self, pool_address: str, tick_data: dict[int, tuple[int, int]]
    ) -> None: ...
    def finish_v3_snapshot(self) -> None: ...
    def load_v4_snapshot_from_py(
        self, py_data: dict[str, dict[str, dict[int, tuple[int, int]]]]
    ) -> None: ...
    def begin_v4_snapshot_stream(self) -> None: ...
    def insert_v4_pool_snapshot(
        self, pool_manager: str, pool_id_hex: str, tick_data: dict[int, tuple[int, int]]
    ) -> None: ...
    def finish_v4_snapshot(self) -> None: ...

__all__ = [
    "AlloyProvider",
    "AlloySubscription",
    "AsyncAlloyProvider",
    "AsyncContract",
    "BlockData",
    "Contract",
    "LogData",
    "LogFilter",
    "TransactionData",
    "TransactionReceiptData",
    "UniswapArbEngine",
    "cl_add_delta",
    "cl_compute_swap_step_v3",
    "cl_compute_swap_step_v4",
    "cl_div_rounding_up",
    "cl_get_amount0_delta",
    "cl_get_amount1_delta",
    "cl_get_next_sqrt_price_from_amount0_rounding_up",
    "cl_get_next_sqrt_price_from_amount1_rounding_down",
    "cl_get_next_sqrt_price_from_input",
    "cl_get_next_sqrt_price_from_output",
    "cl_least_significant_bit",
    "cl_max_usable_tick",
    "cl_min_usable_tick",
    "cl_most_significant_bit",
    "cl_muldiv",
    "cl_muldiv_rounding_up",
    "cl_simple_mul_div",
    "decode",
    "decode_return_data",
    "decode_single",
    "encode",
    "encode_function_call",
    "encode_single",
    "get_function_selector",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "to_checksum_address",
]
