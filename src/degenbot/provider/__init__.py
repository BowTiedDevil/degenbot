"""
High-performance Ethereum RPC provider using Alloy.

This module provides a Rust-based provider for fast log fetching and RPC calls.
It replaces web3.py's provider functionality with optimized Rust implementations.

Example:
    >>> from degenbot.provider import AlloyProvider, LogFilter
    >>> provider = AlloyProvider("https://eth-mainnet.example.com")
    >>> logs = provider.get_logs(
    ...     filter=LogFilter(
    ...         from_block=18_000_000,
    ...         to_block=18_010_000,
    ...         addresses=["0x..."],
    ...     )
    ... )
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Self, cast

from degenbot._rs import AlloyProvider as _AlloyProvider

if TYPE_CHECKING:
    from degenbot.types.aliases import BlockNumber


@dataclass(frozen=True)
class LogEntry:
    """
    Typed log entry from Ethereum RPC.

    Attributes:
        address: Contract address that emitted the log
        topics: List of topic hashes (first is event signature)
        data: Raw log data bytes
        block_number: Block number (None if pending)
        block_hash: Block hash (None if pending)
        transaction_hash: Transaction hash (None if pending)
        log_index: Log index within block (None if pending)
    """

    address: str
    topics: list[str]
    data: bytes
    block_number: int | None
    block_hash: str | None
    transaction_hash: str | None
    log_index: int | None


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
        max_connections: Maximum concurrent connections (default: 10)
        timeout: Request timeout in seconds (default: 30.0)
        max_retries: Maximum retry attempts (default: 10)
        max_blocks_per_request: Maximum blocks per log request (default: 5000)

    Example:
        >>> provider = AlloyProvider("https://eth-mainnet.example.com")
        >>> logs = provider.get_logs(filter=LogFilter(from_block=18_000_000, to_block=18_010_000))
    """

    def __init__(
        self,
        rpc_url: str,
        max_connections: int = 10,
        timeout: float = 30.0,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
    ) -> None:
        self._rpc_url = rpc_url
        self._max_connections = max_connections
        self._timeout = timeout
        self._max_retries = max_retries
        self._max_blocks_per_request = max_blocks_per_request

        # Initialize Rust provider
        self._provider = _AlloyProvider(
            rpc_url=rpc_url,
            max_connections=max_connections,
            timeout=timeout,
            max_retries=max_retries,
            max_blocks_per_request=max_blocks_per_request,
        )

    def get_logs(
        self,
        filter: LogFilter,
    ) -> list[LogEntry]:
        """
        Fetch event logs with automatic retry and dynamic block sizing.

        Replaces: fetch_logs_retrying()

        Args:
            filter: LogFilter criteria

        Returns:
            List of LogEntry objects with typed fields for IDE support
        """
        raw_logs = self._provider.get_logs(
            from_block=filter.from_block,
            to_block=filter.to_block,
            addresses=filter.addresses or None,
            topics=filter.topics or None,
        )

        return [
            LogEntry(
                address=log["address"],
                topics=log["topics"],
                data=log["data"],
                block_number=log.get("blockNumber"),
                block_hash=log.get("blockHash"),
                transaction_hash=log.get("transactionHash"),
                log_index=log.get("logIndex"),
            )
            for log in raw_logs
        ]

    def get_block_number(self) -> int:
        """Get current block number."""
        return cast("int", self._provider.get_block_number())

    def get_chain_id(self) -> int:
        """Get chain ID."""
        return cast("int", self._provider.get_chain_id())

    def close(self) -> None:
        """Close connection pool and release resources."""
        self._provider.close()

    @property
    def rpc_url(self) -> str:
        """Get the RPC URL."""
        return self._rpc_url

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
    "LogEntry",
    "LogFilter",
]
