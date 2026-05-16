"""
Type stubs for the degenbot Rust extension module (degenbot_rs).

This module provides high-performance implementations of common operations
used by the degenbot Python package.
"""

from collections.abc import Coroutine
from typing import Any, Literal, overload

from hexbytes import HexBytes

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

def encode(
    types: list[str],
    values: list[Any],
) -> bytes:
    """
    Encode multiple ABI values.

    Args:
        types: List of ABI type strings
        values: List of Python values to encode

    Returns:
        The ABI-encoded bytes.

    Raises:
        ValueError: If values cannot be encoded or type/value counts differ
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

def encode_single(abi_type: str, value: Any) -> bytes:
    """
    Encode a single ABI value.

    Args:
        abi_type: ABI type string (e.g., "uint256", "address", "bytes")
        value: Python value to encode (int, bool, str, bytes, or list)

    Returns:
        The ABI-encoded bytes.

    Raises:
        ValueError: If the value cannot be encoded for the given type
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
        """
        Execute a contract call.

        Args:
            function_signature: Function signature like "balanceOf(address)"
            args: List of arguments as strings
            block_number: Optional block number to query

        Returns:
            List of decoded return values as strings
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
    r"""
    Synchronous Ethereum RPC provider.

    Automatically detects connection type from URL:
    - HTTP/HTTPS URLs use HTTP transport with connection pooling
    - WS/WSS URLs use WebSocket transport
    - File paths (Unix: /path, Windows: \\.\pipe\...) use IPC transport
    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int = 50,
        burst: int = 10,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> int: ...
    def get_chain_id(self) -> int: ...
    def get_gas_price(self) -> int: ...
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
        params: list[Any],
    ) -> Any: ...
    def close(self) -> None: ...

class AsyncAlloyProvider:
    """
    Async wrapper for AlloyProvider operations.
    """

    def __init__(self, sync_provider: AlloyProvider) -> None: ...
    @staticmethod
    def create(
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int = 50,
        burst: int = 10,
    ) -> Coroutine[Any, Any, AsyncAlloyProvider]: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> Coroutine[Any, Any, int]: ...
    def get_chain_id(self) -> Coroutine[Any, Any, int]: ...
    def get_gas_price(self) -> Coroutine[Any, Any, int]: ...
    def get_block(self, block_number: int) -> Coroutine[Any, Any, dict[str, Any] | None]: ...
    def get_transaction(self, tx_hash: str) -> Coroutine[Any, Any, dict[str, Any] | None]: ...
    def get_transaction_receipt(
        self, tx_hash: str
    ) -> Coroutine[Any, Any, dict[str, Any] | None]: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Coroutine[Any, Any, list[dict[str, Any]]]: ...
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
        params: list[Any],
    ) -> Coroutine[Any, Any, Any]: ...
    def close(self) -> None: ...

class AsyncContract:
    """
    Async wrapper for contract interactions.
    """

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

# ------------------------------------------------------------------
# Möbius optimizer types
# ------------------------------------------------------------------

class RustHopState:
    """Pool hop state with reserves and fee for float-based Möbius solving."""

    def __init__(self, reserve_in: float, reserve_out: float, fee: float) -> None: ...
    @property
    def reserve_in(self) -> float: ...
    @property
    def reserve_out(self) -> float: ...
    @property
    def fee(self) -> float: ...

class RustMobiusCoefficients:
    """Möbius transformation coefficients for a path."""

    @property
    def coeff_K(self) -> float: ...
    @property
    def coeff_M(self) -> float: ...
    @property
    def coeff_N(self) -> float: ...
    @property
    def is_profitable(self) -> bool: ...
    def path_output(self, x: float) -> float: ...
    def optimal_input(self) -> float: ...
    def profit_at(self, x: float) -> float: ...

