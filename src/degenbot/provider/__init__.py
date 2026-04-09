"""
High-performance Ethereum RPC provider using Alloy.

This module provides a Rust-based provider for fast log fetching and RPC calls.
It replaces web3.py's provider functionality with optimized Rust implementations.

Example:
    >>> from degenbot.provider import AlloyProvider, LogFilter
    >>> provider = AlloyProvider("https://eth-mainnet.example.com")
    >>>
    >>> # Direct property access
    >>> chain_id = provider.chain_id
    >>> block_number = provider.block_number
    >>>
    >>> # Log fetching with LogFilter
    >>> logs = provider.get_logs(
    ...     LogFilter(
    ...         from_block=18_000_000,
    ...         to_block=18_010_000,
    ...         addresses=["0x..."],
    ...     )
    ... )
    >>>
    >>> # Or using keyword arguments
    >>> logs = provider.get_logs(
    ...     from_block=18_000_000,
    ...     to_block=18_010_000,
    ...     addresses=["0x..."],
    ... )
"""

from dataclasses import dataclass, field
from typing import Any, Self, cast

from hexbytes import HexBytes

from degenbot.degenbot_rs import AlloyProvider as _AlloyProvider
from degenbot.provider.interface import (
    EthereumProvider,
    ProviderAdapter,
)
from degenbot.types.aliases import BlockNumber


@dataclass
class LogFilter:
    """
    Filter criteria for log fetching.

    Args:
        from_block: Starting block number (inclusive)
        to_block: Ending block number (inclusive)
        addresses: Contract addresses to filter (optional)
        topics: Event topic signatures, nested by position (optional)

    Example:
        >>> filter = LogFilter(
        ...     from_block=18_000_000,
        ...     to_block=18_010_000,
        ...     addresses=["0xContractAddress..."],
        ...     topics=[["0xTransfer..."]],  # Match first topic
        ... )
    """

    from_block: BlockNumber
    to_block: BlockNumber
    addresses: list[str] = field(default_factory=list)
    topics: list[list[str]] = field(default_factory=list)

    def __post_init__(self) -> None:
        if self.to_block < self.from_block:
            msg = "to_block must be >= from_block"
            raise ValueError(msg)


