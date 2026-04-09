"""Smart contract interface with automatic ABI encoding/decoding.

This module provides a high-level contract interface using the Rust-based
Alloy provider for automatic ABI encoding and decoding of function calls.

Example:
    >>> from degenbot.contract import Contract, get_function_selector
    >>> from degenbot.connection.manager import ConnectionManager
    >>>
    >>> manager = ConnectionManager()
    >>> manager.register_chain(
    ...     ChainConfig(chain_id=1, rpc_urls=["https://eth.example.com"])
    ... )
    >>>
    >>> # Create contract instance
    >>> token = Contract(
    ...     address="0xA0b86a33E6441e3D4e4b8b8b8b8b8b8b8b8b8b8",
    ...     provider=manager.get_provider(1),
    ... )
    >>>
    >>> # Call functions with automatic encoding/decoding
    >>> balance = token.call("balanceOf(address)", ["0x1234..."])
    >>> name, symbol, decimals = token.batch_call([
    ...     ("name()", []),
    ...     ("symbol()", []),
    ...     ("decimals()", []),
    ... ])
"""

from collections.abc import Sequence
from typing import TYPE_CHECKING

from eth_typing import ChecksumAddress as Address

from degenbot.degenbot_rs import Contract as _Contract
from degenbot.degenbot_rs import decode_return_data as _decode_return_data
from degenbot.degenbot_rs import encode_function_call as _encode_function_call
from degenbot.degenbot_rs import get_function_selector as _get_function_selector

if TYPE_CHECKING:
    from degenbot.provider.interface import ProviderAdapter