class RustV3TickRangeHop:
    """Uniswap V3 tick range state for piecewise Möbius solving."""

    def __init__(
        self,
        liquidity: float,
        sqrt_price_current: float,
        sqrt_price_lower: float,
        sqrt_price_upper: float,
        fee: float,
        zero_for_one: bool,
    ) -> None: ...
    @property
    def liquidity(self) -> float: ...
    @property
    def sqrt_price_current(self) -> float: ...
    @property
    def sqrt_price_lower(self) -> float: ...
    @property
    def sqrt_price_upper(self) -> float: ...
    @property
    def fee(self) -> float: ...
    @property
    def zero_for_one(self) -> bool: ...
    def alpha(self) -> float: ...
    def beta(self) -> float: ...
    def to_hop_state(self) -> RustHopState: ...
    def contains_sqrt_price(self, sqrt_price: float) -> bool: ...
    def max_gross_input_in_range(self) -> float: ...

class RustV3TickRangeSequence:
    """Sequence of adjacent V3 tick ranges for multi-range solving."""

    def __init__(self, ranges: list[RustV3TickRangeHop]) -> None: ...
    def compute_crossing(self, k: int) -> RustTickRangeCrossing: ...

class RustTickRangeCrossing:
    """Tick range crossing data for piecewise Möbius calculation."""

    def __init__(
        self,
        crossing_gross_input: float,
        crossing_output: float,
        ending_range: RustV3TickRangeHop,
    ) -> None: ...
    @property
    def crossing_gross_input(self) -> float: ...
    @property
    def crossing_output(self) -> float: ...
    @property
    def ending_range(self) -> RustV3TickRangeHop: ...

class RustMobiusResult:
    """Result from Möbius float solver."""

    @property
    def optimal_input(self) -> float: ...
    @property
    def profit(self) -> float: ...
    @property
    def iterations(self) -> int: ...
    @property
    def success(self) -> bool: ...

class RustMobiusOptimizer:
    """High-level Möbius optimizer for multi-hop paths."""

    def __init__(self) -> None: ...
    def compute_coefficients(self, hops: list[RustHopState]) -> RustMobiusCoefficients: ...
    def simulate_path(self, x: float, hops: list[RustHopState]) -> float: ...
    def solve(self, hops: list[RustHopState], max_input: float | None = None) -> RustMobiusResult: ...
    def solve_v3_candidates(
        self,
        base_hops: list[RustHopState],
        v3_hop_index: int,
        v3_candidates: list[RustV3TickRangeHop],
        max_input: float | None = None,
    ) -> RustMobiusResult: ...
    def estimate_v3_final_sqrt_price(self, amount_in: float, v3_hop: RustV3TickRangeHop) -> float: ...
    def solve_piecewise(
        self,
        hops: list[RustHopState],
        v3_hop_index: int,
        crossings: list[RustTickRangeCrossing],
        max_input: float | None = None,
    ) -> RustMobiusResult: ...
    def solve_v3_sequence(
        self,
        hops: list[RustHopState],
        v3_hop_index: int,
        sequence: RustV3TickRangeSequence,
        max_candidates: int,
        max_input: float | None = None,
    ) -> RustMobiusResult: ...
    def solve_v3_v3(
        self,
        sequence1: RustV3TickRangeSequence,
        sequence2: RustV3TickRangeSequence,
        max_input: float | None = None,
        max_candidates: int = 10,
    ) -> RustMobiusResult: ...
    def solve_batch(
        self,
        hops_array: list[float],
        num_hops: int,
        max_inputs: list[float],
    ) -> dict[str, Any]: ...
    def solve_batch_vectorized(
        self,
        reserves_in: list[float],
        reserves_out: list[float],
        fees: list[float],
        num_hops: int,
        max_inputs: list[float],
    ) -> dict[str, Any]: ...