class AlloyProvider:
    """
    High-performance Ethereum RPC provider using Alloy.

    Replaces web3.py provider for log fetching and basic RPC calls.
    Uses Rust-based HTTP client with connection pooling for optimal performance.

    Args:
        rpc_url: HTTP/HTTPS endpoint URL
        max_retries: Maximum retry attempts (default: 10)
        max_blocks_per_request: Maximum blocks per log request (default: 5000)

    Example:
        >>> provider = AlloyProvider("https://eth-mainnet.example.com")
        >>>
        >>> # Properties
        >>> chain_id = provider.chain_id
        >>> block_number = provider.block_number
        >>>
        >>> # Methods
        >>> block = provider.get_block(18_000_000)
        >>> logs = provider.get_logs(from_block=18_000_000, to_block=18_010_000)
        >>> code = provider.get_code("0x...")
        >>> result = provider.call("0x...", calldata)
    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
    ) -> None:
        self._rpc_url = rpc_url
        self._max_retries = max_retries
        self._max_blocks_per_request = max_blocks_per_request

        # Initialize Rust provider
        self._provider = _AlloyProvider(
            rpc_url=rpc_url,
            max_retries=max_retries,
            max_blocks_per_request=max_blocks_per_request,
        )

    # =========================================================================
    # Properties
    # =========================================================================

    @property
    def rpc_url(self) -> str:
        """Get the RPC URL."""
        return self._rpc_url

    @property
    def chain_id(self) -> int:
        """Get the chain ID."""
        return self._provider.get_chain_id()

    @property
    def block_number(self) -> int:
        """Get the current block number."""
        return self._provider.get_block_number()

    # =========================================================================
    # Methods
    # =========================================================================

    def get_block_number(self) -> int:
        """Get current block number."""
        return self._provider.get_block_number()

    def get_chain_id(self) -> int:
        """Get chain ID."""
        return self._provider.get_chain_id()

    def get_block(self, block_number: int) -> dict[str, Any] | None:
        """Get a block by number.

        Returns:
            Block data as dictionary with HexBytes for hash fields, or None if not found.
        """
        return self._provider.get_block(block_number)

    def get_code(self, address: str, block_number: int | None = None) -> HexBytes:
        """Get contract code at an address.

        Args:
            address: Contract address
            block_number: Block number to get code at (default: latest)

        Returns:
            Contract bytecode as HexBytes
        """
        return cast("HexBytes", self._provider.get_code(address, block_number))

    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> HexBytes:
        """
        Execute an eth_call to a contract.

        Args:
            to: Contract address to call
            data: Calldata bytes (function selector + encoded arguments)
            block_number: Block number to execute call at (default: latest)

        Returns:
            Raw return data from the contract call as HexBytes

        Example:
            >>> # Call ERC20 balanceOf
            >>> selector = bytes.fromhex("70a08231")  # balanceOf(address)
            >>> address = bytes.fromhex("000000000000000000000000" + "1234...")
            >>> calldata = selector + address
            >>> result = provider.call("0xTokenAddress", calldata)
            >>> balance = int.from_bytes(result, "big")
        """
        return cast("HexBytes", self._provider.call(to, data, block_number))

    def get_logs(
        self,
        filter_param: LogFilter | None = None,
        *,
        from_block: BlockNumber | None = None,
        to_block: BlockNumber | None = None,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """
        Fetch event logs with automatic retry and dynamic block sizing.

        Flexible API that accepts either a LogFilter object or individual
        filter parameters as keyword arguments. Returns logs in web3.py
        compatible format.

        Args:
            filter_param: LogFilter object with filter criteria (optional)
            from_block: Starting block number (required if filter_param not provided)
            to_block: Ending block number (required if filter_param not provided)
            addresses: Contract addresses to filter (optional)
            topics: Event topic signatures (optional)

        Returns:
            List of log dictionaries with web3.py-compatible format:
            - address: Contract address
            - topics: List of topic hashes
            - data: Raw log data bytes
            - blockNumber: Block number
            - blockHash: Block hash
            - transactionHash: Transaction hash
            - logIndex: Log index within block

        Raises:
            ValueError: If neither filter_param nor from_block/to_block are provided,
                       or if from_block > to_block

        Example:
            >>> # Using LogFilter
            >>> logs = provider.get_logs(LogFilter(from_block=18_000_000, to_block=18_010_000))
            >>>
            >>> # Using keyword arguments
            >>> logs = provider.get_logs(
            ...     from_block=18_000_000,
            ...     to_block=18_010_000,
            ...     addresses=["0xContract..."],
            ...     topics=[["0xEventSignature..."]],
            ... )
        """
        # Determine filter parameters
        if filter_param is not None:
            # Use LogFilter object
            from_block_val = filter_param.from_block
            to_block_val = filter_param.to_block
            addresses_val = filter_param.addresses or None
            topics_val = filter_param.topics or None
        else:
            # Use keyword arguments
            if from_block is None or to_block is None:
                msg = "Either filter_param or from_block/to_block must be provided"
                raise ValueError(msg)
            from_block_val = from_block
            to_block_val = to_block
            addresses_val = addresses
            topics_val = topics

        # Validate block range
        if from_block_val > to_block_val:
            msg = f"from_block ({from_block_val}) must be <= to_block ({to_block_val})"
            raise ValueError(msg)

        # Call Rust provider's get_logs with keyword arguments
        # The Rust provider now returns list[dict[str, Any]] with HexBytes for hex fields
        return self._provider.get_logs(
            from_block=from_block_val,
            to_block=to_block_val,
            addresses=addresses_val,
            topics=topics_val,
        )

    def close(self) -> None:
        """Close connection pool and release resources."""
        self._provider.close()

    def is_connected(self) -> bool:  # noqa: PLR6301
        """Check if the provider is connected.

        For AlloyProvider, we assume connection is valid if the provider was created.
        """
        return True

    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int:
        """Get the balance of an address in wei.

        Not yet implemented for AlloyProvider.
        """
        msg = "get_balance not implemented for AlloyProvider"
        raise NotImplementedError(msg)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Args:
            address: Contract address
            position: Storage slot position (supports large values like mapping slots)
            block_number: Block number to get storage at (default: latest)

        Returns:
            Storage value at the position as HexBytes (32 bytes)
        """
        return cast("HexBytes", self._provider.get_storage_at(address, position, block_number))

    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Not yet implemented for AlloyProvider.
        """
        msg = "get_transaction_count not implemented for AlloyProvider"
        raise NotImplementedError(msg)

    def __enter__(self) -> Self:
        """Context manager entry."""
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: object,
    ) -> None:
        """Context manager exit."""
        self.close()


__all__ = [
    "AlloyProvider",
    "EthereumProvider",
    "LogFilter",
    "ProviderAdapter",
]