class Contract:
    """
    High-level contract interface with automatic ABI encoding/decoding.

    Provides a Pythonic interface for calling smart contract functions with
    automatic ABI encoding of arguments and decoding of return values.

    Args:
        address: Contract address
        provider: AlloyProvider instance (optional, will use default if not provided)

    Example:
        >>> from degenbot.contract import Contract
        >>> from degenbot.connection import get_provider
        >>>
        >>> # Create contract for an ERC20 token
        >>> token = Contract(
        ...     address="0xA0b86a33E6441e3D4e4b8b8b8b8b8b8b8b8b8b8",
        ...     provider=get_provider(),
        ... )
        >>>
        >>> # Call balanceOf function
        >>> balance = token.call(
        ...     "balanceOf(address)",
        ...     ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"],
        ... )
        >>> print(f"Balance: {balance[0]}")
        Balance: 1000000000000000000
        >>>
        >>> # Batch multiple calls for efficiency
        >>> name, symbol, decimals = token.batch_call([
        ...     ("name()", []),
        ...     ("symbol()", []),
        ...     ("decimals()", []),
        ... ])
    """

    def __init__(
        self,
        address: Address,
        provider: "ProviderAdapter | None" = None,
        provider_url: str | None = None,
    ) -> None:
        """
        Create a new contract instance.

        Args:
            address: Contract address
            provider: AlloyProvider instance (optional, for future use)
            provider_url: RPC provider URL (optional, defaults to http://localhost:8545)

        Raises:
            ValueError: If the address is invalid
        """
        self._address = address
        self._provider = provider
        # Extract provider URL from provider if available, otherwise use provider_url
        url = provider_url
        if provider is not None and hasattr(provider, "rpc_url"):
            url = provider.rpc_url
        self._contract = _Contract(address, url)

    @property
    def address(self) -> Address:
        """Get the contract address."""
        return self._address

    def call(
        self,
        function_signature: str,
        args: Sequence[str] | None = None,
        block_number: int | str | None = None,
    ) -> list[str]:
        """
        Execute a contract call with automatic encoding/decoding.

        Args:
            function_signature: Function signature like "balanceOf(address)" or
                "transfer(address,uint256) returns (bool)"
            args: Function arguments as strings (optional)
            block_number: Block to query (default: latest)
                Can be "latest", "pending", "safe", "finalized", or block number

        Returns:
            List of decoded return values as strings

        Raises:
            ValueError: If the call fails or encoding/decoding fails

        Example:
            >>> # Simple call without arguments
            >>> name = contract.call("name()")
            >>>
            >>> # Call with arguments
            >>> balance = contract.call(
            ...     "balanceOf(address)",
            ...     ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"],
            ... )
            >>>
            >>> # Call with multiple return values
            >>> results = contract.call("getReserves() returns (uint112,uint112,uint32)")
            >>> reserve0, reserve1, blockTimestampLast = results
        """
        if args is None:
            args = []

        # Handle block number - for now we only support int or None
        # Full block tag support will be added in Phase 4
        block_num: int | None = None
        if isinstance(block_number, int):
            block_num = block_number
        elif block_number is not None and block_number != "latest":
            msg = f"Block tag '{block_number}' not yet implemented. Use int or 'latest'."
            raise NotImplementedError(msg)

        return list(self._contract.call(function_signature, list(args), block_num))

    def batch_call(
        self,
        calls: Sequence[tuple[str, Sequence[str] | None]],
        block_number: int | str | None = None,
    ) -> list[list[str]]:
        """
        Execute multiple contract calls efficiently.

        Args:
            calls: List of (function_signature, args) tuples
            block_number: Block to query (default: latest)

        Returns:
            List of results, where each result is a list of decoded return values

        Raises:
            ValueError: If any call fails

        Example:
            >>> # Fetch multiple token properties in one batch
            >>> results = contract.batch_call([
            ...     ("name()", []),
            ...     ("symbol()", []),
            ...     ("decimals()", []),
            ...     ("totalSupply()", []),
            ... ])
            >>> name, symbol, decimals, total_supply = [r[0] for r in results]
        """
        results = []
        for func_sig, args in calls:
            result = self.call(func_sig, args, block_number)
            results.append(result)
        return results

    @staticmethod
    def encode_function_call(
        function_signature: str,
        args: Sequence[str] | None = None,
    ) -> bytes:
        """
        Encode a function call without executing it.

        Useful for manual transaction building or debugging.

        Args:
            function_signature: Function signature
            args: Function arguments

        Returns:
            Encoded calldata as bytes

        Example:
            >>> calldata = contract.encode_function_call(
            ...     "transfer(address,uint256)",
            ...     ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", "1000000000000000000"],
            ... )
            >>> print(f"0x{calldata.hex()}")
            0xa9059cbb...
        """
        if args is None:
            args = []
        return _encode_function_call(function_signature, list(args))

    @staticmethod
    def get_function_selector(function_signature: str) -> str:
        """
        Get the 4-byte function selector for a signature.

        Args:
            function_signature: Function signature like "transfer(address,uint256)"

        Returns:
            4-byte selector as hex string with 0x prefix

        Example:
            >>> Contract.get_function_selector("transfer(address,uint256)")
            '0xa9059cbb'
            >>> Contract.get_function_selector("balanceOf(address)")
            '0x70a08231'
        """
        return _get_function_selector(function_signature)

    @staticmethod
    def decode_return_data(data: bytes, output_types: Sequence[str]) -> list[str]:
        """
        Decode return data based on expected output types.

        Args:
            data: Raw return data from eth_call
            output_types: List of output type strings like ["uint256", "address"]

        Returns:
            List of decoded values as strings

        Example:
            >>> decoded = Contract.decode_return_data(
            ...     data=b'...',
            ...     output_types=["uint256", "address"],
            ... )
            >>> balance, owner = decoded
        """
        return _decode_return_data(data, list(output_types))


def get_function_selector(function_signature: str) -> str:
    """
    Get the 4-byte function selector for a signature.

    Args:
        function_signature: Function signature like "transfer(address,uint256)"

    Returns:
        4-byte selector as hex string with 0x prefix

    Example:
        >>> get_function_selector("transfer(address,uint256)")
        '0xa9059cbb'
    """
    return _get_function_selector(function_signature)


def encode_function_call(function_signature: str, args: Sequence[str] | None = None) -> bytes:
    """
    Encode a function call without executing it.

    Args:
        function_signature: Function signature
        args: Function arguments

    Returns:
        Encoded calldata as bytes
    """
    if args is None:
        args = []
    return _encode_function_call(function_signature, list(args))


def decode_return_data(data: bytes, output_types: Sequence[str]) -> list[str]:
    """
    Decode return data based on expected output types.

    Args:
        data: Raw return data from eth_call
        output_types: List of output type strings like ["uint256", "address"]

    Returns:
        List of decoded values as strings
    """
    return _decode_return_data(data, list(output_types))


__all__ = [
    "Contract",
    "decode_return_data",
    "encode_function_call",
    "get_function_selector",
]