class RustArbResult:
    """Result from unified arbitrage solver (RustArbSolver)."""

    @property
    def optimal_input(self) -> float: ...
    @property
    def profit(self) -> float: ...
    @property
    def optimal_input_int(self) -> int | None: ...
    @property
    def profit_int(self) -> int | None: ...
    @property
    def iterations(self) -> int: ...
    @property
    def success(self) -> bool: ...
    @property
    def supported(self) -> bool: ...
    @property
    def method(self) -> int: ...

class RustArbSolver:
    """Unified arbitrage solver with automatic method selection."""

    def __init__(self) -> None: ...
    def solve(
        self,
        hops: list[RustHopState | RustIntHopState | tuple[float, float, float]],
        v3_sequences: list[tuple[int, RustV3TickRangeSequence]] | None = None,
        max_input: float | None = None,
        max_candidates: int = 10,
    ) -> RustArbResult: ...
    def solve_raw(
        self,
        int_hops_flat: list[int],
        max_input: float | None = None,
    ) -> RustArbResult: ...

class RustPoolCache:
    """Cached pool state storage for fast solve-by-ID operations."""

    def __init__(self) -> None: ...
    def insert(
        self,
        pool_id: int,
        reserve_in: int,
        reserve_out: int,
        gamma_numer: int,
        fee_denom: int,
    ) -> None: ...
    def remove(self, pool_id: int) -> bool: ...
    def solve(self, path: list[int], max_input: float | None = None) -> RustArbResult: ...
    def contains(self, pool_id: int) -> bool: ...
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...

class RustIntHopState:
    """Integer-based hop state for EVM-exact Möbius solving."""

    def __init__(
        self,
        reserve_in: int,
        reserve_out: int,
        gamma_numer: int,
        fee_denom: int,
    ) -> None: ...
    @property
    def reserve_in(self) -> int: ...
    @property
    def reserve_out(self) -> int: ...
    @property
    def gamma_numer(self) -> int: ...
    @property
    def fee_numer(self) -> int: ...
    @property
    def fee_denom(self) -> int: ...

class RustIntMobiusResult:
    """Result from integer Möbius solver."""

    @property
    def optimal_input(self) -> int: ...
    @property
    def profit(self) -> int: ...
    @property
    def iterations(self) -> int: ...
    @property
    def success(self) -> bool: ...

def py_compute_mobius_coefficients(
    hops: list[RustHopState],
) -> RustMobiusCoefficients: ...

def py_mobius_solve(
    hops: list[RustHopState],
    max_input: float | None = None,
) -> RustMobiusResult: ...

def py_simulate_path(x: float, hops: list[RustHopState]) -> float: ...

def py_estimate_v3_final_sqrt_price(
    amount_in: float,
    v3_hop: RustV3TickRangeHop,
) -> float: ...

def py_int_mobius_solve(
    hops: list[RustIntHopState],
) -> RustIntMobiusResult: ...

def py_int_simulate_path(x: int, hops: list[RustIntHopState]) -> int: ...

def py_mobius_refine_int(
    x_approx: float,
    hops: list[RustIntHopState],
    max_input: float | None = None,
) -> RustIntMobiusResult: ...

__all__ = [
    "AlloyProvider",
    "AsyncAlloyProvider",
    "AsyncContract",
    "Contract",
    "LogFilter",
    "RustArbResult",
    "RustArbSolver",
    "RustHopState",
    "RustIntHopState",
    "RustIntMobiusResult",
    "RustMobiusCoefficients",
    "RustMobiusOptimizer",
    "RustMobiusResult",
    "RustPoolCache",
    "RustTickRangeCrossing",
    "RustV3TickRangeHop",
    "RustV3TickRangeSequence",
    "decode",
    "decode_return_data",
    "decode_single",
    "encode",
    "encode_function_call",
    "encode_single",
    "get_function_selector",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "py_compute_mobius_coefficients",
    "py_estimate_v3_final_sqrt_price",
    "py_int_mobius_solve",
    "py_int_simulate_path",
    "py_mobius_refine_int",
    "py_mobius_solve",
    "py_simulate_path",
    "to_checksum_address",
]
